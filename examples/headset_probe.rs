//! Talks to an Arctis Nova Pro Wireless base station and prints what comes back, raw, from EVERY
//! HID collection it exposes: the status reply, then, with software ChatMix switched on, every
//! event for a while (turn the dial, press mute, turn the volume). This is how the byte facts in
//! docs/headsets.md get checked on real hardware, and how to find which collection carries what.
//! cargo run --example headset_probe -- [seconds]
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(15);
    let api = hidapi::HidApi::new()?;
    let mut links = Vec::new();
    for info in api.device_list().filter(|d| d.vendor_id() == 0x1038 && [0x12E0, 0x12E5, 0x225D].contains(&d.product_id())) {
        let path = info.path().to_string_lossy().to_ascii_lowercase();
        let tag = path.split('#').nth(1).unwrap_or(&path).split('&').filter(|p| p.starts_with("mi_") || p.starts_with("col")).collect::<Vec<_>>().join("&");
        let opened = info.open_device(&api);
        println!("{tag:<12} usage page {:04x} usage {:04x}  {}", info.usage_page(), info.usage(), if opened.is_ok() { "opened" } else { "can't open" });
        if let Ok(device) = opened {
            links.push((tag, device));
        }
    }
    let command = links.iter().position(|(tag, _)| tag == "mi_04&col01").ok_or("no command collection (mi_04&col01): is the base station plugged in?")?;
    let send = |bytes: &[u8]| -> Result<(), hidapi::HidError> {
        let mut message = [0u8; 64];
        message[..bytes.len()].copy_from_slice(bytes);
        links[command].1.write(&message)?;
        std::thread::sleep(Duration::from_millis(50)); // the firmware drops commands that come too fast
        Ok(())
    };
    let listen = |for_: Duration| {
        let began = Instant::now();
        let mut report = [0u8; 1024];
        let mut last = Vec::new();
        while began.elapsed() < for_ {
            for (tag, device) in &links {
                let n = device.read_timeout(&mut report, 5).unwrap_or(0);
                // The status reply repeats every 2 s (the installed app asks too): show it when it changes.
                if n > 0 && !(report[..2] == [0x06, 0xB0] && last == report[..16]) {
                    if report[..2] == [0x06, 0xB0] {
                        last = report[..16].to_vec();
                    }
                    println!("  {:>6.2}s  {tag:<12} {}", began.elapsed().as_secs_f32(), report[..n.min(16)].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
                }
            }
        }
    };

    println!("status (06 b0): [6] battery 0-8, [7] slot battery, [9] mic muted, [0a] ANC, [0e] pairing, [0f] power");
    send(&[0x06, 0xB0])?;
    listen(Duration::from_millis(400));
    println!("software ChatMix on (06 8d 01, 06 49 01). For {seconds} s: turn the ChatMix dial, turn the volume, press mute.");
    send(&[0x06, 0x8D, 0x01])?;
    send(&[0x06, 0x49, 0x01])?;
    listen(Duration::from_secs(seconds));
    println!("software ChatMix off (06 49 00, 06 8d 00)");
    send(&[0x06, 0x49, 0x00])?;
    send(&[0x06, 0x8D, 0x00])?;
    Ok(())
}
