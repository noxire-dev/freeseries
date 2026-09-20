// Everything the UI needs from outside the webview goes through these interfaces. The Tauri
// implementations are built in main.tsx; a browser preview (plain `npm run dev`) gets the mocks.

export type Kind = "peak" | "lowShelf" | "highShelf";
export interface Band { kind: Kind; fc: number; gainDb: number; q: number }
export interface Preset { preampDb: number; bands: Band[] }
export interface Device { id: string; name: string; channels: number; sampleRate: number }
/** The virtual device apps play into, and the real outputs in order of preference, each with its own EQ. */
export interface Plan { from: string; outputs: { id: string; preset: Preset }[] }
export type Status = { state: "stopped" } | { state: "running"; output: string } | { state: "reconnecting"; error: string };
export interface Snapshot { status: Status; bypass: boolean }

export interface EngineApi {
  listOutputs(): Promise<Device[]>;
  snapshot(): Promise<Snapshot>;
  /** `null` stops audio. The engine plays on the first output that is switched on and moves by itself. */
  setPlan(plan: Plan | null): Promise<void>;
  /** Live EQ edit for the output that is playing. */
  setPreset(preset: Preset): Promise<void>;
  setBypass(bypass: boolean): Promise<void>;
  parsePreset(text: string): Promise<Preset>;
}

export interface WindowApi {
  minimize(): void;
  toggleMaximize(): void;
  close(): void;
}

export interface StartupApi {
  startsWithWindows(): Promise<boolean>;
  /** Rejects with a readable message when it can't be changed (e.g. a development build). */
  setStartsWithWindows(enabled: boolean): Promise<void>;
}

export type LogLevel = "info" | "warn" | "error";
export interface LogEntry { level: LogLevel; message: string }

export interface LogApi {
  /** Calls `onEntry` for every engine log line from now on. Resolves to the unsubscribe function. */
  subscribe(onEntry: (entry: LogEntry) => void): Promise<() => void>;
  openFolder(): Promise<void>;
}

export interface AutoEqEntry { name: string; source: string; path: string }

/** The AutoEq headphone database on GitHub. Fetched on demand, nothing is bundled. */
export interface AutoEqApi {
  index(): Promise<AutoEqEntry[]>;
  fetchPreset(entry: AutoEqEntry): Promise<string>;
}

const AUTOEQ = "https://raw.githubusercontent.com/jaakkopasanen/AutoEq/master/results";

export function githubAutoEq(): AutoEqApi {
  let cached: Promise<AutoEqEntry[]> | undefined;
  return {
    index: () =>
      (cached ??= fetch(`${AUTOEQ}/INDEX.md`)
        .then((r) => (r.ok ? r.text() : Promise.reject(new Error(`AutoEq index: HTTP ${r.status}`))))
        .then(parseAutoEqIndex)
        .catch((e) => {
          cached = undefined; // let the next search retry
          throw e;
        })),
    fetchPreset: async (entry) => {
      const file = `${entry.path}/${encodeURIComponent(entry.name)}%20ParametricEQ.txt`;
      const r = await fetch(`${AUTOEQ}/${file}`);
      if (!r.ok) throw new Error(`AutoEq preset: HTTP ${r.status}`);
      return r.text();
    },
  };
}

/** Lines look like `- [Name](./oratory1990/over-ear/Name) by oratory1990 on GRAS 43AG-7`. */
export function parseAutoEqIndex(markdown: string): AutoEqEntry[] {
  const entries: AutoEqEntry[] = [];
  for (const m of markdown.matchAll(/^- \[(.+?)\]\(\.\/(.+?)\)\s+by\s+(.+)$/gm)) {
    entries.push({ name: m[1], path: m[2], source: m[3].trim() });
  }
  return entries;
}

export async function tauriEngine(): Promise<EngineApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  return {
    listOutputs: () => invoke("list_outputs"),
    snapshot: () => invoke("snapshot"),
    setPlan: (plan) => invoke("set_plan", { plan }),
    setPreset: (preset) => invoke("set_preset", { preset }),
    setBypass: (bypass) => invoke("set_bypass", { bypass }),
    parsePreset: (text) => invoke("parse_preset", { text }),
  };
}

export async function tauriStartup(): Promise<StartupApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  return {
    startsWithWindows: () => invoke("starts_with_windows"),
    setStartsWithWindows: (enabled) => invoke("set_starts_with_windows", { enabled }),
  };
}

export async function tauriLog(): Promise<LogApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { attachLogger } = await import("@tauri-apps/plugin-log");
  // Plugin levels are numeric: 1 trace, 2 debug, 3 info, 4 warn, 5 error.
  const level = (n: number): LogLevel => (n >= 5 ? "error" : n === 4 ? "warn" : "info");
  return {
    // Lines arrive as "[date][time][target][LEVEL] message"; the Log page has its own time column.
    subscribe: (onEntry) => attachLogger((r) => onEntry({ level: level(r.level), message: r.message.replace(/^(\[[^\]]*\])+\s*/, "") })),
    openFolder: () => invoke("open_log_folder"),
  };
}

export async function tauriWindow(): Promise<WindowApi> {
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const w = getCurrentWindow();
  return { minimize: () => void w.minimize(), toggleMaximize: () => void w.toggleMaximize(), close: () => void w.close() };
}

/** Browser preview: a fake engine with the devices of a typical setup. */
export function mockEngine(): EngineApi {
  let state: Snapshot = { status: { state: "stopped" }, bypass: false };
  const kinds: Record<string, Kind> = { PK: "peak", LSC: "lowShelf", LS: "lowShelf", HSC: "highShelf", HS: "highShelf" };
  return {
    listOutputs: async () => [
      { id: "wasapi:fx", name: "Speakers (FxSound Audio Enhancer)", channels: 2, sampleRate: 48000 },
      { id: "wasapi:hdmi", name: "Odyssey G65B (NVIDIA High Definition Audio)", channels: 2, sampleRate: 48000 },
      { id: "wasapi:rtk", name: "Speakers (Realtek(R) Audio)", channels: 2, sampleRate: 48000 },
      { id: "wasapi:arctis", name: "Headphones (Arctis Nova Pro Wireless)", channels: 2, sampleRate: 48000 },
    ],
    snapshot: async () => state,
    setPlan: async (plan) => void (state = { ...state, status: plan?.outputs.length ? { state: "running", output: plan.outputs[0].id } : { state: "stopped" } }),
    setPreset: async () => {},
    setBypass: async (bypass) => void (state = { ...state, bypass }),
    parsePreset: async (text) => {
      const preset: Preset = { preampDb: 0, bands: [] };
      for (const line of text.split("\n")) {
        const pre = line.match(/^Preamp:\s*(-?[\d.]+)/i);
        if (pre) preset.preampDb = Number(pre[1]);
        const f = line.match(/^Filter.*?ON\s+(\w+)\s+Fc\s+([\d.]+)\s+Hz\s+Gain\s+(-?[\d.]+)\s+dB(?:\s+Q\s+([\d.]+))?/i);
        if (f && kinds[f[1]]) preset.bands.push({ kind: kinds[f[1]], fc: Number(f[2]), gainDb: Number(f[3]), q: Number(f[4] ?? 0.707) });
      }
      return preset;
    },
  };
}

export function mockStartup(): StartupApi {
  let on = false;
  return { startsWithWindows: async () => on, setStartsWithWindows: async (enabled) => void (on = enabled) };
}

export const mockLog = (): LogApi => ({
  subscribe: async (onEntry) => {
    onEntry({ level: "info", message: "[preview] opening FxSound Audio Enhancer (2 ch) -> Arctis Nova Pro Wireless (2 ch) at 48000 Hz" });
    onEntry({ level: "info", message: "[preview] engine Running" });
    onEntry({ level: "warn", message: "[preview] capture glitch: A buffer underrun or overrun occurred." });
    onEntry({ level: "error", message: "[preview] capture stream died: device invalidated" });
    return () => {};
  },
  openFolder: async () => {},
});

export const mockWindow = (): WindowApi => ({ minimize() {}, toggleMaximize() {}, close() {} });
