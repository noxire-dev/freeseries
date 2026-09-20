use freeseries::engine::{AudioBackend, DeviceInfo, Engine, Route, Status};
use freeseries::eq::Preset;
use freeseries::wasapi::Wasapi;
use std::sync::Mutex;
use tauri::{Manager, State};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};
use tauri_plugin_opener::OpenerExt;

struct App {
    engine: Engine,
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
        .manage(App { engine: Engine::start(Wasapi), route: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![list_outputs, snapshot, set_route, set_preset, set_bypass, parse_preset, open_log_folder])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
