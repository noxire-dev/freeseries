//! Draws a test pattern on the Arctis Nova Pro base station's OLED (128x64, 1 bit), holds it, then
//! gives the display back. Frame format as documented by ggoled: a 1024-byte feature report
//! `06 93 x y w h` followed by the pixels column by column, least significant bit first, in strips
//! of at most 64 columns; `06 95` returns to the station's own UI.
//! cargo run --example oled_probe -- [seconds]
use std::time::Duration;

const W: usize = 128;
const H: usize = 64;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(4);
    let api = hidapi::HidApi::new()?;
    let info = api
        .device_list()
        .find(|d| d.vendor_id() == 0x1038 && [0x12E0, 0x12E5, 0x225D].contains(&d.product_id()) && d.path().to_string_lossy().to_ascii_lowercase().contains("mi_04&col01"))
        .ok_or("no Nova Pro Wireless base station found")?;
    let device = info.open_device(&api)?;

    // A frame, a cross through it and a filled block: enough to see orientation and bit order.
    let mut pixels = [[false; W]; H];
    for y in 0..H {
        for x in 0..W {
            pixels[y][x] = x == 0 || y == 0 || x == W - 1 || y == H - 1 || x == y * 2 || x == (H - 1 - y) * 2 || (x < 24 && y < 12);
        }
    }
    for strip in 0..W.div_ceil(64) {
        let (x0, w) = (strip * 64, 64.min(W - strip * 64));
        let mut report = [0u8; 1024];
        report[..6].copy_from_slice(&[0x06, 0x93, x0 as u8, 0, w as u8, H as u8]);
        for x in 0..w {
            for y in 0..H {
                let bit = x * H + y;
                report[6 + bit / 8] |= (pixels[y][x0 + x] as u8) << (bit % 8);
            }
        }
        device.send_feature_report(&report)?;
    }
    println!("pattern sent: a border, a cross, a filled block top left. Holding {seconds} s.");
    std::thread::sleep(Duration::from_secs(seconds));
    let mut back = [0u8; 64];
    (back[0], back[1]) = (0x06, 0x95);
    device.write(&back)?;
    println!("display handed back");
    Ok(())
}
