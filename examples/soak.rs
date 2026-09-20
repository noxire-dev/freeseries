//! Glitch test for a running engine: plays a continuous quiet 1 kHz tone into one endpoint and
//! watches the loopback of another in 10 ms blocks. A block whose level sags is an underrun (the
//! engine ran dry and played silence); a step in the tone's phase between blocks means samples were
//! dropped or inserted. Clock drift between two devices only slides the phase slowly, so it passes.
//!
//! cargo run --release --example soak -- <play#> <measure#> [seconds]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = |n: usize| std::env::args().nth(n);
    let devices: Vec<cpal::Device> = cpal::default_host().output_devices()?.collect();
    let usage = "usage: soak <play#> <measure#> [seconds]";
    let play = devices.get(arg(1).ok_or(usage)?.parse::<usize>()?).ok_or("no such play device")?;
    let listen = devices.get(arg(2).ok_or(usage)?.parse::<usize>()?).ok_or("no such measure device")?;
    let seconds: u64 = arg(3).map(|s| s.parse()).transpose()?.unwrap_or(20);

    let heard = listen.default_output_config()?;
    let (in_ch, block) = (heard.channels() as usize, heard.sample_rate() as usize / 100);
    let tone = std::f64::consts::TAU * 1000.0 / heard.sample_rate() as f64;
    let levels = Arc::new(Mutex::new(Vec::<(f32, f64)>::new())); // (RMS, tone phase) of L per 10 ms block
    let sink = levels.clone();
    let (mut sum, mut count, mut re, mut im, mut t) = (0f32, 0usize, 0f64, 0f64, 0u64);
    let capture = listen.build_input_stream(
        heard.config(),
        move |data: &[f32], _: &_| {
            for frame in data.chunks_exact(in_ch) {
                sum += frame[0] * frame[0];
                re += frame[0] as f64 * (tone * t as f64).cos();
                im -= frame[0] as f64 * (tone * t as f64).sin();
                t += 1;
                count += 1;
                if count == block {
                    sink.lock().unwrap().push(((sum / block as f32).sqrt(), im.atan2(re)));
                    (sum, count, re, im) = (0.0, 0, 0.0, 0.0);
                }
            }
        },
        |_| {},
        None,
    )?;

    let cfg = play.default_output_config()?;
    let (out_ch, w) = (cfg.channels() as usize, std::f64::consts::TAU * 1000.0 / cfg.sample_rate() as f64);
    let mut n = 0u64;
    let playback = play.build_output_stream(
        cfg.config(),
        move |data: &mut [f32], _: &_| {
            for frame in data.chunks_exact_mut(out_ch) {
                frame.fill(0.05 * (w * n as f64).sin() as f32);
                n += 1;
            }
        },
        |e| eprintln!("playback error: {e}"),
        None,
    )?;

    playback.play()?;
    capture.play()?;
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    drop((capture, playback));

    let levels = levels.lock().unwrap();
    let steady = &levels[levels.len().min(100)..]; // skip the first second while everything starts
    let mut sorted: Vec<f32> = steady.iter().map(|b| b.0).collect();
    sorted.sort_by(f32::total_cmp);
    let nominal = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
    let dropouts: Vec<usize> = steady.iter().enumerate().filter(|(_, b)| b.0 < nominal * 0.8).map(|(i, _)| i).collect();
    let jumps: Vec<usize> = steady
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            let step = (w[1].1 - w[0].1).rem_euclid(std::f64::consts::TAU);
            step.min(std::f64::consts::TAU - step) > 0.3 // 0.3 rad at 1 kHz = ~2 samples
        })
        .map(|(i, _)| i + 1)
        .collect();
    println!("{} blocks of 10 ms, nominal level {:.1} dBFS", steady.len(), 20.0 * nominal.max(1e-9).log10());
    for (label, hits) in [("level dropouts", &dropouts), ("phase jumps", &jumps)] {
        let at: Vec<String> = hits.iter().take(15).map(|i| format!("{:.2}s", 1.0 + *i as f32 / 100.0)).collect();
        println!("{label}: {} {}", hits.len(), at.join(" "));
    }
    Ok(())
}
