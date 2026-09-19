# Architecture decision (2026-09-19)

Goal: free, open-source, account-less replacement for SteelSeries Sonar's EQ + spatial audio, installable without touching Equalizer APO.

## Shape

```
game/apps -> virtual 7.1 render endpoint -> [WASAPI loopback capture] -> HRTF 7.1->binaural -> parametric EQ -> [WASAPI render] -> real headset
```

One user-mode engine (Rust, this crate). UI comes later (Tauri, talks to the engine crate).

## Why not the alternatives

- **APO only (wrap Equalizer APO / HeSuVi):** an APO cannot change the format the endpoint advertises, so games send stereo to a stereo headset and there is nothing to spatialise. HeSuVi only works with a 7.1 virtual cable underneath. Equalizer APO also depends on `DisableProtectedAudioDG` (a Microsoft "test key") and its install is wiped by Windows/driver updates. HeSuVi has no license and is unmaintained since 2021.
  - https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/audio-processing-object-architecture
  - https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/implementing-audio-processing-objects
- **Own kernel driver:** needs a Microsoft signature via Partner Center: registered business, EV cert (~$290-500/yr), ID verification. Attestation signing is now labelled "testing only". SignPath-signed drivers do not load under Secure Boot. No user-mode virtual endpoint exists (ACX is KMDF-only).
  - https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-reqs
  - https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/driver-signing-offerings
  - https://github.com/VirtualDrivers/Virtual-Audio-Driver/issues/15

## Virtual endpoint: reuse, don't write

| Driver | License | Loads on stock Win11 | 7.1 |
|---|---|---|---|
| FxSound `fxvad.sys` | AGPL-3.0 + MS-LPL | yes, WHCP-signed | source allows 8 ch; FxSound calls it hit-and-miss. **Unverified on hardware.** |
| VB-Cable | closed donationware, bundling allowed with attribution + donation notice | yes | 8 ch per manual |
| Virtual-Audio-Driver | MIT | no (SignPath only) | stereo only |
| Scream | MS-PL | no (cert expired) | multichannel |

Plan: depend on the signed `fxvad.sys` binary (rebuilding it loses the signature), VB-Cable as fallback. The engine only needs "a render endpoint to loopback-capture", so it is driver-agnostic. Bundling an AGPL driver makes AGPL-3.0 the no-argument license for this project.

## DSP building blocks

- Virtual surround: Steam Audio `iplVirtualSurroundEffect` (Apache-2.0, C API, 7.1 -> binaural, custom SOFA). https://valvesoftware.github.io/steam-audio/doc/capi/virtual-surround-effect.html
- HRTFs to ship: SADIE II (Apache-2.0), HUTUBS (CC BY). Do not ship HeSuVi's HRIRs (captures of Dolby/DTS/Creative, no license); let users load their own.
- Headphone presets: AutoEq `ParametricEQ.txt` (MIT code; measurement copyright question open in AutoEq #790). `src/eq.rs` parses this format directly.

## Known ceilings

- Latency: WASAPI shared mode is ~10 ms per side, so ~20-30 ms added. `IAudioClient3` small buffers can cut it; cpal does not expose that.
- Exclusive-mode / ASIO apps bypass any virtual-endpoint design, same as Sonar.
- The April 2026 Windows Driver Policy could tighten what loads; fxvad is WHCP-signed, which is the safe side of that line.
