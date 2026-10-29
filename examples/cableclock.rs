//! How fast an output's clock runs against the wall clock, in ppm. The mic path keeps the cable
//! fed from the mic's clock; if the cable eats faster than the mic delivers, the feed stretches
//! the voice by the difference. This plays silence into an output and watches its audio clock.
//!
//! cargo run --release --example cableclock -- <out#> [seconds]      (numbers: `cargo run` lists them)

#![allow(unsafe_op_in_unsafe_fn)]

use std::time::Instant;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index: u32 = std::env::args().nth(1).ok_or("out#")?.parse()?;
    let seconds: f64 = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(60.0);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let all: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = all.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?.Item(index)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let (rate, channels) = ((*format).nSamplesPerSec as f64, (*format).nChannels as usize);
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 1_000_000, 0, format, None)?;
        let writer: IAudioRenderClient = client.GetService()?;
        let clock: IAudioClock = client.GetService()?;
        let size = client.GetBufferSize()?;
        let freq = clock.GetFrequency()? as f64;
        client.Start()?;
        let started = Instant::now();
        let (mut pos0, mut qpc0) = (0u64, 0u64);
        clock.GetPosition(&mut pos0, Some(&mut qpc0))?;
        let mut written = 0u64;
        let mut last_report = 0.0;
        while started.elapsed().as_secs_f64() < seconds {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let space = size - client.GetCurrentPadding()?;
            if space > 0 {
                let buffer = writer.GetBuffer(space)?;
                std::ptr::write_bytes(buffer, 0, space as usize * channels * 4);
                writer.ReleaseBuffer(space, 0)?;
                written += space as u64;
            }
            let elapsed = started.elapsed().as_secs_f64();
            if elapsed - last_report >= 10.0 {
                last_report = elapsed;
                let (mut pos, mut qpc) = (0u64, 0u64);
                clock.GetPosition(&mut pos, Some(&mut qpc))?;
                let device_s = (pos - pos0) as f64 / freq;
                let wall_s = (qpc - qpc0) as f64 / 1e7;
                println!("{elapsed:5.1} s: device clock {device_s:.4} s over {wall_s:.4} s of wall clock = {:+.0} ppm", (device_s / wall_s - 1.0) * 1e6);
            }
        }
        client.Stop()?;
        println!("{} ch at {rate} Hz, clock frequency {freq}, {written} frames of silence written", channels);
    }
    Ok(())
}
