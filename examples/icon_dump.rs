//! Writes an executable's icon as a BMP, to eyeball what `routing::icon` returns.
//! cargo run --release --example icon_dump -- <exe path> <out.bmp>
use freeseries::routing::{AppRouting, WindowsRouting};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let icon = WindowsRouting.icon(&args[1]).ok_or("no icon")?;
    let (w, h) = (icon.width, icon.height);
    let mut bmp = Vec::new();
    bmp.extend(b"BM");
    bmp.extend((54 + w * h * 4).to_le_bytes());
    bmp.extend([0u8; 4]);
    bmp.extend(54u32.to_le_bytes());
    bmp.extend(40u32.to_le_bytes());
    bmp.extend((w as i32).to_le_bytes());
    bmp.extend((-(h as i32)).to_le_bytes()); // top-down, like the data
    bmp.extend(1u16.to_le_bytes());
    bmp.extend(32u16.to_le_bytes());
    bmp.extend([0u8; 24]);
    // Composite on the app's lane colour so transparency shows up as it will in the UI.
    for p in icon.rgba.chunks_exact(4) {
        let a = p[3] as u32;
        let over = |c: u8, bg: u32| ((c as u32 * a + bg * (255 - a)) / 255) as u8;
        bmp.extend([over(p[2], 0x2c), over(p[1], 0x2c), over(p[0], 0x2c), 255]);
    }
    std::fs::write(&args[2], bmp)?;
    Ok(())
}
