//! Does loopback capture on an endpoint contain Windows spatial sound (Sonic/Atmos/DTS) rendering?
//! Plays quiet tones into the back-left channel of a 7.1 stream and measures, at the tone frequency
//! only (so other playing audio doesn't pollute it), the L/R level gap and phase difference in the
//! loopback. A matrix downmix gives a constant gap and 0/180 deg phase; an HRTF gives neither.
//!
//! cargo run --example probe -- <device#>

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

const BACK_LEFT: usize = 4; // FL FR FC LFE BL BR SL SR

/// Returns (L level dBFS, L-R gap dB, L-R phase deg) of the loopback while `freq` plays in back-left.
fn measure(device: &cpal::Device, freq: f64, out_ch: usize, target: usize) -> Result<(f64, f64, f64), Box<dyn std::error::Error>> {
    let mix = device.default_output_config()?;
    let (rate, in_ch) = (mix.sample_rate() as f64, mix.channels() as usize);
    let w = std::f64::consts::TAU * freq / rate;
    // Single-bin DFT per channel: [re_l, im_l, re_r, im_r], plus frame count.
    let acc = Arc::new(Mutex::new(([0f64; 4], 0u64)));
    let sink = acc.clone();
    let capture = device.build_input_stream(
        mix.config(),
        move |data: &[f32], _: &_| {
            let mut a = sink.lock().unwrap();
            for frame in data.chunks_exact(in_ch) {
                let (sin, cos) = (w * a.1 as f64).sin_cos();
                a.0[0] += frame[0] as f64 * cos;
                a.0[1] -= frame[0] as f64 * sin;
                a.0[2] += frame[1] as f64 * cos;
                a.0[3] -= frame[1] as f64 * sin;
                a.1 += 1;
            }
        },
        |_| {}, // overruns don't matter here
        None,
    )?;
    let mut n = 0u64;
    let playback = device.build_output_stream(
        cpal::StreamConfig { channels: out_ch as u16, ..mix.config() },
        move |data: &mut [f32], _: &_| {
            for frame in data.chunks_exact_mut(out_ch) {
                frame.fill(0.0);
                frame[target] = 0.1 * (w * n as f64).sin() as f32;
                n += 1;
            }
        },
        |e| eprintln!("playback error: {e}"),
        None,
    )?;
    playback.play()?;
    std::thread::sleep(std::time::Duration::from_millis(300)); // let the tone settle first
    capture.play()?;
    std::thread::sleep(std::time::Duration::from_millis(1500));
    drop((capture, playback));
    let (a, frames) = *acc.lock().unwrap();
    let (l, r) = (a[0].hypot(a[1]), a[2].hypot(a[3]));
    let phase = (a[1].atan2(a[0]) - a[3].atan2(a[2])).to_degrees().rem_euclid(360.0);
    let l_dbfs = 20.0 * (2.0 * l / frames.max(1) as f64).max(1e-9).log10();
    Ok((l_dbfs, 20.0 * (l / r.max(1e-12)).log10(), phase))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index: usize = std::env::args().nth(1).ok_or("usage: probe <device#>")?.parse()?;
    let device = cpal::default_host().output_devices()?.nth(index).ok_or("no such device")?;
    println!("{}", device.description()?.name());
    // Stereo-left is the control: spatial sound leaves stereo streams alone, so anything
    // frequency-dependent there comes from the endpoint's own effects, not the spatial renderer.
    for (label, out_ch, target) in [("stereo L (control)", 2, 0), ("7.1 back-left", 8, BACK_LEFT)] {
        println!("  {label}");
        for freq in [250.0, 500.0, 1000.0, 2000.0, 4000.0] {
            let (l, gap, phase) = measure(&device, freq, out_ch, target)?;
            println!("  {freq:>6} Hz: L {l:6.1} dBFS   L-R gap {gap:5.1} dB   L-R phase {phase:5.1} deg");
        }
    }
    Ok(())
}
