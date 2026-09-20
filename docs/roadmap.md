# Roadmap

Wishlist from the first day of real use (2026-09-20), in the order I'd build it. Nothing here is started.

## 1. Outputs that follow you (built 2026-09-20, headset-off switch untested on hardware)

One feature, three symptoms:

- **No virtual-device picker.** freeseries only works with its own virtual endpoint, so find the FxSound device by name and hide that choice. Keep the picker as a fallback when no such device exists (VB-Cable users).
- **Per-device EQ.** Each output remembers its own preset: PSense on the Arctis, something else on the monitor.
- **Auto-switch.** A priority list of outputs. Headset turns off -> audio moves to the monitor and its EQ; headset comes back -> audio moves back. The engine already notices a dead output (`Status::Reconnecting`); today it retries the same device forever instead of falling to the next one.

This is also the robustness story: a missing headset must never mean silent audio.

## 2. Setup wizard (built 2026-09-20, real install path untested)

`app/src-tauri/resources/setup.ps1` installs the vendored, Microsoft-signed fxvad package (one UAC prompt) and names the endpoint "freeseries". The app opens the wizard by itself when no virtual device exists, watches for the device becoming the Windows default, then hands over to Devices. Verified: the script on Windows PowerShell 5.1 (dry run and rename), the elevation command line (unit test), the whole flow against the preview's simulated fresh PC. **Not verified:** the elevated install from inside the app, because the only test machine already has the driver.

Found on the way: renaming an endpoint (`PKEY_Device_DeviceDesc`) needs **no** admin rights, so it never prompts. The "(FxSound Audio Enhancer)" suffix is the driver's interface name and stays.

Still open: a one-click "make it default" (needs the undocumented `IPolicyConfig` COM interface that EarTrumpet and SoundSwitch use; today the wizard deep-links to Sound settings and detects the result), and uninstall.

## 3. Editor and UI pass (first pass done 2026-09-20)

Done since: a live spectrum analyser behind the curve (`src/spectrum.rs`: a tap on the processed audio, an own radix-2 FFT run off the audio thread, 96 log bands; drawn on a canvas outside React). Its display range (-84 to -12 dBFS after a 4.5 dB/octave tilt) was tuned against synthetic data only and wants a look with real music.

The EQ editor needs love and the app reads as an open-source project. Do it as a proper design pass with the `impeccable` skill, not piecemeal.

## Done 2026-09-20 (second batch)

- **Layered presets.** Each output has a headphone correction (AutoEq, belongs to the device) under a sound preset (what the editor edits, what you switch). The engine gets the two concatenated; the editor draws the correction as a dashed reference line.
- **Starter sound presets** written for freeseries: Footsteps, Bass boost, Voice clarity, Late night, each with a preamp that cancels its loudest point.
- **Import / export** in Equalizer APO `ParametricEQ.txt` text: import from a file, copy the current sound to the clipboard.
- **Soft limiter** after the EQ (`eq::limit`): identity below -1 dBFS, bends towards full scale above it, no look-ahead so no latency.
- **Global hotkeys**: Ctrl+Shift+F9 / F10 previous / next sound preset, Ctrl+Shift+F11 bypass. Not Ctrl+Alt (that is AltGr on European layouts). A shortcut another app already owns is reported in Settings instead of failing the launch. **Untested in the real app**: the Rust side compiles, nothing has pressed the keys yet.

Still to do from the same list: switch the sound preset when a game launches (needs foreground-process watching and a rules UI), the `IAudioClient3` backend, README / updater / code signing for a public release.

## 4. Sonar preset import

Let Sonar users bring their configs. Format is **unresearched**: find where GG stores Sonar configs on disk and what a shared community-config link contains, then map its filters onto our bands.

## 5. Game / Media / Chat channels, mic EQ

Measured 2026-09-20 with `examples/process_probe.rs` (child process plays tones, parent captures its process-tree loopback and the endpoint loopback at once):

- Windows' per-process loopback returns the app's **raw stream**: a stereo tone comes back bit-for-level (-20.0 dBFS, L only); of a 7.1 stream it returns the front pair only (back-left: silence). It is pre-mix and pre-spatial, so it can never carry the Atmos/DTS render.
- It is taken **after** the session volume: muting the app in the mixer silences the endpoint loopback *and* the per-process capture. So "mute it on the endpoint, capture it per process" doesn't work, and neither does subtracting captures from the mix (the spatial renderer doesn't pass stereo bit-exactly).

What that leaves, best first:

1. **A second instance of the same signed driver.** fxvad is root-enumerated; Windows can create another `Root\FXVAD` devnode on the package already installed, giving two virtual endpoints ("freeseries Game", "freeseries Chat"), each with its own loopback, EQ and volume, spatial sound on Game only. This is Sonar's own model, with no new driver to sign. **Unverified:** whether fxvad tolerates two instances. Test: `setup.ps1 -AddInstance` (elevated); a failed instance is removed with `pnputil /remove-device`.
2. **Mixer-only channels** as the fallback: group apps into Game/Chat/Media and drive their Windows session volumes (`ISimpleAudioVolume`, what the Volume Mixer does). Works with Atmos/DTS and adds no latency, but no per-channel EQ.

Mic processing (noise suppression, mic EQ) needs a virtual *capture* device; fxvad's capture pin is disabled in its signed INF. Parked.

## Latency (measured 2026-09-20 with `examples/latency.rs`)

The tool plays quiet noise into the virtual device, records the loopback of that device and of the output, and cross-correlates them on WASAPI's own capture timestamps. Self-against-self reads 0.0 ms.

- 0.1.1 as installed, FxSound -> Arctis: **64 ms**. Most of that was backlog: audio that queued up while the streams started (capture runs first) and was never given back. It varied from start to start.
- With the queue trimming in `src/wasapi.rs` (drop whatever is always left over, keep an 8 ms cushion), FxSound -> monitor: **25-27 ms**, the same on every start. Not yet measured on the Arctis route.
- Asking cpal for one-period buffers changed nothing. ~25 ms is the floor of shared-mode WASAPI at its 10 ms period: one period to capture, one to render, plus the cushion.

**Next step for latency:** our own WASAPI backend on `IAudioClient3`, which lets shared-mode streams run at the driver's minimum period (often ~3 ms) instead of 10 ms. `AudioBackend` is already the seam for it. Target: ~10 ms. Whether fxvad and the user's outputs support small periods is unverified.

## Observed, not yet acted on

- **Glitches under heavy CPU load.** The installed app logged three capture glitches at 12:59 on 2026-09-20 while a Rust build was saturating the CPU, and none in the quiet 75 minutes before. The capture and playback callbacks share a mutex'd queue and run at whatever priority cpal gives them. If this shows up while gaming, the fix is the lock-free ring buffer already flagged in `src/wasapi.rs`, plus checking the audio threads register with MMCSS ("Pro Audio").
