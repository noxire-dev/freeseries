//! Mic spike: records a microphone for a few seconds, runs RNNoise (`nnnoiseless`) over it, and
//! reports what that costs and what it removes. Writes `mic-raw.wav` and `mic-denoised.wav` next
//! to the working directory so a person can listen to both. Nothing is played.
//!
//! cargo run --release --example micspike                 lists the microphones
//! cargo run --release --example micspike -- <mic#> [seconds]

#![allow(unsafe_op_in_unsafe_fn)]

use nnnoiseless::DenoiseState;
use std::time::Instant;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};

const FRAME: usize = DenoiseState::FRAME_SIZE; // 480 samples: 10 ms at 48 kHz

fn main() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let all: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let mics = all.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let name = |i: u32| -> String { mics.Item(i).and_then(|d| d.OpenPropertyStore(STGM_READ)).and_then(|s| s.GetValue(&PKEY_Device_FriendlyName)).map(|v| v.to_string()).unwrap_or_default() };
        let Some(index) = std::env::args().nth(1).and_then(|a| a.parse::<u32>().ok()) else {
            for i in 0..mics.GetCount()? {
                println!("{i}: {}", name(i));
            }
            return Ok(());
        };
        let seconds: f64 = std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(10.0);

        let device = mics.Item(index)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = client.GetMixFormat()?;
        let (rate, channels, bits) = ((*format).nSamplesPerSec as usize, (*format).nChannels as usize, (*format).wBitsPerSample);
        println!("{}: {rate} Hz, {channels} ch, {bits}-bit float mix format", name(index));
        if rate != 48000 || bits != 32 {
            return Err("this spike only handles a 48 kHz float mix format (RNNoise wants 48 kHz; the engine will resample)".into());
        }
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 2_000_000, 0, format, None)?;
        let reader: IAudioCaptureClient = client.GetService()?;
        client.Start()?;

        // Record mono (first channel), then process offline so timing is the model's alone.
        let mut raw: Vec<f32> = Vec::with_capacity(rate * seconds as usize);
        let started = Instant::now();
        while started.elapsed().as_secs_f64() < seconds {
            std::thread::sleep(std::time::Duration::from_millis(5));
            while reader.GetNextPacketSize()? > 0 {
                let (mut data, mut got, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                reader.GetBuffer(&mut data, &mut got, &mut flags, None, None)?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                let samples = std::slice::from_raw_parts(data as *const f32, got as usize * channels);
                raw.extend(samples.chunks_exact(channels).map(|f| if silent { 0.0 } else { f[0] }));
                reader.ReleaseBuffer(got)?;
            }
        }
        client.Stop()?;

        let mut state = DenoiseState::new();
        let (mut denoised, mut vad, mut cost) = (Vec::with_capacity(raw.len()), Vec::new(), Vec::new());
        let mut out = [0f32; FRAME];
        for frame in raw.chunks_exact(FRAME) {
            // RNNoise works in 16-bit units.
            let input: Vec<f32> = frame.iter().map(|s| s * 32767.0).collect();
            let t = Instant::now();
            vad.push(state.process_frame(&mut out, &input));
            cost.push(t.elapsed().as_secs_f64() * 1000.0);
            denoised.extend(out.iter().map(|s| s / 32767.0));
        }
        // The first frame fades in; everything after it is comparable.
        let (raw, denoised) = (&raw[FRAME..denoised.len()], &denoised[FRAME..]);

        let db = |x: f32| 20.0 * x.max(1e-9).log10();
        let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt();
        let floor = |s: &[f32]| {
            let mut levels: Vec<f32> = s.chunks_exact(FRAME).map(rms).collect();
            levels.sort_by(f32::total_cmp);
            rms(&levels[..levels.len() / 5]) // the quietest fifth of the frames: what is left between words
        };
        let mean = cost.iter().sum::<f64>() / cost.len() as f64;
        let worst = cost.iter().cloned().fold(0.0, f64::max);
        println!("{:.1} s recorded, {} frames of 10 ms", raw.len() as f64 / rate as f64, cost.len());
        println!("RNNoise cost: {mean:.3} ms per 10 ms frame on average, {worst:.2} ms worst (real-time factor {:.3})", mean / 10.0);
        println!("level overall:  raw {:.1} dBFS -> denoised {:.1} dBFS", db(rms(raw)), db(rms(denoised)));
        println!("quiet frames:   raw {:.1} dBFS -> denoised {:.1} dBFS  (the noise floor between words)", db(floor(raw)), db(floor(denoised)));
        println!("voice detected in {:.0}% of frames", 100.0 * vad.iter().filter(|&&p| p > 0.5).count() as f64 / vad.len() as f64);
        write_wav("mic-raw.wav", raw, rate as u32)?;
        write_wav("mic-denoised.wav", denoised, rate as u32)?;
        println!("wrote mic-raw.wav and mic-denoised.wav");
    }
    Ok(())
}

fn write_wav(path: &str, samples: &[f32], rate: u32) -> std::io::Result<()> {
    let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut bytes = Vec::with_capacity(44 + data.len());
    bytes.extend(b"RIFF");
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes()); // PCM
    bytes.extend(1u16.to_le_bytes()); // mono
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    std::fs::write(path, bytes)
}
