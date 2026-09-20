// Runnable check that the drawn curve matches the engine's filters: `npm run check`.
import { bandGainDb, totalGainDb } from "./eqmath.ts";

const near = (actual: number, expected: number, what: string) => {
  if (Math.abs(actual - expected) >= 0.1) throw new Error(`${what}: got ${actual.toFixed(2)}, expected ${expected}`);
};

near(bandGainDb({ kind: "peak", fc: 1000, gainDb: 6, q: 1 }, 1000), 6, "peak at its centre");
near(bandGainDb({ kind: "peak", fc: 1000, gainDb: 6, q: 1 }, 60), 0, "peak far below its centre");
near(bandGainDb({ kind: "lowShelf", fc: 200, gainDb: -4, q: 1 }, 20), -4, "low shelf plateau");
near(bandGainDb({ kind: "highShelf", fc: 4000, gainDb: 5, q: 1 }, 18000), 5, "high shelf plateau");
// Same band the engine measured on hardware: +6 dB peak at 1 kHz, Q 1.41 read +1.1 dB at 500 Hz.
near(bandGainDb({ kind: "peak", fc: 1000, gainDb: 6, q: 1.41 }, 500), 1.1, "skirt matches the hardware measurement");
near(totalGainDb({ preampDb: -3, bands: [{ kind: "peak", fc: 1000, gainDb: 6, q: 1 }] }, 1000), 3, "preamp adds in");
console.log("eqmath ok");
