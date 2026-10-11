//! The audio backend on WASAPI directly, without cpal. One thread per session: the render
//! endpoint's event wakes it every period, it drains every channel's loopback, runs the channel
//! EQ and volume, mixes, runs the output EQ and the limiter, and writes the render buffer. No
//! second thread and no hand-off between threads, which is where the cpal backend spent 12 to
//! 40 ms (`docs/roadmap.md`, the lowlat spike). Same `Feed` logic as the cpal backend for
//! priming and easing, so a sound that starts from silence is never cut.

#![allow(unsafe_op_in_unsafe_fn)] // whole functions are unsafe here, block by block would say nothing more

use crate::engine::{AudioBackend, ChannelDsp, DeviceInfo, Dsp, Mix};
use crate::eq::{limit, Eq};
use crate::wasapi::{join_pro_audio, Feed, Tuning, CUSHION_MS, SETTLED_AFTER_MS, SILENT_AFTER_MS};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::core::{GUID, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// How cpal spells a device id, and what the app has stored: "wasapi:" then Windows' own id.
pub(crate) const ID_PREFIX: &str = "wasapi:";
const IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
/// 200 ms, in the 100 ns units WASAPI counts in.
pub(crate) const CAPTURE_BUFFER_HNS: i64 = 2_000_000;
/// We keep two 10 ms periods in the device: a wake later than that found it empty.
const LATE_WAKE_MS: f64 = 20.0;

pub struct Native;

/// The stream ends when this is dropped.
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

pub(crate) struct Format {
    pub(crate) rate: u32,
    pub(crate) channels: usize,
}

/// The endpoint's mix format, if it is what shared mode gives everyone: float samples.
pub(crate) unsafe fn format_of(client: &IAudioClient) -> Result<(Format, *mut WAVEFORMATEX), String> {
    let f = client.GetMixFormat().map_err(|e| e.to_string())?;
    let tag = (*f).wFormatTag;
    let float = tag == WAVE_FORMAT_IEEE_FLOAT || (tag == WAVE_FORMAT_EXTENSIBLE && std::ptr::read_unaligned(std::ptr::addr_of!((*(f as *const WAVEFORMATEXTENSIBLE)).SubFormat)) == IEEE_FLOAT);
    if !float || (*f).wBitsPerSample != 32 {
        return Err("the endpoint's shared-mode format is not 32-bit float".into());
    }
    Ok((Format { rate: (*f).nSamplesPerSec, channels: (*f).nChannels as usize }, f))
}

pub(crate) fn enumerator() -> Result<IMMDeviceEnumerator, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| e.to_string())
    }
}

pub(crate) fn device(id: &str) -> Result<IMMDevice, String> {
    let raw = id.strip_prefix(ID_PREFIX).unwrap_or(id);
    let wide: Vec<u16> = raw.encode_utf16().chain([0]).collect();
    unsafe { enumerator()?.GetDevice(PCWSTR(wide.as_ptr())).map_err(|e| format!("device {raw}: {e}")) }
}

impl AudioBackend for Native {
    type Session = Session;

    fn outputs(&self) -> Result<Vec<DeviceInfo>, String> {
        unsafe {
            let all = enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE).map_err(|e| e.to_string())?;
            let mut found = Vec::new();
            for i in 0..all.GetCount().map_err(|e| e.to_string())? {
                let d = all.Item(i).map_err(|e| e.to_string())?;
                let id = d.GetId().map_err(|e| e.to_string())?.to_string().map_err(|e| e.to_string())?;
                let name = d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&PKEY_Device_FriendlyName)).map(|v| v.to_string()).unwrap_or_default();
                let Ok(client) = d.Activate::<IAudioClient>(CLSCTX_ALL, None) else { continue };
                let Ok((format, _)) = format_of(&client) else { continue };
                // A spatial renderer offers dynamic objects; with spatial sound off there are none.
                let spatial = d.Activate::<ISpatialAudioClient>(CLSCTX_ALL, None).and_then(|c| c.GetMaxDynamicObjectCount()).is_ok_and(|objects| objects > 0);
                found.push(DeviceInfo { id: format!("{ID_PREFIX}{id}"), name, channels: format.channels as u16, sample_rate: format.rate, spatial });
            }
            Ok(found)
        }
    }

    fn open(&self, mix: &Mix, dsp: Arc<Dsp>, on_lost: Box<dyn Fn() + Send + Sync>) -> Result<Session, String> {
        let mix = mix.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        // Everything WASAPI is created on the thread that will use it.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                match unsafe { start(&mix, &dsp) } {
                    Ok(session) => {
                        let _ = ready_tx.send(Ok(()));
                        if let Err(e) = unsafe { run(session, &dsp, &stopping) } {
                            log::error!("audio session died: {e}");
                            on_lost();
                        }
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|_| "the audio thread ended before it started".to_string())??;
        Ok(Session { stop, thread: Some(thread) })
    }
}

struct Capture {
    channel: String,
    client: IAudioClient,
    reader: IAudioCaptureClient,
    in_ch: usize,
    eq: Eq,
    dsp: Arc<ChannelDsp>,
    feed: Feed,
    scratch: Vec<f32>,
    last_packet_ms: Arc<AtomicU64>,
    woke_ms: u64,
    /// The device position the next packet starts at, if nothing is lost in between.
    next_position: u64,
}

struct Running {
    render: IAudioClient,
    writer: IAudioRenderClient,
    event: HANDLE,
    out_size: u32,
    out_ch: usize,
    rate: u32,
    captures: Vec<Capture>,
    master: Eq,
    tuning: Tuning,
}

unsafe fn start(mix: &Mix, dsp: &Arc<Dsp>) -> Result<Running, String> {
    let render: IAudioClient = device(&mix.to)?.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
    let (out, out_format) = format_of(&render)?;
    let (rate, out_ch) = (out.rate, out.channels);
    let ms = |n: usize| rate as usize * out_ch * n / 1000;

    let mut captures = Vec::new();
    for source in &mix.sources {
        let Ok(from) = device(&source.from) else {
            log::warn!("channel {}: its device is gone, leaving it out", source.channel);
            continue;
        };
        let client: IAudioClient = from.Activate(CLSCTX_ALL, None).map_err(|e| e.to_string())?;
        let (input, in_format) = format_of(&client)?;
        // ponytail: no resampler, every device must share the output's rate.
        if input.rate != rate {
            log::warn!("channel {}: runs at {} Hz, the output at {rate} Hz; leaving it out", source.channel, input.rate);
            continue;
        }
        // The default buffer holds two packets. Three are waiting whenever our wake comes a little late at the
        // wrong moment, and the third is lost. Room costs no delay: it is only used when we are late.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, CAPTURE_BUFFER_HNS, 0, in_format, None).map_err(|e| format!("channel {}: {e}", source.channel))?;
        let reader: IAudioCaptureClient = client.GetService().map_err(|e| e.to_string())?;
        let channel = dsp.channel(&source.channel);
        let last_packet_ms = Arc::new(AtomicU64::new(0));
        let queue = Arc::new(Mutex::new(VecDeque::with_capacity(ms(200))));
        captures.push(Capture {
            channel: source.channel.clone(),
            eq: Eq::new(&channel.eq.get(), rate as f64, out_ch),
            feed: Feed::new(queue, last_packet_ms.clone()),
            last_packet_ms,
            woke_ms: 0,
            next_position: 0,
            client,
            reader,
            in_ch: input.channels,
            dsp: channel,
            scratch: Vec::new(),
        });
        let room = captures.last().and_then(|c: &Capture| c.client.GetBufferSize().ok()).unwrap_or(0) as usize * 1000 / rate as usize;
        log::info!("channel {}: {} ch -> {out_ch} ch at {rate} Hz (native, room for {room} ms of capture)", source.channel, input.channels);
    }
    if captures.is_empty() {
        return Err("none of the FreeSeries audio devices can be captured".into());
    }

    render.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 0, 0, out_format, None).map_err(|e| format!("output: {e}"))?;
    let event = CreateEventW(None, false, false, PCWSTR::null()).map_err(|e| e.to_string())?;
    render.SetEventHandle(event).map_err(|e| e.to_string())?;
    let writer: IAudioRenderClient = render.GetService().map_err(|e| e.to_string())?;
    let out_size = render.GetBufferSize().map_err(|e| e.to_string())?;
    let master = Eq::new(&dsp.eq.get(), rate as f64, out_ch);
    let tuning = Tuning { cushion: ms(CUSHION_MS), slack: ms(2), frame: out_ch, per_ms: ms(1) };
    for c in &captures {
        c.client.Start().map_err(|e| e.to_string())?;
    }
    render.Start().map_err(|e| e.to_string())?;
    Ok(Running { render, writer, event, out_size, out_ch, rate, captures, master, tuning })
}

/// Drains everything the device has for this channel into its queue.
unsafe fn drain(c: &mut Capture, dsp: &Dsp, out_ch: usize, now_ms: u64, wake_gap_ms: f64, max_queued: usize) -> windows::core::Result<()> {
    loop {
        if c.reader.GetNextPacketSize()? == 0 {
            return Ok(());
        }
        let (mut data, mut got, mut flags, mut position) = (std::ptr::null_mut::<u8>(), 0u32, 0u32, 0u64);
        c.reader.GetBuffer(&mut data, &mut got, &mut flags, Some(&mut position), None)?;
        let waking = now_ms - c.last_packet_ms.load(Relaxed) >= SILENT_AFTER_MS;
        if waking {
            c.woke_ms = now_ms;
        }
        c.last_packet_ms.store(now_ms, Relaxed);
        if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 && !waking && now_ms - c.woke_ms >= SETTLED_AFTER_MS {
            // Frames the device counted that never reached us. None: Windows' own engine hiccuped upstream.
            let lost = position.saturating_sub(c.next_position);
            log::warn!("{} capture glitch: {lost} frames lost; this wake came {wake_gap_ms:.0} ms after the one before", c.channel);
        }
        c.next_position = position + got as u64;
        let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
        let samples = std::slice::from_raw_parts(data as *const f32, got as usize * c.in_ch);
        if let Some(preset) = c.dsp.eq.take_changed() {
            c.eq.retune(&preset);
        }
        c.scratch.clear();
        for frame in samples.chunks_exact(c.in_ch) {
            // Channel mismatch keeps front L/R; a spatial endpoint always hands us stereo.
            c.scratch.extend((0..out_ch).map(|ch| if silent { 0.0 } else { frame[ch.min(c.in_ch - 1)] }));
        }
        if !dsp.bypassed() {
            c.eq.process(&mut c.scratch);
        }
        let volume = c.dsp.volume();
        if volume != 1.0 {
            c.scratch.iter_mut().for_each(|x| *x *= volume);
        }
        c.dsp.meter.feed(&c.scratch);
        {
            let mut q = c.feed.queue.lock().unwrap();
            q.extend(c.scratch.iter());
            let excess = q.len().saturating_sub(max_queued);
            q.drain(..excess);
        }
        c.reader.ReleaseBuffer(got)?;
    }
}

unsafe fn run(mut s: Running, dsp: &Arc<Dsp>, stop: &AtomicBool) -> Result<(), String> {
    join_pro_audio();
    let started = Instant::now();
    let max_queued = s.tuning.per_ms * 100; // hard ceiling on a channel's delay
    let window = s.tuning.per_ms * 1000;
    let mut played = 0usize;
    // The device tells us how much of its buffer it has left each time it wakes us. Filling all of
    // the rest is the whole 22 ms buffer of delay on a device that empties it between wakes (the
    // Arctis reads 30-40 ms added where the monitor reads 9); two periods ahead is enough for any
    // wake jitter seen (3 ms) and keeps the render side at roughly one period.
    let period = s.rate as usize / 100; // frames; every endpoint on this PC runs 10 ms periods
    let ahead = (2 * period) as u32;
    let mut stats = (u32::MAX, 0u32, 0u64, 0u32); // padding at wake: min, max, sum, count, over the first seconds
    let (mut last_wake, mut late) = (Instant::now(), (0u32, 0f64)); // late wakes this second: how many, the worst
    let result = loop {
        if stop.load(Relaxed) {
            break Ok(());
        }
        if WaitForSingleObject(s.event, 1000) != WAIT_OBJECT_0 {
            continue; // a second without the render event: if the device is gone, the next call says so
        }
        let now_ms = started.elapsed().as_millis() as u64;
        let gap_ms = last_wake.elapsed().as_secs_f64() * 1000.0;
        last_wake = Instant::now();
        if gap_ms > LATE_WAKE_MS {
            late = (late.0 + 1, late.1.max(gap_ms));
        }
        if let Some(e) = s.captures.iter_mut().find_map(|c| drain(c, dsp, s.out_ch, now_ms, gap_ms, max_queued).err().map(|e| format!("{} capture: {e}", c.channel))) {
            break Err(e);
        }

        let padding = match s.render.GetCurrentPadding() {
            Ok(p) => p,
            Err(e) => break Err(format!("output: {e}")),
        };
        if stats.3 < 500 {
            stats = (stats.0.min(padding), stats.1.max(padding), stats.2 + padding as u64, stats.3 + 1);
            if stats.3 == 500 {
                log::info!("output buffer {} frames; at wake the device still held {}..{} frames, {} on average", s.out_size, stats.0, stats.1, stats.2 / 500);
            }
        }
        let space = (s.out_size - padding).min(ahead.saturating_sub(padding));
        if space == 0 {
            continue;
        }
        let buffer = match s.writer.GetBuffer(space) {
            Ok(b) => b,
            Err(e) => break Err(format!("output: {e}")),
        };
        let data = std::slice::from_raw_parts_mut(buffer as *mut f32, space as usize * s.out_ch);
        data.fill(0.0);
        played += data.len();
        let second_over = played >= window;
        for c in s.captures.iter_mut() {
            c.feed.mix_into(data, now_ms, second_over, &s.tuning);
        }
        if second_over {
            played = 0;
            // The device plays out what it holds while it waits for us: a wake this late is a hole in the sound,
            // whoever held it up (a stalled USB controller, a PC too busy to run this thread).
            if late.0 > 0 {
                log::warn!("the output woke the engine late {} times in the last second, worst {:.0} ms after the wake before", late.0, late.1);
            }
            late = (0, 0.0);
        }
        if let Some(preset) = dsp.eq.take_changed() {
            s.master.retune(&preset);
        }
        if !dsp.bypassed() {
            s.master.process(data);
            limit(data);
        }
        dsp.meter.feed(data);
        dsp.tap(data, s.out_ch, s.rate as f32);
        if let Err(e) = s.writer.ReleaseBuffer(space, 0) {
            break Err(format!("output: {e}"));
        }
    };
    let _ = s.render.Stop();
    for c in &s.captures {
        let _ = c.client.Stop();
    }
    let _ = CloseHandle(s.event);
    result
}
