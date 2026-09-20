//! CLI for the engine, for development and soak testing. The app lives in `src-tauri`.

use freeseries::engine::{AudioBackend, Engine, Route, Status};
use freeseries::{eq, wasapi::Wasapi};
use std::error::Error;
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
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

    let mut last = Status::Stopped;
    loop {
        let status = engine.status();
        if status != last {
            println!("{status:?}");
            last = status;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
