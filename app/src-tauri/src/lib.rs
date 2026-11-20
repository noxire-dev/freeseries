mod agent;
mod autostart;
mod handback;
mod setup;

use autostart::{Autostart, TaskScheduler};
use setup::{ScriptSetup, Setup};
use freeseries::headset::{Headset, Usb};
use freeseries::engine::{AudioBackend, DeviceInfo, Engine, Plan, Status};
use freeseries::eq::Preset;
use freeseries::routing::{AppRouting, AudioApp, Icon, WindowsRouting};
use freeseries::native::Native;
use freeseries::wasapi::Wasapi;
use std::os::windows::process::CommandExt;
use tauri::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State, WindowEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};
use tauri_plugin_opener::OpenerExt;

struct App {
    engine: Engine,
    autostart: Box<dyn Autostart>,
    /// The MCP endpoint, while the user has agent access switched on.
    agent: std::sync::Mutex<Option<agent::Server>>,
}

/// A development build runs next to the installed app, so it listens next door.
const AGENT_PORT: u16 = if cfg!(debug_assertions) { 47631 } else { 47630 };
/// Long enough for a tool that listens to the audio for a while.
const AGENT_PATIENCE: std::time::Duration = std::time::Duration::from_secs(30);

/// Carries an agent's messages to the UI, where the tools live, and the answers back.
#[derive(Default)]
struct UiBridge {
    app: std::sync::OnceLock<tauri::AppHandle>,
    waiting: std::sync::Mutex<std::collections::HashMap<u64, std::sync::mpsc::Sender<Option<String>>>>,
    next: std::sync::atomic::AtomicU64,
}

#[derive(Clone, serde::Serialize)]
struct AgentRequest {
    id: u64,
    message: String,
}

impl agent::Handler for UiBridge {
    fn handle(&self, message: String) -> agent::Reply {
        let Some(app) = self.app.get() else { return agent::Reply::Unavailable };
        let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (answer, answered) = std::sync::mpsc::channel();
        self.waiting.lock().unwrap().insert(id, answer);
        let sent = app.emit("agent-request", AgentRequest { id, message });
        let reply = sent.ok().and_then(|_| answered.recv_timeout(AGENT_PATIENCE).ok());
        self.waiting.lock().unwrap().remove(&id);
        match reply {
            Some(Some(json)) => agent::Reply::Json(json),
            Some(None) => agent::Reply::Accepted,
            None => agent::Reply::Unavailable,
        }
    }
}

/// The UI's answer to an "agent-request". No message: it was a notification.
#[tauri::command]
fn agent_reply(bridge: State<std::sync::Arc<UiBridge>>, id: u64, message: Option<String>) {
    if let Some(answer) = bridge.waiting.lock().unwrap().remove(&id) {
        let _ = answer.send(message);
    }
}

/// Opens or closes the MCP endpoint. Returns its URL while open.
#[tauri::command]
fn set_agent(app: State<App>, bridge: State<std::sync::Arc<UiBridge>>, enabled: bool, token: String) -> Result<Option<String>, String> {
    let mut server = app.agent.lock().unwrap();
    *server = None; // frees the port before it is taken again
    if !enabled {
        return Ok(None);
    }
    if token.len() < 16 {
        return Err("agent access needs a token".into());
    }
    let started = agent::Server::start(AGENT_PORT, token, bridge.inner().clone()).map_err(|e| format!("Couldn't open port {AGENT_PORT} for agents: {e}"))?;
    log::info!("agent access on at {}", started.url());
    let url = started.url();
    *server = Some(started);
    Ok(Some(url))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    status: Status,
    bypass: bool,
}

#[tauri::command(async)] // off the UI thread: listing activates every endpoint, which blocks while a USB device drops off
fn list_outputs() -> Result<Vec<DeviceInfo>, String> {
    Native.outputs()
}

#[tauri::command]
fn snapshot(app: State<App>) -> Snapshot {
    Snapshot {
        status: app.engine.status(),
        bypass: app.engine.dsp.bypassed(),
    }
}

/// The virtual device plus the user's outputs in order of preference. `None` stops audio.
#[tauri::command]
fn set_plan(app: State<App>, plan: Option<Plan>) {
    app.engine.set_plan(plan);
}

/// Live EQ edit for whichever output is playing.
#[tauri::command]
fn set_preset(app: State<App>, preset: Preset) {
    app.engine.set_preset(preset);
}

#[tauri::command]
fn set_bypass(app: State<App>, bypass: bool) {
    app.engine.dsp.set_bypass(bypass);
}

/// Turns the analyser tap on while the equalizer is on screen.
#[tauri::command]
fn set_analysing(app: State<App>, on: bool) {
    app.engine.dsp.set_analysing(on);
}

/// dBFS per log-spaced band (20 Hz to 20 kHz) of what is playing right now.
#[tauri::command]
fn spectrum(app: State<App>, bands: usize) -> Option<Vec<f32>> {
    app.engine.dsp.spectrum(bands.clamp(8, 256))
}

/// Parses AutoEq / Equalizer APO `ParametricEQ.txt` text into a preset for the editor.
#[tauri::command]
fn parse_preset(text: String) -> Preset {
    freeseries::eq::parse(&text)
}

/// The output Windows plays to by default, so setup can tell when the virtual device has taken over.
#[tauri::command]
fn default_output() -> Option<String> {
    Wasapi.default_output_id()
}

/// Installs the virtual audio device. Shows a UAC prompt; blocks until setup is done.
#[tauri::command(async)]
fn install_device(setup: State<Box<dyn Setup>>) -> Result<Vec<String>, String> {
    setup.install_device()
}

/// Names the virtual device's endpoint "FreeSeries". No prompt.
#[tauri::command(async)]
fn name_device(setup: State<Box<dyn Setup>>) -> Result<Vec<String>, String> {
    setup.name_device()
}

/// Installs the virtual device behind one more channel (Media, then Chat). Shows a UAC prompt.
#[tauri::command(async)]
fn add_channel_device(setup: State<Box<dyn Setup>>) -> Result<Vec<String>, String> {
    setup.add_channel_device()
}

/// The apps that are playing, or ready to play, audio.
#[tauri::command(async)]
fn audio_apps(routing: State<Box<dyn AppRouting>>) -> Result<Vec<AudioApp>, String> {
    routing.apps()
}

/// The icon Windows shows for an executable, as raw RGBA for a canvas.
#[tauri::command(async)]
fn app_icon(routing: State<Box<dyn AppRouting>>, path: String) -> Option<Icon> {
    routing.icon(&path)
}

/// What the headset says about itself: switched on, battery, mic mute, dial. Changes also arrive as "headset" events.
#[tauri::command]
fn headset_state(headset: State<Headset>) -> freeseries::headset::State {
    headset.state()
}

/// What the plugged-in headset can do, so Settings can build itself from it.
#[tauri::command]
fn headset_features(headset: State<Headset>) -> Option<freeseries::headset::Features> {
    headset.features()
}

#[tauri::command]
fn headset_set(headset: State<Headset>, key: String, value: u8) -> Result<(), String> {
    headset.set(&key, value)
}

/// A frame for the headset's display: one bit per pixel, row by row, most significant bit first.
#[tauri::command]
fn display_draw(headset: State<Headset>, bits: Vec<u8>) -> Result<(), String> {
    let pixels: Vec<bool> = bits.iter().flat_map(|byte| (0..8).map(move |i| byte & (0x80 >> i) != 0)).collect();
    headset.draw(&pixels)
}

/// Seconds since the keyboard or mouse was last touched.
#[tauri::command]
fn idle_seconds() -> u64 {
    freeseries::idle::idle_for().as_secs()
}

#[tauri::command]
fn display_release(headset: State<Headset>) -> Result<(), String> {
    headset.release_display()
}

/// Have the headset's ChatMix dial report to us (it then drives the Game / Media balance).
#[tauri::command]
fn set_dial(headset: State<Headset>, enabled: bool) {
    headset.set_dial(enabled);
}

/// Peak level per channel (and "out" for the mix) since the last call, linear, 1.0 = full scale.
#[tauri::command]
fn levels(app: State<App>) -> std::collections::HashMap<String, f32> {
    app.engine.dsp.levels()
}

/// Pins an app to a device, or with no device lets it follow the Windows default again.
#[tauri::command(async)]
fn assign_app(routing: State<Box<dyn AppRouting>>, pid: u32, device: Option<String>) -> Result<(), String> {
    routing.assign(pid, device.as_deref())
}

#[tauri::command]
fn open_sound_settings(app: tauri::AppHandle) -> Result<(), String> {
    app.opener().open_url("ms-settings:sound", None::<&str>).map_err(|e| e.to_string())
}

/// Work while a game has the keyboard. Ctrl+Shift+F-keys on purpose: Ctrl+Alt is AltGr on most
/// European layouts, and plain F-keys belong to the game.
const HOTKEYS: [(Code, &str, &str); 3] = [
    (Code::F9, "previous-sound", "Ctrl+Shift+F9"),
    (Code::F10, "next-sound", "Ctrl+Shift+F10"),
    (Code::F11, "bypass", "Ctrl+Shift+F11"),
];
const HOTKEY_MODIFIERS: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);

/// Turns the global shortcuts on or off. Returns the ones that couldn't be taken, usually because
/// another program already owns them: that must never stop the app from starting.
#[tauri::command]
fn set_hotkeys(app: tauri::AppHandle, enabled: bool) -> Vec<&'static str> {
    let mut refused = Vec::new();
    for (code, _, label) in HOTKEYS {
        let shortcut = Shortcut::new(Some(HOTKEY_MODIFIERS), code);
        let keys = app.global_shortcut();
        let _ = keys.unregister(shortcut);
        if enabled {
            if let Err(e) = keys.register(shortcut) {
                log::warn!("shortcut {label} is not available: {e}");
                refused.push(label);
            }
        }
    }
    refused
}

#[derive(serde::Serialize)]
struct About {
    version: &'static str,
    commit: &'static str,
    /// A development build: runs next to the installed app and never starts audio by itself.
    dev: bool,
}

#[tauri::command]
fn about() -> About {
    About { version: env!("CARGO_PKG_VERSION"), commit: env!("FREESERIES_COMMIT"), dev: cfg!(debug_assertions) }
}

/// Where the log files live, for attaching to bug reports.
#[tauri::command]
fn open_log_folder(app: tauri::AppHandle) -> Result<(), String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(|e| e.to_string())
}

/// The microphone path: the route the user set, the live session, and what went wrong last. A
/// session that dies, or a mic that is not there yet, is tried again every two seconds.
#[derive(Default)]
struct Mic(std::sync::Arc<MicShared>);

#[derive(Default)]
struct MicShared {
    settings: std::sync::Arc<freeseries::mic::Settings>,
    /// Cleared by the session itself when it dies: that thread must not take the lock below, the
    /// lock holder may be joining it.
    alive: std::sync::atomic::AtomicBool,
    state: std::sync::Mutex<MicState>,
}

#[derive(Default)]
struct MicState {
    /// (microphone, cable input, output to hear yourself on), while the user has the mic switched on.
    route: Option<(String, String, Option<String>)>,
    session: Option<freeseries::mic::Session>,
    error: Option<String>,
    /// Bumped by every set_mic, so that a retry from before it gives up.
    generation: u64,
}

#[derive(serde::Serialize)]
struct MicSnapshot {
    running: bool,
    error: Option<String>,
}

const MIC_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

/// Opens the mic path for the stored route, or closes it when there is none.
fn mic_apply(mic: &std::sync::Arc<MicShared>, generation: u64) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut s = mic.state.lock().unwrap();
    if s.generation != generation {
        return;
    }
    s.session = None; // joins the old thread
    let Some((from, into, monitor)) = s.route.clone() else {
        s.error = None;
        return;
    };
    let retry = {
        let mic = mic.clone();
        move || {
            mic.alive.store(false, Relaxed);
            let mic = mic.clone();
            std::thread::spawn(move || {
                std::thread::sleep(MIC_RETRY);
                mic_apply(&mic, generation);
            });
        }
    };
    match freeseries::mic::start(&from, &into, monitor.as_deref(), mic.settings.clone(), Box::new(retry.clone())) {
        Ok(session) => {
            s.session = Some(session);
            s.error = None;
            mic.alive.store(true, Relaxed);
        }
        Err(e) => {
            // AUDCLNT_E_DEVICE_INVALIDATED, "element not found": the device under its saved id is gone.
            let e = if e.contains("0x88890004") || e.contains("0x80070490") { format!("the microphone isn't plugged in ({e})") } else { e };
            if s.error.as_ref() != Some(&e) {
                log::warn!("mic: {e} (tried again every {} s, said once)", MIC_RETRY.as_secs());
            }
            s.error = Some(e);
            retry();
        }
    }
}

#[tauri::command(async)]
fn list_mics() -> Result<Vec<DeviceInfo>, String> {
    freeseries::mic::mics()
}

/// `from` a microphone and `into` the cable's input start the mic path; either missing stops it.
/// `monitor` is an output to also play the voice on ("hear yourself").
#[tauri::command(async)]
fn set_mic(mic: State<Mic>, from: Option<String>, into: Option<String>, monitor: Option<String>) {
    let generation = {
        let mut s = mic.0.state.lock().unwrap();
        s.route = from.zip(into).map(|(from, into)| (from, into, monitor));
        s.generation += 1;
        s.generation
    };
    mic_apply(&mic.0, generation);
}

/// How much background noise is removed: 0 none, 1 all RNNoise can.
#[tauri::command]
fn set_mic_strength(mic: State<Mic>, strength: f32) {
    mic.0.settings.set_strength(strength);
}

#[tauri::command]
fn set_mic_eq(mic: State<Mic>, preset: Preset) {
    mic.0.settings.eq.set(preset);
}

/// Turns the mic's analyser on while an agent listens.
#[tauri::command]
fn set_mic_analysing(mic: State<Mic>, on: bool) {
    mic.0.settings.set_analysing(on);
}

/// dBFS per log-spaced band of the voice as it leaves, or null while the analyser is off.
#[tauri::command]
fn mic_spectrum(mic: State<Mic>, bands: usize) -> Option<Vec<f32>> {
    mic.0.settings.spectrum(bands.clamp(8, 256))
}

#[tauri::command]
fn mic_snapshot(mic: State<Mic>) -> MicSnapshot {
    let s = mic.0.state.lock().unwrap();
    MicSnapshot { running: s.session.is_some() && mic.0.alive.load(std::sync::atomic::Ordering::Relaxed), error: s.error.clone() }
}

/// Peak of the cleaned-up voice since the last call, linear, 1.0 = full scale.
#[tauri::command]
fn mic_level(mic: State<Mic>) -> f32 {
    mic.0.settings.meter.take()
}

/// VB-CABLE's home: the virtual microphone comes from there.
#[tauri::command]
fn open_cable_site(app: tauri::AppHandle) -> Result<(), String> {
    app.opener().open_url("https://vb-audio.com/Cable/", None::<&str>).map_err(|e| e.to_string())
}

/// Passed by the start-with-Windows entry so a boot launch stays in the tray.
const MINIMIZED: &str = "--minimized";

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[tauri::command]
fn starts_with_windows(app: State<App>) -> Result<bool, String> {
    app.autostart.is_enabled()
}

#[tauri::command]
fn set_starts_with_windows(app: State<App>, enabled: bool) -> Result<(), String> {
    // A dev build would register target\debug\...exe, which needs the Vite server and breaks at boot.
    if cfg!(debug_assertions) {
        return Err("Start with Windows only works in the installed app, not in a development build.".into());
    }
    app.autostart.set_enabled(enabled)?;
    // 0.1.0 registered a Run-key entry instead; left behind it would launch a second time, late.
    let _ = std::process::Command::new("reg.exe")
        .args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "freeseries", "/f"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
    Ok(())
}

/// Our virtual devices: every endpoint of the FxSound driver.
fn virtual_devices() -> Vec<DeviceInfo> {
    Wasapi.outputs().unwrap_or_default().into_iter().filter(|d| d.name.to_lowercase().contains("fxsound")).collect()
}

/// The one Windows should play into: setup names it plain "FreeSeries", the channels "FreeSeries Media" and so on.
fn main_virtual_device(ours: &[DeviceInfo]) -> Option<String> {
    ours.iter().find(|d| d.name.to_lowercase().starts_with("freeseries (")).or(ours.first()).map(|d| d.id.clone())
}

type WindowsHandback = handback::Handback<freeseries::default_output::WindowsDefaults, handback::FileStore>;

/// How long a failure may last before it is more than a hiccup: the default is handed back, the
/// tray says so, and Windows shows one notification.
const HICCUP: std::time::Duration = std::time::Duration::from_secs(10);

/// One thing that can fail (the output, the mic), followed over time. Failures were found by friends
/// in Discord calls or in the log (2026-11-20: the mic retried a missing headset 1411 times in 47
/// minutes, and the app said nothing).
#[derive(Default)]
struct Failing {
    since: Option<std::time::Instant>,
    told: bool,
}

impl Failing {
    /// Takes what is wrong now, if anything. Returns it once it has lasted `HICCUP`, and whether this
    /// is the moment to tell the user (once per failure).
    fn update(&mut self, problem: Option<String>, now: std::time::Instant) -> (Option<String>, bool) {
        let Some(problem) = problem else {
            *self = Failing::default();
            return (None, false);
        };
        if now.duration_since(*self.since.get_or_insert(now)) < HICCUP {
            return (None, false);
        }
        let tell = !self.told;
        self.told = true;
        (Some(problem), tell)
    }
}

/// Keeps the Windows default where the engine is: ours while it plays, the real output when it
/// doesn't, or when it has been reconnecting for longer than a hiccup. Apps pinned to a channel are
/// handed back with it (the UI puts them back once audio runs). And says what is wrong, in the
/// tray's tooltip and once as a Windows notification.
/// A development build leaves the default and the apps alone: they belong to the installed app.
fn follow_engine(app: tauri::AppHandle, state_file: std::path::PathBuf) {
    let installed = !cfg!(debug_assertions);
    // Outlives us, so it can act when we can't: a crash, End task.
    if installed {
        if let Ok(exe) = std::env::current_exe() {
            let spawned = std::process::Command::new(exe).arg(handback::WATCHDOG).arg(std::process::id().to_string()).arg(&state_file).creation_flags(0x0800_0000).spawn(); // CREATE_NO_WINDOW
            if let Err(e) = spawned {
                log::warn!("no watchdog: a crash would leave Windows playing into nothing ({e})");
            }
        }
    }
    std::thread::spawn(move || {
        use tauri_plugin_notification::NotificationExt;
        let handback = WindowsHandback { defaults: freeseries::default_output::WindowsDefaults, store: handback::FileStore(state_file) };
        let mut playing_on: Option<String> = None;
        let (mut output, mut mic) = (Failing::default(), Failing::default());
        let mut tooltip = String::new();
        loop {
            let now = std::time::Instant::now();
            let status = app.state::<App>().engine.status();
            let (output_problem, tell_output) = output.update(match &status { Status::Reconnecting { error } => Some(format!("No sound: {error}")), _ => None }, now);
            let mic_error = app.state::<Mic>().0.state.lock().unwrap().error.clone(); // set only while the mic is on
            let (mic_problem, tell_mic) = mic.update(mic_error.map(|e| format!("Mic off: {e}")), now);

            let target = match &status {
                Status::Running { output } => Some(output.clone()),
                Status::Stopped => None,
                Status::Reconnecting { .. } if output_problem.is_some() => None,
                Status::Reconnecting { .. } => playing_on.clone(), // a hiccup, not an absence
            };
            if installed && target != playing_on {
                let ours = virtual_devices();
                match &target {
                    Some(output) => handback.running(output, main_virtual_device(&ours).as_deref()),
                    None => {
                        release_apps(app.state::<Box<dyn AppRouting>>().inner().as_ref());
                        handback.away(&ours.into_iter().map(|d| d.id).collect::<Vec<_>>());
                    }
                }
            }
            playing_on = target;

            for (problem, tell) in [(&output_problem, tell_output), (&mic_problem, tell_mic)] {
                if let (true, Some(problem)) = (tell, problem) {
                    log::warn!("telling the user: {problem}");
                    let _ = app.notification().builder().title("FreeSeries").body(problem.as_str()).show();
                }
            }
            let problems: Vec<String> = [output_problem, mic_problem].into_iter().flatten().collect();
            let text = std::iter::once(app.package_info().name.clone()).chain(problems.iter().cloned()).collect::<Vec<_>>().join("
");
            if text != tooltip {
                if let Some(tray) = app.tray_by_id(TRAY) {
                    let _ = tray.set_tooltip(Some(&text));
                }
                tooltip = text;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

/// `--watchdog <pid> <state file>`: waits for the app to be gone, then makes sure Windows isn't left playing into our device.
fn watchdog(pid: &str, state_file: &str) {
    freeseries::default_output::wait_for_exit(pid.parse().unwrap_or(0));
    let ours: Vec<String> = virtual_devices().into_iter().map(|d| d.id).collect();
    WindowsHandback { defaults: freeseries::default_output::WindowsDefaults, store: handback::FileStore(state_file.into()) }.away(&ours);
    release_apps(&WindowsRouting);
}

/// Quitting takes the engine away, and with it everything that listens to the virtual devices. Apps
/// the Mixer moved onto a channel are pinned there by Windows, so they would stay silent even after
/// the user points Windows back at their headphones. They are handed back first; the UI remembers
/// where they belong and puts them back when FreeSeries runs again. The same goes for Stop, an
/// engine that stays down, and a crash (the watchdog).
fn release_apps(routing: &dyn AppRouting) {
    let ours: Vec<String> = virtual_devices().into_iter().map(|d| d.id).collect();
    match freeseries::routing::release(routing, &ours) {
        Ok(released) if !released.is_empty() => log::info!("audio stopped: handed back to the Windows default: {}", released.join(", ")),
        Ok(_) => {}
        Err(e) => log::warn!("audio stopped: couldn't hand apps back to the Windows default: {e}"),
    }
}

const TRAY: &str = "main";

/// An entry of the tray's right-click menu. The UI owns what the menu shows (outputs, presets, the
/// switches) and sends the whole list whenever that changes; a click comes back as a "tray" event
/// carrying the id.
#[derive(serde::Deserialize)]
struct TrayItem {
    id: String,
    /// "-" is a separator.
    label: String,
    /// Set for a check item.
    checked: Option<bool>,
    /// Set for a submenu.
    items: Option<Vec<TrayItem>>,
}

type MenuEntry = Box<dyn IsMenuItem<tauri::Wry>>;

fn tray_items<M: Manager<tauri::Wry>>(app: &M, items: &[TrayItem]) -> tauri::Result<Vec<MenuEntry>> {
    items
        .iter()
        .map(|item| -> tauri::Result<MenuEntry> {
            Ok(if item.label == "-" {
                Box::new(PredefinedMenuItem::separator(app)?)
            } else if let Some(sub) = &item.items {
                let inner = tray_items(app, sub)?;
                Box::new(Submenu::with_id_and_items(app, &item.id, &item.label, true, &inner.iter().map(|i| i.as_ref()).collect::<Vec<_>>())?)
            } else if let Some(checked) = item.checked {
                Box::new(CheckMenuItem::with_id(app, &item.id, &item.label, true, checked, None::<&str>)?)
            } else {
                Box::new(MenuItem::with_id(app, &item.id, &item.label, true, None::<&str>)?)
            })
        })
        .collect()
}

/// Open on top, Quit at the bottom, the UI's items between.
fn tray_menu<M: Manager<tauri::Wry>>(app: &M, items: &[TrayItem]) -> tauri::Result<Menu<tauri::Wry>> {
    // "FreeSeries", or "FreeSeries dev" for a development build running next to the installed app.
    let name = app.config().product_name.clone().unwrap_or_else(|| "FreeSeries".into());
    let mut all: Vec<MenuEntry> = vec![Box::new(MenuItem::with_id(app, "open", format!("Open {name}"), true, None::<&str>)?)];
    if !items.is_empty() {
        all.push(Box::new(PredefinedMenuItem::separator(app)?));
        all.extend(tray_items(app, items)?);
    }
    all.push(Box::new(PredefinedMenuItem::separator(app)?));
    all.push(Box::new(MenuItem::with_id(app, "quit", "Quit (stops audio processing)", true, None::<&str>)?));
    Menu::with_items(app, &all.iter().map(|i| i.as_ref()).collect::<Vec<_>>())
}

#[tauri::command]
fn set_tray_menu(app: tauri::AppHandle, items: Vec<TrayItem>) -> Result<(), String> {
    let tray = app.tray_by_id(TRAY).ok_or("no tray icon")?;
    tray.set_menu(Some(tray_menu(&app, &items).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())
}

fn tray(app: &tauri::App) -> tauri::Result<()> {
    let name = app.config().product_name.clone().unwrap_or_else(|| "FreeSeries".into());
    let mut tray = TrayIconBuilder::with_id(TRAY)
        .tooltip(&name)
        .menu(&tray_menu(app, &[])?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => {
                release_apps(app.state::<Box<dyn AppRouting>>().inner().as_ref());
                if !cfg!(debug_assertions) {
                    if let Ok(dir) = app.path().app_data_dir() {
                        let ours: Vec<String> = virtual_devices().into_iter().map(|d| d.id).collect();
                        WindowsHandback { defaults: freeseries::default_output::WindowsDefaults, store: handback::FileStore(dir.join(HANDBACK_FILE)) }.away(&ours);
                    }
                }
                app.state::<Headset>().stop(); // gives the dial back to the hardware
                app.exit(0);
            }
            // The UI's items: it knows what they mean.
            id => {
                let _ = app.emit("tray", id);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
const HANDBACK_FILE: &str = "handback.json";

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    if let [_, flag, pid, state_file] = args.as_slice() {
        if flag == handback::WATCHDOG {
            return watchdog(pid, state_file);
        }
    }
    // The headset watcher starts before the app exists (the engine needs it to know which outputs
    // are switched on), so its events wait for the handle.
    let events: std::sync::Arc<std::sync::OnceLock<tauri::AppHandle>> = Default::default();
    let to_ui = events.clone();
    let headset = Headset::start(Usb, &freeseries::headsets::ALL, move |state| {
        if let Some(app) = to_ui.get() {
            let _ = app.emit("headset", state);
        }
    });
    tauri::Builder::default()
        // Must be first. A second copy would run a second engine and double the audio.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // A refused boot launch must not pop the window open.
            if !args.iter().any(|a| a == MINIMIZED) {
                show_main(app);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            // No shortcuts yet: the UI switches them on (set_hotkeys) once it knows the user's setting.
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        if let Some((_, action, _)) = HOTKEYS.iter().find(|(code, _, _)| shortcut.matches(HOTKEY_MODIFIERS, *code)) {
                            let _ = app.emit("hotkey", action);
                        }
                    }
                })
                .build(),
        )
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .timezone_strategy(TimezoneStrategy::UseLocal)
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir { file_name: None }),
                    Target::new(TargetKind::Webview),
                ])
                .max_file_size(1_000_000)
                .rotation_strategy(RotationStrategy::KeepSome(3))
                .build(),
        )
        .manage(App { engine: Engine::start(Native, headset.clone()), autostart: Box::new(TaskScheduler { task: "freeseries", args: MINIMIZED }), agent: Default::default() })
        .manage(headset)
        .manage(Mic::default())
        .invoke_handler(tauri::generate_handler![
            list_outputs,
            snapshot,
            set_plan,
            set_preset,
            set_bypass,
            set_analysing,
            spectrum,
            parse_preset,
            open_log_folder,
            starts_with_windows,
            set_starts_with_windows,
            default_output,
            install_device,
            name_device,
            add_channel_device,
            audio_apps,
            app_icon,
            levels,
            assign_app,
            open_sound_settings,
            about,
            set_hotkeys,
            set_tray_menu,
            set_agent,
            headset_state,
            headset_features,
            headset_set,
            display_draw,
            display_release,
            idle_seconds,
            set_dial,
            agent_reply,
            list_mics,
            set_mic,
            set_mic_strength,
            set_mic_eq,
            set_mic_analysing,
            mic_spectrum,
            mic_snapshot,
            mic_level,
            open_cable_site
        ])
        .setup(move |app| {
            let _ = events.set(app.handle().clone());
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            follow_engine(app.handle().clone(), data.join(HANDBACK_FILE));
            let resources = app.path().resource_dir()?.join("resources");
            let bridge = std::sync::Arc::new(UiBridge::default());
            let _ = bridge.app.set(app.handle().clone());
            app.manage(bridge);
            app.manage(Box::new(WindowsRouting) as Box<dyn AppRouting>);
            app.manage(Box::new(ScriptSetup { script: resources.join("setup.ps1"), package: resources.join("fxvad") }) as Box<dyn Setup>);
            tray(app)?;
            // The window is created hidden; a boot launch leaves it that way.
            if !std::env::args().any(|a| a == MINIMIZED) {
                show_main(app.handle());
            }
            Ok(())
        })
        // Closing the window must not stop the audio: hide to the tray, quit only from its menu.
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{Failing, HICCUP};
    use std::time::{Duration, Instant};

    #[test]
    fn a_failure_is_told_once_after_a_hiccup() {
        let (mut f, t) = (Failing::default(), Instant::now());
        let gone = || Some("gone".to_string());
        assert_eq!(f.update(gone(), t), (None, false), "a hiccup says nothing");
        assert_eq!(f.update(gone(), t + HICCUP), (gone(), true), "lasting: shown and told");
        assert_eq!(f.update(gone(), t + HICCUP * 2), (gone(), false), "told once");
        assert_eq!(f.update(None, t + HICCUP * 3), (None, false));
        assert_eq!(f.update(gone(), t + HICCUP * 3 + Duration::from_secs(1)), (None, false), "a new failure starts over");
    }
}
