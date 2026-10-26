//! The mic path's processing, offline: a 48 kHz 16-bit mono WAV goes through RNNoise at a given
//! strength, the way `mic.rs` does it, and out to another WAV, with what changed printed per
//! band. For judging the noise removal on a voice without a microphone in the loop.
//!
//! cargo run --release --example micfile -- in.wav out.wav [strength 0..1] [noise dBFS]
//!
//! `noise` adds white noise at that level first, so a clean recording stands in for a real room.

use nnnoiseless::DenoiseState;

const FRAME: usize = DenoiseState::FRAME_SIZE;
const DELAY: usize = 480;
const RATE: usize = 48000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (input, output) = (args.get(1).ok_or("in.wav")?, args.get(2).ok_or("out.wav")?);
    let strength: f32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1.0);
    let noise_db: Option<f32> = args.get(4).and_then(|a| a.parse().ok());
    let mut raw = read_wav(input)?;
    if let Some(db) = noise_db {
        let amp = 10f32.powf(db / 20.0);
        let mut seed = 0x2545F4914F6CDD1Du64;
        for s in raw.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *s += (seed as f32 / u64::MAX as f32 - 0.5) * 2.0 * amp * 1.73; // uniform, RMS = amp
        }
    }
    let mut state = DenoiseState::new();
    let mut out = Vec::with_capacity(raw.len());
    let mut clean = [0f32; FRAME];
    let mut dry = std::collections::VecDeque::from(vec![0f32; DELAY]);
    let mut voiced = 0usize;
    for frame in raw.chunks_exact(FRAME) {
        let scaled: Vec<f32> = frame.iter().map(|x| x * 32767.0).collect();
        if state.process_frame(&mut clean, &scaled) > 0.5 {
            voiced += 1;
        }
        for (c, &x) in clean.iter_mut().zip(frame) {
            dry.push_back(x);
            let r = dry.pop_front().unwrap();
            *c = r + strength * (*c / 32767.0 - r);
        }
        out.extend_from_slice(&clean);
    }
    // Line the two up (the output lags by DELAY) and skip the first frame, which fades in.
    let a = &raw[FRAME..out.len() - DELAY];
    let b = &out[FRAME + DELAY..];
    let db = |x: f32| 20.0 * x.max(1e-9).log10();
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt();
    println!("{:.1} s, strength {strength}, voice detected in {:.0}% of frames", a.len() as f32 / RATE as f32, 100.0 * voiced as f32 / (raw.len() / FRAME) as f32);
    println!("level: in {:.1} dBFS, out {:.1} dBFS", db(rms(a)), db(rms(b)));
    // Speech-only frames: the loudest third, where the voice is. What the bands lose there is what
    // the listener hears as muffled; what the quiet third loses is the noise removal working.
    let mut by_level: Vec<(f32, usize)> = a.chunks_exact(FRAME).enumerate().map(|(i, f)| (rms(f), i)).collect();
    by_level.sort_by(|x, y| y.0.total_cmp(&x.0));
    let loud: Vec<usize> = by_level[..by_level.len() / 3].iter().map(|x| x.1).collect();
    let quiet: Vec<usize> = by_level[by_level.len() * 2 / 3..].iter().map(|x| x.1).collect();
    println!("{:>12} {:>12} {:>12}", "band", "loud frames", "quiet frames");
    for (lo, hi) in [(80, 300), (300, 1000), (1000, 3000), (3000, 6000), (6000, 10000), (10000, 16000)] {
        let energy = |s: &[f32], frames: &[usize]| -> f32 {
            frames.iter().map(|&i| band_energy(&s[i * FRAME..(i + 1) * FRAME], lo, hi)).sum::<f32>() / frames.len() as f32
        };
        let (la, lb) = (energy(a, &loud), energy(b, &loud));
        let (qa, qb) = (energy(a, &quiet), energy(b, &quiet));
        println!("{:>5}-{:<6} {:>+9.1} dB {:>+9.1} dB", lo, hi, 10.0 * (lb / la.max(1e-12)).max(1e-9).log10(), 10.0 * (qb / qa.max(1e-12)).max(1e-9).log10());
    }
    write_wav(output, b)?;
    println!("wrote {output}");
    Ok(())
}

/// Energy of `s` between two frequencies: a plain DFT over the frame's bins, no window.
fn band_energy(s: &[f32], lo: usize, hi: usize) -> f32 {
    let n = s.len();
    let bin = |k: usize| -> f32 {
        let (mut re, mut im) = (0f32, 0f32);
        for (i, &x) in s.iter().enumerate() {
            let t = 2.0 * std::f32::consts::PI * k as f32 * i as f32 / n as f32;
            re += x * t.cos();
            im -= x * t.sin();
        }
        re * re + im * im
    };
    (lo * n / RATE..hi * n / RATE).map(bin).sum()
}

fn read_wav(path: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        if id == b"fmt " {
            let (ch, rate, bits) = (u16::from_le_bytes(bytes[at + 10..at + 12].try_into()?), u32::from_le_bytes(bytes[at + 12..at + 16].try_into()?), u16::from_le_bytes(bytes[at + 22..at + 24].try_into()?));
            if ch != 1 || rate as usize != RATE || bits != 16 {
                return Err(format!("need 48 kHz 16-bit mono, got {rate} Hz {bits}-bit {ch} ch").into());
            }
        } else if id == b"data" {
            return Ok(bytes[at + 8..at + 8 + size.min(bytes.len() - at - 8)].chunks_exact(2).map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0).collect());
        }
        at += 8 + size + (size & 1);
    }
    Err("no data chunk".into())
}

fn write_wav(path: &str, samples: &[f32]) -> std::io::Result<()> {
    let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut bytes = Vec::with_capacity(44 + data.len());
    bytes.extend(b"RIFF");
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend((RATE as u32).to_le_bytes());
    bytes.extend((RATE as u32 * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    std::fs::write(path, bytes)
}
