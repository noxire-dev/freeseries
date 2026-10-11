//! Delay through the mic path. Plays a 1 ms click once a second into an output and listens on a
//! capture endpoint for it, both timed on the same performance counter: the click's play time
//! from the output's audio clock, the arrival from the capture packet's QPC stamp.
//!
//! With the mic session running from that output's loopback into the cable
//! (`freeseries mic o<out#> <cable#>`), listening on CABLE Output measures: loopback capture ->
//! RNNoise -> cable input -> cable -> cable output. A real mic replaces the loopback with its own
//! USB delay, which every program pays alike.
//!
//! cargo run --release --example miclatency -- <click out#> <listen mic#> [clicks]
//! (out# as `freeseries` lists outputs, mic# as `freeseries mics` lists microphones)

#![allow(unsafe_op_in_unsafe_fn)]

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

const CLICK_FRAMES: usize = 48; // 1 ms at 48 kHz
const LEVEL: f32 = 0.5;

fn qpc_hz() -> f64 {
    let mut f = 0i64;
    unsafe { QueryPerformanceFrequency(&mut f).ok().unwrap() };
    f as f64
}

/// Plays the clicks; returns each click's play time in seconds of QPC.
unsafe fn player(index: u32, clicks: usize, stop: Arc<AtomicBool>, times: Arc<Mutex<Vec<f64>>>) -> windows::core::Result<()> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let all: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let d = all.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?.Item(index)?;
    let client: IAudioClient = d.Activate(CLSCTX_ALL, None)?;
    let format = client.GetMixFormat()?;
    let (rate, channels) = ((*format).nSamplesPerSec as f64, (*format).nChannels as usize);
    client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0, 0, format, None)?;
    let event = CreateEventW(None, false, false, PCWSTR::null())?;
    client.SetEventHandle(event)?;
    let writer: IAudioRenderClient = client.GetService()?;
    let clock: IAudioClock = client.GetService()?;
    let clock_hz = clock.GetFrequency()? as f64;
    let size = client.GetBufferSize()?;
    client.Start()?;
    let (mut written, mut next_click, mut done) = (0u64, rate as u64, 0usize); // frames
    while done < clicks && !stop.load(Relaxed) {
        if WaitForSingleObject(event, 1000) != WAIT_OBJECT_0 {
            continue;
        }
        let space = size - client.GetCurrentPadding()?;
        if space == 0 {
            continue;
        }
        let buffer = writer.GetBuffer(space)?;
        let data = std::slice::from_raw_parts_mut(buffer as *mut f32, space as usize * channels);
        data.fill(0.0);
        if written + space as u64 > next_click {
            let at = (next_click - written) as usize;
            for frame in data.chunks_exact_mut(channels).skip(at).take(CLICK_FRAMES) {
                frame.fill(LEVEL);
            }
            // When will frame `next_click` reach the device? From where the device is right now.
            let (mut pos, mut qpc) = (0u64, 0u64);
            clock.GetPosition(&mut pos, Some(&mut qpc))?;
            let device_frames = pos as f64 * rate / clock_hz;
            let play_at = qpc as f64 / 10_000_000.0 + (next_click as f64 - device_frames) / rate;
            times.lock().unwrap().push(play_at);
            next_click += rate as u64;
            done += 1;
        }
        written += space as u64;
        writer.ReleaseBuffer(space, 0)?;
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    client.Stop()?;
    Ok(())
}

/// Listens; returns each click's arrival time in seconds of QPC.
unsafe fn listener(index: u32, seconds: f64, stop: Arc<AtomicBool>) -> windows::core::Result<Vec<f64>> {
    let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    let all: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let d = all.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?.Item(index)?;
    let client: IAudioClient = d.Activate(CLSCTX_ALL, None)?;
    let format = client.GetMixFormat()?;
    let (rate, channels) = ((*format).nSamplesPerSec as f64, (*format).nChannels as usize);
    client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, format, None)?;
    let reader: IAudioCaptureClient = client.GetService()?;
    client.Start()?;
    let started = std::time::Instant::now();
    let (mut arrivals, mut quiet_for) = (Vec::new(), 0usize);
    while started.elapsed().as_secs_f64() < seconds && !stop.load(Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(2));
        while reader.GetNextPacketSize()? > 0 {
            let (mut data, mut got, mut flags, mut qpc) = (std::ptr::null_mut::<u8>(), 0u32, 0u32, 0u64);
            reader.GetBuffer(&mut data, &mut got, &mut flags, None, Some(&mut qpc))?;
            let samples = std::slice::from_raw_parts(data as *const f32, got as usize * channels);
            for (i, frame) in samples.chunks_exact(channels).enumerate() {
                if frame[0].abs() > LEVEL / 4.0 {
                    if quiet_for > 4800 {
                        arrivals.push(qpc as f64 / 10_000_000.0 + i as f64 / rate);
                    }
                    quiet_for = 0;
                } else {
                    quiet_for += 1;
                }
            }
            reader.ReleaseBuffer(got)?;
        }
    }
    client.Stop()?;
    Ok(arrivals)
}

fn main() {
    let arg = |n: usize| std::env::args().nth(n).and_then(|a| a.parse::<u32>().ok());
    let (Some(out), Some(mic)) = (arg(1), arg(2)) else {
        return eprintln!("usage: miclatency <click out#> <listen mic#> [clicks]");
    };
    let clicks = arg(3).unwrap_or(10) as usize;
    let _ = qpc_hz();
    let stop = Arc::new(AtomicBool::new(false));
    let times = Arc::new(Mutex::new(Vec::new()));
    let (t, s) = (times.clone(), stop.clone());
    let play = std::thread::spawn(move || unsafe { player(out, clicks, s, t) }.map_err(|e| e.to_string()));
    let heard = unsafe { listener(mic, clicks as f64 + 2.0, stop.clone()) }.unwrap_or_else(|e| {
        eprintln!("listening failed: {e}");
        Vec::new()
    });
    stop.store(true, Relaxed);
    if let Err(e) = play.join().unwrap() {
        eprintln!("playing failed: {e}");
    }
    let sent = times.lock().unwrap().clone();
    let mut delays: Vec<f64> = Vec::new();
    for (i, &at) in sent.iter().enumerate() {
        // The arrival that follows this click and precedes the next.
        let next = sent.get(i + 1).copied().unwrap_or(f64::MAX);
        if let Some(&got) = heard.iter().find(|&&h| h >= at - 0.005 && h < next - 0.005) {
            delays.push((got - at) * 1000.0);
        }
    }
    println!("{} clicks sent, {} heard", sent.len(), heard.len());
    if delays.is_empty() {
        return println!("nothing matched: is the path running, and is the cable's output the device you listen on?");
    }
    delays.sort_by(f64::total_cmp);
    let text: Vec<String> = delays.iter().map(|d| format!("{d:.1}")).collect();
    println!("delay ms: {}", text.join(" "));
    println!("median {:.1} ms, min {:.1}, max {:.1}", delays[delays.len() / 2], delays[0], delays[delays.len() - 1]);
}
