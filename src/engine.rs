//! Engine control: owns the audio session on its own thread and keeps audio flowing.
//!
//! The user hands it a plan: the channels (one virtual device each: Game, Chat, ...) with their
//! own volume and EQ, and the real outputs in order of preference, each with its own EQ. The
//! engine mixes the channels and plays them on the best output that is usable right now, moving
//! when that changes (headset switched off, device unplugged, headset back on). It also reopens
//! the session when the OS invalidates it (spatial sound toggled, format changed).

use crate::eq::Preset;
use crate::spectrum::Tap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
}

/// What one audio session does: capture everything played into each source, mix, play on `to`.
#[derive(Debug, Clone, PartialEq)]
pub struct Mix {
    pub sources: Vec<Source>,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    /// Which channel's volume and EQ apply (`Dsp::channel`).
    pub channel: String,
    /// The virtual device to capture, by device id.
    pub from: String,
}

/// One virtual device apps play into, with the volume and EQ that belong to it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    /// Stable key: "main", "chat", ...
    pub id: String,
    pub from: String,
    /// Linear gain, 0 (muted) to 1.
    pub volume: f32,
    pub preset: Preset,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Output {
    pub id: String,
    pub preset: Preset,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// The virtual devices apps play into. Mixed together.
    pub channels: Vec<Channel>,
    /// Real outputs, most preferred first.
    pub outputs: Vec<Output>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    Stopped,
    Running { output: String },
    /// A plan is set but nothing is playing; holds the last error. Retried every `Timing::retry`.
    Reconnecting { error: String },
}

/// The audio I/O boundary. The real one is `wasapi::Wasapi`; tests use a fake.
pub trait AudioBackend: Send + 'static {
    /// Dropping the session stops audio.
    type Session;
    fn outputs(&self) -> Result<Vec<DeviceInfo>, String>;
    /// `on_lost` must be called when the session dies on its own and needs reopening.
    fn open(&self, mix: &Mix, dsp: Arc<Dsp>, on_lost: Box<dyn Fn() + Send + Sync>) -> Result<Self::Session, String>;
}

/// Whether an output that Windows still lists can actually be heard. A wireless headset with a
/// base station keeps its audio endpoint while the headset itself is switched off.
pub trait Presence: Send + 'static {
    /// `false` only when the device is known to be off. Anything unknown counts as on.
    fn is_on(&self, output: &DeviceInfo) -> bool;
}

/// No extra knowledge: every listed output counts as on.
pub struct AlwaysOn;
impl Presence for AlwaysOn {
    fn is_on(&self, _: &DeviceInfo) -> bool {
        true
    }
}

#[derive(Clone, Copy)]
pub struct Timing {
    /// Between attempts while nothing is playing.
    pub retry: Duration,
    /// Between checks for a better or vanished output while playing.
    pub watch: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { retry: Duration::from_millis(500), watch: Duration::from_secs(2) }
    }
}

/// A preset the UI sets and an audio callback picks up, without the callback ever blocking.
#[derive(Default)]
pub struct PresetCell {
    preset: Mutex<Preset>,
    dirty: AtomicBool,
}

impl PresetCell {
    pub fn set(&self, preset: Preset) {
        *self.preset.lock().unwrap() = preset;
        self.dirty.store(true, Relaxed);
    }

    pub fn get(&self) -> Preset {
        self.preset.lock().unwrap().clone()
    }

    /// For the audio callback: the preset, if it changed since the last call.
    pub fn take_changed(&self) -> Option<Preset> {
        if !self.dirty.swap(false, Relaxed) {
            return None;
        }
        match self.preset.try_lock() {
            Ok(p) => Some(p.clone()),
            Err(_) => {
                self.dirty.store(true, Relaxed); // UI holds the lock right now, pick it up next callback
                None
            }
        }
    }
}

/// One channel's settings, shared with its capture callback.
pub struct ChannelDsp {
    pub eq: PresetCell,
    volume: AtomicU32,
}

impl Default for ChannelDsp {
    fn default() -> Self {
        ChannelDsp { eq: PresetCell::default(), volume: AtomicU32::new(1f32.to_bits()) }
    }
}

impl ChannelDsp {
    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume.clamp(0.0, 1.0).to_bits(), Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Relaxed))
    }
}

/// Everything the audio callbacks share with the rest of the app. `eq` is the output's EQ, applied
/// to the mix; each channel has its own on top of that.
#[derive(Default)]
pub struct Dsp {
    pub eq: PresetCell,
    bypass: AtomicBool,
    analysing: AtomicBool,
    tap: Mutex<Tap>,
    channels: Mutex<HashMap<String, Arc<ChannelDsp>>>,
}

impl Dsp {
    /// The settings of one channel, created on first use.
    pub fn channel(&self, id: &str) -> Arc<ChannelDsp> {
        self.channels.lock().unwrap().entry(id.to_string()).or_default().clone()
    }

    pub fn set_bypass(&self, bypass: bool) {
        self.bypass.store(bypass, Relaxed);
    }

    pub fn bypassed(&self) -> bool {
        self.bypass.load(Relaxed)
    }

    /// The analyser only costs anything while someone is looking at it.
    pub fn set_analysing(&self, on: bool) {
        self.analysing.store(on, Relaxed);
    }

    /// For the audio callback: hands the processed audio to the analyser. Never blocks; a block
    /// that arrives while the UI is reading is simply skipped.
    pub fn tap(&self, interleaved: &[f32], channels: usize, sample_rate: f32) {
        if self.analysing.load(Relaxed) {
            if let Ok(mut tap) = self.tap.try_lock() {
                tap.push(interleaved, channels, sample_rate);
            }
        }
    }

    /// dBFS per log-spaced band of what is playing, or `None` while the analyser is off.
    pub fn spectrum(&self, bands: usize) -> Option<Vec<f32>> {
        self.analysing.load(Relaxed).then(|| self.tap.lock().unwrap().spectrum(bands))
    }
}

enum Cmd {
    Plan(Option<Plan>),
    Preset(Preset),
    Lost(u64),
    Quit,
}

pub struct Engine {
    cmd: mpsc::Sender<Cmd>,
    pub dsp: Arc<Dsp>,
    status: Arc<Mutex<Status>>,
}

impl Engine {
    pub fn start<B: AudioBackend, P: Presence>(backend: B, presence: P) -> Engine {
        Self::start_with(backend, presence, Timing::default())
    }

    pub fn start_with<B: AudioBackend, P: Presence>(backend: B, presence: P, timing: Timing) -> Engine {
        let (tx, rx) = mpsc::channel();
        let dsp = Arc::new(Dsp::default());
        let status = Arc::new(Mutex::new(Status::Stopped));
        let engine = Engine { cmd: tx.clone(), dsp: dsp.clone(), status: status.clone() };

        std::thread::spawn(move || {
            let mut plan: Option<Plan> = None;
            // The live session and the id of the output it plays on.
            let mut session: Option<(B::Session, String)> = None;
            // Both streams of a session usually die together; the generation lets the second
            // "lost" of an already-replaced session be ignored instead of killing the new one.
            let mut generation = 0u64;
            loop {
                let wait = match (&plan, &session) {
                    (None, _) => None,
                    (Some(_), None) => Some(timing.retry),
                    (Some(_), Some(_)) => Some(timing.watch),
                };
                let cmd = match wait {
                    Some(wait) => match rx.recv_timeout(wait) {
                        Ok(cmd) => Some(cmd),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    },
                    None => match rx.recv() {
                        Ok(cmd) => Some(cmd),
                        Err(_) => return,
                    },
                };
                match cmd {
                    Some(Cmd::Quit) => return,
                    Some(Cmd::Plan(new)) => {
                        if new.as_ref().map(sources) != plan.as_ref().map(sources) {
                            session = None; // other virtual devices: nothing of the old session survives
                        }
                        plan = new;
                        if let Some(p) = &plan {
                            // Volumes and channel EQs apply live, with or without a reopen.
                            for c in &p.channels {
                                let live = dsp.channel(&c.id);
                                live.set_volume(c.volume);
                                if c.preset != live.eq.get() {
                                    live.eq.set(c.preset.clone());
                                }
                            }
                            // The playing output may have a new preset in this plan.
                            if let Some(o) = session.as_ref().and_then(|(_, active)| p.outputs.iter().find(|o| &o.id == active)) {
                                if o.preset != dsp.eq.get() {
                                    dsp.eq.set(o.preset.clone());
                                }
                            }
                        }
                    }
                    // A live edit belongs to the output that is playing, so it is still there when
                    // audio moves away and comes back.
                    Some(Cmd::Preset(preset)) => {
                        if let (Some(p), Some((_, active))) = (&mut plan, &session) {
                            if let Some(o) = p.outputs.iter_mut().find(|o| &o.id == active) {
                                o.preset = preset.clone();
                            }
                        }
                        dsp.eq.set(preset);
                        continue;
                    }
                    Some(Cmd::Lost(g)) if g == generation => session = None,
                    Some(Cmd::Lost(_)) => continue,
                    None => {}
                }
                let Some(p) = &plan else {
                    session = None;
                    set_status(&status, Status::Stopped);
                    continue;
                };

                let present = backend.outputs().unwrap_or_default();
                let usable: Vec<&Output> =
                    p.outputs.iter().filter(|o| present.iter().any(|d| d.id == o.id && presence.is_on(d))).collect();
                if session.as_ref().is_some_and(|(_, active)| !usable.iter().any(|o| &o.id == active)) {
                    session = None; // the playing output went away or was switched off
                }
                let mut error = "none of your outputs is available".to_string();
                for output in usable {
                    if session.as_ref().is_some_and(|(_, active)| active == &output.id) {
                        break; // already on the best output that opens
                    }
                    // Open the better output before letting go of the current one, so a failed
                    // attempt costs nothing.
                    generation += 1;
                    let (tx, g) = (tx.clone(), generation);
                    let on_lost = Box::new(move || drop(tx.send(Cmd::Lost(g))));
                    match backend.open(&Mix { sources: sources(p), to: output.id.clone() }, dsp.clone(), on_lost) {
                        Ok(s) => {
                            // Only now: a failed attempt must not touch the EQ of what is playing.
                            dsp.eq.set(output.preset.clone());
                            session = Some((s, output.id.clone()));
                            break;
                        }
                        Err(e) => error = e,
                    }
                }
                set_status(
                    &status,
                    match &session {
                        Some((_, active)) => Status::Running { output: active.clone() },
                        None => Status::Reconnecting { error },
                    },
                );
            }
        });
        engine
    }

    /// `None` stops audio.
    pub fn set_plan(&self, plan: Option<Plan>) {
        let _ = self.cmd.send(Cmd::Plan(plan));
    }

    /// Changes the EQ of the output that is playing, and remembers it for that output.
    pub fn set_preset(&self, preset: Preset) {
        let _ = self.cmd.send(Cmd::Preset(preset));
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }
}

fn sources(plan: &Plan) -> Vec<Source> {
    plan.channels.iter().map(|c| Source { channel: c.id.clone(), from: c.from.clone() }).collect()
}

/// Logs transitions only, so a device that stays gone doesn't write a line every retry.
fn set_status(status: &Mutex<Status>, next: Status) {
    let mut current = status.lock().unwrap();
    if *current != next {
        match &next {
            Status::Reconnecting { error } => log::warn!("audio is down, retrying: {error}"),
            other => log::info!("engine {other:?}"),
        }
        *current = next;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eq::{Band, Kind};
    use std::collections::{HashSet, VecDeque};
    use std::time::Instant;

    type Lost = Box<dyn Fn() + Send + Sync>;

    #[derive(Clone, Default)]
    struct Fake {
        results: Arc<Mutex<VecDeque<Result<(), String>>>>,
        opened: Arc<Mutex<Vec<(String, Lost)>>>,
        mixes: Arc<Mutex<Vec<Mix>>>,
        off: Arc<Mutex<HashSet<String>>>,
    }

    impl AudioBackend for Fake {
        type Session = ();
        fn outputs(&self) -> Result<Vec<DeviceInfo>, String> {
            Ok(["headset", "monitor"].map(|id| DeviceInfo { id: id.into(), name: id.into(), channels: 2, sample_rate: 48000 }).into())
        }
        fn open(&self, mix: &Mix, _: Arc<Dsp>, on_lost: Lost) -> Result<(), String> {
            self.results.lock().unwrap().pop_front().unwrap_or(Ok(()))?;
            self.mixes.lock().unwrap().push(mix.clone());
            self.opened.lock().unwrap().push((mix.to.clone(), on_lost));
            Ok(())
        }
    }

    impl Presence for Fake {
        fn is_on(&self, output: &DeviceInfo) -> bool {
            !self.off.lock().unwrap().contains(&output.id)
        }
    }

    const FAST: Timing = Timing { retry: Duration::from_millis(10), watch: Duration::from_millis(10) };

    fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let start = Instant::now();
        while !cond() {
            assert!(start.elapsed() < Duration::from_secs(3), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn preset(gain_db: f64) -> Preset {
        Preset { preamp_db: 0.0, bands: vec![Band { kind: Kind::Peak, fc: 1000.0, gain_db, q: 1.0 }] }
    }

    fn plan() -> Plan {
        let channel = |id: &str, from: &str| Channel { id: id.into(), from: from.into(), volume: 1.0, preset: Preset::default() };
        Plan { channels: vec![channel("main", "virtual"), channel("chat", "virtual-chat")], outputs: vec![Output { id: "headset".into(), preset: preset(1.0) }, Output { id: "monitor".into(), preset: preset(2.0) }] }
    }

    fn running(id: &str) -> Status {
        Status::Running { output: id.into() }
    }

    #[test]
    fn retries_failed_opens_and_reopens_lost_sessions() {
        let fake = Fake::default();
        fake.off.lock().unwrap().insert("monitor".into()); // only the headset is usable
        fake.results.lock().unwrap().push_back(Err("device busy".into()));
        let engine = Engine::start_with(fake.clone(), fake.clone(), FAST);
        let opened = || fake.opened.lock().unwrap().len();

        engine.set_plan(Some(plan()));
        wait_for("reconnecting", || engine.status() == Status::Reconnecting { error: "device busy".into() });
        wait_for("running after retry", || engine.status() == running("headset"));
        assert_eq!(opened(), 1);

        (fake.opened.lock().unwrap()[0].1)(); // the OS kills the session
        wait_for("reopen", || opened() == 2);

        (fake.opened.lock().unwrap()[0].1)(); // the dead session's second stream reports too
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(opened(), 2, "a stale lost signal must not tear down the new session");

        engine.set_plan(Some(plan())); // same plan again
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(opened(), 2, "re-sending the active plan must not reopen the session");

        engine.set_plan(None);
        wait_for("stopped", || engine.status() == Status::Stopped);
    }

    #[test]
    fn channels_are_mixed_and_their_settings_apply_without_a_reopen() {
        let fake = Fake::default();
        let engine = Engine::start_with(fake.clone(), fake.clone(), FAST);
        engine.set_plan(Some(plan()));
        wait_for("running", || engine.status() == running("headset"));
        let sources: Vec<String> = fake.mixes.lock().unwrap()[0].sources.iter().map(|s| format!("{}<-{}", s.channel, s.from)).collect();
        assert_eq!(sources, ["main<-virtual", "chat<-virtual-chat"], "one session captures every channel");

        // The Game/Chat balance is dragged: chat goes quiet and gets its own EQ.
        let mut quieter = plan();
        quieter.channels[1].volume = 0.25;
        quieter.channels[1].preset = preset(3.0);
        engine.set_plan(Some(quieter));
        wait_for("chat volume", || engine.dsp.channel("chat").volume() == 0.25);
        assert_eq!(engine.dsp.channel("chat").eq.get(), preset(3.0));
        assert_eq!(engine.dsp.channel("main").volume(), 1.0);
        assert_eq!(fake.opened.lock().unwrap().len(), 1, "a volume or EQ change must not interrupt the audio");

        // The chat device goes away: that is a different capture, so the session is rebuilt.
        let mut solo = plan();
        solo.channels.truncate(1);
        engine.set_plan(Some(solo));
        wait_for("reopen with one channel", || fake.mixes.lock().unwrap().len() == 2);
        assert_eq!(fake.mixes.lock().unwrap()[1].sources.len(), 1);
    }

    #[test]
    fn follows_the_best_usable_output_and_keeps_each_eq() {
        let fake = Fake::default();
        let engine = Engine::start_with(fake.clone(), fake.clone(), FAST);

        engine.set_plan(Some(plan()));
        wait_for("headset first", || engine.status() == running("headset"));
        assert_eq!(engine.dsp.eq.get(), preset(1.0));

        engine.set_preset(preset(1.5)); // the user tunes the headset
        wait_for("live edit", || engine.dsp.eq.get() == preset(1.5));

        fake.off.lock().unwrap().insert("headset".into()); // headset switched off
        wait_for("falls to the monitor", || engine.status() == running("monitor"));
        assert_eq!(engine.dsp.eq.get(), preset(2.0), "the monitor plays with its own EQ");

        fake.off.lock().unwrap().clear(); // headset back on
        wait_for("back to the headset", || engine.status() == running("headset"));
        assert_eq!(engine.dsp.eq.get(), preset(1.5), "the headset gets its tuned EQ back, not the plan's original");

        // A better output that refuses to open must not cost the one that works.
        fake.off.lock().unwrap().insert("headset".into());
        wait_for("monitor again", || engine.status() == running("monitor"));
        for _ in 0..50 {
            fake.results.lock().unwrap().push_back(Err("busy".into()));
        }
        fake.off.lock().unwrap().clear();
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(engine.status(), running("monitor"), "stays on the monitor while the headset won't open");
    }
}
