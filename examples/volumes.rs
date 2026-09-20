//! Prints every playback device's Windows volume. The engine plays into the real output at whatever
//! that device is set to, so a low value there caps how loud freeseries can get.
//! cargo run --example volumes
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eRender, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};

fn main() -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoints = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..endpoints.GetCount()? {
            let device = endpoints.Item(i)?;
            let name = device.OpenPropertyStore(STGM_READ)?.GetValue(&PKEY_Device_FriendlyName)?.to_string();
            let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            println!("{:>4.0}%  {:>6.1} dB  {}{}", volume.GetMasterVolumeLevelScalar()? * 100.0, volume.GetMasterVolumeLevel()?, name, if volume.GetMute()?.as_bool() { "  (muted)" } else { "" });
        }
    }
    Ok(())
}
