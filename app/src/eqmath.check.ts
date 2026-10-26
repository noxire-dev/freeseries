// Runnable check that the drawn curve matches the engine's filters: `npm run check`.
import { bandGainDb, combine, formatPreset, totalGainDb } from "./eqmath.ts";
import { FLAT, migrate } from "./store.ts";
import { trayMenu, trayPick } from "./tray.ts";
import { adoptRenamed } from "./devices.ts";

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
const correction = { preampDb: -5.8, bands: [{ kind: "peak" as const, fc: 4875, gainDb: 6.6, q: 1.76 }] };
const sound = { preampDb: -4.7, bands: [{ kind: "lowShelf" as const, fc: 110, gainDb: -4, q: 0.71 }] };
const both = combine(correction, sound);
if (both.preampDb !== -10.5 || both.bands.length !== 2) throw new Error(`layers should add up, got ${JSON.stringify(both)}`);
if (combine(undefined, sound) !== sound) throw new Error("no correction means the sound preset as it is");
const text = formatPreset(both);
if (text !== "Preamp: -10.5 dB\nFilter 1: ON PK Fc 4875 Hz Gain 6.6 dB Q 1.76\nFilter 2: ON LSC Fc 110 Hz Gain -4.0 dB Q 0.71\n") throw new Error(`unexpected export:\n${text}`);
console.log("eqmath ok");

// Settings from before the sound moved onto the channels: a channel that named a preset keeps it,
// the main channel otherwise inherits what the output played, and outputs keep only their correction.
const curve = { preampDb: -2, bands: [] };
const stacked = migrate({ outputs: [{ id: "a", name: "A", presetName: "Mine (edited)", preset: curve, correction: null }], channels: { main: { volume: 0.5, muted: false, presetName: "PSense Game" }, chat: { volume: 1, muted: true, presetName: null } } });
if (stacked.channels!.main.presetName !== "PSense Game" || stacked.channels!.main.preset.preampDb !== -4.5 || stacked.channels!.main.volume !== 0.5) throw new Error(`a channel's own pick wins: ${JSON.stringify(stacked.channels!.main)}`);
if (stacked.channels!.chat.presetName !== "Flat" || !stacked.channels!.chat.muted) throw new Error(`chat starts flat: ${JSON.stringify(stacked.channels!.chat)}`);
if ("preset" in stacked.outputs![0]) throw new Error("outputs keep no sound");
const single = migrate({ outputs: [{ id: "a", name: "A", presetName: "Mine (edited)", preset: curve }] });
if (single.channels!.main.presetName !== "Mine (edited)" || single.channels!.main.preset !== curve) throw new Error(`main inherits the output's sound: ${JSON.stringify(single.channels)}`);
const old = migrate({ route: { to: "a" }, presetName: "Bass boost", preset: curve });
if (old.channels!.main.preset !== curve || old.outputs![0].id !== "a") throw new Error(`0.1.x settings come through: ${JSON.stringify(old)}`);
const current = { outputs: [{ id: "a", name: "A" }], channels: { main: { volume: 1, muted: false, presetName: "Flat", preset: curve } } };
if (migrate(current) !== current) throw new Error("current settings are left alone");

// The agent's door: MCP handshake, tool listing, a checked and clamped set_eq with its automatic preamp.
import { createAgent, toPreset } from "./agent.ts";
import type { AgentHost } from "./agent.ts";
const applied: { channel: string; name: string; preset: ReturnType<typeof toPreset> }[] = [];
let mic = { strength: 1, eq: { preampDb: 0, bands: [] } as ReturnType<typeof toPreset> };
const host = {
  state: () => ({ running: true, bypass: false, channels: [{ key: "main", label: "Game", presetName: "Flat", preset: { preampDb: 0, bands: [] }, volume: 1, muted: false }], output: null, presets: ["Footsteps"] }),
  setSound: (channel: string, name: string, preset: ReturnType<typeof toPreset>) => void applied.push({ channel, name, preset }),
  usePreset: () => false,
  mic: async () => ({ cable: true, on: true, running: true, error: null, device: "Arctis", devices: ["Arctis"], ...mic }),
  setMic: (change: Partial<typeof mic>) => void (mic = { ...mic, ...change }),
  micSpectrum: async (bands: number) => Array.from({ length: bands }, (_, i) => (i === 10 || i === 11 ? -20 : -80)),
} as unknown as AgentHost;
const agent = createAgent(host, "0.0.0");
const rpc = async (method: string, params?: unknown, notify = false) => JSON.parse((await agent(JSON.stringify({ jsonrpc: "2.0", ...(!notify && { id: 1 }), method, params }))) ?? "null");

const hello = await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "check", version: "0" } });
if (hello.result.protocolVersion !== "2025-06-18" || !hello.result.capabilities.tools) throw new Error(`initialize: ${JSON.stringify(hello)}`);
if ((await rpc("initialize", { protocolVersion: "1999-01-01" })).result.protocolVersion !== "2025-11-25") throw new Error("an unknown version gets our latest");
if ((await rpc("notifications/initialized", undefined, true)) !== null) throw new Error("notifications get no answer");
if ((await rpc("tools/list")).result.tools.length !== 10) throw new Error("ten tools");
if ((await rpc("nope")).error.code !== -32601) throw new Error("unknown method");
if ((await rpc("tools/call", { name: "nope" })).error.code !== -32602) throw new Error("unknown tool");

const set = await rpc("tools/call", { name: "set_eq", arguments: { channel: "game", name: "Test", bands: [{ type: "peak", fc: 3000, gain_db: 4 }, { type: "peak", fc: 5, gain_db: -40, q: 99 }, { type: "high_shelf", fc: 9000, gain_db: -2 }] } });
if (set.result.isError) throw new Error(`set_eq: ${set.result.content[0].text}`);
const made = applied[0].preset;
if (applied[0].channel !== "main" || applied[0].name !== "Test") throw new Error("set_eq finds the channel by its label");
if (made.bands[0].q !== 1.41 || made.bands[1].fc !== 20 || made.bands[1].gainDb !== -18 || made.bands[1].q !== 10 || made.bands[2].kind !== "highShelf" || made.bands[2].q !== 0.71) throw new Error(`defaults and clamps: ${JSON.stringify(made)}`);
near(made.preampDb, -4, "automatic preamp cancels the loudest boost");
near(JSON.parse(set.result.content[0].text).loudest_point_db, 0, "so the loudest point sits at 0 dB");
for (const bad of [{ channel: "nope", bands: [] }, { channel: "main", bands: [{ type: "notch", fc: 1000, gain_db: 1 }] }, { channel: "main", bands: [{ type: "peak", fc: "1k", gain_db: 1 }] }, { channel: "main", bands: new Array(21).fill({ type: "peak", fc: 1000, gain_db: 1 }) }]) {
  const refused = await rpc("tools/call", { name: "set_eq", arguments: bad });
  if (!refused.result.isError) throw new Error(`should have been refused: ${JSON.stringify(bad)}`);
}
if (applied.length !== 1) throw new Error("a refused set_eq changes nothing");
const missing = await rpc("tools/call", { name: "use_preset", arguments: { channel: "main", name: "Nope" } });
if (!missing.result.isError || !missing.result.content[0].text.includes("Footsteps")) throw new Error("a missing preset names the ones there are");
if ((await agent("{")) === null || JSON.parse((await agent("{"))!).error.code !== -32700) throw new Error("parse error");

// The mic tools: get_mic reads the host, set_mic applies the strength and a preamp-corrected EQ, listen_mic averages and sums.
const tool = async (name: string, args: object) => {
  const reply = (await rpc("tools/call", { name, arguments: args })).result;
  return reply.isError ? { error: reply.content[0].text } : JSON.parse(reply.content[0].text);
};
if (!(await tool("get_mic", {})).virtual_microphone.startsWith("running")) throw new Error("get_mic reads the host");
const micSet = await tool("set_mic", { strength: 0.6, bands: [{ type: "high_shelf", fc: 3000, gain_db: 3 }] });
if (micSet.error || mic.strength !== 0.6 || mic.eq.bands.length !== 1 || mic.eq.preampDb >= 0) throw new Error(`set_mic: ${JSON.stringify(micSet)}`);
if (!(await tool("set_mic", {})).error || !(await tool("set_mic", { strength: "loud" })).error) throw new Error("set_mic refuses nothing and nonsense");
const heard = await tool("listen_mic", { seconds: 1 });
if (heard.error || heard.bands.length !== 30 || heard.bands[10].db !== -20) throw new Error(`listen_mic: ${JSON.stringify(heard)}`);
near(heard.level_db, -17, "two -20 dB bands make a -17 dB level");
console.log("agent ok");

// The tray menu: the output in use is ticked, an edited preset still ticks the one it came from,
// no mic item without a cable, and a device id with colons in it survives the round trip.
const tray = trayMenu({ outputs: [{ id: "wasapi:{a}", name: "Arctis" }, { id: "wasapi:{b}", name: "Odyssey" }], listening: "wasapi:{b}", sounds: ["Flat", "PSense Game"], sound: "PSense Game (edited)", bypass: false, running: true, micOn: null });
const plays = tray.find((i) => i.id === "outputs")!.items!;
if (plays[0].checked || !plays[1].checked) throw new Error(`the output in use is ticked: ${JSON.stringify(plays)}`);
if (trayPick(plays[0].id).join("|") !== "output|wasapi:{a}") throw new Error(`device id round trip: ${trayPick(plays[0].id)}`);
if (!tray.find((i) => i.id === "sounds")!.items![1].checked) throw new Error("an edited preset ticks the one it came from");
if (tray.some((i) => i.id === "mic")) throw new Error("no mic item without a cable");
if (!tray.find((i) => i.id === "bypass")!.checked) throw new Error("EQ on is ticked while not bypassed");
console.log("tray ok");

// A monitor back under a new endpoint id is adopted by its saved entry (correction and all); a
// device that is already saved is not stolen; nothing moved returns the same array.
const savedOutputs = [{ id: "wasapi:{old}", name: "Odyssey G65B (NVIDIA High Definition Audio)", correction: { name: "x", preset: FLAT } }, { id: "wasapi:{a}", name: "Headphones (Arctis)" }];
const listed = [{ id: "wasapi:{new}", name: "Odyssey G65B (NVIDIA High Definition Audio)", channels: 2, sampleRate: 48000 }, { id: "wasapi:{a}", name: "Headphones (Arctis)", channels: 2, sampleRate: 48000 }];
const adopted = adoptRenamed(savedOutputs, listed);
if (adopted[0].id !== "wasapi:{new}" || adopted[0].correction?.name !== "x" || adopted[1].id !== "wasapi:{a}") throw new Error(`adoption: ${JSON.stringify(adopted)}`);
if (adoptRenamed(adopted, listed) !== adopted) throw new Error("nothing moved should return the same array");
if (adoptRenamed([{ id: "wasapi:{gone}", name: "Headphones (Arctis)" }, savedOutputs[1]], listed)[0].id !== "wasapi:{gone}") throw new Error("a saved device must not be stolen by another entry");
console.log("devices ok");
