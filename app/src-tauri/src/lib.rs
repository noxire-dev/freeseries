mod autostart;

use autostart::{Autostart, TaskScheduler};
use freeseries::engine::{AudioBackend, DeviceInfo, Engine, Route, Status};
use freeseries::eq::Preset;
use freeseries::wasapi::Wasapi;
use std::os::windows::process::CommandExt;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, State, WindowEvent};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};
use tauri_plugin_opener::OpenerExt;

struct App {
    engine: Engine,
    autostart: Box<dyn Autostart>,
    route: Mutex<Option<Route>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    status: Status,
    route: Option<Route>,
    preset: Preset,
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
        route: app.route.lock().unwrap().clone(),
        preset: app.engine.dsp.preset(),
        bypass: app.engine.dsp.bypassed(),
    }
}

/// `None` stops audio.
#[tauri::command]
fn set_route(app: State<App>, route: Option<Route>) {
    *app.route.lock().unwrap() = route.clone();
    app.engine.set_route(route);
}

#[tauri::command]
fn set_preset(app: State<App>, preset: Preset) {
    app.engine.dsp.set_preset(preset);
}

#[tauri::command]
fn set_bypass(app: State<App>, bypass: bool) {
    app.engine.dsp.set_bypass(bypass);
}

/// Parses AutoEq / Equalizer APO `ParametricEQ.txt` text into a preset for the editor.
#[tauri::command]
fn parse_preset(text: String) -> Preset {
    freeseries::eq::parse(&text)
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

fn tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open freeseries", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit (stops audio processing)", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("freeseries")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
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
pub fn run() {
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
        .manage(App { engine: Engine::start(Wasapi), autostart: Box::new(TaskScheduler { task: "freeseries", args: MINIMIZED }), route: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![
            list_outputs,
            snapshot,
            set_route,
            set_preset,
            set_bypass,
            parse_preset,
            open_log_folder,
            starts_with_windows,
            set_starts_with_windows
        ])
        .setup(|app| {
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
