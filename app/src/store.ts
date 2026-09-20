import type { Preset } from "./api";

export interface NamedPreset { name: string; preset: Preset }

/** One of the user's real outputs, with the EQ that belongs to it. */
export interface SavedOutput {
  id: string;
  name: string;
  /** The sound preset: what the editor edits and the Presets page switches. */
  presetName: string;
  preset: Preset;
  /** Headphone correction (AutoEq). Belongs to the device, stays put when the sound preset changes. */
  correction?: NamedPreset | null;
}

/** A channel's mixer settings. Volume is the slider position, 0 to 1. */
export interface SavedChannel { volume: number; muted: boolean; presetName: string | null }

export interface Saved {
  /** Only set when the virtual device couldn't be found by name and the user picked one. */
  source: string | null;
  /** Most preferred first. */
  outputs: SavedOutput[];
  running: boolean;
  /** Move between outputs by itself. Off: only the first output is ever used. */
  autoSwitch: boolean;
  /** Global keyboard shortcuts for bypass and switching the sound preset. */
  hotkeys: boolean;
  /** Mixer settings by channel key ("main", "chat", ...). */
  channels: Record<string, SavedChannel>;
  /** Game / Chat balance: -1 all game, 0 both, +1 all chat. */
  balance: number;
  bypass: boolean;
  library: NamedPreset[];
}

export interface SettingsStore {
  load(): Saved;
  save(saved: Saved): void;
}

const EMPTY: Saved = { source: null, outputs: [], running: false, autoSwitch: true, hotkeys: true, channels: {}, balance: 0, bypass: false, library: [] };

/** 0.1.x stored a single route with one global preset. */
function migrate(raw: Record<string, any>): Partial<Saved> {
  if (raw.outputs || !raw.route) return raw;
  return {
    bypass: raw.bypass,
    library: raw.library,
    running: true,
    outputs: [{ id: raw.route.to, name: "", presetName: raw.presetName ?? "Flat", preset: raw.preset ?? { preampDb: 0, bands: [] } }],
  };
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
