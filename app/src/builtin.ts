import type { Band } from "./api";
import type { NamedPreset } from "./store";

// Ten octave bands, the layout of a classic graphic EQ.
const OCTAVES = [31, 62, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
const graphic = (gains: number[]): Band[] =>
  gains.map((gainDb, i) => ({ kind: "peak" as const, fc: OCTAVES[i], gainDb, q: 1.41 })).filter((b) => b.gainDb !== 0);

const pk = (fc: number, gainDb: number, q = 1.41): Band => ({ kind: "peak", fc, gainDb, q });
const ls = (fc: number, gainDb: number): Band => ({ kind: "lowShelf", fc, gainDb, q: 0.71 });
const hs = (fc: number, gainDb: number): Band => ({ kind: "highShelf", fc, gainDb, q: 0.71 });

/**
 * General-purpose starting points, written for freeseries. They shape the sound rather than
 * correct a headphone, so they work on top of any headphone correction. Each preamp cancels that
 * preset's loudest point.
 */
const STARTERS: NamedPreset[] = [
  // Less rumble to mask them, more of the 2-5 kHz range where footsteps and reloads live.
  { name: "Footsteps", preset: { preampDb: -4.7, bands: [ls(110, -4), pk(250, -2, 1), pk(2500, 4, 1.2), pk(4500, 3, 1.5), hs(9000, 1.5)] } },
  { name: "Bass boost", preset: { preampDb: -6, bands: [ls(90, 6), pk(250, -1.5, 1)] } },
  // Thins out the boxy low mids and lifts consonants, for calls, streams and dialogue.
  { name: "Voice clarity", preset: { preampDb: -3.5, bands: [ls(120, -3), pk(300, -2, 1), pk(3000, 3.5, 1), hs(8000, 1)] } },
  // Ears lose bass and treble first at low volume; this puts some back.
  { name: "Late night", preset: { preampDb: -4, bands: [ls(80, 4), pk(1000, -1, 0.8), hs(8000, 3)] } },
];

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
  ...STARTERS,
  { name: "PSense Game", preset: { preampDb: -4.5, bands: graphic([2.2, 4.5, -2, -2.2, -0.6, -0.3, 2.5, 0.1, 3.3, 1.3]) } },
  { name: "PSense Media", preset: { preampDb: -6.1, bands: graphic([5, 5.5, -2.4, -1.6, -1.1, -0.6, 2.1, -0.3, 2.9, 1.8]) } },
];
