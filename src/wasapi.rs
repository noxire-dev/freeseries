//! The real audio backend: WASAPI (through cpal). Loopback-captures the source render endpoint,
//! runs the EQ, plays the result on the target endpoint.

use crate::engine::{AudioBackend, DeviceInfo, Dsp, Route};
use crate::eq::Eq;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct Wasapi;

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

    // ponytail: mutex'd queue between two audio callbacks, and no clock-drift compensation between
    // the two devices (overflow drops oldest, underrun plays silence). examples/soak.rs shows it
    // clean over 30 s; swap for a lock-free ring + async resampler if long sessions glitch.
    let queue = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(rate * out_ch)));
    let max_queued = rate * out_ch / 10; // 100 ms ceiling on added latency

    let on_error = |side: &'static str| {
        let on_lost = on_lost.clone();
        move |e: cpal::Error| {
            eprintln!("{side} error: {e}");
            if e.kind() != cpal::ErrorKind::Xrun {
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
            }
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
        move |data: &mut [f32], _: &_| {
            let mut q = queue.lock().unwrap();
            for s in data.iter_mut() {
                *s = q.pop_front().unwrap_or(0.0);
            }
        },
        on_error("playback"),
        None,
    )?;

    capture.play()?;
    playback.play()?;
    Ok((capture, playback))
}
