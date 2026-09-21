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

/// cpal runs our callbacks on threads at normal priority, where a busy PC (a build, a game loading)
/// holds them up for tens of milliseconds: the capture side falls behind, playback runs dry. Windows
/// has a class for exactly this, the one every audio program's render threads are in; a thread
/// joins it once and keeps it for life. Called at the top of every callback; only the first call does anything.
fn join_pro_audio() {
    thread_local!(static JOINED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if JOINED.with(|joined| joined.replace(true)) {
        return;
    }
    let mut task = 0u32;
    // The handle is kept by not closing it: the thread is in the class until it ends.
    if let Err(e) = unsafe { windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(windows::core::w!("Pro Audio"), &mut task) } {
        log::warn!("an audio thread stays at normal priority: {e}");
    }
}

/// Sizes in samples (all channels counted), fixed for a session.
struct Tuning {
    cushion: usize,
    slack: usize,
    /// Samples per frame: drops are whole frames.
    frame: usize,
    per_ms: usize,
}

/// One channel's audio on its way from its capture callback to the playback callback.
struct Feed {
    queue: Arc<Mutex<VecDeque<f32>>>,
    last_packet_ms: Arc<AtomicU64>,
    /// When this channel last started talking after a silence.
    talking_since_ms: Option<u64>,
    /// The least audio left over after a callback took its share, this second.
    least_left: usize,
    dry: u32,
    /// Silence we played because the channel had nothing for us although it was talking: its capture
    /// thread was held up. That audio will arrive late, and its moment has passed.
    owed: usize,
}

impl Feed {
    fn new(queue: Arc<Mutex<VecDeque<f32>>>, last_packet_ms: Arc<AtomicU64>) -> Feed {
        Feed { queue, last_packet_ms, talking_since_ms: None, least_left: usize::MAX, dry: 0, owed: 0 }
    }

    /// Adds this channel's share to `data`. `second_over`: the once-a-second look at the leftovers is due.
    ///
    /// Everything still in the queue after a callback has taken what it needs is pure delay. It
    /// builds up while the streams start (capture runs first) and whenever the output clock runs
    /// slow. Over each second, the smallest leftover seen is delay nobody needed: keep a cushion
    /// against scheduling jitter and drop the rest, once.
    ///
    /// A stall is different: the capture thread is held up (a busy PC), we run dry and play
    /// silence, and then all the audio arrives at once. Played in full it would sit in the queue
    /// as extra delay until the next trim cuts it out: a second glitch a second after the first.
    /// So the late audio is dropped where the silence already is, and the listener gets one gap.
    fn mix_into(&mut self, data: &mut [f32], now_ms: u64, second_over: bool, t: &Tuning) {
        let mut q = self.queue.lock().unwrap();
        let talking = now_ms.saturating_sub(self.last_packet_ms.load(Relaxed)) < SILENT_AFTER_MS;
        self.talking_since_ms = if talking { self.talking_since_ms.or(Some(now_ms)) } else { None };
        let settled = self.talking_since_ms.is_some_and(|since| now_ms - since > SETTLED_AFTER_MS);
        if self.owed > 0 {
            // Never below what this callback needs plus the cushion: late audio goes, the safety margin stays.
            let late = (self.owed.min(q.len().saturating_sub(data.len() + t.cushion))) / t.frame * t.frame;
            if late > 0 {
                q.drain(..late);
                self.owed -= late;
                log::info!("skipped {} ms that arrived too late", late / t.per_ms);
            }
        }
        if settled {
            self.least_left = self.least_left.min(q.len().saturating_sub(data.len()));
            if q.len() < data.len() {
                self.dry += 1;
            }
        }
        if second_over {
            if self.least_left != usize::MAX && self.least_left > t.cushion + t.slack {
                let excess = ((self.least_left - t.cushion) / t.frame * t.frame).min(q.len()); // whole frames
                q.drain(..excess);
                log::info!("dropped {} ms of built-up delay", excess / t.per_ms);
            }
            if self.dry > 0 {
                log::warn!("playback ran dry {} times in the last second", self.dry);
            }
            (self.least_left, self.dry) = (usize::MAX, 0);
        }
        let mut taken = 0;
        for s in data.iter_mut() {
            match q.pop_front() {
                Some(x) => *s += x,
                None => break,
            }
            taken += 1;
        }
        // A channel that stopped talking owes nothing: it ran out because it had nothing to say.
        self.owed = if settled { (self.owed + data.len() - taken).min(t.per_ms * 100) } else { 0 };
    }
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
        feeds.push(Feed::new(queue.clone(), last_packet_ms.clone()));

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
                join_pro_audio();
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

    let tuning = Tuning { cushion: ms(CUSHION_MS), slack: ms(2), frame: out_ch, per_ms: ms(1) };
    let window = ms(1000);
    let mut played = 0usize;
    let mut master = Eq::new(&dsp.eq.get(), rate as f64, out_ch);
    let playback = to.build_output_stream(
        out_cfg.config(),
        move |data: &mut [f32], _: &_| {
            join_pro_audio();
            let now_ms = started.elapsed().as_millis() as u64;
            played += data.len();
            let second_over = played >= window;
            data.fill(0.0);
            for feed in feeds.iter_mut() {
                feed.mix_into(data, now_ms, second_over, &tuning);
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

#[cfg(test)]
mod tests {
    use super::*;

    const TICK_MS: u64 = 10;
    const PACKET: usize = 480; // 10 ms of mono at 48 kHz

    /// A channel carrying a ramp (1, 2, 3, ...), so every sample that is played says where it came from.
    struct Rig {
        feed: Feed,
        tuning: Tuning,
        now_ms: u64,
        next: f32,
        played: Vec<f32>,
    }

    impl Rig {
        fn new() -> Rig {
            let feed = Feed::new(Arc::new(Mutex::new(VecDeque::new())), Arc::new(AtomicU64::new(0)));
            Rig { feed, tuning: Tuning { cushion: 48 * CUSHION_MS, slack: 48 * 2, frame: 1, per_ms: 48 }, now_ms: 0, next: 1.0, played: Vec::new() }
        }
        fn capture(&mut self, packets: usize) {
            let mut q = self.feed.queue.lock().unwrap();
            for _ in 0..packets * PACKET {
                q.push_back(self.next);
                self.next += 1.0;
            }
            self.feed.last_packet_ms.store(self.now_ms, Relaxed);
        }
        /// One playback callback, 10 ms later than the last.
        fn play(&mut self) {
            self.now_ms += TICK_MS;
            let mut data = vec![0.0; PACKET];
            self.feed.mix_into(&mut data, self.now_ms, self.now_ms % 1000 == 0, &self.tuning);
            self.played.extend(data);
        }
        fn delay_ms(&self) -> usize {
            self.feed.queue.lock().unwrap().len() / 48
        }
    }

    #[test]
    fn a_capture_stall_costs_one_gap_and_no_delay() {
        let mut rig = Rig::new();
        rig.capture(3); // capture runs first: start-up backlog
        for _ in 0..300 {
            rig.capture(1);
            rig.play();
        }
        let settled = rig.delay_ms();
        assert!(settled <= CUSHION_MS + 2, "the start-up backlog is trimmed to the cushion, got {settled} ms");

        // The capture thread doesn't get to run for 30 ms (a busy PC), then hands over everything at once.
        // (From SILENT_AFTER_MS on, a stall can't be told from the channel going quiet.)
        let before_stall = rig.played.len();
        for _ in 0..3 {
            rig.play();
        }
        rig.capture(3);
        for _ in 0..5 {
            rig.capture(1);
            rig.play();
        }
        assert!(rig.delay_ms() <= settled + 1, "the audio that arrived late must not stay in the queue as delay: {} ms, was {settled} ms", rig.delay_ms());

        // From here on nothing may be cut again: the listener has had their one gap.
        let after_recovery = rig.played.len();
        for _ in 0..300 {
            rig.capture(1);
            rig.play();
        }
        let later = &rig.played[after_recovery..];
        assert!(later.windows(2).all(|w| w[1] == w[0] + 1.0), "audio was cut again after the gap had passed");
        let silent = rig.played[before_stall..after_recovery].iter().filter(|&&s| s == 0.0).count();
        assert!(silent > 0 && silent <= 3 * PACKET, "the stall itself is heard as silence, once: {silent} samples");
    }
}
