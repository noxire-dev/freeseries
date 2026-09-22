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

/// Delay that built up is eased out by playing this much faster until it is gone: half a percent
/// is 9 cents, which nobody hears for two seconds. Only a big backlog (the streams starting, a long
/// stall) goes faster. The alternative, cutting the excess out in one piece, is a click in the
/// middle of whatever is playing.
const EASE: f64 = 0.005;
const EASE_FAST: f64 = 0.02;
const EASE_FAST_ABOVE_MS: usize = 10;

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
    /// A channel waking from silence holds its first samples until the cushion is there, so the
    /// sound doesn't start with holes in it.
    primed: bool,
    /// Frames of delay still to be eased out, and how far into the front frame of the queue the
    /// reading has got (0 unless easing: then the output is interpolated between two frames).
    ease: f64,
    pos: f64,
}

impl Feed {
    fn new(queue: Arc<Mutex<VecDeque<f32>>>, last_packet_ms: Arc<AtomicU64>) -> Feed {
        Feed { queue, last_packet_ms, talking_since_ms: None, least_left: usize::MAX, dry: 0, primed: false, ease: 0.0, pos: 0.0 }
    }

    /// Adds this channel's share to `data`. `second_over`: the once-a-second look at the leftovers is due.
    ///
    /// Everything still in the queue after a callback has taken what it needs is pure delay. It
    /// builds up while the streams start (capture runs first) and whenever the output clock runs
    /// slow. Over each second, the smallest leftover seen is delay nobody needed: keep a cushion
    /// against scheduling jitter and ease the rest out (see `EASE`).
    ///
    fn mix_into(&mut self, data: &mut [f32], now_ms: u64, second_over: bool, t: &Tuning) {
        let mut q = self.queue.lock().unwrap();
        let talking = now_ms.saturating_sub(self.last_packet_ms.load(Relaxed)) < SILENT_AFTER_MS;
        self.talking_since_ms = if talking { self.talking_since_ms.or(Some(now_ms)) } else { None };
        let settled = self.talking_since_ms.is_some_and(|since| now_ms - since > SETTLED_AFTER_MS);
        if settled {
            self.least_left = self.least_left.min(q.len().saturating_sub(data.len()));
            if q.len() < data.len() {
                self.dry += 1;
            }
        }
        if second_over {
            if self.least_left != usize::MAX && self.least_left > t.cushion + t.slack {
                // What is still left of an earlier easing is part of this measurement: replace, don't add.
                // Less the fraction the reader already stands at, so that it ends exactly on a frame.
                self.ease = ((self.least_left - t.cushion) / t.frame) as f64 - self.pos.fract();
                log::info!("easing out {} ms of built-up delay", (self.least_left - t.cushion) / t.per_ms);
            }
            // Running out because the sound ended is not a glitch: only a channel that is still going counts.
            if self.dry > 0 && talking {
                log::warn!("playback ran dry {} times in the last second", self.dry);
            }
            (self.least_left, self.dry) = (usize::MAX, 0);
        }
        if !talking {
            self.primed = false; // whatever is left plays out below
        } else if !self.primed {
            // Twice the cushion: how much margin the timing jitter really needs is measured over the
            // next second and the rest eased out. Starting at the bare cushion runs dry on the first late packet.
            let start_with = data.len() + 2 * t.cushion;
            if q.len() < start_with {
                return; // nothing of the sound is lost: it begins a few milliseconds later
            }
            self.primed = true;
            self.pos = 0.0;
            self.ease = ((q.len() - start_with) / t.frame) as f64; // loopback wakes with a burst
        }

        // Normally this copies frames. While easing it reads a little faster than it plays,
        // interpolating between neighbours, and lands exactly on a frame again when it is done.
        let frames = q.len() / t.frame;
        for out in data.chunks_exact_mut(t.frame) {
            let (at, between) = (self.pos as usize, self.pos.fract());
            if at + (between > 0.0) as usize >= frames {
                break;
            }
            for (c, s) in out.iter_mut().enumerate() {
                let here = q[at * t.frame + c];
                *s += if between > 0.0 { here + (q[(at + 1) * t.frame + c] - here) * between as f32 } else { here };
            }
            let faster = if self.ease > (EASE_FAST_ABOVE_MS * t.per_ms / t.frame) as f64 { EASE_FAST } else { EASE }.min(self.ease);
            self.ease -= faster;
            self.pos += 1.0 + faster;
            if self.ease == 0.0 && (self.pos - self.pos.round()).abs() < 1e-6 {
                self.pos = self.pos.round(); // back on a frame: bit-exact copying from here on
            }
        }
        let used = (self.pos as usize).min(frames);
        q.drain(..used * t.frame);
        self.pos -= used as f64;
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
        // packets after a gap as discontinuities, which cpal reports as xruns. A sound starting is
        // not a glitch: only an xrun in a channel that has been going for a while is worth reporting.
        let woke_ms = Arc::new(AtomicU64::new(0));
        let (seen, woke, report) = (last_packet_ms.clone(), woke_ms.clone(), on_error(format!("{} capture", source.channel)));
        let on_capture_error = move |e: cpal::Error| {
            let now = started.elapsed().as_millis() as u64;
            let waking = now - seen.load(Relaxed) >= SILENT_AFTER_MS || now - woke.load(Relaxed) < SETTLED_AFTER_MS;
            if e.kind() != cpal::ErrorKind::Xrun || !waking {
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
                let now = started.elapsed().as_millis() as u64;
                if now - last_packet_ms.load(Relaxed) >= SILENT_AFTER_MS {
                    woke_ms.store(now, Relaxed);
                }
                last_packet_ms.store(now, Relaxed);
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
    fn a_sound_that_starts_from_silence_is_never_cut() {
        let mut rig = Rig::new();
        for _ in 0..50 {
            rig.play(); // the channel has nothing to say
        }
        // Loopback wakes up with a burst: the first packets arrive together.
        rig.capture(2);
        for _ in 0..300 {
            rig.capture(1);
            rig.play();
        }
        let heard: Vec<f32> = rig.played.iter().copied().filter(|&s| s != 0.0).collect();
        assert_eq!(heard.first(), Some(&1.0), "the sound starts with its first sample");
        assert!(heard.windows(2).all(|w| w[1] > w[0] && w[1] - w[0] < 1.03), "nothing may be cut out of it: delay is eased out, a fraction of a sample at a time");
        assert_eq!(rig.feed.ease, 0.0, "three seconds in, the easing is over");
        let tail = &heard[heard.len() - 480..];
        assert!(tail.windows(2).all(|w| w[1] == w[0] + 1.0) && tail[0].fract() == 0.0, "and the samples are copied untouched again");
        assert!(rig.delay_ms() <= CUSHION_MS + 2, "and it runs at the cushion, not at whatever the burst left: {} ms", rig.delay_ms());
    }

    /// What a game night looks like: capture packets that are late by more than the cushion, now and
    /// then. The queue has to grow to cover that on its own, and nothing may ever be cut.
    #[test]
    fn jitter_is_absorbed_by_a_deeper_queue_never_by_cutting() {
        let mut rig = Rig::new();
        rig.capture(3);
        for i in 0..1200 {
            // Every 7th packet arrives one callback late, so playback sees a 10 ms hole once in a while.
            if i % 7 != 6 {
                rig.capture(1);
            } else {
                rig.play();
                rig.capture(2);
            }
            rig.play();
        }
        let heard: Vec<f32> = rig.played.iter().copied().filter(|&s| s != 0.0).collect();
        let cuts = heard.windows(2).filter(|w| w[1] - w[0] > 1.03).count();
        assert_eq!(cuts, 0, "audio was cut {cuts} times: the queue must deepen to cover the jitter instead");
        let quiet_after_settling = rig.played[rig.played.len() / 2..].iter().filter(|&&s| s == 0.0).count();
        assert_eq!(quiet_after_settling, 0, "once the queue has grown to cover the jitter, nothing runs dry");
    }

    /// A capture stall (the thread held up, a busy PC) is heard once, as silence. The audio that
    /// then arrives late is played, not cut: cutting it would run the queue dry again on the next
    /// late packet, and that loop was a crackle once a second on a game night (2026-09-22). The
    /// delay it leaves behind eases out on its own once the next second shows it wasn't needed.
    #[test]
    fn a_capture_stall_is_one_gap_and_the_delay_eases_out() {
        let mut rig = Rig::new();
        rig.capture(3);
        for _ in 0..400 {
            rig.capture(1);
            rig.play();
        }
        let settled = rig.delay_ms();
        let before_stall = rig.played.len();
        for _ in 0..3 {
            rig.play(); // 30 ms with nothing from the capture side
        }
        rig.capture(3);
        for _ in 0..400 {
            rig.capture(1);
            rig.play();
        }
        let heard: Vec<f32> = rig.played[before_stall..].iter().copied().filter(|&s| s != 0.0).collect();
        // Steps of at most 1.02 (easing), plus f32 rounding on a ramp in the hundred thousands.
        assert!(heard.windows(2).all(|w| w[1] > w[0] && w[1] - w[0] < 1.1), "nothing may be cut after a stall");
        let silent = rig.played[before_stall..].iter().filter(|&&s| s == 0.0).count();
        assert!(silent > 0 && silent <= 3 * PACKET, "the stall itself is heard as silence, once: {silent} samples");
        assert!(rig.delay_ms() <= CUSHION_MS + 3, "four seconds later the delay the stall left has eased out (to the cushion plus the 2 ms slack): {} ms, was {settled} ms", rig.delay_ms());
    }
}
