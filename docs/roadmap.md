# Roadmap

Wishlist from the first day of real use (2026-09-20), in the order I'd build it. Nothing here is started.

## 1. Outputs that follow you (built 2026-09-20, headset-off switch untested on hardware)

One feature, three symptoms:

- **No virtual-device picker.** freeseries only works with its own virtual endpoint, so find the FxSound device by name and hide that choice. Keep the picker as a fallback when no such device exists (VB-Cable users).
- **Per-device correction.** Each output remembers its own headphone correction. (Until 2026-09-20 it also held the sound preset; see "Where the sound lives" below.)
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

- **Where the sound lives (changed 2026-09-20).** A sound preset belongs to a channel, a headphone correction to an output, and nothing else carries an EQ. Before, the output held a sound preset too and the Mixer let a channel name a second one: both ran in series, so a user with "PSense Game" on the channel and "PSense Media" on the output got two curves and two preamps, 10.6 dB down (found when the user reported low volume; every Windows endpoint volume measured 100% with `examples/volumes.rs`). Now the Equalizer and the Presets page work on one channel, picked with a Game / Chat / Media switch in the toolbar; the Mixer strip's picker sets the same curve and its thumbnail opens the editor on that channel. The engine is unchanged: channel presets travel in the plan (a plan with the same devices retunes the running session), the output preset is the correction alone. Saved settings migrate (`store.ts`, checked by `npm run check`): a channel's own pick wins, otherwise the main channel inherits what the first output played. The editor still draws the active output's correction as a dashed reference line, and headroom is worked out over correction plus sound. The analyser shows the whole mix, not the channel being edited. **Checked in the browser preview only.**
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

1. **A second instance of the same signed driver. VERIFIED 2026-09-20.** `setup.ps1 -AddInstance` (elevated) created `ROOT\MEDIA\0002` from the package already installed: status OK, its own endpoint, and `examples/probe.rs` shows the two loopbacks are fully independent (a tone played into one reads -20.0 dBFS on its own loopback and digital silence on the other, both ways). Endpoints are named per instance without admin rights: "freeseries", "freeseries Chat", then "freeseries Media". This is Sonar's own model with no new driver to sign.

   **Built 2026-09-20.** The engine captures every channel's device in one session, applies that channel's EQ and volume, sums them and runs the output's EQ and the limiter over the mix (`src/wasapi.rs`). Hardware check through the CLI (`freeseries run 0,1 2`): a tone into either device arrives at the output at -19.9 dBFS, a 24 s soak shows 0 dropouts and 0 phase jumps, latency 13-31 ms per run. The app has a Mixer page (Game/Chat balance, per-channel volume, mute and EQ) and an "Add the chat device" action that runs `setup.ps1 -AddInstance` behind one UAC prompt. **Untested in the real app**, and that action has only ever been run by hand.

   **Apps into channels, built 2026-09-20.** `src/routing.rs` drives the per-app output device of the Windows volume mixer through the undocumented `Windows.Media.Internal.AudioPolicyConfig` (layout and IIDs from EarTrumpet; same vtable on Windows 11 and on Windows 10 before 21H2, different IID). Measured with a test process: pinned to "freeseries Chat" its tone reads -20.0 dBFS there and silence on the main device, the pin survives a relaunch (Windows stores it per app), un-pinning sends it back to the default. Two limits found: an app can only be pinned once it has an audio session (`E_INVALIDARG` before that), and an app moves when it next opens a stream (real apps reopen at once; a stream that ignores the invalidation stays put). The Mixer page has a board: a lane per channel, drag an app across, all processes of the same executable move together, with a per-app select as the keyboard route.

   **Console, built 2026-09-20.** The Mixer page is a mixing console: one strip per channel (EQ thumbnail and picker, its apps, fader, mute, name plate), an output strip, and a Game/Chat crossfader. The whole strip is the drop target. Live peak meters come from the engine (`Dsp::levels()`, per channel after EQ and volume, plus "out" for the mix after the limiter), polled 20 times a second and painted outside React. Apps show their real icon (`routing::icon_of`, `SHGetFileInfoW` to RGBA; checked by eye against Spotify, Discord and Chrome with `examples/icon_dump.rs`). **Checked in the browser preview only**: a real mouse drag, the meters and the icons have not been seen inside the Tauri window.

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
