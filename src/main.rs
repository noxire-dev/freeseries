//! CLI for the engine, for development and soak testing. The app lives in `src-tauri`.

use freeseries::engine::{AudioBackend, Engine, Route};
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

    if args.first().map(String::as_str) != Some("run") || args.len() < 3 {
        for (i, d) in devices.iter().enumerate() {
            println!("{i}: {} ({} ch, {} Hz)", d.name, d.channels, d.sample_rate);
        }
        println!("\nusage: freeseries run <source#> <target#> [ParametricEQ.txt]");
        return Ok(());
    }

    let device = |arg: &str| Ok::<_, Box<dyn Error>>(devices.get(arg.parse::<usize>()?).ok_or("no such device")?.id.clone());
    let engine = Engine::start(Wasapi);
    if let Some(path) = args.get(3) {
        engine.dsp.set_preset(eq::parse(&std::fs::read_to_string(path)?));
    }
    println!("{} EQ bands. Ctrl+C to stop.", engine.dsp.preset().bands.len());
    engine.set_route(Some(Route { from: device(&args[1])?, to: device(&args[2])? }));

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
