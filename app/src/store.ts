import type { Preset, Route } from "./api";

export interface NamedPreset { name: string; preset: Preset }

export interface Saved {
  route: Route | null;
  preset: Preset;
  presetName: string;
  bypass: boolean;
  library: NamedPreset[];
}

export interface SettingsStore {
  load(): Saved;
  save(saved: Saved): void;
}

const EMPTY: Saved = { route: null, preset: { preampDb: 0, bands: [] }, presetName: "Flat", bypass: false, library: [] };

export function localSettings(key = "freeseries"): SettingsStore {
  return {
    load() {
      try {
        return { ...EMPTY, ...JSON.parse(localStorage.getItem(key) ?? "{}") };
      } catch {
        return EMPTY; // unreadable settings start fresh rather than block the app
      }
    },
    save: (saved) => localStorage.setItem(key, JSON.stringify(saved)),
  };
}
