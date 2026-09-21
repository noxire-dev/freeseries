// What FreeSeries shows on a headset's display (the Arctis Nova Pro's base station has a 128x64
// OLED), and when it shows nothing. OLEDs burn in: whatever sits still for months stays forever.
// So our own screen never sits still (it moves by a pixel every minute), goes dark while the
// user is away. Its knobs are ours to show: the dial draws the balance, the volume knob the
// volume. Anything else the device says by itself that we can't read means someone is in its own
// menu, which is drawn on the same display, so then it gets its screen back for a while.

import { useEffect, useRef } from "react";
import type { HeadsetApi, HeadsetFeatures, HeadsetState } from "./api";

/** "stock": the device's own screen, ours only while the dial turns. "ours": the FreeSeries screen. "off": dark. */
export type ScreenMode = "stock" | "ours" | "off";
export interface ScreenSettings {
  mode: ScreenMode;
  /** Move our screen around by a pixel or two every minute. */
  shift: boolean;
  /** Go dark after this long without keyboard or mouse. 0: never. */
  idleMinutes: number;
}
export const DEFAULT_SCREEN: ScreenSettings = { mode: "stock", shift: true, idleMinutes: 5 };

const OVERLAY_MS = 2000; // the balance or the volume stays this long after the knob stopped
const HANDS_ON_MS = 10000; // someone is in the device's own menu: it keeps its screen this long
const ORBIT = [[0, 0], [1, 0], [2, 1], [2, 2], [1, 2], [0, 2], [-1, 1], [-1, 0]]; // one step a minute

type Size = [number, number];

/** Draws with a canvas (fonts for free) and thresholds to one bit per pixel, row by row, most significant bit first. */
function frame([width, height]: Size, [dx, dy]: number[], draw: (g: CanvasRenderingContext2D) => void): Uint8Array {
  const canvas = Object.assign(document.createElement("canvas"), { width, height });
  const g = canvas.getContext("2d", { willReadFrequently: true })!;
  g.fillStyle = "#000";
  g.fillRect(0, 0, width, height);
  g.translate(dx, dy);
  g.fillStyle = "#fff";
  g.textBaseline = "top";
  draw(g);
  const rgba = g.getImageData(0, 0, width, height).data;
  const bits = new Uint8Array(Math.ceil((width * height) / 8));
  for (let i = 0; i < width * height; i++) if (rgba[i * 4] > 127) bits[i >> 3] |= 0x80 >> (i & 7);
  return bits;
}

/** The two names, a track, and a knob where the dial stands. */
export function balanceFrame(size: Size, balance: number, left: string, right: string): Uint8Array {
  const [width, height] = size;
  return frame(size, [0, 0], (g) => {
    g.font = "bold 13px sans-serif";
    g.fillText(left.toUpperCase(), 2, 6);
    g.textAlign = "right";
    g.fillText(right.toUpperCase(), width - 2, 6);
    const [track, y] = [width - 16, Math.round(height * 0.62)];
    g.fillRect(8, y, track, 2);
    g.fillRect(8 + track / 2 - 1, y - 5, 2, 12); // the middle
    g.fillRect(Math.round(8 + ((balance + 1) / 2) * track) - 4, y - 9, 8, 20);
  });
}

/** The volume as a number and a bar. */
export function volumeFrame(size: Size, volume: number): Uint8Array {
  const [width, height] = size;
  return frame(size, [0, 0], (g) => {
    g.font = "bold 11px sans-serif";
    g.fillText("VOLUME", 4, 4);
    g.textAlign = "right";
    g.font = "bold 30px sans-serif";
    g.fillText(String(Math.round(volume * 100)), width - 4, 2);
    g.strokeStyle = "#fff";
    g.strokeRect(4.5, height - 18.5, width - 9, 12);
    g.fillRect(7, height - 16, Math.round((width - 14) * volume), 7);
  });
}

/** The time, and underneath what the headset and the main channel are up to. Everything stays 3 px clear of the edges so it can move. */
export function idleFrame(size: Size, now: Date, battery: number | null, sound: string, offset: number[]): Uint8Array {
  const [width] = size;
  return frame(size, offset, (g) => {
    g.textAlign = "center";
    g.font = "bold 30px sans-serif";
    g.fillText(now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false }), width / 2, 6);
    g.font = "11px sans-serif";
    g.fillText([battery !== null && `${Math.round(battery * 100)}%`, sound].filter(Boolean).join("  "), width / 2, 46, width - 8);
  });
}

const same = (a: Uint8Array | "released" | undefined, b: Uint8Array | "released") =>
  a === b || (a instanceof Uint8Array && b instanceof Uint8Array && a.length === b.length && a.every((v, i) => v === b[i]));

/**
 * Keeps the headset's display showing the right thing. `balance` is what the dial overlay shows
 * (null: the dial isn't driving anything). `idle` resolves to seconds since the last key or mouse move.
 */
export function useScreen(p: {
  headset: HeadsetApi;
  idle(): Promise<number>;
  features: HeadsetFeatures | null;
  state: HeadsetState | undefined;
  settings: ScreenSettings;
  balance: { value: number; left: string; right: string } | null;
  sound: string;
}) {
  const latest = useRef(p);
  latest.current = p;
  const seen = useRef({ dial: "", volume: null as number | null, activity: 0, dialAt: 0, volumeAt: 0, handsOnAt: 0, sent: undefined as Uint8Array | "released" | undefined, timer: 0 });

  const size = p.features?.display ?? null;
  const dial = JSON.stringify(p.state?.dial ?? null);
  const activity = p.state?.activity ?? 0;
  const volume = p.state?.volume ?? null;

  useEffect(() => {
    if (!size) return;
    const s = seen.current;
    const now = Date.now();
    if (dial !== s.dial && s.dial !== "") s.dialAt = now;
    if (volume !== s.volume && volume !== null) s.volumeAt = now;
    if (activity !== s.activity) s.handsOnAt = now;
    [s.dial, s.volume, s.activity] = [dial, volume, activity];

    let live = true;
    const render = async () => {
      clearTimeout(s.timer);
      const { settings, balance, state, sound, headset, idle } = latest.current;
      const at = Date.now();
      const away = settings.idleMinutes > 0 && (await idle().catch(() => 0)) >= settings.idleMinutes * 60;
      if (!live) return;
      const want: Uint8Array | "released" =
        balance && at - s.dialAt < OVERLAY_MS && s.dialAt >= s.volumeAt ? balanceFrame(size, balance.value, balance.left, balance.right)
        : settings.mode !== "stock" && state?.volume != null && at - s.volumeAt < OVERLAY_MS ? volumeFrame(size, state.volume) // in its own mode the device shows its own
        : settings.mode === "stock" || at - s.handsOnAt < HANDS_ON_MS ? "released"
        : settings.mode === "off" || away ? new Uint8Array(Math.ceil((size[0] * size[1]) / 8))
        : idleFrame(size, new Date(at), state?.online ? state.battery : null, sound, settings.shift ? ORBIT[Math.floor(at / 60000) % ORBIT.length] : [0, 0]);
      if (!same(s.sent, want)) {
        s.sent = want;
        (want === "released" ? headset.releaseDisplay() : headset.draw(want)).catch(() => (s.sent = undefined));
      }
      // Next look: when the overlay or the hands-on time runs out, at the next minute, and every half minute for "away".
      const waits = [30000, 60000 - (at % 60000) + 50, OVERLAY_MS - (at - s.dialAt), OVERLAY_MS - (at - s.volumeAt), HANDS_ON_MS - (at - s.handsOnAt)].filter((ms) => ms > 0);
      s.timer = window.setTimeout(render, Math.min(...waits));
    };
    render();
    return () => {
      live = false;
      clearTimeout(s.timer);
    };
  }, [size?.[0], size?.[1], dial, volume, activity, p.settings.mode, p.settings.shift, p.settings.idleMinutes, p.balance?.value, p.sound, p.state?.battery, p.state?.online]);

  // The display goes back to the device when its plugin goes away or the app closes the page.
  useEffect(() => () => void (size && latest.current.headset.releaseDisplay().catch(() => {})), [!!size]);
}
