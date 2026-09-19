mod eq;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::error::Error;
use std::sync::{Arc, Mutex};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let devices: Vec<cpal::Device> = cpal::default_host().output_devices()?.collect();

    match args.first().map(String::as_str) {
        Some("run") if args.len() >= 3 => {
            let from = devices.get(args[1].parse::<usize>()?).ok_or("no such source device")?;
            let to = devices.get(args[2].parse::<usize>()?).ok_or("no such target device")?;
            let preset = match args.get(3) {
                Some(path) => eq::parse(&std::fs::read_to_string(path)?),
                None => eq::Preset::default(),
            };
            run(from, to, &preset)
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

/// Loopback-captures everything played into `from`, EQs it, and plays it on `to`.
fn run(from: &cpal::Device, to: &cpal::Device, preset: &eq::Preset) -> Result<(), Box<dyn Error>> {
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

    let mut eq = eq::Eq::new(preset, rate as f64, out_ch);
    let mut scratch: Vec<f32> = Vec::new();
    let producer = queue.clone();
    let capture = from.build_input_stream(
        in_cfg.config(),
        move |data: &[f32], _: &_| {
            scratch.clear();
            for frame in data.chunks_exact(in_ch) {
                // ponytail: channel mismatch keeps front L/R only. The HRTF downmix replaces this.
                scratch.extend((0..out_ch).map(|c| frame[c.min(in_ch - 1)]));
            }
            eq.process(&mut scratch);
            let mut q = producer.lock().unwrap();
            q.extend(scratch.iter());
            let excess = q.len().saturating_sub(max_queued);
            q.drain(..excess);
        },
        |e| eprintln!("capture error: {e}"),
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
        |e| eprintln!("playback error: {e}"),
        None,
    )?;

    capture.play()?;
    playback.play()?;
    std::thread::park();
    Ok(())
}
