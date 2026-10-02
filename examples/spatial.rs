//! Which endpoints have spatial sound (Atmos, DTS, Sonic) switched on, and how big a loopback
//! capture buffer each gives. Read-only: nothing is played and no stream is started.
//!
//! Spatial sound belongs on the FreeSeries devices only. On the real output as well, everything
//! is spatialised twice, which sounds hollow and phasey.
//!
//! cargo run --release --example spatial

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let all: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoints = all.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..endpoints.GetCount()? {
            let d = endpoints.Item(i)?;
            let name = d.OpenPropertyStore(STGM_READ).and_then(|s| s.GetValue(&PKEY_Device_FriendlyName)).map(|v| v.to_string()).unwrap_or_default();
            // A spatial renderer offers dynamic objects; with spatial sound off there are none. (The static
            // bed mask says nothing: it reads the same on every endpoint here, on or off.)
            let objects = d.Activate::<ISpatialAudioClient>(CLSCTX_ALL, None).and_then(|c| c.GetMaxDynamicObjectCount());
            let loopback = d.Activate::<IAudioClient>(CLSCTX_ALL, None).and_then(|c| {
                let format = c.GetMixFormat()?;
                c.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 0, 0, format, None)?;
                Ok((c.GetBufferSize()?, (*format).nSamplesPerSec))
            });
            println!("{name}");
            println!("    spatial sound: {}  ({objects:?} dynamic objects)", if matches!(objects, Ok(n) if n > 0) { "ON" } else { "off" });
            match loopback {
                Ok((frames, rate)) => println!("    default loopback buffer: {frames} frames = {} ms", frames * 1000 / rate),
                Err(e) => println!("    loopback: {e}"),
            }
        }
    }
    Ok(())
}
