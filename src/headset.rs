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

/// Something the headset lets us set: one command, one value.
pub struct Setting {
    /// Stable name the app stores the user's choice under.
    pub key: &'static str,
    pub label: &'static str,
    /// The value goes right after these bytes.
    pub command: &'static [u8],
    pub min: u8,
    pub max: u8,
    /// What the values are called, from `min` up. Empty: a plain level.
    pub names: &'static [&'static str],
}

/// A 1-bit display we may draw on.
pub struct Oled {
    pub width: usize,
    pub height: usize,
    /// A frame travels as feature reports of this length, each `draw` + x, y, w, h + the pixels of
    /// a strip at most `strip` columns wide, column by column, least significant bit first.
    pub report_length: usize,
    pub strip: usize,
    pub draw: &'static [u8],
    /// Gives the display back to the device's own UI.
    pub release: &'static [u8],
}

pub struct Model {
    pub name: &'static str,
    pub vendor: u16,
    pub products: &'static [u16],
    /// Part of the Windows device path that picks the HID collection commands go to and answers come from.
    pub interface: &'static str,
    /// Further collections of the same device that only talk: Windows splits a device by report id,
    /// so events can arrive somewhere else than the answers do.
    pub event_interfaces: &'static [&'static str],
    /// Part of the name of the audio output this headset plays on, to tie it to a `DeviceInfo`.
    pub output_name: &'static str,
    pub report_length: usize,
    /// Asked every `POLL`; the answer is one of `reports`.
    pub status_request: &'static [u8],
    pub reports: &'static [Report],
    /// Make the dial report its position to us / give it back to the hardware. Empty: no dial.
    pub dial_on: &'static [&'static [u8]],
    pub dial_off: &'static [&'static [u8]],
    pub settings: &'static [Setting],
    pub oled: Option<Oled>,
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
    /// Counts everything the device said without being asked: a knob turned, a button pressed.
    /// Somebody is at the device, which matters to whoever is drawing on its display.
    pub activity: u32,
}

/// What a model can do, for a UI that builds itself from it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Features {
    pub model: &'static str,
    pub dial: bool,
    pub settings: Vec<SettingInfo>,
    /// Width and height of the display, if there is one.
    pub display: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SettingInfo {
    pub key: &'static str,
    pub label: &'static str,
    pub min: u8,
    pub max: u8,
    pub names: &'static [&'static str],
}

impl Model {
    pub fn features(&self) -> Features {
        let settings = self.settings.iter().map(|s| SettingInfo { key: s.key, label: s.label, min: s.min, max: s.max, names: s.names }).collect();
        Features { model: self.name, dial: !self.dial_on.is_empty(), settings, display: self.oled.as_ref().map(|o| (o.width, o.height)) }
    }
}

/// A full frame (`pixels`: row by row, `width * height`) as the feature reports that draw it.
pub fn frames(oled: &Oled, pixels: &[bool]) -> Vec<Vec<u8>> {
    (0..oled.width.div_ceil(oled.strip))
        .map(|strip| {
            let (x0, w) = (strip * oled.strip, oled.strip.min(oled.width - strip * oled.strip));
            let mut report = vec![0u8; oled.report_length];
            report[..oled.draw.len()].copy_from_slice(oled.draw);
            let at = oled.draw.len();
            report[at..at + 4].copy_from_slice(&[x0 as u8, 0, w as u8, oled.height as u8]);
            for x in 0..w {
                for y in 0..oled.height {
                    let bit = x * oled.height + y;
                    if pixels.get(y * oled.width + x0 + x).copied().unwrap_or(false) {
                        report[at + 4 + bit / 8] |= 1 << (bit % 8);
                    }
                }
            }
            report
        })
        .collect()
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
    /// A feature report: how bulk data such as a display frame travels.
    fn feature(&mut self, report: &[u8]) -> Result<(), String>;
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

/// Work for the watcher's thread, which is the only one that talks to the device.
enum Order {
    Write(Vec<u8>),
    Feature(Vec<u8>),
}

struct Shared {
    state: Mutex<State>,
    orders: Mutex<Vec<Order>>,
    dial_wanted: AtomicBool,
    stop: AtomicBool,
}

/// Watches for a known headset and keeps `state()` current. Cheap to clone; stops with `stop()`.
#[derive(Clone)]
pub struct Headset {
    /// The models this watcher was told to look for.
    models: &'static [&'static Model],
    shared: Arc<Shared>,
    worker: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

impl Headset {
    /// `on_change` runs on the watcher's thread whenever the state changed.
    pub fn start<T: HidTransport>(mut transport: T, models: &'static [&'static Model], on_change: impl Fn(&State) + Send + 'static) -> Headset {
        let shared = Arc::new(Shared { state: Mutex::default(), orders: Mutex::default(), dial_wanted: AtomicBool::new(false), stop: AtomicBool::new(false) });
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
        Headset { models, shared, worker: Arc::new(Mutex::new(worker.ok())) }
    }

    pub fn state(&self) -> State {
        self.shared.state.lock().unwrap().clone()
    }

    fn model(&self) -> Result<&'static Model, String> {
        let name = self.state().model.ok_or("no headset we know is plugged in")?;
        self.models.iter().copied().find(|m| m.name == name).ok_or_else(|| "unknown headset".into())
    }

    /// What the plugged-in headset can do. `None`: there is none.
    pub fn features(&self) -> Option<Features> {
        self.model().ok().map(Model::features)
    }

    /// Changes one of the model's settings. The value is clamped to what the setting allows.
    pub fn set(&self, key: &str, value: u8) -> Result<(), String> {
        let setting = self.model()?.settings.iter().find(|s| s.key == key).ok_or_else(|| format!("this headset has no setting \"{key}\""))?;
        let mut command = setting.command.to_vec();
        command.push(value.clamp(setting.min, setting.max));
        self.shared.orders.lock().unwrap().push(Order::Write(command));
        Ok(())
    }

    /// Shows a frame on the headset's display: `pixels` row by row, width times height.
    pub fn draw(&self, pixels: &[bool]) -> Result<(), String> {
        let oled = self.model()?.oled.as_ref().ok_or("this headset has no display")?;
        let mut orders = self.shared.orders.lock().unwrap();
        orders.retain(|o| !matches!(o, Order::Feature(_))); // a frame nobody has seen yet is replaced, not queued behind
        orders.extend(frames(oled, pixels).into_iter().map(Order::Feature));
        Ok(())
    }

    /// Gives the display back to the headset's own UI.
    pub fn release_display(&self) -> Result<(), String> {
        let oled = self.model()?.oled.as_ref().ok_or("this headset has no display")?;
        self.shared.orders.lock().unwrap().push(Order::Write(oled.release.to_vec()));
        Ok(())
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
        for order in std::mem::take(&mut *shared.orders.lock().unwrap()) {
            match order {
                Order::Write(command) => send(model, link, &[&command])?,
                Order::Feature(report) => link.feature(&report)?,
            }
        }
        if stopping {
            // Whatever we drew must not stay on an OLED for good.
            if let Some(oled) = &model.oled {
                send(model, link, &[oled.release])?;
            }
            return Ok(());
        }
        if asked.is_none_or(|at| at.elapsed() >= POLL) {
            send(model, link, &[model.status_request])?;
            asked = Some(Instant::now());
        }
        let n = link.read(&mut message, LISTEN)?;
        if n > 0 {
            let unasked = !message[..n].starts_with(model.status_request); // an answer echoes its request
            if unasked {
                state.activity = state.activity.wrapping_add(1);
            }
            if decode(model, &message[..n], &mut state) || unasked {
                publish(state.clone());
            }
        }
    }
}

impl Presence for Headset {
    fn is_on(&self, output: &DeviceInfo) -> bool {
        // Only the output our headset plays on, and only when it said so itself: anything unknown counts as on.
        let ours = self.model().is_ok_and(|m| output.name.contains(m.output_name));
        !ours || self.state().online.unwrap_or(true)
    }
}

/// USB HID through hidapi.
#[derive(Default)]
pub struct Usb;

/// The command collection first, then the ones that only talk.
pub struct UsbLink(Vec<hidapi::HidDevice>);

impl HidLink for UsbLink {
    fn write(&mut self, message: &[u8]) -> Result<(), String> {
        self.0[0].write(message).map(|_| ()).map_err(|e| e.to_string())
    }
    fn feature(&mut self, report: &[u8]) -> Result<(), String> {
        self.0[0].send_feature_report(report).map_err(|e| e.to_string())
    }
    fn read(&mut self, into: &mut [u8], timeout: Duration) -> Result<usize, String> {
        let each = (timeout.as_millis() as i32 / self.0.len() as i32).max(1);
        for device in &self.0 {
            let n = device.read_timeout(into, each).map_err(|e| e.to_string())?;
            if n > 0 {
                return Ok(n);
            }
        }
        Ok(0)
    }
}

impl HidTransport for Usb {
    type Link = UsbLink;
    fn open(&mut self, models: &'static [&'static Model]) -> Option<(&'static Model, UsbLink)> {
        let api = hidapi::HidApi::new().ok()?;
        let path_of = |info: &hidapi::DeviceInfo| info.path().to_string_lossy().to_ascii_lowercase();
        for model in models {
            let ours = |info: &&hidapi::DeviceInfo| model.vendor == info.vendor_id() && model.products.contains(&info.product_id());
            let Some(command) = api.device_list().filter(ours).find(|info| path_of(info).contains(model.interface)) else { continue };
            let Ok(command) = command.open_device(&api) else { continue };
            // A collection that won't open costs its events, not the headset.
            let talkers = api.device_list().filter(ours).filter(|info| model.event_interfaces.iter().any(|i| path_of(info).contains(i))).filter_map(|info| info.open_device(&api).ok());
            return Some((*model, UsbLink(std::iter::once(command).chain(talkers).collect())));
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
        event_interfaces: &[],
        output_name: "Toy Headset",
        report_length: 8,
        status_request: &[0x01],
        reports: &[Report { prefix: &[0x01], fields: &[(Field::Online, 1, Decode::Is(1))] }, Report { prefix: &[0x02], fields: &[(Field::DialGame, 1, Decode::Percent), (Field::DialChat, 2, Decode::Percent)] }],
        dial_on: &DIAL_ON,
        dial_off: &DIAL_OFF,
        settings: &[Setting { key: "sidetone", label: "Sidetone", command: &[0x39], min: 0, max: 3, names: &[] }],
        oled: Some(Oled { width: 10, height: 8, report_length: 16, strip: 8, draw: &[0x93], release: &[0x95] }),
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
        fn feature(&mut self, report: &[u8]) -> Result<(), String> {
            self.sent.send(report.to_vec()).map_err(|e| e.to_string())
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

        headset.set("sidetone", 9).unwrap();
        assert!(headset.set("rgb", 1).is_err());
        let mut pixels = vec![false; 10 * 8];
        (pixels[0], pixels[9], pixels[8 * 10 - 1]) = (true, true, true); // top left, top right, bottom right
        headset.draw(&pixels).unwrap();
        headset.release_display().unwrap();
        std::thread::sleep(Duration::from_millis(300));

        headset.stop();
        let sent: Vec<Vec<u8>> = log.try_iter().collect();
        assert!(sent.iter().any(|m| m[..2] == [0x39, 3]), "a value past the maximum is clamped, not refused");
        // Two strips (8 + 2 columns), pixels column by column, least significant bit first.
        let strips: Vec<&Vec<u8>> = sent.iter().filter(|m| m[0] == 0x93).collect();
        assert_eq!(strips[0][..6], [0x93, 0, 0, 8, 8, 0b0000_0001]);
        assert_eq!(strips[1][..7], [0x93, 8, 0, 2, 8, 0, 0b1000_0001]);
        let log = sent.into_iter();
        let commands: Vec<Vec<u8>> = log.map(|m| m[..2].to_vec()).collect();
        assert_eq!(commands.first(), Some(&vec![0x01, 0x00]), "asks for the status first");
        assert_eq!(commands[commands.len() - 2..], [vec![0xAA, 0x00], vec![0x95, 0x00]], "the dial and the display go back to the hardware before we leave");
        assert_eq!(commands.iter().filter(|c| **c == [0xAA, 0x01]).count(), 1);
    }
}
