//! Which device Windows plays to, and moving that.
//!
//! While the engine runs, Windows plays into our virtual device. The moment the engine is gone
//! (Stop, Quit, a crash, the installer closing us) that device is a dead end and the PC is silent
//! until somebody finds the setting. So whenever the engine goes away, the default goes back to
//! the output the user was listening on, and comes back to us when the engine does.
//!
//! Reading the default is documented API. Setting it is not: `IPolicyConfig` is what the Sound
//! control panel itself uses, unchanged since Windows 7 apart from two early Windows 10 builds.
//! GUIDs and method order as used by EarTrumpet (`Interop/MMDeviceAPI/IPolicyConfig.cs`).

use std::ffi::c_void;
use windows::core::{Interface, GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Media::Audio::{eCommunications, eConsole, eMultimedia, eRender, ERole, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

/// What a default can be the default for. Windows keeps three and its own Settings page moves them together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Role {
    Console,
    Multimedia,
    Communications,
}

pub const ROLES: [Role; 3] = [Role::Console, Role::Multimedia, Role::Communications];

/// Device ids are `DeviceInfo::id`s.
pub trait DefaultOutput {
    fn get(&self, role: Role) -> Option<String>;
    fn set(&self, role: Role, device: &str) -> Result<(), String>;
}

/// The engine is going away: every role that points at one of `ours` moves to `fallback`.
/// Returns the roles that were moved, which is what `take_back` needs later.
pub fn hand_back(defaults: &dyn DefaultOutput, ours: &[String], fallback: &str) -> Vec<Role> {
    // Decide first, move second: Windows drags Multimedia along when Console moves, and a role
    // that has already followed would no longer look like one of ours.
    let mine: Vec<Role> = ROLES.into_iter().filter(|&role| defaults.get(role).is_some_and(|now| ours.contains(&now))).collect();
    mine.into_iter().filter(|&role| defaults.set(role, fallback).is_ok()).collect()
}

/// The engine is back: the roles we moved away point at `main` again, unless the user has moved
/// them somewhere else in the meantime (then that is what they want).
pub fn take_back(defaults: &dyn DefaultOutput, moved: &[Role], fallback: &str, main: &str) -> Vec<Role> {
    let still_there: Vec<Role> = moved.iter().copied().filter(|&role| defaults.get(role).as_deref() == Some(fallback)).collect();
    still_there.into_iter().filter(|&role| defaults.set(role, main).is_ok()).collect()
}

pub struct WindowsDefaults;

const DEVICE_ID_PREFIX: &str = "wasapi:"; // how cpal spells a device id; Windows' own id follows
const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
/// IUnknown (3), eight methods about formats and share modes, GetPropertyValue, SetPropertyValue.
const SET_DEFAULT_ENDPOINT: usize = 13;
type SetDefaultFn = unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT;

fn erole(role: Role) -> ERole {
    match role {
        Role::Console => eConsole,
        Role::Multimedia => eMultimedia,
        Role::Communications => eCommunications,
    }
}

impl DefaultOutput for WindowsDefaults {
    fn get(&self, role: Role) -> Option<String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let id = devices.GetDefaultAudioEndpoint(eRender, erole(role)).ok()?.GetId().ok()?;
            let text = id.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as *const c_void));
            text.map(|id| format!("{DEVICE_ID_PREFIX}{id}"))
        }
    }

    fn set(&self, role: Role, device: &str) -> Result<(), String> {
        let id = HSTRING::from(device.strip_prefix(DEVICE_ID_PREFIX).unwrap_or(device));
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let policy: windows::core::IUnknown = CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL).map_err(|e| e.to_string())?;
            let mut raw: *mut c_void = std::ptr::null_mut();
            policy.query(&IID_POLICY_CONFIG, &mut raw).ok().map_err(|e| format!("this version of Windows has no IPolicyConfig: {e}"))?;
            let config = windows::core::IUnknown::from_raw(raw); // released on drop
            let vtable = *(config.as_raw() as *const *const *const c_void);
            let set_default: SetDefaultFn = std::mem::transmute(*vtable.add(SET_DEFAULT_ENDPOINT));
            set_default(config.as_raw(), PCWSTR(id.as_ptr()), erole(role)).ok().map_err(|e| e.to_string())
        }
    }
}

/// Blocks until that process has exited. One that can't be opened has already gone.
pub fn wait_for_exit(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE};
    unsafe {
        if let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(process, INFINITE);
            let _ = CloseHandle(process);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct Fake(RefCell<HashMap<&'static str, String>>);
    fn key(role: Role) -> &'static str {
        match role {
            Role::Console => "console",
            Role::Multimedia => "multimedia",
            Role::Communications => "communications",
        }
    }
    impl DefaultOutput for Fake {
        fn get(&self, role: Role) -> Option<String> {
            self.0.borrow().get(key(role)).cloned()
        }
        fn set(&self, role: Role, device: &str) -> Result<(), String> {
            self.0.borrow_mut().insert(key(role), device.into());
            if role == Role::Console {
                self.0.borrow_mut().insert("multimedia", device.into()); // as seen on Windows 11: the two move together
            }
            Ok(())
        }
    }

    #[test]
    fn hands_back_only_what_points_at_us_and_takes_back_only_what_is_still_there() {
        let ours = ["virtual".to_string(), "virtual-media".to_string()];
        let windows = Fake(RefCell::new(HashMap::from([("console", "virtual".into()), ("multimedia", "virtual".into()), ("communications", "usb-mic-speaker".into())])));

        let moved = hand_back(&windows, &ours, "headset");
        assert_eq!(moved, [Role::Console, Role::Multimedia], "the user's own choice for calls is none of our business");
        assert_eq!(windows.get(Role::Console).as_deref(), Some("headset"));
        assert_eq!(hand_back(&windows, &ours, "headset"), [], "nothing left to hand back");

        // While we were away the user pointed everything at the monitor: that stays.
        windows.set(Role::Console, "monitor").unwrap();
        assert_eq!(take_back(&windows, &moved, "headset", "virtual"), []);
        assert_eq!(windows.get(Role::Console).as_deref(), Some("monitor"));

        // Left alone, it all comes back.
        windows.set(Role::Console, "headset").unwrap();
        assert_eq!(take_back(&windows, &moved, "headset", "virtual"), [Role::Console, Role::Multimedia]);
        assert_eq!((windows.get(Role::Console).as_deref(), windows.get(Role::Multimedia).as_deref()), (Some("virtual"), Some("virtual")));
    }
}
