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
use freeseries::wasapi::Wasapi;
use std::os::windows::process::CommandExt;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
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

#[tauri::command]
fn list_outputs() -> Result<Vec<DeviceInfo>, String> {
    Wasapi.outputs()
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

/// Keeps the Windows default where the engine is: ours while it plays, the real output when it doesn't.
/// A development build leaves the default alone: it belongs to the installed app running next to it.
fn follow_engine(app: tauri::AppHandle, state_file: std::path::PathBuf) {
    if cfg!(debug_assertions) {
        return;
    }
    // Outlives us, so it can act when we can't: a crash, End task.
    if let Ok(exe) = std::env::current_exe() {
        let spawned = std::process::Command::new(exe).arg(handback::WATCHDOG).arg(std::process::id().to_string()).arg(&state_file).creation_flags(0x0800_0000).spawn(); // CREATE_NO_WINDOW
        if let Err(e) = spawned {
            log::warn!("no watchdog: a crash would leave Windows playing into nothing ({e})");
        }
    }
    std::thread::spawn(move || {
        let handback = WindowsHandback { defaults: freeseries::default_output::WindowsDefaults, store: handback::FileStore(state_file) };
        let mut playing_on: Option<String> = None;
        loop {
            let now = match app.state::<App>().engine.status() {
                Status::Running { output } => Some(output),
                Status::Stopped => None,
                Status::Reconnecting { .. } => playing_on.clone(), // a hiccup, not an absence
            };
            if now != playing_on {
                let ours = virtual_devices();
                match &now {
                    Some(output) => handback.running(output, main_virtual_device(&ours).as_deref()),
                    None => handback.away(&ours.into_iter().map(|d| d.id).collect::<Vec<_>>()),
                }
                playing_on = now;
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
}

/// Quitting takes the engine away, and with it everything that listens to the virtual devices. Apps
/// the Mixer moved onto a channel are pinned there by Windows, so they would stay silent even after
/// the user points Windows back at their headphones. They are handed back first; the UI remembers
/// where they belong and puts them back when FreeSeries runs again.
fn release_apps(app: &tauri::AppHandle) {
    let ours: Vec<String> = virtual_devices().into_iter().map(|d| d.id).collect();
    match freeseries::routing::release(app.state::<Box<dyn AppRouting>>().inner().as_ref(), &ours) {
        Ok(released) if !released.is_empty() => log::info!("quitting: handed back to the Windows default: {}", released.join(", ")),
        Ok(_) => {}
        Err(e) => log::warn!("quitting: couldn't hand apps back to the Windows default: {e}"),
    }
}

fn tray(app: &tauri::App) -> tauri::Result<()> {
    // "FreeSeries", or "FreeSeries dev" for a development build running next to the installed app.
    let name = app.config().product_name.clone().unwrap_or_else(|| "FreeSeries".into());
    let open = MenuItem::with_id(app, "open", format!("Open {name}"), true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit (stops audio processing)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip(&name)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => {
                release_apps(app);
                if !cfg!(debug_assertions) {
                    if let Ok(dir) = app.path().app_data_dir() {
                        let ours: Vec<String> = virtual_devices().into_iter().map(|d| d.id).collect();
                        WindowsHandback { defaults: freeseries::default_output::WindowsDefaults, store: handback::FileStore(dir.join(HANDBACK_FILE)) }.away(&ours);
                    }
                }
                app.state::<Headset>().stop(); // gives the dial back to the hardware
                app.exit(0);
            }
            _ => {}
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
        .manage(App { engine: Engine::start(Wasapi, headset.clone()), autostart: Box::new(TaskScheduler { task: "freeseries", args: MINIMIZED }), agent: Default::default() })
        .manage(headset)
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
            set_agent,
            headset_state,
            headset_features,
            headset_set,
            display_draw,
            display_release,
            set_dial,
            agent_reply
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
