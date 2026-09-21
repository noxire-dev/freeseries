//! What a headset can tell us beyond audio: is it switched on, how full is its battery, is the mic
//! muted, where does its ChatMix dial stand.
//!
//! A headset is a table (`Model`, one file per headset in `headsets/`), not code: which USB device
//! it is, what to send, and which byte of which reply means what. Adding a headset means adding a
//! table and one line in `headsets::ALL`; nobody needs the hardware to review it, because tables
//! are tested against canned replies. Everything that touches USB goes through `HidTransport`.

use crate::engine::{DeviceInfo, Presence};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Field {
    /// The headset itself is switched on and connected (the USB device can be a base station that always is).
    Online,
    Battery,
    MicMuted,
    /// The two sides of a ChatMix dial. Both full in the middle; turning it takes one side down.
    DialGame,
    DialChat,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decode {
    /// True when the byte is this value.
    Is(u8),
    /// True unless the byte is this value.
    IsNot(u8),
    /// A level in steps from 0 to this value.
    Steps(u8),
    /// A level from 0 to 100.
    Percent,
}

/// One kind of message from the device, recognised by how it starts, and what its bytes mean.
pub struct Report {
    pub prefix: &'static [u8],
    pub fields: &'static [(Field, usize, Decode)],
}

pub struct Model {
    pub name: &'static str,
    pub vendor: u16,
    pub products: &'static [u16],
    /// Part of the Windows device path that picks the right HID interface of the device.
    pub interface: &'static str,
    /// Part of the name of the audio output this headset plays on, to tie it to a `DeviceInfo`.
    pub output_name: &'static str,
    pub report_length: usize,
    /// Asked every `POLL`; the answer is one of `reports`.
    pub status_request: &'static [u8],
    pub reports: &'static [Report],
    /// Make the dial report its position to us / give it back to the hardware. Empty: no dial.
    pub dial_on: &'static [&'static [u8]],
    pub dial_off: &'static [&'static [u8]],
    /// Firmware drops commands that arrive faster than this.
    pub pace: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// Which headset this is. `None`: none we know is plugged in.
    pub model: Option<&'static str>,
    pub online: Option<bool>,
    /// 0 to 1.
    pub battery: Option<f32>,
    pub mic_muted: Option<bool>,
    /// Game and chat side of the dial, each 0 to 1.
    pub dial: Option<(f32, f32)>,
}

/// Reads one message into `state`. False: not a message this model describes.
pub fn decode(model: &Model, message: &[u8], state: &mut State) -> bool {
    let Some(report) = model.reports.iter().find(|r| message.starts_with(r.prefix)) else { return false };
    let mut dial = state.dial.unwrap_or((1.0, 1.0));
    for &(field, offset, how) in report.fields {
        let Some(&byte) = message.get(offset) else { continue };
        let (flag, level) = match how {
            Decode::Is(v) => (byte == v, 0.0),
            Decode::IsNot(v) => (byte != v, 0.0),
            Decode::Steps(max) => (false, (byte.min(max) as f32) / max as f32),
            Decode::Percent => (false, (byte.min(100) as f32) / 100.0),
        };
        match field {
            Field::Online => state.online = Some(flag),
            Field::MicMuted => state.mic_muted = Some(flag),
            Field::Battery => state.battery = Some(level),
            Field::DialGame => dial.0 = level,
            Field::DialChat => dial.1 = level,
        }
    }
    if report.fields.iter().any(|(f, ..)| matches!(f, Field::DialGame | Field::DialChat)) {
        state.dial = Some(dial);
    }
    true
}

/// An open connection to one headset.
pub trait HidLink: Send {
    fn write(&mut self, message: &[u8]) -> Result<(), String>;
    /// Waits up to `timeout`. `Ok(0)`: nothing came.
    fn read(&mut self, into: &mut [u8], timeout: Duration) -> Result<usize, String>;
}

pub trait HidTransport: Send + 'static {
    type Link: HidLink;
    /// The first of `models` that is plugged in.
    fn open(&mut self, models: &'static [&'static Model]) -> Option<(&'static Model, Self::Link)>;
}

const POLL: Duration = Duration::from_secs(2);
const LISTEN: Duration = Duration::from_millis(100);

struct Shared {
    state: Mutex<State>,
    dial_wanted: AtomicBool,
    stop: AtomicBool,
}

/// Watches for a known headset and keeps `state()` current. Cheap to clone; stops with `stop()`.
#[derive(Clone)]
pub struct Headset {
    shared: Arc<Shared>,
    worker: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl Headset {
    /// `on_change` runs on the watcher's thread whenever the state changed.
    pub fn start<T: HidTransport>(mut transport: T, models: &'static [&'static Model], on_change: impl Fn(&State) + Send + 'static) -> Headset {
        let shared = Arc::new(Shared { state: Mutex::default(), dial_wanted: AtomicBool::new(false), stop: AtomicBool::new(false) });
        let theirs = shared.clone();
        let worker = std::thread::Builder::new().name("headset".into()).spawn(move || {
            let publish = |next: State| {
                let mut state = theirs.state.lock().unwrap();
                if *state != next {
                    *state = next.clone();
                    drop(state);
                    on_change(&next);
                }
            };
            while !theirs.stop.load(Ordering::Relaxed) {
                match transport.open(models) {
                    Some((model, mut link)) => {
                        log::info!("headset: {}", model.name);
                        if let Err(e) = converse(model, &mut link, &theirs, &publish) {
                            log::info!("headset: {} went away ({e})", model.name);
                        }
                        publish(State::default());
                    }
                    None => sleep_unless_stopped(&theirs, POLL),
                }
            }
        });
        Headset { shared, worker: Arc::new(Mutex::new(worker.ok())) }
    }

    pub fn state(&self) -> State {
        self.shared.state.lock().unwrap().clone()
    }

    /// Have the dial report to us instead of doing whatever the hardware does with it by itself.
    pub fn set_dial(&self, wanted: bool) {
        self.shared.dial_wanted.store(wanted, Ordering::Relaxed);
    }

    /// Gives the dial back to the hardware and ends the watcher. Returns when that is done.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
}

fn sleep_unless_stopped(shared: &Shared, how_long: Duration) {
    let until = Instant::now() + how_long;
    while Instant::now() < until && !shared.stop.load(Ordering::Relaxed) {
        std::thread::sleep(LISTEN);
    }
}

fn send(model: &Model, link: &mut impl HidLink, commands: &[&[u8]]) -> Result<(), String> {
    for command in commands {
        let mut message = vec![0u8; model.report_length];
        message[..command.len()].copy_from_slice(command);
        link.write(&message)?;
        std::thread::sleep(model.pace);
    }
    Ok(())
}

/// Talks to one headset until it goes away or we are told to stop.
fn converse(model: &'static Model, link: &mut impl HidLink, shared: &Shared, publish: &impl Fn(State)) -> Result<(), String> {
    let mut state = State { model: Some(model.name), ..State::default() };
    let mut dial_is_ours = false;
    let mut asked: Option<Instant> = None;
    let mut message = vec![0u8; model.report_length];
    loop {
        let stopping = shared.stop.load(Ordering::Relaxed);
        let dial_wanted = !stopping && !model.dial_on.is_empty() && shared.dial_wanted.load(Ordering::Relaxed);
        if dial_wanted != dial_is_ours {
            send(model, link, if dial_wanted { model.dial_on } else { model.dial_off })?;
            dial_is_ours = dial_wanted;
            if !dial_wanted {
                state.dial = None;
            }
        }
        if stopping {
            return Ok(());
        }
        if asked.is_none_or(|at| at.elapsed() >= POLL) {
            send(model, link, &[model.status_request])?;
            asked = Some(Instant::now());
        }
        let n = link.read(&mut message, LISTEN)?;
        if n > 0 && decode(model, &message[..n], &mut state) {
            publish(state.clone());
        }
    }
}

impl Presence for Headset {
    fn is_on(&self, output: &DeviceInfo) -> bool {
        // Only the output our headset plays on, and only when it said so itself: anything unknown counts as on.
        let state = self.state();
        let ours = state.model.and_then(|name| crate::headsets::ALL.iter().find(|m| m.name == name)).is_some_and(|m| output.name.contains(m.output_name));
        !ours || state.online.unwrap_or(true)
    }
}

/// USB HID through hidapi.
#[derive(Default)]
pub struct Usb;

pub struct UsbLink(hidapi::HidDevice);

impl HidLink for UsbLink {
    fn write(&mut self, message: &[u8]) -> Result<(), String> {
        self.0.write(message).map(|_| ()).map_err(|e| e.to_string())
    }
    fn read(&mut self, into: &mut [u8], timeout: Duration) -> Result<usize, String> {
        self.0.read_timeout(into, timeout.as_millis() as i32).map_err(|e| e.to_string())
    }
}

impl HidTransport for Usb {
    type Link = UsbLink;
    fn open(&mut self, models: &'static [&'static Model]) -> Option<(&'static Model, UsbLink)> {
        let api = hidapi::HidApi::new().ok()?;
        for info in api.device_list() {
            let path = info.path().to_string_lossy().to_ascii_lowercase();
            let model = models.iter().find(|m| m.vendor == info.vendor_id() && m.products.contains(&info.product_id()) && path.contains(m.interface));
            if let Some(model) = model {
                if let Ok(device) = info.open_device(&api) {
                    return Some((model, UsbLink(device)));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    static DIAL_ON: [&[u8]; 1] = [&[0xAA, 0x01]];
    static DIAL_OFF: [&[u8]; 1] = [&[0xAA, 0x00]];
    static TOY: Model = Model {
        name: "Toy",
        vendor: 1,
        products: &[2],
        interface: "",
        output_name: "Toy Headset",
        report_length: 8,
        status_request: &[0x01],
        reports: &[Report { prefix: &[0x01], fields: &[(Field::Online, 1, Decode::Is(1))] }, Report { prefix: &[0x02], fields: &[(Field::DialGame, 1, Decode::Percent), (Field::DialChat, 2, Decode::Percent)] }],
        dial_on: &DIAL_ON,
        dial_off: &DIAL_OFF,
        pace: Duration::ZERO,
    };
    static TOYS: [&Model; 1] = [&TOY];

    /// Answers a status request with "online", and sends one dial event once the dial was switched on.
    struct ToyLink {
        sent: mpsc::Sender<Vec<u8>>,
        pending: Vec<Vec<u8>>,
    }
    impl HidLink for ToyLink {
        fn write(&mut self, message: &[u8]) -> Result<(), String> {
            match message[..2] {
                [0x01, _] => self.pending.push(vec![0x01, 0x01]),
                [0xAA, 0x01] => self.pending.push(vec![0x02, 100, 40]),
                _ => {}
            }
            self.sent.send(message.to_vec()).map_err(|e| e.to_string())
        }
        fn read(&mut self, into: &mut [u8], timeout: Duration) -> Result<usize, String> {
            let Some(next) = self.pending.pop() else {
                std::thread::sleep(timeout.min(Duration::from_millis(5)));
                return Ok(0);
            };
            into[..next.len()].copy_from_slice(&next);
            Ok(next.len())
        }
    }
    struct ToyUsb(Option<mpsc::Sender<Vec<u8>>>);
    impl HidTransport for ToyUsb {
        type Link = ToyLink;
        fn open(&mut self, models: &'static [&'static Model]) -> Option<(&'static Model, ToyLink)> {
            self.0.take().map(|sent| (models[0], ToyLink { sent, pending: Vec::new() })) // plugged in once
        }
    }

    #[test]
    fn follows_a_headset_and_hands_the_dial_back() {
        let (sent, log) = mpsc::channel();
        let (changed, changes) = mpsc::channel();
        let headset = Headset::start(ToyUsb(Some(sent)), &TOYS, move |s| changed.send(s.clone()).unwrap());
        let wait_for = |what: &str, ok: &dyn Fn(&State) -> bool| loop {
            let state = changes.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| panic!("never saw: {what}"));
            if ok(&state) {
                break state;
            }
        };
        wait_for("online", &|s| s.online == Some(true));
        let toy = DeviceInfo { id: "x".into(), name: "Speakers (Toy Headset)".into(), channels: 2, sample_rate: 48000 };
        assert!(headset.is_on(&toy));

        headset.set_dial(true);
        let turned = wait_for("the dial", &|s| s.dial.is_some());
        assert_eq!(turned.dial, Some((1.0, 0.4)));

        headset.stop();
        let commands: Vec<Vec<u8>> = log.try_iter().map(|m| m[..2].to_vec()).collect();
        assert_eq!(commands.first(), Some(&vec![0x01, 0x00]), "asks for the status first");
        assert_eq!(commands.last(), Some(&vec![0xAA, 0x00]), "the dial goes back to the hardware before we leave");
        assert_eq!(commands.iter().filter(|c| **c == [0xAA, 0x01]).count(), 1);
    }
}
