// Everything the UI needs from outside the webview goes through these interfaces. The Tauri
// implementations are built in main.tsx; a browser preview (plain `npm run dev`) gets the mocks.

export type Kind = "peak" | "lowShelf" | "highShelf";
export interface Band { kind: Kind; fc: number; gainDb: number; q: number }
export interface Preset { preampDb: number; bands: Band[] }
export interface Device { id: string; name: string; channels: number; sampleRate: number }
/** The virtual device apps play into, and the real outputs in order of preference, each with its own EQ. */
export interface Plan {
  /** One per virtual device apps play into, each with its own volume (0 to 1, linear) and EQ. Mixed together. */
  channels: { id: string; from: string; volume: number; preset: Preset }[];
  outputs: { id: string; preset: Preset }[];
}
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
  /** Turns the analyser tap on or off. It only costs anything while it's on. */
  setAnalysing(on: boolean): Promise<void>;
  /** dBFS per log-spaced band from 20 Hz to 20 kHz of what is playing, or null while the analyser is off. */
  spectrum(bands: number): Promise<number[] | null>;
  /** Peak level per channel key, and "out" for the mix, since the last call. Linear: 1 is full scale. */
  levels(): Promise<Record<string, number>>;
  parsePreset(text: string): Promise<Preset>;
}

export interface WindowApi {
  minimize(): void;
  toggleMaximize(): void;
  close(): void;
}

export interface SetupApi {
  /** Installs the virtual audio device. Windows asks for permission first; rejects with a readable reason. */
  installDevice(): Promise<string[]>;
  /** Names the device's endpoint "freeseries". No prompt. */
  nameDevice(): Promise<string[]>;
  /** Installs the virtual device for one more channel (Media, then Chat). Windows asks for permission. */
  addChannelDevice(): Promise<string[]>;
  openSoundSettings(): Promise<void>;
  /** Id of the output Windows plays to by default. */
  defaultOutput(): Promise<string | null>;
}

export interface About { version: string; commit: string; dev: boolean }

export interface InfoApi {
  about(): Promise<About>;
}

/** The way in for an AI agent: a local MCP endpoint whose messages are answered by the UI (`agent.ts`). */
export interface AgentApi {
  /** Opens or closes the endpoint. Resolves to its URL while open. Rejects when the port is taken. */
  setEnabled(enabled: boolean, token: string): Promise<string | null>;
  /** `answer` gets every JSON-RPC message and resolves to the response, or null for a notification. */
  subscribe(answer: (message: string) => Promise<string | null>): Promise<() => void>;
}

export type HotkeyAction = "previous-sound" | "next-sound" | "bypass";

export interface HotkeyApi {
  /** Turns the global shortcuts on or off. Resolves to the ones another program already owns. */
  setEnabled(enabled: boolean): Promise<string[]>;
  /** Calls `onAction` when a shortcut is pressed, in any app. Resolves to the unsubscribe function. */
  subscribe(onAction: (action: HotkeyAction) => void): Promise<() => void>;
}

/** An app with an audio session. `device` is where it is pinned; null means it follows the Windows default. */
/** Raw pixels of an app icon: `width * height * 4` RGBA bytes. */
export interface Icon { width: number; height: number; rgba: number[] }

export interface AudioApp { pid: number; name: string; path: string; device: string | null }

/** Which app plays into which device: the per-app output setting of the Windows volume mixer. */
export interface AppsApi {
  list(): Promise<AudioApp[]>;
  /** Pins an app to a device; `null` lets it follow the default again. Windows remembers it per app. */
  assign(pid: number, device: string | null): Promise<void>;
  /** The icon Windows shows for that executable, or null. */
  icon(path: string): Promise<Icon | null>;
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
    setAnalysing: (on) => invoke("set_analysing", { on }),
    spectrum: (bands) => invoke("spectrum", { bands }),
    levels: () => invoke("levels"),
    parsePreset: (text) => invoke("parse_preset", { text }),
  };
}

export async function tauriSetup(): Promise<SetupApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  return {
    installDevice: () => invoke("install_device"),
    nameDevice: () => invoke("name_device"),
    addChannelDevice: () => invoke("add_channel_device"),
    openSoundSettings: () => invoke("open_sound_settings"),
    defaultOutput: () => invoke("default_output"),
  };
}

export async function tauriInfo(): Promise<InfoApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  return { about: () => invoke("about") };
}

export async function tauriHotkeys(): Promise<HotkeyApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  return {
    setEnabled: (enabled) => invoke("set_hotkeys", { enabled }),
    subscribe: (onAction) => listen<HotkeyAction>("hotkey", (e) => onAction(e.payload)),
  };
}

export async function tauriAgent(): Promise<AgentApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  const { listen } = await import("@tauri-apps/api/event");
  return {
    setEnabled: (enabled, token) => invoke("set_agent", { enabled, token }),
    subscribe: (answer) =>
      listen<{ id: number; message: string }>("agent-request", (e) =>
        answer(e.payload.message).then(
          (message) => invoke("agent_reply", { id: e.payload.id, message }),
          () => {}, // no reply: the endpoint answers 503 by itself
        ),
      ),
  };
}

export async function tauriApps(): Promise<AppsApi> {
  const { invoke } = await import("@tauri-apps/api/core");
  return { list: () => invoke("audio_apps"), assign: (pid, device) => invoke("assign_app", { pid, device }), icon: (path) => invoke("app_icon", { path }) };
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

// Browser preview state. `?fresh` previews a PC where nothing is set up yet.
const preview = { installed: !location.search.includes("fresh"), isDefault: !location.search.includes("fresh"), media: location.search.includes("media") };
const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const mockSetup = (): SetupApi => ({
  installDevice: async () => {
    await wait(1400);
    preview.installed = true;
    return ["driver installed", "endpoint: 'Speakers' -> 'freeseries'"];
  },
  nameDevice: async () => ["endpoint: 'Speakers' -> 'freeseries'"],
  addChannelDevice: async () => {
    await wait(1400);
    preview.media = true;
    return ["driver installed", "endpoint: 'Speakers' -> 'freeseries Media'"];
  },
  openSoundSettings: async () => void wait(2500).then(() => (preview.isDefault = true)), // "the user" picks it
  defaultOutput: async () => (preview.installed && preview.isDefault ? "wasapi:fx" : "wasapi:rtk"),
});

/** Browser preview: a fake engine with the devices of a typical setup. */
export function mockEngine(): EngineApi {
  let state: Snapshot = { status: { state: "stopped" }, bypass: false };
  const kinds: Record<string, Kind> = { PK: "peak", LSC: "lowShelf", LS: "lowShelf", HSC: "highShelf", HS: "highShelf" };
  return {
    listOutputs: async () => [
      ...(preview.installed ? [{ id: "wasapi:fx", name: "freeseries (FxSound Audio Enhancer)", channels: 2, sampleRate: 48000 }] : []),
      ...(preview.installed && preview.media ? [{ id: "wasapi:fx2", name: "freeseries Media (2- FxSound Audio Enhancer)", channels: 2, sampleRate: 48000 }] : []),
      { id: "wasapi:hdmi", name: "Odyssey G65B (NVIDIA High Definition Audio)", channels: 2, sampleRate: 48000 },
      { id: "wasapi:rtk", name: "Speakers (Realtek(R) Audio)", channels: 2, sampleRate: 48000 },
      { id: "wasapi:arctis", name: "Headphones (Arctis Nova Pro Wireless)", channels: 2, sampleRate: 48000 },
    ],
    snapshot: async () => state,
    setPlan: async (plan) => void (state = { ...state, status: plan?.outputs.length ? { state: "running", output: plan.outputs[0].id } : { state: "stopped" } }),
    setPreset: async () => {},
    setBypass: async (bypass) => void (state = { ...state, bypass }),
    setAnalysing: async () => {},
    // Something that moves like a game, a voice and the two together.
    levels: async (): Promise<Record<string, number>> => {
      if (state.status.state !== "running") return {};
      const t = performance.now() / 1000;
      const main = 0.35 + 0.25 * Math.abs(Math.sin(t * 5.3)) * Math.abs(Math.sin(t * 0.9));
      const media = Math.sin(t * 0.6) > 0 ? 0.12 + 0.2 * Math.abs(Math.sin(t * 9)) : 0.002;
      return { main, media, chat: 0, out: Math.min(1, main + media) };
    },
    // Something that moves like music: a pink-ish slope, a bass pulse, a wandering presence bump.
    spectrum: async (bands) => {
      if (state.status.state !== "running") return null;
      const t = performance.now() / 1000;
      return Array.from({ length: bands }, (_, i) => {
        const octave = (i / bands) * 10;
        const bass = 14 * Math.exp(-((octave - 1.6) ** 2)) * (0.6 + 0.4 * Math.sin(t * 7));
        const presence = 9 * Math.exp(-((octave - 6.5 - Math.sin(t * 0.7)) ** 2) / 1.5);
        return -34 - octave * 3.2 + bass + presence + 4 * Math.sin(i * 1.7 + t * 9);
      });
    },
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

export const mockInfo = (): InfoApi => ({ about: async () => ({ version: "0.0.0", commit: "preview", dev: true }) });

/** No endpoint in the browser preview; `window.agent(message)` talks to the same handler instead. */
export const mockAgent = (): AgentApi => ({
  setEnabled: async (enabled) => (enabled ? "http://127.0.0.1:47630/mcp" : null),
  subscribe: async (answer) => {
    (window as any).agent = answer;
    return () => void ((window as any).agent === answer && delete (window as any).agent); // a later subscriber stays
  },
});

export const mockHotkeys = (): HotkeyApi => ({ setEnabled: async () => [], subscribe: async () => () => {} });

export function mockApps(): AppsApi {
  const apps: AudioApp[] = [
    { pid: 101, name: "Discord", path: "", device: null },
    { pid: 102, name: "Discord", path: "", device: null },
    { pid: 201, name: "chrome", path: "", device: null },
    { pid: 301, name: "Spotify", path: "", device: null },
    { pid: 401, name: "VALORANT-Win64-Shipping", path: "", device: null },
  ];
  return { list: async () => apps.map((a) => ({ ...a })), assign: async (pid, device) => void (apps.find((a) => a.pid === pid)!.device = device), icon: async () => null };
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
