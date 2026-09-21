//! Talks to an Arctis Nova Pro Wireless base station: prints the status reply, then switches
//! software ChatMix on, prints every event for a while (turn the dial, press mute), and switches
//! it off again. Byte facts from docs/headsets.md; this is how they get checked on real hardware.
//! cargo run --example headset_probe -- [seconds]
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    let api = hidapi::HidApi::new()?;
    let info = api
        .device_list()
        .find(|d| d.vendor_id() == 0x1038 && [0x12E0, 0x12E5, 0x225D].contains(&d.product_id()) && d.path().to_string_lossy().to_ascii_lowercase().contains("mi_04&col01"))
        .ok_or("no Nova Pro Wireless base station found")?;
    println!("found {:04x}:{:04x} {}", info.vendor_id(), info.product_id(), info.product_string().unwrap_or("?"));
    let device = info.open_device(&api)?;
    let send = |bytes: &[u8]| -> Result<(), hidapi::HidError> {
        let mut message = [0u8; 64];
        message[..bytes.len()].copy_from_slice(bytes);
        device.write(&message)?;
        std::thread::sleep(Duration::from_millis(50)); // the firmware drops commands that come too fast
        Ok(())
    };
    let listen = |for_: Duration| -> Result<(), hidapi::HidError> {
        let until = Instant::now() + for_;
        let mut report = [0u8; 64];
        while Instant::now() < until {
            let n = device.read_timeout(&mut report, 100)?;
            if n > 0 {
                let shown = if report[0] == 0x06 { 16 } else { 6 };
                println!("  {:>6.2}s  {}", (for_ - until.saturating_duration_since(Instant::now())).as_secs_f32(), report[..shown.min(n)].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
            }
        }
        Ok(())
    };

    println!("status (06 b0): [6] battery 0-8, [7] slot battery, [9] mic muted, [0a] ANC, [0e] pairing, [0f] power");
    send(&[0x06, 0xB0])?;
    listen(Duration::from_millis(400))?;

    println!("software ChatMix on (06 8d 01, 06 49 01); turn the dial, press mute: {seconds} s");
    send(&[0x06, 0x8D, 0x01])?;
    send(&[0x06, 0x49, 0x01])?;
    let heard = listen(Duration::from_secs(seconds));
    println!("software ChatMix off (06 49 00, 06 8d 00)");
    send(&[0x06, 0x49, 0x00])?;
    send(&[0x06, 0x8D, 0x00])?;
    heard?;
    Ok(())
}
