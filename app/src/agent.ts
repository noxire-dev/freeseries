// What an AI agent can do in freeseries, spoken as MCP (JSON-RPC). The Rust side (`agent.rs`) is
// only the HTTP door; every message lands here, next to the state the tools change, so an agent's
// edit goes down the same path as a click in the UI.

import type { Band, Kind, Preset } from "./api";
import { DB_RANGE, F_MAX, F_MIN, clamp, combine, frequencies, totalGainDb } from "./eqmath.ts";

/** What the tools need from the app. */
export interface AgentHost {
  state(): {
    running: boolean;
    bypass: boolean;
    channels: { key: string; label: string; presetName: string; preset: Preset; volume: number; muted: boolean }[];
    output: { name: string; correction: { name: string; preset: Preset } | null } | null;
    presets: string[];
  };
  /** Replaces a channel's sound. */
  setSound(channel: string, name: string, preset: Preset): void;
  /** Loads a saved or built-in preset. False: there is none by that name. */
  usePreset(channel: string, name: string): boolean;
  /** Keeps the channel's current sound in the library under that name. */
  savePreset(channel: string, name: string): void;
  /** dBFS per log-spaced band (20 Hz to 20 kHz) of the mix right now. Null: silence. */
  spectrum(bands: number): Promise<number[] | null>;
  searchHeadphones(query: string): Promise<string[]>;
  /** Null removes the correction. False: no measurement by that name. */
  useCorrection(name: string | null): Promise<boolean>;
}

const VERSIONS = ["2025-11-25", "2025-06-18", "2025-03-26"]; // keep in step with agent.rs
const KINDS: Record<string, Kind> = { peak: "peak", low_shelf: "lowShelf", high_shelf: "highShelf" };
const KIND_NAMES = Object.fromEntries(Object.entries(KINDS).map(([name, kind]) => [kind, name]));
const MAX_BANDS = 20;
const PEAK_FREQS = frequencies(120);
const LISTEN_BANDS = 30; // about third-octave

const INSTRUCTIONS = `freeseries is a system-wide equalizer for Windows. Audio is split into channels (Game, and optionally Chat and Media); each channel has its own sound: a parametric EQ that runs after Windows spatial sound (Dolby Atmos, DTS). The output device carries a separate headphone correction (AutoEq measurements).
You cannot hear. Ask what the person listens on and what bothers them, call get_state first, and use listen while their game or music is playing to see where the energy actually is. If they name their headphones and no correction is set, offer search_headphones and use_headphone_correction before shaping the sound: a sound curve on uncorrected headphones is guesswork.
Tune with few, broad bands (Q 0.7 to 2) and changes of 1 to 4 dB; cut before you boost. Leave preamp_db out and freeseries keeps the loudest point at 0 dB so nothing clips. Every change is audible at once, so tell the person what you changed and ask what they hear. A curve the person tuned by hand and never saved is kept in the presets as "(before agent)" when you replace it.`;

const channelArg = { type: "string", description: 'Channel key or label from get_state, e.g. "main" or "Game".' };

export const TOOLS = [
  {
    name: "get_state",
    title: "Read the current setup",
    description: "Channels with their sound (bands, preamp, loudest point) and volume; the output device and its headphone correction; the names of all presets.",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "listen",
    title: "Measure what is playing",
    description: `Averages the spectrum of everything that is playing (the mix, after EQ) for a few seconds and returns dBFS in ${LISTEN_BANDS} bands from 20 Hz to 20 kHz. It shows what the content is doing, not how the headphones sound. Returns an error when nothing is playing.`,
    inputSchema: { type: "object", properties: { seconds: { type: "number", minimum: 1, maximum: 10, description: "How long to listen. Default 4." } } },
  },
  {
    name: "set_eq",
    title: "Set a channel's sound",
    description: `Replaces the channel's parametric EQ. Applies at once. At most ${MAX_BANDS} bands; values outside the limits are clamped. Returns the curve as applied and its loudest point.`,
    inputSchema: {
      type: "object",
      required: ["channel", "bands"],
      properties: {
        channel: channelArg,
        name: { type: "string", description: 'Shown in the UI, e.g. "Footsteps for Valorant". Default "Agent".' },
        preamp_db: { type: "number", description: "Leave out to have freeseries cancel the loudest boost, which is what you want unless you have a reason." },
        bands: {
          type: "array",
          items: {
            type: "object",
            required: ["type", "fc", "gain_db"],
            properties: {
              type: { type: "string", enum: Object.keys(KINDS) },
              fc: { type: "number", description: `Hz, ${F_MIN} to ${F_MAX}.` },
              gain_db: { type: "number", description: `-${DB_RANGE} to ${DB_RANGE}.` },
              q: { type: "number", description: "0.1 to 10. Default 1.41 for a peak, 0.71 for a shelf. A shelf above 0.71 overshoots: it grows a resonant bump next to fc." },
            },
          },
        },
      },
    },
  },
  {
    name: "use_preset",
    title: "Load a preset into a channel",
    description: 'Loads a built-in or saved preset by name (see get_state). "Flat" removes the EQ.',
    inputSchema: { type: "object", required: ["channel", "name"], properties: { channel: channelArg, name: { type: "string" } } },
  },
  {
    name: "save_preset",
    title: "Save a channel's sound as a preset",
    description: "Keeps the channel's current sound in the user's presets under this name, replacing a saved preset of the same name.",
    inputSchema: { type: "object", required: ["channel", "name"], properties: { channel: channelArg, name: { type: "string" } } },
  },
  {
    name: "search_headphones",
    title: "Find a headphone correction",
    description: "Searches the AutoEq measurement database by model name. Returns up to 20 exact names for use_headphone_correction.",
    inputSchema: { type: "object", required: ["query"], properties: { query: { type: "string", description: 'e.g. "Arctis Nova Pro"' } } },
  },
  {
    name: "use_headphone_correction",
    title: "Set the output's headphone correction",
    description: "Downloads and applies the AutoEq correction with exactly this name to the current output device. It assumes the headset's own EQ is off. Leave name out to remove the correction.",
    inputSchema: { type: "object", properties: { name: { type: "string" } } },
  },
];

class ToolError extends Error {}

const round = (v: number, step = 0.1) => Math.round(v / step) * step;
const tidy = (v: number) => Number(v.toFixed(2));

/** The loudest point of a curve, in dB. */
const peakDb = (preset: Preset) => Math.max(...PEAK_FREQS.map((hz) => totalGainDb(preset, hz)));

/** An agent's bands as a preset: checked, clamped, with a preamp that keeps the loudest point at 0 dB unless told otherwise. */
export function toPreset(args: Record<string, unknown>): Preset {
  if (!Array.isArray(args.bands)) throw new ToolError("bands must be an array");
  if (args.bands.length > MAX_BANDS) throw new ToolError(`at most ${MAX_BANDS} bands, got ${args.bands.length}`);
  const bands = args.bands.map((raw: Record<string, unknown>, i): Band => {
    const kind = KINDS[String(raw?.type)];
    if (!kind) throw new ToolError(`band ${i + 1}: type must be one of ${Object.keys(KINDS).join(", ")}`);
    const number = (key: string, fallback?: number) => {
      const value = raw[key] ?? fallback;
      if (typeof value !== "number" || !Number.isFinite(value)) throw new ToolError(`band ${i + 1}: ${key} must be a number`);
      return value;
    };
    return { kind, fc: Math.round(clamp(number("fc"), F_MIN, F_MAX)), gainDb: tidy(clamp(number("gain_db"), -DB_RANGE, DB_RANGE)), q: tidy(clamp(number("q", kind === "peak" ? 1.41 : 0.71), 0.1, 10)) };
  });
  if (args.preamp_db !== undefined && (typeof args.preamp_db !== "number" || !Number.isFinite(args.preamp_db))) throw new ToolError("preamp_db must be a number");
  const preampDb = args.preamp_db === undefined ? -Math.max(0, peakDb({ preampDb: 0, bands })) : clamp(args.preamp_db as number, -DB_RANGE, DB_RANGE);
  return { preampDb: tidy(round(preampDb)), bands };
}

const describe = (preset: Preset) => ({
  preamp_db: preset.preampDb,
  bands: preset.bands.map((b) => ({ type: KIND_NAMES[b.kind], fc: b.fc, gain_db: b.gainDb, q: b.q })),
});

async function call(host: AgentHost, name: string, args: Record<string, unknown>): Promise<unknown> {
  const state = host.state();
  const channel = () => {
    const wanted = String(args.channel ?? "").toLowerCase();
    const found = state.channels.find((c) => c.key === wanted || c.label.toLowerCase() === wanted);
    if (!found) throw new ToolError(`no channel "${args.channel}". There ${state.channels.length === 1 ? "is" : "are"}: ${state.channels.map((c) => `${c.key} (${c.label})`).join(", ")}`);
    return found;
  };
  const text = (key: string) => {
    const value = args[key];
    if (typeof value !== "string" || !value.trim()) throw new ToolError(`${key} must be a non-empty string`);
    return value.trim().slice(0, 80);
  };
  // What reaches the output is the correction with the sound on top, so that is what may clip.
  const headroom = (sound: Preset) => tidy(round(peakDb(combine(state.output?.correction?.preset, sound))));

  switch (name) {
    case "get_state":
      return {
        audio: state.running ? (state.bypass ? "running, but the user has the EQ bypassed" : "running") : "stopped: changes are saved but nobody hears them",
        channels: state.channels.map((c) => ({
          key: c.key,
          label: c.label,
          sound: c.presetName,
          ...describe(c.preset),
          loudest_point_db: headroom(c.preset),
          volume_db: c.muted || c.volume <= 0 ? "muted" : tidy(round(40 * Math.log10(c.volume))),
        })),
        output: state.output ? { device: state.output.name, headphone_correction: state.output.correction?.name ?? null } : null,
        presets: ["Flat", ...state.presets],
      };
    case "listen": {
      if (!state.running) throw new ToolError("audio is stopped, there is nothing to measure");
      const seconds = clamp(typeof args.seconds === "number" && Number.isFinite(args.seconds) ? args.seconds : 4, 1, 10);
      const power = new Array<number>(LISTEN_BANDS).fill(0);
      let frames = 0;
      const until = Date.now() + seconds * 1000;
      while (Date.now() < until) {
        const now = await host.spectrum(LISTEN_BANDS);
        if (now) {
          now.forEach((db, i) => (power[i] += 10 ** (db / 10)));
          frames++;
        }
        await new Promise((r) => setTimeout(r, 100));
      }
      if (!frames) throw new ToolError("nothing is playing. Ask the person to start their game or music, then listen again.");
      return { seconds, note: "dBFS of the whole mix after EQ. Shows the content, not the headphones.", bands: frequencies(LISTEN_BANDS).map((hz, i) => ({ hz: Math.round(hz), db: tidy(round(10 * Math.log10(power[i] / frames))) })) };
    }
    case "set_eq": {
      const preset = toPreset(args);
      host.setSound(channel().key, args.name === undefined ? "Agent" : text("name"), preset);
      const loudest = headroom(preset);
      return { applied: describe(preset), loudest_point_db: loudest, ...(loudest > 0.5 && { warning: `the loudest point is ${loudest} dB over full scale${state.output?.correction ? " together with the headphone correction" : ""}: the limiter will catch it, but lower preamp_db` }) };
    }
    case "use_preset":
      if (!host.usePreset(channel().key, text("name"))) throw new ToolError(`no preset "${args.name}". There are: Flat, ${state.presets.join(", ")}`);
      return { loaded: text("name"), into: channel().label };
    case "save_preset":
      host.savePreset(channel().key, text("name"));
      return { saved: text("name") };
    case "search_headphones":
      return { matches: await host.searchHeadphones(text("query")) };
    case "use_headphone_correction": {
      if (!state.output) throw new ToolError("the user has not added an output device yet (Devices page)");
      const wanted = args.name === undefined || args.name === null ? null : text("name");
      if (!(await host.useCorrection(wanted))) throw new ToolError(`no measurement called "${wanted}". Use search_headphones for the exact name.`);
      return { output: state.output.name, headphone_correction: wanted };
    }
    default:
      return undefined;
  }
}

/** Answers one JSON-RPC message. Null: it was a notification, there is nothing to say. */
export function createAgent(host: AgentHost, version: string): (message: string) => Promise<string | null> {
  return async (message) => {
    let request: { id?: unknown; method?: unknown; params?: Record<string, any> };
    try {
      request = JSON.parse(message);
    } catch {
      return JSON.stringify({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "Parse error" } });
    }
    if (Array.isArray(request) || typeof request?.method !== "string") return JSON.stringify({ jsonrpc: "2.0", id: null, error: { code: -32600, message: "Invalid request" } });
    if (request.id === undefined || request.id === null) return null;
    const result = (value: unknown) => JSON.stringify({ jsonrpc: "2.0", id: request.id, result: value });
    const error = (code: number, text: string) => JSON.stringify({ jsonrpc: "2.0", id: request.id, error: { code, message: text } });

    switch (request.method) {
      case "initialize": {
        const asked = request.params?.protocolVersion;
        return result({ protocolVersion: VERSIONS.includes(asked) ? asked : VERSIONS[0], capabilities: { tools: {} }, serverInfo: { name: "freeseries", title: "freeseries", version }, instructions: INSTRUCTIONS });
      }
      case "ping":
        return result({});
      case "tools/list":
        return result({ tools: TOOLS });
      case "tools/call": {
        const name = request.params?.name;
        if (!TOOLS.some((t) => t.name === name)) return error(-32602, `Unknown tool: ${name}`);
        try {
          return result({ content: [{ type: "text", text: JSON.stringify(await call(host, name, request.params?.arguments ?? {})) }], isError: false });
        } catch (e) {
          // The agent can fix a bad argument, so it gets to read why; anything else is ours.
          return result({ content: [{ type: "text", text: e instanceof ToolError ? e.message : `freeseries failed: ${e instanceof Error ? e.message : e}` }], isError: true });
        }
      }
      default:
        return error(-32601, `Method not found: ${request.method}`);
    }
  };
}
