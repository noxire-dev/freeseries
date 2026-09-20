//! Which app plays into which device: the per-app "Output device" setting of the Windows volume
//! mixer, driven from code. This is how an app gets into the Chat or Media channel.
//!
//! Windows has no documented API for it. `Windows.Media.Internal.AudioPolicyConfig` is the
//! interface the Settings app itself uses; EarTrumpet and SoundSwitch have relied on it for years.
//! Layout taken from EarTrumpet (`IAudioPolicyConfigFactoryVariantFor21H2`): IInspectable, 19
//! methods we don't use, then Set / Get / ClearAll. Windows 10 before 21H2 has the same layout
//! under a different IID. Windows remembers a pin per app, across restarts, by itself.

use std::ffi::c_void;
use windows::core::{Interface, GUID, HRESULT, HSTRING, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, AudioSessionStateExpired, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
    DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioApp {
    pub pid: u32,
    /// The executable's name without ".exe": "Discord", "chrome".
    pub name: String,
    pub path: String,
    /// The device this app is pinned to, as a `DeviceInfo::id`. `None`: it follows the Windows default.
    pub device: Option<String>,
}

pub trait AppRouting: Send + Sync {
    /// Every app that has an audio session on a playback device right now.
    fn apps(&self) -> Result<Vec<AudioApp>, String>;
    /// Pins an app to a device (`DeviceInfo::id`), or with `None` lets it follow the default again.
    fn assign(&self, pid: u32, device: Option<&str>) -> Result<(), String>;
}

pub struct WindowsRouting;

const DEVICE_ID_PREFIX: &str = "wasapi:"; // how cpal spells a device id; Windows' own id follows
const MMDEVAPI: &str = r"\\?\SWD#MMDEVAPI#";
const RENDER_INTERFACE: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";
const POLICY_CLASS: &str = "Windows.Media.Internal.AudioPolicyConfig";
const IID_21H2: GUID = GUID::from_u128(0xab3d4648_e242_459f_b02f_541c70306324);
const IID_DOWNLEVEL: GUID = GUID::from_u128(0x2a59116d_6c4f_45e0_a74f_707e3fef9258);
/// IUnknown (3) + IInspectable (3) + 19 unused methods.
const SET_PERSISTED: usize = 25;
const GET_PERSISTED: usize = 26;

type SetFn = unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut c_void) -> HRESULT;
type GetFn = unsafe extern "system" fn(*mut c_void, u32, i32, i32, *mut *mut c_void) -> HRESULT;

/// The policy object, released on drop.
struct Policy(windows::core::IUnknown);

impl Policy {
    fn open() -> Result<Policy, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let class = HSTRING::from(POLICY_CLASS);
            for iid in [IID_21H2, IID_DOWNLEVEL] {
                let mut raw: *mut c_void = std::ptr::null_mut();
                let class_abi: *mut c_void = std::mem::transmute_copy(&class);
                if roget(class_abi, &iid, &mut raw).is_ok() && !raw.is_null() {
                    return Ok(Policy(windows::core::IUnknown::from_raw(raw)));
                }
            }
            Err("this version of Windows doesn't offer per-app audio routing".into())
        }
    }

    /// The function at `index` of the object's vtable. The caller names its real signature.
    unsafe fn method<F: Copy>(&self, index: usize) -> F {
        unsafe {
            let vtable = *(self.0.as_raw() as *const *const *const c_void);
            std::mem::transmute_copy(&*vtable.add(index))
        }
    }
}

/// `RoGetActivationFactory` with an IID chosen at run time (the generated binding fixes it at compile time).
unsafe fn roget(class: *mut c_void, iid: &GUID, out: *mut *mut c_void) -> windows::core::Result<()> {
    #[link(name = "combase", kind = "raw-dylib")] // no import library needed: this toolchain has no combase.lib
    unsafe extern "system" {
        #[link_name = "RoGetActivationFactory"]
        fn raw(class: *mut c_void, iid: *const GUID, factory: *mut *mut c_void) -> HRESULT;
    }
    unsafe { raw(class, iid, out).ok() }
}

fn executable_of(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let found = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        found.ok()?;
        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    }
}

impl AppRouting for WindowsRouting {
    fn apps(&self) -> Result<Vec<AudioApp>, String> {
        let policy = Policy::open()?;
        let mut apps: Vec<AudioApp> = Vec::new();
        let scan = || -> windows::core::Result<Vec<u32>> {
            unsafe {
                let mut pids = Vec::new();
                let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let endpoints = devices.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
                for i in 0..endpoints.GetCount()? {
                    let manager: IAudioSessionManager2 = endpoints.Item(i)?.Activate(CLSCTX_ALL, None)?;
                    let sessions = manager.GetSessionEnumerator()?;
                    for j in 0..sessions.GetCount()? {
                        let session: IAudioSessionControl2 = sessions.GetSession(j)?.cast()?;
                        if session.GetState()? != AudioSessionStateExpired {
                            pids.push(session.GetProcessId()?);
                        }
                    }
                }
                Ok(pids)
            }
        };
        let mut pids = scan().map_err(|e| e.to_string())?;
        pids.sort_unstable();
        pids.dedup();
        for pid in pids {
            // pid 0 is "System sounds"; our own engine plays the mix and must never be re-routed.
            if pid == 0 || pid == std::process::id() {
                continue;
            }
            let Some(path) = executable_of(pid) else { continue };
            let name = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            // Any freeseries (installed, dev build, CLI) plays the finished mix: moving it would break the chain.
            if name.to_lowercase().starts_with("freeseries") {
                continue;
            }
            apps.push(AudioApp { pid, name, device: pinned_device(&policy, pid), path });
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        Ok(apps)
    }

    fn assign(&self, pid: u32, device: Option<&str>) -> Result<(), String> {
        let policy = Policy::open()?;
        // An empty HSTRING (a null handle) clears the pin.
        let target = device.map(|id| HSTRING::from(format!("{MMDEVAPI}{}{RENDER_INTERFACE}", id.strip_prefix(DEVICE_ID_PREFIX).unwrap_or(id))));
        unsafe {
            let set: SetFn = policy.method(SET_PERSISTED);
            let handle: *mut c_void = target.as_ref().map_or(std::ptr::null_mut(), |h| std::mem::transmute_copy(h));
            for role in [eMultimedia, eConsole] {
                set(policy.0.as_raw(), pid, eRender.0, role.0, handle).ok().map_err(|e| format!("Windows refused to move that app: {e}"))?;
            }
        }
        Ok(())
    }
}

fn pinned_device(policy: &Policy, pid: u32) -> Option<String> {
    unsafe {
        let get: GetFn = policy.method(GET_PERSISTED);
        let mut handle: *mut c_void = std::ptr::null_mut();
        get(policy.0.as_raw(), pid, eRender.0, eMultimedia.0, &mut handle).ok().ok()?;
        let id: HSTRING = std::mem::transmute(handle); // takes ownership, frees on drop
        let id = id.to_string();
        let id = id.strip_prefix(MMDEVAPI).unwrap_or(&id);
        let id = id.strip_suffix(RENDER_INTERFACE).unwrap_or(id);
        (!id.is_empty()).then(|| format!("{DEVICE_ID_PREFIX}{id}"))
    }
}
