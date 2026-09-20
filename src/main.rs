//! CLI for the engine, for development and soak testing. The app lives in `src-tauri`.

use freeseries::engine::{AlwaysOn, AudioBackend, Channel, Engine, Output, Plan};
use freeseries::{eq, wasapi::Wasapi};
use std::error::Error;
use std::time::Duration;

/// The engine logs through the `log` facade; the CLI just prints it.
struct Stderr;
impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        eprintln!("{:5} {}", record.level(), record.args());
    }
    fn flush(&self) {}
}

fn main() -> Result<(), Box<dyn Error>> {
    log::set_logger(&Stderr).map_err(|e| e.to_string())?;
    log::set_max_level(log::LevelFilter::Info);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let devices = Wasapi.outputs()?;

    // `apps`: who has an audio session, and where they are pinned. `pin <pid> <device#|default>`: move one.
    if args.first().map(String::as_str) == Some("apps") {
        use freeseries::routing::{AppRouting, WindowsRouting};
        for app in WindowsRouting.apps()? {
            let pinned = app.device.as_ref().map(|id| devices.iter().find(|d| &d.id == id).map_or(id.clone(), |d| d.name.clone()));
            println!("{:>6}  {:<28} {}", app.pid, app.name, pinned.unwrap_or_else(|| "(default device)".into()));
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("pin") && args.len() == 3 {
        use freeseries::routing::{AppRouting, WindowsRouting};
        let target = if args[2] == "default" { None } else { Some(devices.get(args[2].parse::<usize>()?).ok_or("no such device")?.id.clone()) };
        WindowsRouting.assign(args[1].parse()?, target.as_deref())?;
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("headset") {
        println!("Arctis Nova Pro headset online: {:?}", freeseries::arctis::ArctisNovaPro::default().headset_online());
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("run") || args.len() < 3 {
        for (i, d) in devices.iter().enumerate() {
            println!("{i}: {} ({} ch, {} Hz)", d.name, d.channels, d.sample_rate);
        }
        println!("\nusage: freeseries run <source#>[,<source#>...] <target#> [ParametricEQ.txt]");
        return Ok(());
    }

    let device = |arg: &str| Ok::<_, Box<dyn Error>>(devices.get(arg.parse::<usize>()?).ok_or("no such device")?.id.clone());
    let preset = match args.get(3) {
        Some(path) => eq::parse(&std::fs::read_to_string(path)?),
        None => eq::Preset::default(),
    };
    println!("{} EQ bands. Ctrl+C to stop.", preset.bands.len());
    let engine = Engine::start(Wasapi, AlwaysOn);
    // Several sources ("0,1") are mixed, the way the app's channels are.
    let channels = args[1]
        .split(',')
        .enumerate()
        .map(|(i, source)| Ok(Channel { id: format!("channel{i}"), from: device(source)?, volume: 1.0, preset: eq::Preset::default() }))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    engine.set_plan(Some(Plan { channels, outputs: vec![Output { id: device(&args[2])?, preset }] }));

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
