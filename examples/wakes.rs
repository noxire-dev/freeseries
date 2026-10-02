//! How regularly an output wakes its client. Opens a silent, event-driven shared stream on each
//! device named (the same kind the engine opens), one "Pro Audio" thread per device, and prints
//! every wake that came late. Nothing audible is played.
//!
//! The engine drains its loopback captures on the output's wake. The capture buffer is 22 ms,
//! so a wake more than about 20 ms after the last one loses audio: a "capture glitch" in the log.
//! Run this next to the installed app and compare the times. Late on one device only: that
//! device (or its USB path). Late on all at once: the PC stalled.
//!
//! cargo run --release --example wakes -- <seconds> <device#> [<device#>...]

#![allow(unsafe_op_in_unsafe_fn)]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject};

const LATE_MS: f64 = 15.0;

fn clock() -> String {
    let s = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    format!("{:02}:{:02}:{:02}.{:03} UTC", s / 3_600_000 % 24, s / 60_000 % 60, s / 1000 % 60, s % 1000)
}

unsafe fn watch(index: u32, seconds: u64) -> windows::core::Result<()> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let all: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let d = all.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?.Item(index)?;
    let name = d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&PKEY_Device_FriendlyName)).map(|v| v.to_string()).unwrap_or_default();
    let client: IAudioClient = d.Activate(CLSCTX_ALL, None)?;
    let format = client.GetMixFormat()?;
    let channels = (*format).nChannels as usize;
    client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0, 0, format, None)?;
    let event = CreateEventW(None, false, false, PCWSTR::null())?;
    client.SetEventHandle(event)?;
    let writer: IAudioRenderClient = client.GetService()?;
    let size = client.GetBufferSize()?;
    let mut task = 0u32;
    let _ = AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task);
    client.Start()?;

    let started = Instant::now();
    let (mut last, mut wakes, mut late, mut worst) = (Instant::now(), 0u64, 0u64, 0f64);
    while started.elapsed() < Duration::from_secs(seconds) {
        if WaitForSingleObject(event, 1000) != WAIT_OBJECT_0 {
            println!("{}  {name}: no wake for a whole second", clock());
            continue;
        }
        let gap = last.elapsed().as_secs_f64() * 1000.0;
        last = Instant::now();
        wakes += 1;
        if wakes > 100 && gap > LATE_MS {
            late += 1;
            worst = worst.max(gap);
            println!("{}  {name}: woke {gap:.1} ms after the last one", clock());
        }
        // Keep the stream fed with silence, the way a real client keeps it fed with sound.
        let space = size - client.GetCurrentPadding()?;
        if space > 0 {
            let buffer = writer.GetBuffer(space)?;
            std::slice::from_raw_parts_mut(buffer as *mut f32, space as usize * channels).fill(0.0);
            writer.ReleaseBuffer(space, 0)?;
        }
    }
    client.Stop()?;
    println!("{name}: {wakes} wakes in {seconds} s, {late} later than {LATE_MS} ms, worst {worst:.1} ms");
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((seconds, devices)) = args.split_first().and_then(|(s, d)| Some((s.parse::<u64>().ok()?, d))) else {
        return eprintln!("usage: wakes <seconds> <device#> [<device#>...]   (numbers as `freeseries` lists them)");
    };
    let threads: Vec<_> = devices
        .iter()
        .filter_map(|d| d.parse::<u32>().ok())
        .map(|index| std::thread::spawn(move || unsafe { watch(index, seconds) }.unwrap_or_else(|e| println!("device {index}: {e}"))))
        .collect();
    threads.into_iter().for_each(|t| t.join().unwrap());
}
