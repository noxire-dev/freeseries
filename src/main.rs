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

fn device_id(devices: &[freeseries::engine::DeviceInfo], number: &str) -> Result<String, Box<dyn Error>> {
    Ok(devices.get(number.parse::<usize>()?).ok_or("no such device")?.id.clone())
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
            let icon = WindowsRouting.icon(&app.path).map_or("no icon".to_string(), |i| format!("{}x{} icon, {} visible px", i.width, i.height, i.rgba.chunks_exact(4).filter(|p| p[3] > 0).count()));
            println!("{:>6}  {:<26} {:<24} {}", app.pid, app.name, icon, pinned.unwrap_or_else(|| "(default device)".into()));
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("pin") && args.len() == 3 {
        use freeseries::routing::{AppRouting, WindowsRouting};
        let target = if args[2] == "default" { None } else { Some(devices.get(args[2].parse::<usize>()?).ok_or("no such device")?.id.clone()) };
        WindowsRouting.assign(args[1].parse()?, target.as_deref())?;
        return Ok(());
    }
    // `default`: what Windows plays to, per role. `default <device#>`: point all three roles at a device.
    if args.first().map(String::as_str) == Some("default") {
        use freeseries::default_output::{DefaultOutput, WindowsDefaults, ROLES};
        if let Some(target) = args.get(1) {
            let id = device_id(&devices, target)?;
            for role in ROLES {
                WindowsDefaults.set(role, &id)?;
            }
        }
        for role in ROLES {
            let now = WindowsDefaults.get(role);
            println!("{role:?}: {}", now.as_ref().and_then(|id| devices.iter().find(|d| &d.id == id)).map_or(format!("{now:?}"), |d| d.name.clone()));
        }
        return Ok(());
    }
    // `mics`: the microphones. `mic <mic#|o<out#>> <cable#> [seconds] [raw]`: run the mic path, RNNoise on unless `raw`;
    // `o2` takes output 2's loopback as the mic, for measuring.
    if args.first().map(String::as_str) == Some("mics") {
        for (i, m) in freeseries::mic::mics()?.iter().enumerate() {
            println!("{i}: {} ({} ch, {} Hz)", m.name, m.channels, m.sample_rate);
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("mic") && args.len() >= 3 {
        let from = match args[1].strip_prefix('o') {
            Some(n) => device_id(&devices, n)?,
            None => freeseries::mic::mics()?.get(args[1].parse::<usize>()?).ok_or("no such mic")?.id.clone(),
        };
        let into = device_id(&devices, &args[2])?;
        let settings = std::sync::Arc::new(freeseries::mic::Settings::default());
        settings.suppress.store(!args.iter().any(|a| a == "raw"), std::sync::atomic::Ordering::Relaxed);
        let session = freeseries::mic::start(&from, &into, settings.clone(), Box::new(|| log::error!("the mic path stopped")))?;
        let seconds: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
        for _ in 0..seconds {
            std::thread::sleep(Duration::from_secs(1));
            let peak = settings.meter.take();
            log::info!("mic peak {:.1} dBFS", 20.0 * peak.max(1e-6).log10());
        }
        drop(session);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("headset") {
        // Follows the headset for a while: switch it on and off, press mute, turn the dial.
        use freeseries::headset::{Headset, Usb};
        let headset = Headset::start(Usb, &freeseries::headsets::ALL, |state| println!("{state:?}"));
        headset.set_dial(true);
        std::thread::sleep(Duration::from_secs(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20)));
        headset.stop();
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("run") || args.len() < 3 {
        for (i, d) in devices.iter().enumerate() {
            println!("{i}: {} ({} ch, {} Hz){}", d.name, d.channels, d.sample_rate, if args.first().map(String::as_str) == Some("ids") { format!("  {}", d.id) } else { String::new() });
        }
        println!("\nusage: freeseries run <source#>[,<source#>...] <target#> [ParametricEQ.txt]");
        return Ok(());
    }

    let preset = match args.get(3) {
        Some(path) => eq::parse(&std::fs::read_to_string(path)?),
        None => eq::Preset::default(),
    };
    println!("{} EQ bands. Ctrl+C to stop.", preset.bands.len());
    // The native backend is the one shipped; FREESERIES_CPAL=1 runs the old cpal one for comparison.
    if std::env::var("FREESERIES_CPAL").is_ok() {
        serve(Engine::start(Wasapi, AlwaysOn), &args, &devices, preset)
    } else {
        serve(Engine::start(freeseries::native::Native, AlwaysOn), &args, &devices, preset)
    }
}

fn serve(engine: Engine, args: &[String], devices: &[freeseries::engine::DeviceInfo], preset: eq::Preset) -> Result<(), Box<dyn Error>> {
    let device = |arg: &str| Ok::<_, Box<dyn Error>>(devices.get(arg.parse::<usize>()?).ok_or("no such device")?.id.clone());
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
