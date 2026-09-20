//! Does Windows' per-process loopback capture contain the spatial renderer's output?
//!
//! Decides how Game / Chat / Media channels can be built. This process plays a tone into the
//! back-left channel of a 7.1 stream on a spatial-sound endpoint (as examples/probe.rs does) and
//! measures it two ways at once: the endpoint's loopback, which is known to carry the HRTF render,
//! and the loopback of this process alone. Same numbers = per-app capture is post-spatial, and
//! each channel can have its own EQ. Different = per-app capture is the app's raw stream.
//!
//! cargo run --release --example process_probe -- <spatial device#>

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BACK_LEFT: usize = 4; // FL FR FC LFE BL BR SL SR
const RATE: usize = 48_000;

/// Single-bin DFT of a stereo stream at one frequency.
#[derive(Default)]
struct Meter {
    w: f64,
    n: u64,
    sums: [f64; 4], // re_l, im_l, re_r, im_r
    on: bool,
}

impl Meter {
    fn push(&mut self, l: f32, r: f32) {
        if !self.on {
            return;
        }
        let (sin, cos) = (self.w * self.n as f64).sin_cos();
        self.sums[0] += l as f64 * cos;
        self.sums[1] -= l as f64 * sin;
        self.sums[2] += r as f64 * cos;
        self.sums[3] -= r as f64 * sin;
        self.n += 1;
    }

    fn start(&mut self, freq: f64) {
        *self = Meter { w: std::f64::consts::TAU * freq / RATE as f64, on: true, ..Meter::default() };
    }

    /// (L dBFS, L-R gap dB, L-R phase deg)
    fn read(&mut self) -> (f64, f64, f64) {
        self.on = false;
        let s = self.sums;
        let (l, r) = (s[0].hypot(s[1]), s[2].hypot(s[3]));
        let phase = (s[1].atan2(s[0]) - s[3].atan2(s[2])).to_degrees().rem_euclid(360.0);
        (20.0 * (2.0 * l / self.n.max(1) as f64).max(1e-9).log10(), 20.0 * (l / r.max(1e-12)).log10(), phase)
    }
}

/// Child mode: play `freq` into back-left of a 7.1 stream until killed. The tone comes from another
/// process because that is the real use (capturing a game), and a process may not hear itself.
fn play_forever(device: &cpal::Device, freq: f64, channels: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mix = device.default_output_config()?;
    let w = std::f64::consts::TAU * freq / RATE as f64;
    let mut n = 0u64;
    let tone = device.build_output_stream(
        cpal::StreamConfig { channels: channels as u16, ..mix.config() },
        move |data: &mut [f32], _: &_| {
            for frame in data.chunks_exact_mut(channels) {
                frame.fill(0.0);
                // 7.1: back-left only. Stereo control: left only.
                frame[if channels == 8 { BACK_LEFT } else { 0 }] = 0.1 * (w * n as f64).sin() as f32;
                n += 1;
            }
        },
        |e| eprintln!("playback error: {e}"),
        None,
    )?;
    tone.play()?;
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// Mutes (or unmutes) every audio session of `pid` on the default render endpoint: what the
/// Windows Volume Mixer does when you mute one app.
fn mute_sessions_of(pid: u32, mute: bool) -> Result<usize, Box<dyn std::error::Error>> {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{eConsole, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let manager: IAudioSessionManager2 = devices.GetDefaultAudioEndpoint(eRender, eConsole)?.Activate(CLSCTX_ALL, None)?;
        let sessions = manager.GetSessionEnumerator()?;
        let mut touched = 0;
        for i in 0..sessions.GetCount()? {
            let session: IAudioSessionControl2 = sessions.GetSession(i)?.cast()?;
            if session.GetProcessId()? == pid {
                session.cast::<ISimpleAudioVolume>()?.SetMute(mute, std::ptr::null())?;
                touched += 1;
            }
        }
        Ok(touched)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let index: usize = std::env::args().nth(1).ok_or("usage: process_probe <spatial device#>")?.parse()?;
    let device = cpal::default_host().output_devices()?.nth(index).ok_or("no such device")?;
    if let Some(freq) = std::env::args().nth(2) {
        let channels = std::env::args().nth(3).map_or(Ok(8), |c| c.parse())?;
        return play_forever(&device, freq.parse()?, channels);
    }
    let mix = device.default_output_config()?;
    if mix.sample_rate() as usize != RATE {
        return Err("this probe assumes a 48 kHz endpoint".into());
    }
    println!("{}", device.description()?.name());

    // 1. The endpoint's loopback (what the engine captures today).
    let endpoint = Arc::new(Mutex::new(Meter::default()));
    let (sink, in_ch) = (endpoint.clone(), mix.channels() as usize);
    let endpoint_stream = device.build_input_stream(
        mix.config(),
        move |data: &[f32], _: &_| {
            let mut m = sink.lock().unwrap();
            for f in data.chunks_exact(in_ch) {
                m.push(f[0], f[1]);
            }
        },
        |_| {},
        None,
    )?;
    endpoint_stream.play()?;

    // 2. The loopback of one other process, captured on its own thread (its own COM apartment).
    //    Returns the bytes it saw, so "captured silence" and "captured nothing" can be told apart.
    let capture_process = |pid: u32, meter: Arc<Mutex<Meter>>, stop: Arc<std::sync::atomic::AtomicBool>| {
        std::thread::spawn(move || -> Result<usize, String> {
            let run = || -> Result<usize, Box<dyn std::error::Error>> {
                wasapi::initialize_mta().ok()?;
                let format = wasapi::WaveFormat::new(32, 32, &wasapi::SampleType::Float, RATE, 2, None);
                let mut client = wasapi::AudioClient::new_application_loopback_client(pid, true)?;
                client.initialize_client(&format, &wasapi::Direction::Capture, &wasapi::StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 })?;
                let event = client.set_get_eventhandle()?;
                let capture = client.get_audiocaptureclient()?;
                client.start_stream()?;
                let (mut bytes, mut total) = (VecDeque::<u8>::new(), 0usize);
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let _ = event.wait_for_event(100);
                    while capture.get_next_packet_size()?.unwrap_or(0) > 0 {
                        capture.read_from_device_to_deque(&mut bytes)?;
                    }
                    total += bytes.len();
                    let mut m = meter.lock().unwrap();
                    while bytes.len() >= 8 {
                        let mut frame = [0u8; 8];
                        for b in frame.iter_mut() {
                            *b = bytes.pop_front().unwrap();
                        }
                        m.push(f32::from_le_bytes(frame[..4].try_into().unwrap()), f32::from_le_bytes(frame[4..].try_into().unwrap()));
                    }
                }
                Ok(total)
            };
            run().map_err(|e| e.to_string())
        })
    };
    let process = Arc::new(Mutex::new(Meter::default()));

    println!("{:>8}  {:^36}  {:^36}", "", "endpoint loopback", "that process only");
    // The last row mutes the player's session first, the way the Volume Mixer would.
    for (freq, channels, muted) in [(1000.0, 2, false), (250.0, 8, false), (1000.0, 8, false), (4000.0, 8, false), (1000.0, 2, true)] {
        let mut player = std::process::Command::new(std::env::current_exe()?).args([index.to_string(), freq.to_string(), channels.to_string()]).spawn()?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let capturing = capture_process(player.id(), process.clone(), stop.clone());
        std::thread::sleep(Duration::from_millis(1500)); // let the child start and the tone settle
        if muted {
            println!("         muting the player's session on the endpoint ({} session found)", mute_sessions_of(player.id(), true)?);
            std::thread::sleep(Duration::from_millis(300));
        }
        endpoint.lock().unwrap().start(freq);
        process.lock().unwrap().start(freq);
        std::thread::sleep(Duration::from_millis(1500));
        let (e, p) = (endpoint.lock().unwrap().read(), process.lock().unwrap().read());
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let captured = capturing.join().map_err(|_| "capture thread panicked")?;
        player.kill()?;
        player.wait()?;
        let row = |(l, gap, phase): (f64, f64, f64)| format!("L {l:6.1} dBFS  gap {gap:5.1} dB  {phase:5.1} deg");
        println!("{freq:>6} Hz {}  {}  {}   [{}]", if channels == 8 { "7.1" } else { "2.0" }, row(e), row(p), match captured { Ok(n) => format!("{} KB captured", n / 1024), Err(e) => format!("capture failed: {e}") });
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}
