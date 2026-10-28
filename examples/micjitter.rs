//! How evenly a microphone delivers its packets. A wireless headset's mic can hold a packet back
//! and then send two; the mic path keeps the cable only 20 ms ahead, so a packet later than that
//! is a gap in the voice. This listens for a while and prints the late ones.
//!
//! cargo run --release --example micjitter -- <mic#> [seconds]      (numbers: `micspike` lists them)

#![allow(unsafe_op_in_unsafe_fn)]

use std::time::Instant;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index: u32 = std::env::args().nth(1).ok_or("mic#")?.parse()?;
    let seconds: f64 = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(60.0);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let all: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = all.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?.Item(index)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let rate = (*format).nSamplesPerSec as f64;
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, format, None)?;
        let reader: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let started = Instant::now();
        let (mut last, mut packets, mut frames, mut gaps, mut discontinuities) = (None::<Instant>, 0u32, 0u64, Vec::new(), 0u32);
        while started.elapsed().as_secs_f64() < seconds {
            std::thread::sleep(std::time::Duration::from_millis(1));
            let mut any = false;
            while reader.GetNextPacketSize()? > 0 {
                let (mut data, mut got, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                reader.GetBuffer(&mut data, &mut got, &mut flags, None, None)?;
                if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                    discontinuities += 1;
                }
                frames += got as u64;
                packets += 1;
                any = true;
                reader.ReleaseBuffer(got)?;
            }
            if any {
                let now = Instant::now();
                if let Some(before) = last {
                    let ms = now.duration_since(before).as_secs_f64() * 1000.0;
                    if ms > 15.0 {
                        gaps.push((started.elapsed().as_secs_f64(), ms));
                    }
                }
                last = Some(now);
            }
        }
        client.Stop()?;
        let elapsed = started.elapsed().as_secs_f64();
        println!("{elapsed:.1} s: {packets} packets, {frames} frames = {:.1} s of audio at {rate} Hz ({:+.0} ppm against the wall clock), {discontinuities} marked as discontinuities", frames as f64 / rate, (frames as f64 / rate / elapsed - 1.0) * 1e6);
        println!("{} waits longer than 15 ms between packets:", gaps.len());
        let mut buckets = [0u32; 6]; // 15-20, 20-30, 30-50, 50-100, 100-200, 200+
        for &(_, ms) in &gaps {
            let b = if ms < 20.0 { 0 } else if ms < 30.0 { 1 } else if ms < 50.0 { 2 } else if ms < 100.0 { 3 } else if ms < 200.0 { 4 } else { 5 };
            buckets[b] += 1;
        }
        for (label, n) in ["15-20 ms", "20-30 ms (a gap in the voice at the current cushion)", "30-50 ms", "50-100 ms", "100-200 ms", "200 ms and more"].iter().zip(buckets) {
            println!("  {n:>5}  {label}");
        }
        for (at, ms) in gaps.iter().filter(|g| g.1 >= 20.0).take(12) {
            println!("  at {at:6.1} s: {ms:.0} ms");
        }
    }
    Ok(())
}
