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

## Second pass (2026-10-11)

Checked against primary sources where they could be read; the rest is marked.

**What this PC has.** No VB-CABLE. SteelSeries GG is installed and left a signed "SteelSeries
Sonar Virtual Audio Device" (15.7.9.759) with a "Sonar - Microphone" capture endpoint, not
present while GG is off: proprietary, not ours to use. The "Microphone (FxSound Audio
Enhancer)" capture endpoints in the registry are state 4, not present: stale entries, and the
installed fxvad INF still has `;AddInterface=%KSCATEGORY_CAPTURE%` commented out. Dead end
confirmed. Mics: Arctis Nova Pro Wireless (48 kHz), Realtek array, two webcams.

**VB-CABLE, from its reference manual (rev. 3, October 2024) and licence page.**
- Latency is a pipe of `Max Latency` samples, default 7168 at 48 kHz (150 ms of memory). The
  manual's rule: `Max Latency = 3 x the biggest buffer any client uses`; WASAPI clients at 10 ms
  periods use 480-sample buffers, so 1536 is the floor and 2048 the safe setting. The delay
  through the cable is then a few of those buffers, not the whole pipe. Actual ms: still
  unverified, needs the cable installed and `examples/latency.rs`-style measurement.
- Set the cable's Internal SR to 48000 so nothing is resampled inside it.
- Licence: bundling is allowed as long as the user can see it is VB-Audio's, is told it comes
  from www.vb-cable.com, sees "VB-CABLE is a donationware, all participations are welcome" and
  can donate. A+B and C+D may not be bundled. No written permission is required by the page;
  asking anyway costs one e-mail. Package: VBCABLE_Driver_Pack45, XP to Windows 11 incl. Arm64.
- Signing: the page says nothing. Check the .cat in the driver store after install
  (`Get-AuthenticodeSignature`): Microsoft Windows Hardware Compatibility Publisher means
  attestation or WHQL, both load under Secure Boot.

**Noise suppressors, in Rust, no C toolchain.**
| | nnnoiseless (RNNoise) | deep_filter (DeepFilterNet3) |
|---|---|---|
| Version | 0.5.2 (2026-09-06), BSD-3 | 0.2.5, MIT/Apache-2 |
| Frame | 480 samples = 10 ms at 48 kHz, `process_frame(out, in) -> vad` in 16-bit units | 20 ms windows, 10 ms hop |
| Added latency | 10 ms (one frame); first output frame fades in, discard it | **40 ms** (paper: 2 frames look-ahead) |
| CPU | tiny (measured below) | real-time factor 0.19 single-threaded on an i5-8250U, i.e. ~2 ms per 10 ms |
| Quality | the 2018 model; fine on steady noise, weaker on keyboards | state of the art |

Start with RNNoise: 10 ms, trivially cheap, and the model can be retrained on keyboard
recordings if it falls short. DeepFilterNet is the upgrade if the keyboard test fails, at the
price of 30 ms more on the voice path.

**What Windows already does.** Voice Clarity (Windows 11 24H2+, this PC runs 25H2) is an
AI echo/noise/reverb stage, but only for streams in the Communications signal-processing
mode (Teams, Phone Link, WhatsApp). Discord runs its own Krisp; most games' voice chat runs
nothing. Our stage sits in front of all of them, so a game lobby gets what Discord users get.

**Discord opens the mic raw** (skips SFX/MFX): irrelevant on route A, since the cable's output
is the mic Discord sees and our processing already happened.

**The headset.** The Nova Pro's ClearCast Gen 2 is a bidirectional capsule (marketing: 25 dB of
noise rejection by pickup pattern); the "AI noise cancellation" is Sonar software on the PC, so
with GG off the mic is plain. That is the gap we fill.

**Spike, step 1 (no cable needed).** `examples/micspike.rs` records a mic for ten seconds,
runs RNNoise over it offline, reports the model's cost per frame and the noise floor between
words before and after, and writes `mic-raw.wav` / `mic-denoised.wav` to listen to.

**Step 2 (needs VB-CABLE installed by the user, admin).** A mic session in the engine: capture
the chosen mic, RNNoise, mic EQ, render into CABLE Input; apps pick CABLE Output. Measure the
delay mic -> CABLE Output with a click, Sonar-style sidetone is out of scope (the base station
does sidetone in hardware). Settings: which mic, suppression on/off, a meter.

**Step 1 result (2026-10-11, this PC).** RNNoise costs 0.03-0.04 ms per 10 ms frame (worst 0.28 ms):
nothing. Noise floor between words: Arctis mic -66.6 -> -94.7 dBFS (nobody spoke; the headset's
own floor), Realtek array with room sound -49.4 -> -102.7 dBFS; voice level dropped 7-11 dB
overall, which is the room between words going away. Whether a keyboard goes with it: listen to
`target/mic-spike/*-denoised.wav` next to `*-raw.wav`. Step 2 waits on VB-CABLE.

**Step 2, first run (2026-10-11, VB-CABLE installed, not yet rebooted; its endpoints were live anyway).**
`src/mic.rs` is the mic session: one thread paced by the cable input's render event, draining
the mic (or an output's loopback, for measuring), RNNoise per 480-sample frame, then the same
`Feed` cushion as the output engine into the cable. `freeseries mics`, `freeseries mic <mic#|o<out#>> <cable#> [s] [raw]`.
`examples/miclatency.rs` plays a 1 ms click into an output and listens on a capture endpoint, both
on the performance counter (play time from `IAudioClock`, arrival from the capture packet's QPC).

| Path | Median |
|---|---|
| CABLE Input -> CABLE Output, nothing of ours | **57 ms** |
| monitor loopback -> mic session (raw) -> cable | 106 ms |
| the same with RNNoise | 113 ms |

The cable itself is 57 ms at its default `VBAudioCableWDM_Latency = 7168` (in
`HKLM\SOFTWARE\VB-Audio\Cable`, read at driver start; the control panel changes it live). The
manual's rule for 480-frame WASAPI clients gives 1536 minimum, 2048 safe: expect the cable to drop
to ~15 ms. Our session adds ~49 ms (loopback capture, 12 ms cushion, two periods ahead, RNNoise's
frame alignment, 7 ms measured). A real mic replaces the loopback with its USB delay.
The cable's .cat is signed by Microsoft Windows Hardware Compatibility Publisher: loads under
Secure Boot. `VBAudioCableWDM_SR` is already 48000.
