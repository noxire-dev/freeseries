# Architecture decision (2026-09-19)

Goal: free, open-source, account-less way to run a parametric EQ together with the spatial renderer the user already owns (Dolby Atmos for Headphones, DTS Headphone:X, Windows Sonic), installable without touching Equalizer APO. We do not build a spatial renderer.

Why it doesn't exist today: Equalizer APO's EQ is not applied to spatial content (tickets #323, #267), and Sonar requires Atmos/DTS to be switched off.

## Shape

```
game/apps -> virtual render endpoint with Atmos/DTS/Sonic enabled on it -> [WASAPI loopback capture, already binaural] -> parametric EQ -> [WASAPI render] -> real headset (spatial sound off)
```

One user-mode engine (Rust, this crate). UI comes later (Tauri, talks to the engine crate). EQ sits after the spatial renderer, which is the correct order for headphone correction.

**Verified on hardware (2026-09-19, `examples/probe.rs`):** loopback capture contains the spatial renderer's output. A tone in the back-left channel of a 7.1 stream comes back frequency-shaped with rotating L/R phase, while the same tone as plain stereo comes back flat with a silent R. Which provider was active on the test endpoints was not identified.

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
| FxSound `fxvad.sys` | AGPL-3.0 + MS-LPL | yes, WHCP-signed | not needed (see DSP). **Open: does it offer the Spatial sound dropdown with Atmos/DTS? Unverified.** |
| VB-Cable | closed donationware, bundling allowed with attribution + donation notice | yes | not needed; spatial dropdown unverified |
| Virtual-Audio-Driver | MIT | no (SignPath only) | stereo only |
| Scream | MS-PL | no (cert expired) | multichannel |

Plan: depend on the signed `fxvad.sys` binary (rebuilding it loses the signature), VB-Cable as fallback. The engine only needs "a render endpoint to loopback-capture", so it is driver-agnostic. Bundling an AGPL driver makes AGPL-3.0 the no-argument license for this project.

## DSP building blocks

- Headphone presets: AutoEq `ParametricEQ.txt` (MIT code; measurement copyright question open in AutoEq #790). `src/eq.rs` parses this format directly.
- Spatial: none of ours. Windows spatial sound also advertises 7.1 to games on a stereo endpoint, so the virtual endpoint does not need to be 8-channel.

## Known ceilings

- Latency: WASAPI shared mode is ~10 ms per side, so ~20-30 ms added. `IAudioClient3` small buffers can cut it; cpal does not expose that.
- Exclusive-mode / ASIO apps bypass any virtual-endpoint design, same as Sonar.
- The April 2026 Windows Driver Policy could tighten what loads; fxvad is WHCP-signed, which is the safe side of that line.
