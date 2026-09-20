//! CLI for the engine, for development and soak testing. The app lives in `src-tauri`.

use freeseries::engine::{AlwaysOn, AudioBackend, Engine, Output, Plan};
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

    if args.first().map(String::as_str) == Some("headset") {
        println!("Arctis Nova Pro headset online: {:?}", freeseries::arctis::ArctisNovaPro::default().headset_online());
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("run") || args.len() < 3 {
        for (i, d) in devices.iter().enumerate() {
            println!("{i}: {} ({} ch, {} Hz)", d.name, d.channels, d.sample_rate);
        }
        println!("\nusage: freeseries run <source#> <target#> [ParametricEQ.txt]");
        return Ok(());
    }

    let device = |arg: &str| Ok::<_, Box<dyn Error>>(devices.get(arg.parse::<usize>()?).ok_or("no such device")?.id.clone());
    let preset = match args.get(3) {
        Some(path) => eq::parse(&std::fs::read_to_string(path)?),
        None => eq::Preset::default(),
    };
    println!("{} EQ bands. Ctrl+C to stop.", preset.bands.len());
    let engine = Engine::start(Wasapi, AlwaysOn);
    engine.set_plan(Some(Plan { from: device(&args[1])?, outputs: vec![Output { id: device(&args[2])?, preset }] }));

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
