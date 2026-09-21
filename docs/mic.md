# Microphone processing: research notes (2026-09-21)

Goal: capture the real mic, process it in our user-mode engine (noise suppression, EQ, gate), and
have Discord and games see the result as a microphone. That needs either a virtual capture
endpoint or code that runs on the real mic's endpoint. Web research only; nothing installed or
run. Items marked unverified were not confirmed.

## Dead ends

- fxvad: all three signed INFs in FxSound's repo have the capture interface commented out
  (`;AddInterface=%KSCATEGORY_CAPTURE%`); the signed package can't be edited.
- VirtualDrivers/Virtual-Audio-Driver (MIT + MS-PL, speaker + mic): the "Signed" release is signed
  by SignPath Foundation under GlobalSign, not by Microsoft, so it does not load with Secure Boot;
  its README still asks for `testsigning on`, issue #15 (Code 52) is open. DeckX's driver looks
  the same (unverified).
- Microsoft SYSVAD / simpleaudiosample: source only, test certificate.
- Steam Streaming Microphone: its render side only passes audio during a Remote Play session
  (found in ClassicOldSong/Apollo PR 1428); silent otherwise. Proprietary.
- Scream (render only), AudioMirror, SAR, VAW: test signing. Voicemeeter, Muzychenko VAC, NVIDIA
  Broadcast, Wave Link: proprietary, usable only if the user already has them.

## Route A: VB-CABLE as the virtual mic

Our existing shape, pointed the other way: capture the real mic, process, play into "CABLE
Input"; apps pick "CABLE Output" as their microphone. All our code stays in user mode; if we
crash, the mic goes quiet and nothing else.

- Microsoft-signed, widely deployed (Secure Boot chain not inspected: unverified).
- Licence (vb-audio.com/Services/licensing.htm): may be bundled free of charge if the user can
  tell it is VB-Audio's, is told where it comes from (www.vb-cable.com), sees "VB-CABLE is a
  donationware, all participations are welcome" and can donate. Cables A+B and C+D may not be
  bundled. Ask VB-Audio for written confirmation before bundling; detecting an existing install
  needs no permission.
- Silent install `VBCABLE_Setup_x64.exe -i -h` as admin, uninstall `-u -h`, reboot advised.
- Buffer ceiling defaults to 7168 samples (~150 ms at 48 kHz), can be lowered to 512; real-world
  latency unverified.
- Closed source. Inside an AGPL installer it is mere aggregation.

## Route B: our own capture APO (no driver)

A user-mode COM DLL that audiodg loads on the real mic's endpoint, the way Equalizer APO does it:
take ownership of `HKLM\...\MMDevices\Audio\Capture\{endpoint}`, write our CLSID into
`FxProperties`, keep the vendor's APO to chain to. Admin once, no kernel code, no EV cert.
Discord, Teams and Zoom open the mic RAW, which skips SFX and MFX but not EFX, so it must
register as an endpoint effect. Latency is one algorithm frame (10 ms for RNNoise). The real mic
becomes the processed mic; no new device.

Costs: Equalizer APO sets `DisableProtectedAudioDG=1`, which weakens audiodg protection for the
whole machine (whether a merely OV-signed APO loads without it: unverified); driver and Windows
updates wipe `FxProperties`; exclusive-mode streams bypass it; our code would run real-time
inside audiodg, so a bug of ours takes down ALL audio; DeepFilterNet is heavy there. Equalizer
APO is GPL-2.0-or-later, reusable from AGPL-3.0.

## Position

Route A first. Route B puts our code inside the Windows audio service and lowers a machine-wide
protection, which is the opposite of what this project is for, and the first thing a reviewer
would flag. Route A reuses the engine we have and fails safe. Route B stays on file as the
no-extra-driver option if VB-CABLE's terms or latency turn out badly.

Experiment for route A: with VB-CABLE installed, capture the mic, run it through a noise
suppressor (`nnnoiseless`, the pure-Rust RNNoise port) and play into CABLE Input; measure the
added latency with `examples/latency.rs` and record Discord's mic test with a keyboard clattering.
