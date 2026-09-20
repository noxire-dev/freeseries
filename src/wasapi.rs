//! The real audio backend: WASAPI (through cpal). Loopback-captures every channel's virtual device,
//! applies that channel's EQ and volume, mixes them, runs the output's EQ over the mix and plays
//! the result on the target endpoint.

use crate::engine::{AudioBackend, ChannelDsp, DeviceInfo, Dsp, Mix};
use crate::eq::{limit, Eq};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Audio kept in hand against scheduling jitter between the capture and playback callbacks.
/// Lower is less delay; too low and playback runs dry (a click) when the PC is busy.
const CUSHION_MS: usize = 8;
/// Loopback capture delivers nothing while a device is silent. A channel whose last packet is
/// older than this isn't running dry, it just has nothing to say.
const SILENT_AFTER_MS: u64 = 50;
/// A channel that just woke from silence starts with an empty queue. That isn't starvation, and
/// its leftovers say nothing about steady-state delay, until it has been going for this long.
const SETTLED_AFTER_MS: u64 = 150;

pub struct Wasapi;

impl Wasapi {
    /// The id of the output Windows sends audio to by default, in the same form as `DeviceInfo::id`.
    pub fn default_output_id(&self) -> Option<String> {
        Some(cpal::default_host().default_output_device()?.id().ok()?.to_string())
    }
}

impl AudioBackend for Wasapi {
    type Session = Vec<cpal::Stream>;

    fn outputs(&self) -> Result<Vec<DeviceInfo>, String> {
        let devices = cpal::default_host().output_devices().map_err(|e| e.to_string())?;
        Ok(devices
            .filter_map(|d| {
                let cfg = d.default_output_config().ok()?;
                Some(DeviceInfo {
                    id: d.id().ok()?.to_string(),
                    name: d.description().ok()?.name().to_string(),
                    channels: cfg.channels(),
                    sample_rate: cfg.sample_rate(),
                })
            })
            .collect())
    }

    fn open(&self, mix: &Mix, dsp: Arc<Dsp>, on_lost: Box<dyn Fn() + Send + Sync>) -> Result<Self::Session, String> {
        open(mix, dsp, Arc::from(on_lost)).map_err(|e| e.to_string())
    }
}

/// What the playback callback needs from one channel.
struct Feed {
    queue: Arc<Mutex<VecDeque<f32>>>,
    last_packet_ms: Arc<AtomicU64>,
    /// When this channel last started talking after a silence.
    talking_since_ms: Option<u64>,
    /// The least audio left over after a callback took its share, this second. See `open`.
    least_left: usize,
    dry: u32,
}

fn open(mix: &Mix, dsp: Arc<Dsp>, on_lost: Arc<dyn Fn() + Send + Sync>) -> Result<Vec<cpal::Stream>, Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let name = |d: &cpal::Device| d.description().map(|d| d.name().to_string()).unwrap_or_default();
    let to = host.device_by_id(&mix.to.parse()?).ok_or("target device is gone")?;
    let out_cfg = to.default_output_config()?;
    if out_cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err("the output must run f32 (WASAPI shared mode normally does)".into());
    }
    let out_ch = out_cfg.channels() as usize;
    let rate = out_cfg.sample_rate() as usize;
    let ms = |n: usize| rate * out_ch * n / 1000;
    let max_queued = ms(100); // hard ceiling on a channel's delay
    let started = Instant::now();

    let on_error = |side: String| {
        let on_lost = on_lost.clone();
        move |e: cpal::Error| {
            if e.kind() == cpal::ErrorKind::Xrun {
                log::warn!("{side} glitch: {e}");
            } else {
                log::error!("{side} stream died: {e}");
                on_lost();
            }
        }
    };

    let mut streams = Vec::new();
    let mut feeds = Vec::new();
    for source in &mix.sources {
        // A channel whose device is missing or mismatched is left out; the others still play.
        let Some(from) = source.from.parse().ok().and_then(|id| host.device_by_id(&id)) else {
            log::warn!("channel {}: its device is gone, leaving it out", source.channel);
            continue;
        };
        let in_cfg = from.default_output_config()?;
        // ponytail: no resampler, every device must share the output's rate. Add one when users hit 44.1k-only devices.
        if in_cfg.sample_format() != cpal::SampleFormat::F32 || in_cfg.sample_rate() != out_cfg.sample_rate() {
            log::warn!("channel {}: {} runs at {} Hz, the output at {} Hz; leaving it out", source.channel, name(&from), in_cfg.sample_rate(), rate);
            continue;
        }
        let in_ch = in_cfg.channels() as usize;
        log::info!("channel {}: {} ({in_ch} ch) -> {} ({out_ch} ch) at {rate} Hz", source.channel, name(&from), name(&to));

        // ponytail: mutex'd queue between two audio callbacks, and no resampler between the device
        // clocks. Drift and start-up backlog are handled by the trim in the playback callback; an
        // underrun plays silence. Swap for a lock-free ring + async resampler if this glitches under load.
        let queue = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(rate * out_ch)));
        let last_packet_ms = Arc::new(AtomicU64::new(0));
        feeds.push(Feed { queue: queue.clone(), last_packet_ms: last_packet_ms.clone(), talking_since_ms: None, least_left: usize::MAX, dry: 0 });

        // Loopback delivers no packets while the endpoint is silent, and WASAPI flags the first
        // packet after a gap as a discontinuity, which cpal reports as an xrun. Only an xrun that
        // lands mid-audio (previous packet was recent) is a real glitch worth reporting.
        let (seen, report) = (last_packet_ms.clone(), on_error(format!("{} capture", source.channel)));
        let on_capture_error = move |e: cpal::Error| {
            let gap = started.elapsed().as_millis() as u64 - seen.load(Relaxed);
            if e.kind() != cpal::ErrorKind::Xrun || gap < 30 {
                report(e);
            }
        };

        let channel: Arc<ChannelDsp> = dsp.channel(&source.channel);
        let mut eq = Eq::new(&channel.eq.get(), rate as f64, out_ch);
        let mut scratch: Vec<f32> = Vec::new();
        let shared = dsp.clone();
        let capture = from.build_input_stream(
            in_cfg.config(),
            move |data: &[f32], _: &_| {
                last_packet_ms.store(started.elapsed().as_millis() as u64, Relaxed);
                if let Some(preset) = channel.eq.take_changed() {
                    eq.retune(&preset);
                }
                scratch.clear();
                for frame in data.chunks_exact(in_ch) {
                    // ponytail: channel mismatch keeps front L/R only; fine while the source is a
                    // spatial-sound endpoint, which always hands us binaural stereo.
                    scratch.extend((0..out_ch).map(|c| frame[c.min(in_ch - 1)]));
                }
                if !shared.bypassed() {
                    eq.process(&mut scratch);
                }
                let volume = channel.volume();
                if volume != 1.0 {
                    scratch.iter_mut().for_each(|s| *s *= volume);
                }
                channel.meter.feed(&scratch);
                let mut q = queue.lock().unwrap();
                q.extend(scratch.iter());
                let excess = q.len().saturating_sub(max_queued);
                q.drain(..excess);
            },
            on_capture_error,
            None,
        )?;
        streams.push(capture);
    }
    if feeds.is_empty() {
        return Err("none of the freeseries audio devices can be captured".into());
    }

    // Everything still in a channel's queue after a callback has taken what it needs is pure
    // delay. It builds up while the streams start (capture runs first) and whenever the output
    // clock runs slow. Over each second, the smallest leftover seen is delay nobody needed: keep a
    // cushion against scheduling jitter and drop the rest, once.
    let (cushion, slack, window) = (ms(CUSHION_MS), ms(2), ms(1000));
    let mut played = 0usize;
    let mut master = Eq::new(&dsp.eq.get(), rate as f64, out_ch);
    let playback = to.build_output_stream(
        out_cfg.config(),
        move |data: &mut [f32], _: &_| {
            let now_ms = started.elapsed().as_millis() as u64;
            played += data.len();
            let second_over = played >= window;
            data.fill(0.0);
            for feed in feeds.iter_mut() {
                let mut q = feed.queue.lock().unwrap();
                let talking = now_ms.saturating_sub(feed.last_packet_ms.load(Relaxed)) < SILENT_AFTER_MS;
                feed.talking_since_ms = if talking { feed.talking_since_ms.or(Some(now_ms)) } else { None };
                if feed.talking_since_ms.is_some_and(|since| now_ms - since > SETTLED_AFTER_MS) {
                    feed.least_left = feed.least_left.min(q.len().saturating_sub(data.len()));
                    if q.len() < data.len() {
                        feed.dry += 1;
                    }
                }
                if second_over {
                    if feed.least_left != usize::MAX && feed.least_left > cushion + slack {
                        let excess = ((feed.least_left - cushion) / out_ch * out_ch).min(q.len()); // whole frames
                        q.drain(..excess);
                        log::info!("dropped {} ms of built-up delay", excess * 1000 / (rate * out_ch));
                    }
                    if feed.dry > 0 {
                        log::warn!("playback ran dry {} times in the last second", feed.dry);
                    }
                    (feed.least_left, feed.dry) = (usize::MAX, 0);
                }
                for s in data.iter_mut() {
                    match q.pop_front() {
                        Some(x) => *s += x,
                        None => break,
                    }
                }
            }
            if second_over {
                played = 0;
            }

            // The output's own EQ (headphone correction + sound) over the mix, then the limiter.
            if let Some(preset) = dsp.eq.take_changed() {
                master.retune(&preset);
            }
            if !dsp.bypassed() {
                master.process(data);
                limit(data);
            }
            dsp.meter.feed(data);
            dsp.tap(data, out_ch, rate as f32);
        },
        on_error("playback".into()),
        None,
    )?;

    for capture in &streams {
        capture.play()?;
    }
    playback.play()?;
    streams.push(playback);
    Ok(streams)
}
