import type { Band } from "./api";
import type { NamedPreset } from "./store";

// Ten octave bands, the layout of a classic graphic EQ.
const OCTAVES = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
const graphic = (gains: number[]): Band[] =>
  gains.map((gainDb, i) => ({ kind: "peak" as const, fc: OCTAVES[i], gainDb, q: 1.41 })).filter((b) => b.gainDb !== 0);

/**
 * Presets that ship with the app.
 *
 * PSense comes from the Arctis Nova Pro hardware-EQ curves in the zhelper fork of G-Helper, tuned
 * for that headset with thick gel earpads. The slider values there were (dB, 31 Hz to 16 kHz):
 *   Game   3  4.5  -1.5  -2.5  -1    0    2.5  1    3.5  1.5
 *   Media  6  6    -1.5  -2    -1.5  -0.5 2    0.5  3    2
 * Neighbouring peaks overlap, so the band gains below were fitted until the summed curve passes
 * through those values (within 0.06 dB). The preamp cancels the loudest point so nothing clips.
 */
export const BUILTIN: NamedPreset[] = [
  { name: "PSense Game", preset: { preampDb: -4.5, bands: graphic([2.2, 4.5, -2, -2.2, -0.6, -0.3, 2.5, 0.1, 3.3, 1.3]) } },
  { name: "PSense Media", preset: { preampDb: -6.1, bands: graphic([5, 5.5, -2.4, -1.6, -1.1, -0.6, 2.1, -0.3, 2.9, 1.8]) } },
];
