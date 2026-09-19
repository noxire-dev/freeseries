mod eq;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::error::Error;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let devices: Vec<cpal::Device> = cpal::default_host().output_devices()?.collect();

    match args.first().map(String::as_str) {
        Some("run") if args.len() >= 3 => {
            let from = devices.get(args[1].parse::<usize>()?).ok_or("no such source device")?.id()?;
            let to = devices.get(args[2].parse::<usize>()?).ok_or("no such target device")?.id()?;
            let preset = match args.get(3) {
                Some(path) => eq::parse(&std::fs::read_to_string(path)?),
                None => eq::Preset::default(),
            };
            run(&from, &to, &preset)
        }
        _ => {
            for (i, d) in devices.iter().enumerate() {
                let c = d.default_output_config()?;
                println!("{i}: {} ({} ch, {} Hz, {:?})", d.description()?.name(), c.channels(), c.sample_rate(), c.sample_format());
            }
            println!("\nusage: freeseries run <source#> <target#> [ParametricEQ.txt]");
            Ok(())
        }
    }
}

/// Keeps the capture -> EQ -> playback chain alive. Windows invalidates every stream on an endpoint
/// when its audio graph changes (spatial sound toggled, format changed, device replugged), so any
/// stream error other than an xrun tears both streams down and reopens them.
fn run(from: &cpal::DeviceId, to: &cpal::DeviceId, preset: &eq::Preset) -> Result<(), Box<dyn Error>> {
    let (lost_tx, lost_rx) = mpsc::channel::<()>();
    loop {
        match open(from, to, preset, &lost_tx) {
            Ok(_streams) => {
                let _ = lost_rx.recv(); // streams stay alive until one reports a fatal error
                eprintln!("stream lost, reconnecting...");
            }
            Err(e) => eprintln!("cannot open streams ({e}), retrying..."),
        }
        std::thread::sleep(Duration::from_millis(500));
        while lost_rx.try_recv().is_ok() {} // both streams usually die together; one reconnect is enough
    }
}

/// Loopback-captures everything played into `from`, EQs it, and plays it on `to`.
fn open(
    from: &cpal::DeviceId,
    to: &cpal::DeviceId,
    preset: &eq::Preset,
    lost: &mpsc::Sender<()>,
) -> Result<(cpal::Stream, cpal::Stream), Box<dyn Error>> {
    let host = cpal::default_host();
    let from = host.device_by_id(from).ok_or("source device is gone")?;
    let to = host.device_by_id(to).ok_or("target device is gone")?;
    let in_cfg = from.default_output_config()?;
    let out_cfg = to.default_output_config()?;
    if in_cfg.sample_format() != cpal::SampleFormat::F32 || out_cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err("both devices must run f32 (WASAPI shared mode normally does)".into());
    }
    // ponytail: no resampler, both endpoints must share a rate. Add one when users hit 44.1k-only devices.
    if in_cfg.sample_rate() != out_cfg.sample_rate() {
        return Err(format!("sample rates differ: {} vs {} Hz", in_cfg.sample_rate(), out_cfg.sample_rate()).into());
    }
    let (in_ch, out_ch) = (in_cfg.channels() as usize, out_cfg.channels() as usize);
    let rate = out_cfg.sample_rate() as usize;
    println!("{in_ch} ch -> {out_ch} ch @ {rate} Hz, {} EQ bands. Ctrl+C to stop.", preset.bands.len());

    // ponytail: mutex'd queue between two audio callbacks, and no clock-drift compensation between
    // the two devices (overflow drops oldest, underrun plays silence). Swap for a lock-free ring +
    // async resampler if it glitches in practice.
    let queue = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(rate * out_ch)));
    let max_queued = rate * out_ch / 10; // 100 ms ceiling on added latency

    let on_error = |side: &'static str| {
        let lost = lost.clone();
        move |e: cpal::Error| {
            eprintln!("{side} error: {e}");
            if e.kind() != cpal::ErrorKind::Xrun {
                let _ = lost.send(());
            }
        }
    };

    let mut eq = eq::Eq::new(preset, rate as f64, out_ch);
    let mut scratch: Vec<f32> = Vec::new();
    let producer = queue.clone();
    let capture = from.build_input_stream(
        in_cfg.config(),
        move |data: &[f32], _: &_| {
            scratch.clear();
            for frame in data.chunks_exact(in_ch) {
                // ponytail: channel mismatch keeps front L/R only; fine while the source is a
                // spatial-sound endpoint, which always hands us binaural stereo.
                scratch.extend((0..out_ch).map(|c| frame[c.min(in_ch - 1)]));
            }
            eq.process(&mut scratch);
            let mut q = producer.lock().unwrap();
            q.extend(scratch.iter());
            let excess = q.len().saturating_sub(max_queued);
            q.drain(..excess);
        },
        on_error("capture"),
        None,
    )?;

    let playback = to.build_output_stream(
        out_cfg.config(),
        move |data: &mut [f32], _: &_| {
            let mut q = queue.lock().unwrap();
            for s in data.iter_mut() {
                *s = q.pop_front().unwrap_or(0.0);
            }
        },
        on_error("playback"),
        None,
    )?;

    capture.play()?;
    playback.play()?;
    Ok((capture, playback))
}
