# Roadmap

Wishlist from the first day of real use (2026-09-20), in the order I'd build it. Nothing here is started.

## 1. Outputs that follow you (built 2026-09-20, headset-off switch untested on hardware)

One feature, three symptoms:

- **No virtual-device picker.** freeseries only works with its own virtual endpoint, so find the FxSound device by name and hide that choice. Keep the picker as a fallback when no such device exists (VB-Cable users).
- **Per-device EQ.** Each output remembers its own preset: PSense on the Arctis, something else on the monitor.
- **Auto-switch.** A priority list of outputs. Headset turns off -> audio moves to the monitor and its EQ; headset comes back -> audio moves back. The engine already notices a dead output (`Status::Reconnecting`); today it retries the same device forever instead of falling to the next one.

This is also the robustness story: a missing headset must never mean silent audio.

## 2. Setup wizard (still owed for v1)

Driver install (elevated, `scripts/install-fxvad.ps1`), set the virtual device as default, turn spatial sound on. Since it runs elevated anyway, rename the endpoint there: "Speakers" -> "freeseries". That part is `PKEY_Device_DeviceDesc` on the endpoint and needs admin to write. The "(FxSound Audio Enhancer)" suffix comes from the signed INF; whether the cached `PKEY_DeviceInterface_FriendlyName` can be overridden without breaking anything is **unverified**.

## 3. Editor and UI pass

The EQ editor needs love and the app reads as an open-source project. Do it as a proper design pass with the `impeccable` skill, not piecemeal.

## 4. Sonar preset import

Let Sonar users bring their configs. Format is **unresearched**: find where GG stores Sonar configs on disk and what a shared community-config link contains, then map its filters onto our bands.

## 5. Game / Media / Chat channels, mic EQ (v2)

The expensive one. fxvad gives us exactly one virtual endpoint and we can't ship a second signed driver (see `architecture.md`). Two candidate routes, both **unresearched**:

- Windows per-process loopback capture (`AUDCLNT_PROCESS_LOOPBACK`, Windows 10 2004+): every app plays into the one endpoint, the engine captures each app's audio separately and mixes with per-channel volume and EQ. Open question that decides it: is a per-process capture taken before or after the spatial renderer? If before, channels and Atmos/DTS don't combine this way.
- A second virtual device the user installs themselves (VB-Cable) for chat.

Research spike first, with `examples/probe.rs`-style measurements, before any design.

## Observed, not yet acted on

- **Glitches under heavy CPU load.** The installed app logged three capture glitches at 12:59 on 2026-09-20 while a Rust build was saturating the CPU, and none in the quiet 75 minutes before. The capture and playback callbacks share a mutex'd queue and run at whatever priority cpal gives them. If this shows up while gaming, the fix is the lock-free ring buffer already flagged in `src/wasapi.rs`, plus checking the audio threads register with MMCSS ("Pro Audio").
