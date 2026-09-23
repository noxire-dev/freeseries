//! How much delay does a running engine add? Plays quiet noise into the virtual device and records
//! the loopback of both that device and the output the engine plays on. Both recordings carry
//! WASAPI's own capture timestamps (same clock), so cross-correlating them gives the time audio
//! spends inside freeseries: capture buffering + queue + render buffering.
//!
//! Not included: what any output adds on its own (DAC, a wireless headset's radio link). Those are
//! the same with or without freeseries.
//!
//! cargo run --release --example latency -- <virtual#> <output#> [settle seconds, default 0.4] [render]
//!
//! With `render` the reference is not the source device's loopback but the noise itself, stamped
//! with the time each buffer was due to play: the whole way from "an app plays" to "the output
//! plays it". Played straight into a device (same number twice) that is the device's own delay;
//! through the engine it is the total, and the difference is what FreeSeries adds.

//! Do not run this right before `soak.rs`: for a while after this tool's noise stream closes, the
//! virtual device's loopback delivers a few phase slips (seen 2026-09-22 with no engine in the path).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Recording {
    mono: Vec<f32>,
    /// Capture time of the first sample, nanoseconds on the audio clock.
    start_ns: Option<u128>,
    /// Packets whose timestamp doesn't follow on from the previous one: the stream wasn't continuous.
    gaps: u32,
}

fn record(device: &cpal::Device) -> Result<(cpal::Stream, Arc<Mutex<Recording>>, f64), Box<dyn std::error::Error>> {
    let cfg = device.default_output_config()?;
    let (channels, rate) = (cfg.channels() as usize, cfg.sample_rate() as f64);
    let rec = Arc::new(Mutex::new(Recording::default()));
    let sink = rec.clone();
    let stream = device.build_input_stream(
        cfg.config(),
        move |data: &[f32], info: &_| {
            let mut r = sink.lock().unwrap();
            let at = info.timestamp().capture.as_nanos();
            match r.start_ns {
                None => r.start_ns = Some(at),
                Some(start) => {
                    let expected = start + (r.mono.len() as f64 / rate * 1e9) as u128;
                    if at.abs_diff(expected) > 5_000_000 {
                        r.gaps += 1;
                    }
                }
            }
            r.mono.extend(data.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32));
        },
        |_| {},
        None,
    )?;
    Ok((stream, rec, rate))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = |n: usize| std::env::args().nth(n);
    let usage = "usage: latency <virtual#> <output#> [settle] [render]";
    let from_render = std::env::args().any(|a| a == "render");
    let devices: Vec<cpal::Device> = cpal::default_host().output_devices()?.collect();
    let source = devices.get(arg(1).ok_or(usage)?.parse::<usize>()?).ok_or("no such virtual device")?;
    let output = devices.get(arg(2).ok_or(usage)?.parse::<usize>()?).ok_or("no such output device")?;
    println!("into: {}\nout of: {}", source.description()?.name(), output.description()?.name());

    let (out_stream, heard_out, out_rate) = record(output)?;
    let heard_in = Arc::new(Mutex::new(Recording::default()));
    let (in_stream, rate) = if from_render { (None, out_rate) } else {
        let (stream, rec, rate) = record(source)?;
        heard_in.lock().unwrap().start_ns = None;
        (Some((stream, rec)), rate)
    };
    let heard_in = in_stream.as_ref().map_or(heard_in.clone(), |(_, rec)| rec.clone());
    if rate != out_rate {
        return Err("the two devices run at different sample rates".into());
    }

    // Quiet white noise: it correlates with itself at exactly one lag, and nothing else.
    let cfg = source.default_output_config()?;
    let channels = cfg.channels() as usize;
    let mut seed = 0x2545_F491u32;
    let reference = if from_render { Some(heard_in.clone()) } else { None };
    let noise = source.build_output_stream(
        cfg.config(),
        move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
            for frame in data.chunks_exact_mut(channels) {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                frame.fill((seed as f32 / u32::MAX as f32 - 0.5) * 0.06);
            }
            if let Some(r) = &reference {
                let mut r = r.lock().unwrap();
                let at = info.timestamp().playback.as_nanos();
                match r.start_ns {
                    None => r.start_ns = Some(at),
                    Some(start) => {
                        let expected = start + (r.mono.len() as f64 / rate * 1e9) as u128;
                        if at.abs_diff(expected) > 5_000_000 {
                            r.gaps += 1;
                        }
                    }
                }
                r.mono.extend(data.chunks_exact(channels).map(|f| f[0]));
            }
        },
        |e| eprintln!("playback error: {e}"),
        None,
    )?;
    noise.play()?;
    // Let it reach both loopbacks before recording. A channel that was silent wakes with extra delay
    // and eases it out over a few seconds: pass a longer settle time to measure where it ends up.
    let settle: f32 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(0.4);
    std::thread::sleep(Duration::from_secs_f32(settle));
    if let Some((stream, _)) = &in_stream {
        stream.play()?;
    }
    out_stream.play()?;
    std::thread::sleep(Duration::from_secs(4));
    drop((in_stream, out_stream, noise));

    let (a, b) = (heard_in.lock().unwrap(), heard_out.lock().unwrap());
    let (Some(a_start), Some(b_start)) = (a.start_ns, b.start_ns) else {
        return Err("one of the devices delivered no audio (is the engine running on that output?)".into());
    };
    if a.gaps + b.gaps > 0 {
        println!("warning: {} + {} timestamp gaps, the result is less trustworthy", a.gaps, b.gaps);
    }

    // Put both on one timeline, then decimate by 4 (12 kHz, 0.083 ms per step) to keep the search quick.
    const DECIMATE: usize = 4;
    let offset = ((b_start as f64 - a_start as f64) / 1e9 * rate).round() as i64; // b's first sample, in a's sample numbers
    let decimate = |x: &[f32]| -> Vec<f32> { x.chunks_exact(DECIMATE).map(|c| c.iter().sum::<f32>() / DECIMATE as f32).collect() };
    let (a, b) = (decimate(&a.mono), decimate(&b.mono));
    let offset = offset as f64 / DECIMATE as f64;
    let step_ms = DECIMATE as f64 / rate * 1000.0;

    let correlation = |lag: i64| -> f64 {
        // b[j] was captured at a-time (j + offset); it should equal a[i] with j + offset = i + lag.
        let mut sum = 0.0;
        for (i, &x) in a.iter().enumerate() {
            let j = i as f64 + lag as f64 - offset;
            if j >= 0.0 && (j as usize) < b.len() {
                sum += x as f64 * b[j as usize] as f64;
            }
        }
        sum
    };
    let lags: Vec<i64> = ((-50.0 / step_ms) as i64..(400.0 / step_ms) as i64).collect();
    let scores: Vec<f64> = lags.iter().map(|&l| correlation(l)).collect();
    let best = (0..scores.len()).max_by(|&x, &y| scores[x].abs().total_cmp(&scores[y].abs())).unwrap();
    // How much the winner stands out from everything more than 2 ms away from it.
    let runner_up = scores.iter().enumerate().filter(|(i, _)| i.abs_diff(best) as f64 * step_ms > 2.0).map(|(_, s)| s.abs()).fold(0.0, f64::max);

    println!("added latency: {:.1} ms", lags[best] as f64 * step_ms);
    println!("confidence: the match is {:.1}x stronger than any other lag", scores[best].abs() / runner_up.max(1e-12));
    Ok(())
}
