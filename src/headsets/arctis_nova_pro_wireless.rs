//! SteelSeries Arctis Nova Pro Wireless. The USB device is the base station, which stays plugged
//! in and keeps its audio output listed while the headset is off, so `Online` is what tells the
//! engine to move to the next output.
//!
//! Byte facts as documented by HeadsetControl, Linux-Arctis-Manager and Arctis Sound Manager;
//! status reply and command acceptance checked on hardware with `examples/headset_probe.rs`.
//! battery, mic mute and the dial confirmed on hardware too. The dial's events arrive on the
//! second collection of interface 4, not where the answers do.

use crate::headset::{Decode, Field, Model, Report};
use std::time::Duration;

pub static MODEL: Model = Model {
    name: "Arctis Nova Pro Wireless",
    vendor: 0x1038,
    products: &[0x12E0, 0x12E5, 0x225D],
    interface: "mi_04&col01",
    event_interfaces: &["mi_04&col02"],
    output_name: "Arctis Nova Pro",
    report_length: 64,
    status_request: &[0x06, 0xB0],
    reports: &[
        Report {
            prefix: &[0x06, 0xB0],
            fields: &[
                (Field::Battery, 6, Decode::Steps(8)),
                (Field::MicMuted, 9, Decode::Is(0x01)),
                (Field::Online, 15, Decode::IsNot(0x01)), // 01 offline, 02 charging by cable, 08 online
            ],
        },
        Report { prefix: &[0x07, 0x45], fields: &[(Field::DialGame, 2, Decode::Percent), (Field::DialChat, 3, Decode::Percent)] },
    ],
    // "Sonar is here", then software ChatMix. Without them the dial says nothing on a PC.
    dial_on: &[&[0x06, 0x8D, 0x01], &[0x06, 0x49, 0x01]],
    dial_off: &[&[0x06, 0x49, 0x00], &[0x06, 0x8D, 0x00]],
    pace: Duration::from_millis(50),
};

#[cfg(test)]
mod tests {
    use super::MODEL;
    use crate::headset::{decode, State};

    #[test]
    fn reads_a_status_reply_captured_from_a_real_base_station() {
        // Headset switched off, a full battery in the charge slot.
        let off = [0x06, 0xB0, 0, 0, 1, 0, 0x00, 0x08, 4, 0, 0, 0x0A, 5, 0, 0x04, 0x01];
        let mut state = State::default();
        assert!(decode(&MODEL, &off, &mut state));
        assert_eq!((state.online, state.battery, state.mic_muted), (Some(false), Some(0.0), Some(false)));

        let mut on = off;
        (on[6], on[9], on[15]) = (6, 1, 0x08);
        assert!(decode(&MODEL, &on, &mut state));
        assert_eq!((state.online, state.battery, state.mic_muted), (Some(true), Some(0.75), Some(true)));
    }

    #[test]
    fn reads_the_dial_and_ignores_the_rest() {
        let mut state = State::default();
        // Captured while the dial was turned towards chat, back to the middle, then towards game.
        assert!(decode(&MODEL, &[0x07, 0x45, 0x5B, 0x64, 0, 0], &mut state));
        assert_eq!(state.dial, Some((0.91, 1.0)));
        assert!(decode(&MODEL, &[0x07, 0x45, 0x64, 0x64, 0, 0], &mut state));
        assert_eq!(state.dial, Some((1.0, 1.0)));
        assert!(decode(&MODEL, &[0x07, 0x45, 0x64, 0x53, 0, 0], &mut state));
        assert_eq!(state.dial, Some((1.0, 0.83)));
        assert!(!decode(&MODEL, &[0x07, 0x25, 30], &mut state), "the volume knob is not something we track");
        assert_eq!(state.dial, Some((1.0, 0.83)));
    }
}
