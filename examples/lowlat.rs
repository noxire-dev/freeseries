//! The low-latency spike. Shared-mode WASAPI through cpal runs at the engine's default period
//! (10 ms). `IAudioClient3` lets a shared stream run at the driver's minimum instead, often a few
//! milliseconds. Does that hold for the virtual device and the user's outputs, and what does it do
//! to end-to-end latency? Two parts:
//!
//!   lowlat periods                    what every playback endpoint says its periods are
//!   lowlat run <from> <to> [frames]   a bare loopback -> render passthrough (no EQ) at that period
//!                                     (the smallest allowed by both when left out); measure it with
//!                                     `examples/latency.rs` while it runs. Device names by substring.
//!
//! Never point `run` at the output the installed app plays on: the audio would come out twice.

use std::collections::VecDeque;
use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject};

struct Endpoint {
    name: String,
    device: IMMDevice,
}

fn endpoints() -> windows::core::Result<Vec<Endpoint>> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let all = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        (0..all.GetCount()?)
            .map(|i| {
                let device = all.Item(i)?;
                let name = device.OpenPropertyStore(STGM_READ)?.GetValue(&PKEY_Device_FriendlyName)?.to_string();
                Ok(Endpoint { name, device })
            })
            .collect()
    }
}

struct Periods {
    default: u32,
    fundamental: u32,
    min: u32,
    max: u32,
    rate: u32,
    channels: u16,
}

fn periods(client: &IAudioClient3) -> windows::core::Result<(Periods, *mut WAVEFORMATEX)> {
    unsafe {
        let format = client.GetMixFormat()?;
        let (mut default, mut fundamental, mut min, mut max) = (0, 0, 0, 0);
        client.GetSharedModeEnginePeriod(format, &mut default, &mut fundamental, &mut min, &mut max)?;
        Ok((Periods { default, fundamental, min, max, rate: (*format).nSamplesPerSec, channels: (*format).nChannels }, format))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let all = endpoints()?;

    if args.first().map(String::as_str) == Some("periods") {
        for e in &all {
            let client: IAudioClient3 = unsafe { e.device.Activate(CLSCTX_ALL, None)? };
            match periods(&client) {
                Ok((p, _)) => {
                    let ms = |frames: u32| frames as f64 * 1000.0 / p.rate as f64;
                    println!("{:<52} {} Hz {} ch  default {:.2} ms  min {:.2} ms  max {:.2} ms  (steps of {} frames)", e.name, p.rate, p.channels, ms(p.default), ms(p.min), ms(p.max), p.fundamental);
                }
                Err(err) => println!("{:<52} no IAudioClient3 periods: {err}", e.name),
            }
        }
        return Ok(());
    }

    let [_, from, to, rest @ ..] = args.as_slice() else {
        return Err("usage: lowlat periods | lowlat run <from> <to> [frames]".into());
    };
    let find = |what: &str| all.iter().find(|e| e.name.to_lowercase().contains(&what.to_lowercase())).ok_or_else(|| format!("no endpoint matching {what:?}"));
    let (from, to) = (find(from)?, find(to)?);
    let capture: IAudioClient3 = unsafe { from.device.Activate(CLSCTX_ALL, None)? };
    let render: IAudioClient3 = unsafe { to.device.Activate(CLSCTX_ALL, None)? };
    let (cp, cf) = periods(&capture)?;
    let (rp, rf) = periods(&render)?;
    if cp.rate != rp.rate || cp.channels != rp.channels || cp.channels != 2 {
        return Err(format!("this spike needs both at the same stereo rate: {} Hz {} ch vs {} Hz {} ch", cp.rate, cp.channels, rp.rate, rp.channels).into());
    }
    let smallest = cp.min.max(rp.min);
    let frames: u32 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(smallest);
    let rate = cp.rate;
    println!("{} -> {}: period {frames} frames = {:.2} ms (smallest allowed {smallest})", from.name, to.name, frames as f64 * 1000.0 / rate as f64);

    unsafe {
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        // Loopback capture never signals its event by itself; the render event paces both.
        // IAudioClient3 refuses the loopback flag (AUDCLNT_E_INVALID_STREAM_FLAG): loopback capture
        // stays on the plain initialise, at the engine's period; the render side asks for `frames`.
        capture.Initialize(AUDCLNT_SHAREMODE_SHARED, flags | AUDCLNT_STREAMFLAGS_LOOPBACK, 0, 0, cf, None)?;
        render.InitializeSharedAudioStream(flags, frames, rf, None)?;
        let capture_event = CreateEventW(None, false, false, PCWSTR::null())?;
        let render_event = CreateEventW(None, false, false, PCWSTR::null())?;
        capture.SetEventHandle(capture_event)?;
        render.SetEventHandle(render_event)?;
        let reader: IAudioCaptureClient = capture.GetService()?;
        let writer: IAudioRenderClient = render.GetService()?;
        let out_size = render.GetBufferSize()?;
        println!("render buffer {out_size} frames = {:.2} ms; capture buffer {} frames", out_size as f64 * 1000.0 / rate as f64, capture.GetBufferSize()?);

        let mut task = 0u32;
        let _ = AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task);
        capture.Start()?;
        render.Start()?;

        // ponytail: no easing, no cushion logic. A queue capped at two periods: whatever the capture
        // clock gains over the render clock is dropped, whatever it loses plays as silence.
        let cap = 2 * frames as usize * 2;
        let mut queue: VecDeque<f32> = VecDeque::with_capacity(cap * 2);
        let (mut dropped, mut dry, mut events) = (0usize, 0usize, 0u64);
        let started = std::time::Instant::now();
        loop {
            WaitForSingleObject(render_event, 1000);
            events += 1;
            loop {
                let packet = reader.GetNextPacketSize()?;
                if packet == 0 {
                    break;
                }
                let (mut data, mut got, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                reader.GetBuffer(&mut data, &mut got, &mut flags, None, None)?;
                let samples = std::slice::from_raw_parts(data as *const f32, got as usize * 2);
                queue.extend(samples);
                reader.ReleaseBuffer(got)?;
            }
            let excess = queue.len().saturating_sub(cap);
            if excess > 0 {
                queue.drain(..excess);
                dropped += excess / 2;
            }
            let space = out_size - render.GetCurrentPadding()?;
            if space == 0 {
                continue;
            }
            let buffer = writer.GetBuffer(space)?;
            let out = std::slice::from_raw_parts_mut(buffer as *mut f32, space as usize * 2);
            for s in out.iter_mut() {
                *s = match queue.pop_front() {
                    Some(x) => x,
                    None => {
                        dry += 1;
                        0.0
                    }
                };
            }
            writer.ReleaseBuffer(space, 0)?;
            if events % (rate as u64 * 5 / frames as u64) == 0 {
                println!("{:>5.0}s  queued {:>4} frames  dropped {dropped} frames  dry {} frames", started.elapsed().as_secs_f32(), queue.len() / 2, dry / 2);
            }
        }
    }
}
