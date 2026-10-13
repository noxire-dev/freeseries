import type { Preset } from "./api";
import { BUILTIN } from "./builtin.ts";
import { DEFAULT_SCREEN } from "./screen.ts";
import type { ScreenSettings } from "./screen.ts";

export interface NamedPreset { name: string; preset: Preset }

/** One of the user's real outputs. The only EQ that belongs to it is the one that fixes the device itself. */
export interface SavedOutput {
  id: string;
  name: string;
  /** Headphone correction (AutoEq). */
  correction?: NamedPreset | null;
}

/** The microphone path: which mic, whether it runs, and whether its background noise is removed. */
export interface SavedMic { device: string | null; on: boolean; suppress: boolean }

/** A channel: its mixer settings and its sound, the curve the editor edits. Volume is the slider position, 0 to 1. */
export interface SavedChannel { volume: number; muted: boolean; presetName: string; preset: Preset }

export const FLAT: Preset = { preampDb: 0, bands: [] };
export const DEFAULT_CHANNEL: SavedChannel = { volume: 1, muted: false, presetName: "Flat", preset: FLAT };

export interface Saved {
  /** Only set when the virtual device couldn't be found by name and the user picked one. */
  source: string | null;
  /** Most preferred first. */
  outputs: SavedOutput[];
  running: boolean;
  /** Move between outputs by itself. Off: only the first output is ever used. */
  autoSwitch: boolean;
  /** An output the user picked to play on now, ahead of the order, until they hand it back. */
  chosen: string | null;
  /** Global keyboard shortcuts for bypass and switching the sound preset. */
  hotkeys: boolean;
  /** Mixer settings by channel key ("main", "chat", ...). */
  channels: Record<string, SavedChannel>;
  /** Balance between the main channel and the second one (Game / Media): -1 all game, 0 both, +1 all of the other. */
  balance: number;
  bypass: boolean;
  library: NamedPreset[];
  /** Which channel an app belongs on, by app name. Apps on the main channel aren't listed: that is where everything plays anyway. */
  assignments: Record<string, string>;
  /** The headset's ChatMix dial sets the balance between the main channel and the second one. */
  dial: boolean;
  /** Show the balance on the headset's display while the dial turns. */
  display: boolean;
  /** What the headset's display shows the rest of the time, and how it is kept from burning in. */
  screen: ScreenSettings;
  /** The user's choices for a headset's settings, by "model:key". Headsets don't report them, so they are sent again whenever the headset appears. */
  headsetSettings: Record<string, number>;
  /** Let an AI agent in through the local MCP endpoint. The token is made the first time this is switched on. */
  agent: boolean;
  agentToken: string;
  mic: SavedMic;
}

export interface SettingsStore {
  load(): Saved;
  save(saved: Saved): void;
}

const EMPTY: Saved = { source: null, outputs: [], running: false, autoSwitch: true, chosen: null, hotkeys: true, channels: {}, balance: 0, bypass: false, library: [], assignments: {}, dial: true, display: true, screen: DEFAULT_SCREEN, headsetSettings: {}, agent: false, agentToken: "", mic: { device: null, on: false, suppress: true } };

/** 0.1.x stored a single route with one global preset. */
function fromRoute(raw: Record<string, any>): Record<string, any> {
  if (raw.outputs || !raw.route) return raw;
  return { bypass: raw.bypass, library: raw.library, running: true, outputs: [{ id: raw.route.to, name: "", presetName: raw.presetName ?? "Flat", preset: raw.preset }] };
}

/**
 * Until 0.2.0 the sound preset sat on the output, and a channel could name a second one that ran
 * in series with it: two curves and two preamps, 10 dB quieter than anyone meant. The sound now
 * belongs to the channel alone. A channel that had picked its own keeps it; otherwise the main
 * channel inherits what the first output was playing.
 */
export function migrate(stored: Record<string, any>): Partial<Saved> {
  const raw = fromRoute(stored);
  const old: Record<string, any>[] = raw.outputs ?? [];
  if (!old.some((o) => "preset" in o) && !Object.values<Record<string, any>>(raw.channels ?? {}).some((c) => !c.preset)) return raw;
  const named = [...BUILTIN, ...(raw.library ?? [])] as NamedPreset[];
  const channels: Record<string, SavedChannel> = {};
  for (const [key, c] of Object.entries<Record<string, any>>({ main: {}, ...raw.channels })) {
    const picked = named.find((n) => n.name === c.presetName);
    const sound =
      c.preset ? { presetName: c.presetName ?? "Flat", preset: c.preset }
      : picked ? { presetName: picked.name, preset: picked.preset }
      : key === "main" && old[0]?.preset ? { presetName: old[0].presetName ?? "Flat", preset: old[0].preset }
      : DEFAULT_CHANNEL;
    channels[key] = { volume: c.volume ?? 1, muted: c.muted ?? false, presetName: sound.presetName, preset: sound.preset };
  }
  return { ...raw, channels, outputs: old.map((o) => ({ id: o.id, name: o.name, correction: o.correction })) };
}

export function localSettings(key = "freeseries"): SettingsStore {
  return {
    load() {
      try {
        return { ...EMPTY, ...migrate(JSON.parse(localStorage.getItem(key) ?? "{}")) };
      } catch {
        return EMPTY; // unreadable settings start fresh rather than block the app
      }
    },
    save: (saved) => localStorage.setItem(key, JSON.stringify(saved)),
  };
}
