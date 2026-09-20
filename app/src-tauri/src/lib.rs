use freeseries::engine::{AudioBackend, DeviceInfo, Engine, Route, Status};
use freeseries::eq::Preset;
use freeseries::wasapi::Wasapi;
use std::sync::Mutex;
use tauri::State;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(App { engine: Engine::start(Wasapi), route: Mutex::new(None) })
        .invoke_handler(tauri::generate_handler![list_outputs, snapshot, set_route, set_preset, set_bypass, parse_preset])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
