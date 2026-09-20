// Magnitude response of the EQ, for drawing. Mirrors the RBJ biquads in the engine's src/eq.rs;
// the engine stays the source of truth for audio, this only has to draw the same curve.

import type { Band, Preset } from "./api";

export const F_MIN = 20;
export const F_MAX = 20000;
export const DB_RANGE = 18;
const SAMPLE_RATE = 48000;

/** Gain in dB of one band at `freq`. */
export function bandGainDb(band: Band, freq: number): number {
  const a = 10 ** (band.gainDb / 40);
  const w0 = (2 * Math.PI * band.fc) / SAMPLE_RATE;
  const cos = Math.cos(w0);
  const alpha = Math.sin(w0) / (2 * band.q);
  let b0: number, b1: number, b2: number, a0: number, a1: number, a2: number;
  if (band.kind === "peak") {
    [b0, b1, b2, a0, a1, a2] = [1 + alpha * a, -2 * cos, 1 - alpha * a, 1 + alpha / a, -2 * cos, 1 - alpha / a];
  } else {
    const s = 2 * Math.sqrt(a) * alpha;
    const sign = band.kind === "lowShelf" ? 1 : -1; // high shelf is the low shelf with cos terms flipped
    b0 = a * (a + 1 - sign * (a - 1) * cos + s);
    b1 = sign * 2 * a * (a - 1 - sign * (a + 1) * cos);
    b2 = a * (a + 1 - sign * (a - 1) * cos - s);
    a0 = a + 1 + sign * (a - 1) * cos + s;
    a1 = sign * -2 * (a - 1 + sign * (a + 1) * cos);
    a2 = a + 1 + sign * (a - 1) * cos - s;
  }
  // |H(e^jw)|^2 for a biquad, evaluated directly.
  const w = (2 * Math.PI * freq) / SAMPLE_RATE;
  const [c1, c2] = [Math.cos(w), Math.cos(2 * w)];
  const num = b0 * b0 + b1 * b1 + b2 * b2 + 2 * (b0 * b1 + b1 * b2) * c1 + 2 * b0 * b2 * c2;
  const den = a0 * a0 + a1 * a1 + a2 * a2 + 2 * (a0 * a1 + a1 * a2) * c1 + 2 * a0 * a2 * c2;
  return 10 * Math.log10(num / den);
}

export function totalGainDb(preset: Preset, freq: number): number {
  return preset.bands.reduce((sum, b) => sum + bandGainDb(b, freq), preset.preampDb);
}

/** Log-spaced frequencies across the audible range. */
export function frequencies(count: number): number[] {
  return Array.from({ length: count }, (_, i) => F_MIN * (F_MAX / F_MIN) ** (i / (count - 1)));
}

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

export function formatHz(hz: number): string {
  return hz >= 1000 ? `${(hz / 1000).toFixed(hz >= 10000 ? 1 : 2).replace(/\.?0+$/, "")} kHz` : `${Math.round(hz)} Hz`;
}

export function formatDb(db: number): string {
  return `${db > 0 ? "+" : db < 0 ? "−" : ""}${Math.abs(db).toFixed(1)} dB`;
}

/** A headphone correction with a sound preset on top: all bands in series, preamps added. */
export function combine(correction: Preset | undefined, sound: Preset): Preset {
  if (!correction) return sound;
  return { preampDb: Math.round((correction.preampDb + sound.preampDb) * 10) / 10, bands: [...correction.bands, ...sound.bands] };
}

const CODES = { peak: "PK", lowShelf: "LSC", highShelf: "HSC" } as const;

/** Equalizer APO / AutoEq `ParametricEQ.txt` text, so presets travel to and from Peace, Equalizer APO and AutoEq. */
export function formatPreset(preset: Preset): string {
  const lines = preset.bands.map((b, i) => `Filter ${i + 1}: ON ${CODES[b.kind]} Fc ${Math.round(b.fc)} Hz Gain ${b.gainDb.toFixed(1)} dB Q ${b.q.toFixed(2)}`);
  return [`Preamp: ${preset.preampDb.toFixed(1)} dB`, ...lines].join("\n") + "\n";
}
