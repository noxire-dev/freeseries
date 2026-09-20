//! Engine control: owns the audio session on its own thread, reopens it when the OS invalidates it
//! (spatial sound toggled, format changed, device replugged), and lets the UI change the route and
//! the EQ while audio runs.

use crate::eq::Preset;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const RETRY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub channels: u16,
    pub sample_rate: u32,
}

/// Capture everything played into `from`, play the processed result on `to`. Both are device ids.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Stopped,
    Running,
    /// A route is set but the session is down; holds the last error. Retried every `RETRY`.
    Reconnecting(String),
}

/// The audio I/O boundary. The real one is `wasapi::Wasapi`; tests use a fake.
pub trait AudioBackend: Send + 'static {
    /// Dropping the session stops audio.
    type Session;
    fn outputs(&self) -> Result<Vec<DeviceInfo>, String>;
    /// `on_lost` must be called when the session dies on its own and needs reopening.
    fn open(&self, route: &Route, dsp: Arc<Dsp>, on_lost: Box<dyn Fn() + Send + Sync>) -> Result<Self::Session, String>;
}

/// EQ settings shared with the audio callback, which never blocks on them.
#[derive(Default)]
pub struct Dsp {
    preset: Mutex<Preset>,
    dirty: AtomicBool,
    bypass: AtomicBool,
}

impl Dsp {
    pub fn set_preset(&self, preset: Preset) {
        *self.preset.lock().unwrap() = preset;
        self.dirty.store(true, Relaxed);
    }

    pub fn preset(&self) -> Preset {
        self.preset.lock().unwrap().clone()
    }

    pub fn set_bypass(&self, bypass: bool) {
        self.bypass.store(bypass, Relaxed);
    }

    pub fn bypassed(&self) -> bool {
        self.bypass.load(Relaxed)
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

enum Cmd {
    Route(Option<Route>),
    Lost(u64),
    Quit,
}

pub struct Engine {
    cmd: mpsc::Sender<Cmd>,
    pub dsp: Arc<Dsp>,
    status: Arc<Mutex<Status>>,
}

impl Engine {
    pub fn start<B: AudioBackend>(backend: B) -> Engine {
        let (tx, rx) = mpsc::channel();
        let dsp = Arc::new(Dsp::default());
        let status = Arc::new(Mutex::new(Status::Stopped));
        let engine = Engine { cmd: tx.clone(), dsp: dsp.clone(), status: status.clone() };

        std::thread::spawn(move || {
            let mut route: Option<Route> = None;
            let mut session: Option<B::Session> = None;
            // Both streams of a session usually die together; the generation lets the second
            // "lost" of an already-replaced session be ignored instead of killing the new one.
            let mut generation = 0u64;
            loop {
                let waiting_to_retry = route.is_some() && session.is_none();
                let cmd = if waiting_to_retry {
                    match rx.recv_timeout(RETRY) {
                        Ok(cmd) => Some(cmd),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                } else {
                    match rx.recv() {
                        Ok(cmd) => Some(cmd),
                        Err(_) => return,
                    }
                };
                match cmd {
                    Some(Cmd::Quit) => return,
                    Some(Cmd::Route(new)) => {
                        session = None;
                        route = new;
                    }
                    Some(Cmd::Lost(g)) if g == generation => session = None,
                    Some(Cmd::Lost(_)) => continue,
                    None => {}
                }
                let Some(r) = &route else {
                    *status.lock().unwrap() = Status::Stopped;
                    continue;
                };
                if session.is_none() {
                    generation += 1;
                    let (tx, g) = (tx.clone(), generation);
                    let on_lost = Box::new(move || drop(tx.send(Cmd::Lost(g))));
                    *status.lock().unwrap() = match backend.open(r, dsp.clone(), on_lost) {
                        Ok(s) => {
                            session = Some(s);
                            Status::Running
                        }
                        Err(e) => Status::Reconnecting(e),
                    };
                }
            }
        });
        engine
    }

    /// `None` stops audio.
    pub fn set_route(&self, route: Option<Route>) {
        let _ = self.cmd.send(Cmd::Route(route));
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
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
    use std::collections::VecDeque;
    use std::time::Instant;

    type Lost = Box<dyn Fn() + Send + Sync>;

    #[derive(Clone, Default)]
    struct Fake {
        results: Arc<Mutex<VecDeque<Result<(), String>>>>,
        opened: Arc<Mutex<Vec<Lost>>>,
    }

    impl AudioBackend for Fake {
        type Session = ();
        fn outputs(&self) -> Result<Vec<DeviceInfo>, String> {
            Ok(vec![])
        }
        fn open(&self, _: &Route, _: Arc<Dsp>, on_lost: Lost) -> Result<(), String> {
            self.results.lock().unwrap().pop_front().unwrap_or(Ok(()))?;
            self.opened.lock().unwrap().push(on_lost);
            Ok(())
        }
    }

    fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let start = Instant::now();
        while !cond() {
            assert!(start.elapsed() < Duration::from_secs(3), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn retries_failed_opens_and_reopens_lost_sessions() {
        let fake = Fake::default();
        fake.results.lock().unwrap().push_back(Err("device busy".into()));
        let engine = Engine::start(fake.clone());
        let opened = || fake.opened.lock().unwrap().len();

        engine.set_route(Some(Route { from: "a".into(), to: "b".into() }));
        wait_for("reconnecting", || engine.status() == Status::Reconnecting("device busy".into()));
        wait_for("running after retry", || engine.status() == Status::Running);
        assert_eq!(opened(), 1);

        (fake.opened.lock().unwrap()[0])(); // the OS kills the session
        wait_for("reopen", || opened() == 2);

        (fake.opened.lock().unwrap()[0])(); // the dead session's second stream reports too
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(opened(), 2, "a stale lost signal must not tear down the new session");

        engine.set_route(None);
        wait_for("stopped", || engine.status() == Status::Stopped);
    }
}
