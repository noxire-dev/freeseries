// `node src/agent.check.ts`: the mic tools against a fake host, through the JSON-RPC door.
import { createAgent } from "./agent.ts";
import type { AgentHost } from "./agent.ts";
import type { Preset } from "./api.ts";

const flat: Preset = { preampDb: 0, bands: [] };
let mic: { strength: number; eq: Preset } = { strength: 1, eq: flat };
const host: AgentHost = {
  state: () => ({ running: false, bypass: false, channels: [], output: null, presets: [] }),
  setSound() {},
  usePreset: () => false,
  savePreset() {},
  spectrum: async () => null,
  searchHeadphones: async () => [],
  useCorrection: async () => false,
  mic: async () => ({ cable: true, on: true, running: true, error: null, device: "Arctis", devices: ["Arctis"], ...mic }),
  setMic: (change) => void (mic = { ...mic, ...change }),
  micSpectrum: async (bands) => Array.from({ length: bands }, (_, i) => (i === 10 ? -20 : -80)),
};
const agent = createAgent(host, "check");
const call = async (name: string, args: object) => {
  const reply = JSON.parse((await agent(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } })))!);
  const text = reply.result.content[0].text;
  return reply.result.isError ? { error: true, message: text } : { error: false, ...JSON.parse(text) };
};
const ok = (cond: unknown, what: string) => {
  if (!cond) throw new Error("FAIL: " + what);
};

const tools = JSON.parse((await agent(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list" })))!).result.tools.map((t: { name: string }) => t.name);
ok(["get_mic", "listen_mic", "set_mic"].every((t) => tools.includes(t)), "mic tools listed");
const got = await call("get_mic", {});
ok(got.virtual_microphone.startsWith("running") && got.noise_removal_strength === 1, "get_mic reads the host");
const set = await call("set_mic", { strength: 0.6, bands: [{ type: "high_shelf", fc: 3000, gain_db: 3 }] });
ok(!set.error && mic.strength === 0.6 && mic.eq.bands.length === 1 && mic.eq.preampDb < 0, "set_mic applies strength and a preamp-corrected EQ: " + JSON.stringify(set));
ok((await call("set_mic", {})).error, "set_mic with nothing is an error");
ok((await call("set_mic", { strength: "loud" })).error, "set_mic with a bad strength is an error");
const heard = await call("listen_mic", { seconds: 1 });
ok(!heard.error && heard.bands.length === 30 && heard.bands[10].db === -20 && heard.level_db > -20 && heard.level_db < -19, "listen_mic averages and sums: " + JSON.stringify({ level: heard.level_db, band10: heard.bands[10] }));
console.log("agent ok");
