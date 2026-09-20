import type { Preset } from "./api";

export interface NamedPreset { name: string; preset: Preset }

/** One of the user's real outputs, with the EQ that belongs to it. */
export interface SavedOutput { id: string; name: string; presetName: string; preset: Preset }

export interface Saved {
  /** Only set when the virtual device couldn't be found by name and the user picked one. */
  source: string | null;
  /** Most preferred first. */
  outputs: SavedOutput[];
  running: boolean;
  bypass: boolean;
  library: NamedPreset[];
}

export interface SettingsStore {
  load(): Saved;
  save(saved: Saved): void;
}

const EMPTY: Saved = { source: null, outputs: [], running: false, bypass: false, library: [] };

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
