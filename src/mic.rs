//! The microphone side: capture the real mic, clean it up, play it into a virtual cable's input
//! so that every app can pick the cable's output as its microphone. Same shape as `native.rs`,
//! pointed the other way: one thread, paced by the cable's render event, draining the mic's
//! capture on every wake. Noise suppression is RNNoise (`nnnoiseless`), 10 ms frames.

#![allow(unsafe_op_in_unsafe_fn)]

use crate::engine::{DeviceInfo, PeakMeter, PresetCell};
use crate::eq::{self, Eq};
use crate::native::{device, enumerator, format_of, CAPTURE_BUFFER_HNS, ID_PREFIX};
use crate::spectrum::Tap;
use crate::wasapi::{join_pro_audio, Feed, Tuning};
use nnnoiseless::DenoiseState;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
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
/// Samples RNNoise's output lags its input: one frame, its overlap-add window (see the test).
const RNNOISE_DELAY: usize = 480;
/// "Hear yourself" plays the cleaned-up voice on the user's own output. Its buffer is kept this
/// far ahead (three 10 ms periods); more than this much voice waiting for it is dropped.
const MONITOR_BUFFER_HNS: i64 = 1_000_000;
const MONITOR_AHEAD: u32 = 3 * 480;
const MONITOR_DROP: usize = 5 * 480;

/// What the UI changes while the mic runs.
pub struct Settings {
    /// How much of RNNoise's cleaned-up voice is used, 0 (the raw mic) to 1 (all of it), as f32 bits.
    strength: AtomicU32,
    pub eq: PresetCell,
    pub meter: PeakMeter,
    /// The analyser (an agent's `listen_mic`) only costs anything while someone is looking.
    analysing: AtomicBool,
    tap: Mutex<Tap>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { strength: AtomicU32::new(1f32.to_bits()), eq: PresetCell::default(), meter: PeakMeter::default(), analysing: AtomicBool::new(false), tap: Mutex::new(Tap::default()) }
    }
}

impl Settings {
    pub fn set_strength(&self, strength: f32) {
        self.strength.store(strength.clamp(0.0, 1.0).to_bits(), Relaxed);
    }

    fn strength(&self) -> f32 {
        f32::from_bits(self.strength.load(Relaxed))
    }

    pub fn set_analysing(&self, on: bool) {
        self.analysing.store(on, Relaxed);
    }

    /// dBFS per log-spaced band of the voice as it leaves (noise removed, EQ applied), or `None` while the analyser is off.
    pub fn spectrum(&self, bands: usize) -> Option<Vec<f32>> {
        self.analysing.load(Relaxed).then(|| self.tap.lock().unwrap().spectrum(bands))
    }

    /// For the mic thread. Never blocks: a frame that arrives while the UI reads is skipped.
    fn tap(&self, mono: &[f32], rate: u32) {
        if self.analysing.load(Relaxed) {
            if let Ok(mut tap) = self.tap.try_lock() {
                tap.push(mono, 1, rate as f32);
            }
        }
    }
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
/// that is how the delay is measured), `into` the cable's input, `monitor` an output to also play
/// the voice on, so the user hears what the settings do. `on_lost` is called if the stream dies,
/// the way the output engine does it.
pub fn start(from: &str, into: &str, monitor: Option<&str>, settings: Arc<Settings>, on_lost: Box<dyn Fn() + Send + Sync>) -> Result<Session, String> {
    let (from, into, monitor) = (from.to_string(), into.to_string(), monitor.map(str::to_string));
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("mic".into())
        .spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            match unsafe { open(&from, &into, monitor.as_deref()) } {
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
    monitor: Option<Monitor>,
}

/// The user's own output, playing the voice back ("hear yourself").
struct Monitor {
    client: IAudioClient,
    writer: IAudioRenderClient,
    ch: usize,
    size: u32,
}

unsafe fn open(from: &str, into: &str, monitor: Option<&str>) -> Result<Running, String> {
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
    // No monitor is not an error: the voice still reaches the cable.
    let monitor = monitor.and_then(|id| match open_monitor(id) {
        Ok(m) => Some(m),
        Err(e) => {
            log::warn!("mic: not playing the voice back: {e}");
            None
        }
    });
    capture.Start().map_err(|e| e.to_string())?;
    render.Start().map_err(|e| e.to_string())?;
    log::info!("mic: {} ch at {} Hz -> cable {} ch, {} ms of mic room{}", input.channels, input.rate, output.channels, capture.GetBufferSize().map_or(0, |f| f * 1000 / input.rate), if monitor.is_some() { ", played back" } else { "" });
    Ok(Running { capture, reader, in_ch: input.channels, render, writer, event, out_size, out_ch: output.channels, rate: output.rate, monitor })
}

unsafe fn open_monitor(id: &str) -> Result<Monitor, String> {
    let client: IAudioClient = device(id)?.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
    let (info, format) = format_of(&client)?;
    if info.rate != RNNOISE_RATE {
        return Err(format!("the output runs at {} Hz, not {RNNOISE_RATE}", info.rate));
    }
    client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, MONITOR_BUFFER_HNS, 0, format, None).map_err(|e| e.to_string())?;
    let writer: IAudioRenderClient = client.GetService().map_err(|e| e.to_string())?;
    let size = client.GetBufferSize().map_err(|e| e.to_string())?;
    client.Start().map_err(|e| e.to_string())?;
    Ok(Monitor { client, writer, ch: info.channels, size })
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
    let mut dry: VecDeque<f32> = std::iter::repeat_n(0.0, RNNOISE_DELAY).collect(); // the raw voice, lined up with RNNoise's output
    let mut eq = Eq::new(&settings.eq.get(), s.rate as f64, 1);
    let mut monitor_q: Vec<f32> = Vec::new(); // mono, waiting for the monitor
    // Evidence for a voice that sounds wrong, once a second: packets the mic reported losing, and
    // wakes that found the cable's buffer empty (it played silence to Discord until this write).
    let (mut skipped, mut ran_dry) = (0u32, 0u32);
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
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                skipped += 1;
            }
            let samples = std::slice::from_raw_parts(data as *const f32, got as usize * s.in_ch);
            pending.extend(samples.chunks_exact(s.in_ch).map(|f| if silent { 0.0 } else { f[0] }));
            if s.reader.ReleaseBuffer(got).is_err() {
                break;
            }
        }
        // Whole frames through RNNoise (it counts in 16-bit units), the strength's share of the raw
        // voice added back (delayed to line up), the mic EQ, then out to every cable channel.
        let strength = settings.strength();
        if let Some(preset) = settings.eq.take_changed() {
            eq.retune(&preset);
        }
        let mut q = queue.lock().unwrap();
        let mut taken = 0;
        for frame in pending.chunks_exact(FRAME) {
            if strength > 0.0 {
                for (s, x) in scaled.iter_mut().zip(frame) {
                    *s = x * 32767.0;
                }
                denoiser.process_frame(&mut clean, &scaled);
                for (c, &x) in clean.iter_mut().zip(frame) {
                    dry.push_back(x);
                    let raw = dry.pop_front().unwrap_or(0.0);
                    *c = raw + strength * (*c / 32767.0 - raw);
                }
            } else {
                clean.copy_from_slice(frame);
            }
            eq.process(&mut clean);
            eq::limit(&mut clean);
            settings.meter.feed(&clean);
            settings.tap(&clean, s.rate);
            q.extend(clean.iter().flat_map(|&x| std::iter::repeat_n(x, s.out_ch)));
            if s.monitor.is_some() {
                monitor_q.extend_from_slice(&clean);
            }
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
        if primed && padding == 0 {
            ran_dry += 1;
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
            if skipped > 0 || ran_dry > 0 {
                log::warn!("mic: in the last second the microphone skipped {skipped} packet(s) and the cable ran dry {ran_dry} time(s): that is a gap in the voice");
                (skipped, ran_dry) = (0, 0);
            }
        }
        if let Err(e) = s.writer.ReleaseBuffer(space, 0) {
            break Err(format!("cable: {e}"));
        }
        if let Some(m) = &s.monitor {
            play_back(m, &mut monitor_q);
        }
    };
    if let Some(m) = &s.monitor {
        let _ = m.client.Stop();
    }
    let _ = s.render.Stop();
    let _ = s.capture.Stop();
    let _ = CloseHandle(s.event);
    result
}

/// Writes what the monitor has room for, keeping its buffer MONITOR_AHEAD at most. The monitor runs
/// on its own clock: when it is slower, voice piles up and is dropped (ponytail: a drop every few
/// minutes instead of a resampler); when faster, a wake without voice leaves a short gap.
unsafe fn play_back(m: &Monitor, voice: &mut Vec<f32>) {
    let Ok(padding) = m.client.GetCurrentPadding() else { return };
    let n = ((m.size - padding).min(MONITOR_AHEAD.saturating_sub(padding)) as usize).min(voice.len());
    if n > 0 {
        if let Ok(buffer) = m.writer.GetBuffer(n as u32) {
            let data = std::slice::from_raw_parts_mut(buffer as *mut f32, n * m.ch);
            for (frame, &x) in data.chunks_exact_mut(m.ch).zip(voice.iter()) {
                frame.fill(x);
            }
            let _ = m.writer.ReleaseBuffer(n as u32, 0);
        }
        voice.drain(..n);
    }
    if voice.len() > MONITOR_DROP {
        voice.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RNNoise's output lags its input by a fixed number of samples. The strength mix adds the dry
    /// voice back in, delayed by exactly that, or it would comb-filter against the cleaned one.
    #[test]
    fn rnnoise_delay() {
        let mut denoiser = DenoiseState::new();
        let n = FRAME * 60;
        // A voiced sound: 210 Hz and its harmonics, on for 100 ms, off for 100 ms.
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / RNNOISE_RATE as f32;
                let on = (i / (FRAME * 10)) % 2 == 1;
                if !on { return 0.0; }
                (1..6).map(|h| (2.0 * std::f32::consts::PI * 210.0 * h as f32 * t).sin() / h as f32).sum::<f32>() * 8000.0
            })
            .collect();
        let mut output = vec![0f32; n];
        for (i, frame) in input.chunks_exact(FRAME).enumerate() {
            denoiser.process_frame(&mut output[i * FRAME..(i + 1) * FRAME], frame);
        }
        // Best lag by cross-correlation over the second half, once the network has settled.
        let from = n / 2;
        let best = (0..2 * FRAME)
            .map(|lag| (lag, (from..n).map(|i| output[i] * input[i - lag]).sum::<f32>()))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap();
        // The band gains and the pitch filter pull the peak a couple of samples off the structural delay.
        println!("rnnoise delay: {} samples (correlation {:.3e})", best.0, best.1);
        assert!((best.0 as i64 - RNNOISE_DELAY as i64).abs() <= 4, "lag {} is not near {RNNOISE_DELAY}", best.0);
    }
}
