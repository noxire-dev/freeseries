//! The low-latency spike. Shared-mode WASAPI through cpal runs at the engine's default period
//! (10 ms). `IAudioClient3` lets a shared stream run at the driver's minimum instead, often a few
//! milliseconds. Does that hold for the virtual device and the user's outputs, and what does it do
//! to end-to-end latency? Two parts:
//!
//!   lowlat periods                    what every playback endpoint says its periods are
//!   lowlat run <from> <to> [frames|x]  a bare loopback -> render passthrough (no EQ) at that period;
//!                                     `x`: the output in EXCLUSIVE mode at the driver's minimum period
//!                                     (the smallest allowed by both when left out); measure it with
//!                                     `examples/latency.rs` while it runs. Device names by substring.
//!
//! Never point `run` at the output the installed app plays on: the audio would come out twice.

use std::collections::VecDeque;

/// 32 (float, the shared-mode format) or 16 (PCM) for the exclusive experiment.
static EXCLUSIVE_BITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(32);
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
                Ok((p, f)) => {
                    let ms = |frames: u32| frames as f64 * 1000.0 / p.rate as f64;
                    // The render buffer Windows hands out for this endpoint: initialised, never started, so nothing plays.
                    let buffer = unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0, 0, f, None).and_then(|_| client.GetBufferSize()) }.map_or("?".into(), |b| format!("{b} frames = {:.1} ms", ms(b)));
                    println!("{:<52} {} Hz {} ch  period {:.2} ms (min {:.2}, max {:.2})  render buffer {buffer}", e.name, p.rate, p.channels, ms(p.default), ms(p.min), ms(p.max));
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
    let mut render: IAudioClient3 = unsafe { to.device.Activate(CLSCTX_ALL, None)? };
    let (cp, cf) = periods(&capture)?;
    let (rp, rf) = periods(&render)?;
    if cp.rate != rp.rate || cp.channels != rp.channels || cp.channels != 2 {
        return Err(format!("this spike needs both at the same stereo rate: {} Hz {} ch vs {} Hz {} ch", cp.rate, cp.channels, rp.rate, rp.channels).into());
    }
    let smallest = cp.min.max(rp.min);
    let exclusive = rest.first().map(String::as_str) == Some("x");
    let frames: u32 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(smallest);
    let rate = cp.rate;
    if !exclusive { println!("{} -> {}: period {frames} frames = {:.2} ms (smallest allowed {smallest})", from.name, to.name, frames as f64 * 1000.0 / rate as f64); }

    unsafe {
        let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        // Loopback capture never signals its event by itself; the render event paces both.
        // IAudioClient3 refuses the loopback flag (AUDCLNT_E_INVALID_STREAM_FLAG): loopback capture
        // stays on the plain initialise, at the engine's period; the render side asks for `frames`.
        capture.Initialize(AUDCLNT_SHAREMODE_SHARED, flags | AUDCLNT_STREAMFLAGS_LOOPBACK, 0, 0, cf, None)?;
        if exclusive {
            // Exclusive: our stream is the device's stream. The engine period no longer applies; the
            // driver's minimum does, and the format must be one the device takes natively.
            let (mut default_100ns, mut min_100ns) = (0i64, 0i64);
            render.GetDevicePeriod(Some(&mut default_100ns), Some(&mut min_100ns))?;
            let mut format: WAVEFORMATEX = std::ptr::read_unaligned(rf);
            let mut closest = std::ptr::null_mut();
            let ok = render.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &format, Some(&mut closest));
            println!("exclusive: device period default {:.2} ms, min {:.2} ms; float32 supported: {:?}", default_100ns as f64 / 10_000.0, min_100ns as f64 / 10_000.0, ok.is_ok());
            if ok.is_err() {
                // 16-bit PCM is what almost every device takes.
                format.wFormatTag = 1; // WAVE_FORMAT_PCM
                format.wBitsPerSample = 16;
                let channels = { format.nChannels };
                let rate = { format.nSamplesPerSec };
                format.nBlockAlign = 2 * channels;
                format.nAvgBytesPerSec = rate * (2 * channels) as u32;
                format.cbSize = 0;
                let ok16 = render.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &format, Some(&mut closest));
                println!("exclusive: 16-bit PCM supported: {:?}", ok16.is_ok());
            }
            let mut period = min_100ns.max(30_000); // no lower than 3 ms
            // Exclusive buffers must be a whole number of frames; a refused size comes back with the aligned one.
            let mut attempt = render.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &format, None);
            if attempt.as_ref().is_err_and(|e| e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED) {
                // The refused client reports the aligned size; a fresh client takes it.
                let frames_in_buffer = render.GetBufferSize()?;
                period = (10_000_000.0 * frames_in_buffer as f64 / rate as f64 + 0.5) as i64;
                render = to.device.Activate(CLSCTX_ALL, None)?;
                attempt = render.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, period, period, &format, None);
            }
            attempt?;
            let bits = { format.wBitsPerSample }; // a copy: the struct is packed
            println!("exclusive: period {:.2} ms, {bits} bit", period as f64 / 10_000.0);
            EXCLUSIVE_BITS.store(bits as usize, std::sync::atomic::Ordering::Relaxed);
        } else {
            render.InitializeSharedAudioStream(flags, frames, rf, None)?;
        }
        let capture_event = CreateEventW(None, false, false, PCWSTR::null())?;
        let render_event = CreateEventW(None, false, false, PCWSTR::null())?;
        capture.SetEventHandle(capture_event)?;
        render.SetEventHandle(render_event)?;
        let reader: IAudioCaptureClient = capture.GetService()?;
        let writer: IAudioRenderClient = render.GetService()?;
        // Self-timing: every queued frame remembers when its capture packet was stamped; at write time
        // the render clock says when the next frame plays. Their difference is this process's latency
        // from the source's loopback stamp to the output's play position, the same sum in both modes.
        let clock: IAudioClock = render.GetService()?;
        let clock_hz: u64 = clock.GetFrequency()?;
        let mut stamps: VecDeque<u64> = VecDeque::new(); // capture qpc (100 ns) per queued frame
        let mut written_total = 0u64;
        let (mut lat_sum, mut lat_n) = (0f64, 0u64);
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
                let (mut data, mut got, mut flags, mut qpc) = (std::ptr::null_mut::<u8>(), 0u32, 0u32, 0u64);
                reader.GetBuffer(&mut data, &mut got, &mut flags, None, Some(&mut qpc))?;
                let samples = std::slice::from_raw_parts(data as *const f32, got as usize * 2);
                queue.extend(samples);
                stamps.extend((0..got as u64).map(|i| qpc + i * 10_000_000 / rate as u64));
                reader.ReleaseBuffer(got)?;
            }
            let excess = queue.len().saturating_sub(cap);
            if excess > 0 {
                queue.drain(..excess);
                stamps.drain(..excess / 2);
                dropped += excess / 2;
            }
            // Exclusive, event-driven: every event hands over the whole buffer; padding says nothing there.
            let space = if exclusive { out_size } else { out_size - render.GetCurrentPadding()? };
            if space == 0 {
                continue;
            }
            let buffer = writer.GetBuffer(space)?;
            if let Some(&first) = stamps.front() {
                let (mut pos, mut now) = (0u64, 0u64);
                if clock.GetPosition(&mut pos, Some(&mut now)).is_ok() && clock_hz > 0 {
                    let played = pos as f64 * rate as f64 / clock_hz as f64; // frames the device has played
                    let plays_at = now as f64 + (written_total as f64 - played) * 10_000_000.0 / rate as f64;
                    lat_sum += (plays_at - first as f64) / 10_000.0;
                    lat_n += 1;
                }
            }
            written_total += space as u64;
            let mut next = || match (queue.pop_front(), stamps.pop_front()) {
                (Some(x), _) => x,
                _ => {
                    dry += 1;
                    0.0
                }
            };
            if EXCLUSIVE_BITS.load(std::sync::atomic::Ordering::Relaxed) == 16 {
                let out = std::slice::from_raw_parts_mut(buffer as *mut i16, space as usize * 2);
                for s in out.iter_mut() {
                    *s = (next().clamp(-1.0, 1.0) * 32767.0) as i16;
                }
            } else {
                let out = std::slice::from_raw_parts_mut(buffer as *mut f32, space as usize * 2);
                for s in out.iter_mut() {
                    *s = next();
                }
            }
            writer.ReleaseBuffer(space, 0)?;
            if events % (rate as u64 * 5 / frames as u64) == 0 {
                println!("{:>5.0}s  queued {:>4} frames  dropped {dropped} frames  dry {} frames  self-timed latency {:.1} ms (loopback stamp -> play position)", started.elapsed().as_secs_f32(), queue.len() / 2, dry / 2, if lat_n > 0 { lat_sum / lat_n as f64 } else { 0.0 });
                (lat_sum, lat_n) = (0.0, 0);
            }
        }
    }
}
