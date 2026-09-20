//! The real audio backend: WASAPI (through cpal). Loopback-captures the source render endpoint,
//! runs the EQ, plays the result on the target endpoint.

use crate::engine::{AudioBackend, DeviceInfo, Dsp, Route};
use crate::eq::Eq;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Audio kept in hand against scheduling jitter between the capture and playback callbacks.
/// Lower is less delay; too low and playback runs dry (a click) when the PC is busy.
const CUSHION_MS: usize = 8;

pub struct Wasapi;

impl Wasapi {
    /// The id of the output Windows sends audio to by default, in the same form as `DeviceInfo::id`.
    pub fn default_output_id(&self) -> Option<String> {
        Some(cpal::default_host().default_output_device()?.id().ok()?.to_string())
    }
}

impl AudioBackend for Wasapi {
    type Session = (cpal::Stream, cpal::Stream);

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

    fn open(&self, route: &Route, dsp: Arc<Dsp>, on_lost: Box<dyn Fn() + Send + Sync>) -> Result<Self::Session, String> {
        open(route, dsp, Arc::from(on_lost)).map_err(|e| e.to_string())
    }
}

fn open(route: &Route, dsp: Arc<Dsp>, on_lost: Arc<dyn Fn() + Send + Sync>) -> Result<(cpal::Stream, cpal::Stream), Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let from = host.device_by_id(&route.from.parse()?).ok_or("source device is gone")?;
    let to = host.device_by_id(&route.to.parse()?).ok_or("target device is gone")?;
    let in_cfg = from.default_output_config()?;
    let out_cfg = to.default_output_config()?;
    if in_cfg.sample_format() != cpal::SampleFormat::F32 || out_cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err("both devices must run f32 (WASAPI shared mode normally does)".into());
    }
    // ponytail: no resampler, both endpoints must share a rate. Add one when users hit 44.1k-only devices.
    if in_cfg.sample_rate() != out_cfg.sample_rate() {
        return Err(format!("sample rates differ: {} vs {} Hz", in_cfg.sample_rate(), out_cfg.sample_rate()).into());
    }
    let (in_ch, out_ch) = (in_cfg.channels() as usize, out_cfg.channels() as usize);
    let rate = out_cfg.sample_rate() as usize;
    let name = |d: &cpal::Device| d.description().map(|d| d.name().to_string()).unwrap_or_default();
    log::info!("opening {} ({in_ch} ch) -> {} ({out_ch} ch) at {rate} Hz", name(&from), name(&to));

    // ponytail: mutex'd queue between two audio callbacks, and no resampler between the two device
    // clocks. Drift and start-up backlog are handled by `trim` below; an underrun plays silence.
    // Swap for a lock-free ring + async resampler if this glitches under load.
    let queue = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(rate * out_ch)));
    let max_queued = rate * out_ch / 10; // hard ceiling: 100 ms
    let ms = |n: usize| rate * out_ch * n / 1000;

    let on_error = |side: &'static str| {
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

    // Loopback delivers no packets while the endpoint is silent, and WASAPI flags the first packet
    // after a gap as a discontinuity, which cpal reports as an xrun. Only an xrun that lands
    // mid-audio (previous packet was recent) is a real glitch worth reporting.
    let started = Instant::now();
    let last_packet_ms = Arc::new(AtomicU64::new(0));
    let (seen, report) = (last_packet_ms.clone(), on_error("capture"));
    let on_capture_error = move |e: cpal::Error| {
        let gap = started.elapsed().as_millis() as u64 - seen.load(Relaxed);
        if e.kind() != cpal::ErrorKind::Xrun || gap < 30 {
            report(e);
        }
    };

    let mut eq = Eq::new(&dsp.preset(), rate as f64, out_ch);
    let mut scratch: Vec<f32> = Vec::new();
    let producer = queue.clone();
    let capture = from.build_input_stream(
        in_cfg.config(),
        move |data: &[f32], _: &_| {
            last_packet_ms.store(started.elapsed().as_millis() as u64, Relaxed);
            if let Some(preset) = dsp.take_changed() {
                eq.retune(&preset);
            }
            scratch.clear();
            for frame in data.chunks_exact(in_ch) {
                // ponytail: channel mismatch keeps front L/R only; fine while the source is a
                // spatial-sound endpoint, which always hands us binaural stereo.
                scratch.extend((0..out_ch).map(|c| frame[c.min(in_ch - 1)]));
            }
            if !dsp.bypassed() {
                eq.process(&mut scratch);
                crate::eq::limit(&mut scratch);
            }
            dsp.tap(&scratch, out_ch, rate as f32);
            let mut q = producer.lock().unwrap();
            q.extend(scratch.iter());
            let excess = q.len().saturating_sub(max_queued);
            q.drain(..excess);
        },
        on_capture_error,
        None,
    )?;

    let playback = to.build_output_stream(
        out_cfg.config(),
        {
            // Everything that is still in the queue after a callback has taken what it needs is
            // pure delay. It builds up while the streams start (capture runs first) and whenever
            // the output clock runs slow. Over each second, the smallest leftover seen is delay
            // nobody needed: keep a cushion against scheduling jitter and drop the rest, once.
            let (cushion, slack, window) = (ms(CUSHION_MS), ms(2), ms(1000));
            let (mut least_left, mut played, mut dry) = (usize::MAX, 0usize, 0u32);
            let mut starting = true; // the first second always runs dry once: playback starts before data arrives
            move |data: &mut [f32], _: &_| {
                let mut q = queue.lock().unwrap();
                least_left = least_left.min(q.len().saturating_sub(data.len()));
                if q.len() < data.len() {
                    dry += 1;
                }
                played += data.len();
                if played >= window {
                    if least_left > cushion + slack {
                        let excess = (least_left - cushion) / out_ch * out_ch; // whole frames
                        q.drain(..excess);
                        log::info!("dropped {} ms of built-up delay", excess * 1000 / (rate * out_ch));
                    }
                    if dry > 0 && !starting {
                        log::warn!("playback ran dry {dry} times in the last second");
                    }
                    (least_left, played, dry, starting) = (usize::MAX, 0, 0, false);
                }
                for s in data.iter_mut() {
                    *s = q.pop_front().unwrap_or(0.0);
                }
            }
        },
        on_error("playback"),
        None,
    )?;

    capture.play()?;
    playback.play()?;
    Ok((capture, playback))
}
