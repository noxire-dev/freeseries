//! The microphone side: capture the real mic, clean it up, play it into a virtual cable's input
//! so that every app can pick the cable's output as its microphone. Same shape as `native.rs`,
//! pointed the other way: one thread, paced by the cable's render event, draining the mic's
//! capture on every wake. Noise suppression is RNNoise (`nnnoiseless`), 10 ms frames.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::engine::{DeviceInfo, PeakMeter};
use crate::native::{device, enumerator, format_of, CAPTURE_BUFFER_HNS, ID_PREFIX};
use crate::wasapi::{join_pro_audio, Feed, Tuning};
use nnnoiseless::DenoiseState;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

const FRAME: usize = DenoiseState::FRAME_SIZE; // 480 samples, 10 ms at 48 kHz
const RNNOISE_RATE: u32 = 48000;

/// What the UI changes while the mic runs.
#[derive(Default)]
pub struct Settings {
    pub suppress: AtomicBool,
    pub meter: PeakMeter,
}

/// The mic stream ends when this is dropped.
pub struct Session {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The microphones Windows has, in the same id form as the outputs.
pub fn mics() -> Result<Vec<DeviceInfo>, String> {
    unsafe {
        let all = enumerator()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE).map_err(|e| e.to_string())?;
        let mut found = Vec::new();
        for i in 0..all.GetCount().map_err(|e| e.to_string())? {
            let d = all.Item(i).map_err(|e| e.to_string())?;
            let id = d.GetId().map_err(|e| e.to_string())?.to_string().map_err(|e| e.to_string())?;
            let name = d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&PKEY_Device_FriendlyName)).map(|v| v.to_string()).unwrap_or_default();
            let Ok(client) = d.Activate::<IAudioClient>(CLSCTX_ALL, None) else { continue };
            let Ok((format, _)) = format_of(&client) else { continue };
            found.push(DeviceInfo { id: format!("{ID_PREFIX}{id}"), name, channels: format.channels as u16, sample_rate: format.rate, spatial: false });
        }
        Ok(found)
    }
}

/// Starts the mic: `from` is a microphone (or an output, whose loopback then stands in for one:
/// that is how the delay is measured), `into` the cable's input. `on_lost` is called if the
/// stream dies, the way the output engine does it.
pub fn start(from: &str, into: &str, settings: Arc<Settings>, on_lost: Box<dyn Fn() + Send + Sync>) -> Result<Session, String> {
    let (from, into) = (from.to_string(), into.to_string());
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("mic".into())
        .spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            match unsafe { open(&from, &into) } {
                Ok(running) => {
                    let _ = ready_tx.send(Ok(()));
                    if let Err(e) = unsafe { run(running, &settings, &stopping) } {
                        log::error!("mic session died: {e}");
                        on_lost();
                    }
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    ready_rx.recv().map_err(|_| "the mic thread ended before it started".to_string())??;
    Ok(Session { stop, thread: Some(thread) })
}

struct Running {
    capture: IAudioClient,
    reader: IAudioCaptureClient,
    in_ch: usize,
    render: IAudioClient,
    writer: IAudioRenderClient,
    event: HANDLE,
    out_size: u32,
    out_ch: usize,
    rate: u32,
}

unsafe fn open(from: &str, into: &str) -> Result<Running, String> {
    let source = device(from)?;
    let capture: IAudioClient = source.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
    let (input, in_format) = format_of(&capture)?;
    let render: IAudioClient = device(into)?.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
    let (output, out_format) = format_of(&render)?;
    // ponytail: no resampler yet. RNNoise wants 48 kHz, and the cable is set to it.
    if input.rate != RNNOISE_RATE || output.rate != RNNOISE_RATE {
        return Err(format!("the mic runs at {} Hz and the cable at {} Hz; both must be {RNNOISE_RATE} Hz", input.rate, output.rate));
    }
    // An output as the source: capture its loopback (the delay measurement plays a click into one).
    let flow = source.cast::<IMMEndpoint>().and_then(|e| e.GetDataFlow()).map_err(|e| e.to_string())?;
    let flags = if flow == eRender { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
    capture.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, CAPTURE_BUFFER_HNS, 0, in_format, None).map_err(|e| format!("mic: {e}"))?;
    let reader: IAudioCaptureClient = capture.GetService().map_err(|e| e.to_string())?;
    render.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0, 0, out_format, None).map_err(|e| format!("cable: {e}"))?;
    let event = CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| e.to_string())?;
    render.SetEventHandle(event).map_err(|e| e.to_string())?;
    let writer: IAudioRenderClient = render.GetService().map_err(|e| e.to_string())?;
    let out_size = render.GetBufferSize().map_err(|e| e.to_string())?;
    capture.Start().map_err(|e| e.to_string())?;
    render.Start().map_err(|e| e.to_string())?;
    log::info!("mic: {} ch at {} Hz -> cable {} ch, {} ms of mic room", input.channels, input.rate, output.channels, capture.GetBufferSize().map_or(0, |f| f * 1000 / input.rate));
    Ok(Running { capture, reader, in_ch: input.channels, render, writer, event, out_size, out_ch: output.channels, rate: output.rate })
}

unsafe fn run(s: Running, settings: &Settings, stop: &AtomicBool) -> Result<(), String> {
    join_pro_audio();
    let started = Instant::now();
    let ms = |n: usize| s.rate as usize * s.out_ch * n / 1000;
    // No cushion in the queue: the device buffer is the cushion. It is kept two periods ahead, and
    // only what the queue holds is written, so a wake without a packet leaves it one period low and
    // the next wake, with two packets, fills it back up. Below two periods for a whole second means
    // the mic's clock is slower than the cable's: the queue is then stretched over the difference.
    let tuning = Tuning { cushion: 0, slack: ms(2), frame: s.out_ch, per_ms: ms(1) };
    let last_packet_ms = Arc::new(AtomicU64::new(0));
    let queue = Arc::new(Mutex::new(VecDeque::with_capacity(ms(200))));
    let mut feed = Feed::new(queue.clone(), last_packet_ms.clone());
    let period = s.rate as usize / 100;
    let ahead = (2 * period) as u32;
    let window = ms(1000);
    let mut played = 0usize;
    let mut primed = false; // the first write fills the device to two periods; until then nothing is written
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME * 4); // mono, waiting for a whole RNNoise frame
    let mut denoiser = DenoiseState::new();
    let mut clean = [0f32; FRAME];
    let mut scaled = [0f32; FRAME];
    let result = loop {
        if stop.load(Relaxed) {
            break Ok(());
        }
        if WaitForSingleObject(s.event, 1000) != WAIT_OBJECT_0 {
            continue;
        }
        let now_ms = started.elapsed().as_millis() as u64;
        // Everything the mic has, as mono.
        loop {
            let next = match s.reader.GetNextPacketSize() {
                Ok(n) => n,
                Err(_) => break,
            };
            if next == 0 {
                break;
            }
            let (mut data, mut got, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
            if s.reader.GetBuffer(&mut data, &mut got, &mut flags, None, None).is_err() {
                break;
            }
            let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
            let samples = std::slice::from_raw_parts(data as *const f32, got as usize * s.in_ch);
            pending.extend(samples.chunks_exact(s.in_ch).map(|f| if silent { 0.0 } else { f[0] }));
            if s.reader.ReleaseBuffer(got).is_err() {
                break;
            }
        }
        // Whole frames through RNNoise (it counts in 16-bit units), then out to every cable channel.
        let mut q = queue.lock().unwrap();
        let mut taken = 0;
        for frame in pending.chunks_exact(FRAME) {
            let out: &[f32] = if settings.suppress.load(Relaxed) {
                for (s, x) in scaled.iter_mut().zip(frame) {
                    *s = x * 32767.0;
                }
                denoiser.process_frame(&mut clean, &scaled);
                for c in clean.iter_mut() {
                    *c /= 32767.0;
                }
                &clean
            } else {
                frame
            };
            settings.meter.feed(out);
            q.extend(out.iter().flat_map(|&x| std::iter::repeat_n(x, s.out_ch)));
            taken += FRAME;
        }
        if taken > 0 {
            pending.drain(..taken);
            last_packet_ms.store(now_ms, Relaxed);
        }
        drop(q);

        let padding = match s.render.GetCurrentPadding() {
            Ok(p) => p,
            Err(e) => break Err(format!("cable: {e}")),
        };
        let ready = feed.ready(&tuning) as u32;
        if !primed && ready < ahead {
            continue;
        }
        primed = true;
        let space = (s.out_size - padding).min(ahead.saturating_sub(padding)).min(ready);
        if space == 0 {
            continue;
        }
        let short = ahead.saturating_sub(padding + space) as usize; // how far below two periods the device is left
        let buffer = match s.writer.GetBuffer(space) {
            Ok(b) => b,
            Err(e) => break Err(format!("cable: {e}")),
        };
        let data = std::slice::from_raw_parts_mut(buffer as *mut f32, space as usize * s.out_ch);
        data.fill(0.0);
        played += data.len();
        let second_over = played >= window;
        feed.mix_into(data, now_ms, second_over, &tuning);
        if second_over {
            played = 0;
            if short * s.out_ch > tuning.slack {
                log::info!("mic: the cable is {} ms short of two periods: stretching the mic over it", short * 1000 / s.rate as usize);
                feed.hold_back(short);
            }
        }
        if let Err(e) = s.writer.ReleaseBuffer(space, 0) {
            break Err(format!("cable: {e}"));
        }
    };
    let _ = s.render.Stop();
    let _ = s.capture.Stop();
    let _ = CloseHandle(s.event);
    result
}
