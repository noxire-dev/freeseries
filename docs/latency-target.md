# What added latency is acceptable (researched 2026-09-22)

Web research by a background agent; figures with a source are marked verified where the agent
read the source itself, "unverified" where only a search snippet or a forum claim was found.

## Perception, one-way (audio lagging the picture or the action)

- ITU-R BT.1359-1: detectable at +45 ms lead / -125 ms lag, acceptable up to +90 / -185 ms (newsreader material). Verified via https://en.wikipedia.org/wiki/Audio-to-video_synchronization
- EBU R37-2007: end to end no more than 40 ms lead / 60 ms lag. Verified, https://tech.ebu.ch/docs/r/r037.pdf
- Games, action to sound: Halbhuber et al. 2022, CS:GO, n=24, extra delay of 0/40/270/500 ms: 40 ms changed neither score nor experience; experts got tense at 270 ms. The setup already carried ~70 ms, which may have hidden the 40 ms step. https://epub.uni-regensburg.de/53766/1/3543758.3543760.pdf
- Audio latency just-noticeable difference (Regensburg 2024, n=37): 49 ms from a 0 ms base, 27 ms from a 64 ms base. https://hci.ur.de/publications/measuring_the_just_noticeable_difference_for_audio_latency_2025
- One's own voice (sidetone): 3-5 ms noticeable, above 10 ms objectionable (Agnew & Thornton 2000, https://pubmed.ncbi.nlm.nih.gov/10858005/); disturbing beyond 15-20 ms (Stone & Moore). Playback tolerance is 5 to 10 times looser than sidetone.

## What comparable software adds

| Product | Added | How known |
|---|---|---|
| Equalizer APO | ~1 ms on top of Windows' own 10 ms APO buffering | measured, HeSuVi wiki mic test (no processing 134 ms, EqAPO 135) https://sourceforge.net/p/hesuvi/wiki/Audio%20Latency%20Measurements/ |
| EqAPO + Voicemeeter | +40 ms | same measurement |
| EqAPO + VB-Cable ("Listen") | +61 ms | same measurement |
| Voicemeeter | "< 20 ms global" at 256-sample buffers | vendor claim, https://voicemeeter.com/how-to-optimize-your-latency-in-voicemeeter/ |
| VB-Cable | up to ~70 ms usable, pipe 7168 samples | vendor manual |
| FxSound | users report ~0.5 s | forum, unverified |
| SteelSeries Sonar | no official figure; ~40 ms from an osu! player's in-game offset | subjective, unverified |
| Dolby Atmos / DTS / Windows Sonic | forum claims from 30 to 250 ms | unverified |
| FreeSeries today (cpal engine) | 29-48 ms | measured, examples/latency.rs |
| FreeSeries raw WASAPI spike | 7-8 ms | measured, examples/lowlat.rs |

Wireless link for context: Arctis Nova Pro Wireless 2.4 GHz ~38 ms and Bluetooth ~184 ms per a
review snippet (unverified); Bluetooth SBC 100-270 ms, aptX LL ~40 ms.

## The target

**At most 10 ms added, hard ceiling 20 ms.** An APO-style EQ costs about 1 ms on top of the
10 ms Windows adds anyway; everything above that is buffering we chose. The game study says
40 ms is invisible in play, so 20 ms is safe for game audio; the number a competitive reviewer
would accept is "10 ms or less, measured, and below the headset's own radio link". Never route
the user's own voice (sidetone) through this path: there 10 ms is already objectionable, and the
headset does sidetone in hardware.

Publish the measurement with the claim: `examples/latency.rs` is the loopback method, a
microphone recording of a gunshot (the HeSuVi method) is what reviewers trust.
