//! Is the SteelSeries Arctis Nova Pro Wireless headset switched on?
//!
//! Its USB audio endpoint belongs to the base station, so Windows keeps listing it while the
//! headset is off and audio just plays into nothing. The base station does know, and says so over
//! its HID command interface. Protocol as used by HeadsetControl and the zhelper fork of G-Helper.

use crate::engine::{DeviceInfo, Presence};
use hidapi::{HidApi, HidDevice};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const STEELSERIES: u16 = 0x1038;
const BASE_STATIONS: [u16; 2] = [0x12E0, 0x12E5]; // PC and Xbox editions
const COMMAND_INTERFACE: &str = "mi_04&col01";
const REPORT_ID: u8 = 0x06;
const REPORT_LENGTH: usize = 64;
const COMMAND_BATTERY: u8 = 0xB0;
const STATUS_OFFLINE: u8 = 0x01;
const REPLY_TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct ArctisNovaPro {
    /// Kept open between checks; dropped on any I/O error and reopened next time.
    device: Mutex<Option<HidDevice>>,
}

/// Battery reply: `[0]` report id, `[1]` echoed command, `[15]` headset status.
fn parse_online(reply: &[u8]) -> Option<bool> {
    (reply.len() >= 16 && reply[0] == REPORT_ID && reply[1] == COMMAND_BATTERY).then(|| reply[15] != STATUS_OFFLINE)
}

fn open() -> Option<HidDevice> {
    let api = HidApi::new().ok()?;
    let info = api.device_list().find(|d| {
        d.vendor_id() == STEELSERIES
            && BASE_STATIONS.contains(&d.product_id())
            && d.path().to_string_lossy().to_ascii_lowercase().contains(COMMAND_INTERFACE)
    })?;
    info.open_device(&api).ok()
}

impl ArctisNovaPro {
    /// `None` when there is no base station or it didn't answer.
    pub fn headset_online(&self) -> Option<bool> {
        let mut slot = self.device.lock().unwrap();
        if slot.is_none() {
            *slot = open();
        }
        let answer = slot.as_ref().and_then(|device| {
            let mut message = [0u8; REPORT_LENGTH];
            (message[0], message[1]) = (REPORT_ID, COMMAND_BATTERY);
            device.write(&message).ok()?;
            // The base station also reports knob and button events here; skip until ours is echoed.
            let deadline = Instant::now() + REPLY_TIMEOUT;
            let mut reply = [0u8; REPORT_LENGTH];
            while Instant::now() < deadline {
                let n = device.read_timeout(&mut reply, 100).ok()?;
                if let Some(online) = parse_online(&reply[..n]) {
                    return Some(online);
                }
            }
            None
        });
        if answer.is_none() {
            *slot = None; // unplugged, or the system resumed: start over next time
        }
        answer
    }
}

impl Presence for ArctisNovaPro {
    fn is_on(&self, output: &DeviceInfo) -> bool {
        if !output.name.contains("Arctis Nova Pro") {
            return true;
        }
        self.headset_online().unwrap_or(true) // can't tell: assume it's on rather than take audio away
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_headset_status_byte() {
        let mut reply = [0u8; 64];
        (reply[0], reply[1]) = (REPORT_ID, COMMAND_BATTERY);
        reply[15] = 0x08;
        assert_eq!(parse_online(&reply), Some(true));
        reply[15] = STATUS_OFFLINE;
        assert_eq!(parse_online(&reply), Some(false));
        reply[1] = 0x25; // a knob event, not our answer
        assert_eq!(parse_online(&reply), None);
        assert_eq!(parse_online(&reply[..10]), None);
    }
}
