import { useCallback, useEffect, useRef, useState } from "react";
import type { AgentApi, HeadsetApi, HeadsetFeatures, HeadsetState, AppsApi, AutoEqApi, Device, EngineApi, HotkeyApi, InfoApi, LogApi, LogEntry, Preset, SetupApi, StartupApi, Status, WindowApi } from "./api";
import { findVirtualDevice, shortName, virtualChannels } from "./devices";
import { BUILTIN } from "./builtin";
import { createAgent } from "./agent";
import { useScreen } from "./screen";
import type { AgentHost } from "./agent";
import { DEFAULT_CHANNEL, FLAT } from "./store";
import type { SavedOutput, SettingsStore } from "./store";
import { Devices, Equalizer, Log, Mixer, Plugins, Presets, Settings, Setup } from "./views";

type View = "equalizer" | "mixer" | "presets" | "devices" | "plugins" | "settings" | "log" | "setup";

const NAV: { view: View; label: string; icon: string }[] = [
  { view: "equalizer", label: "Equalizer", icon: "M4 7h9m4 0h3M4 17h3m4 0h9M15 4.5v5M9 14.5v5" },
  { view: "mixer", label: "Mixer", icon: "M6 4v7m0 4v5M12 4v3m0 4v9M18 4v9m0 4v3M4 11h4M10 7h4M16 13h4" },
  { view: "presets", label: "Presets", icon: "M5 6h14M5 12h14M5 18h9" },
  { view: "devices", label: "Devices", icon: "M5 18v-5a7 7 0 0 1 14 0v5M5 14.5h2.5v5H5zM16.5 14.5H19v5h-2.5z" },
  { view: "plugins", label: "Plugins", icon: "M9 3h6v4a2 2 0 1 0 4 0h2v6h-2a2 2 0 1 0 0 4h2v4H3v-6h2a2 2 0 1 0 0-4H3V7h6z" },
  { view: "settings", label: "Settings", icon: "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z" },
];

const SECONDARY = new Set<View>(["settings"]);
/** Pages reached from Settings keep it lit in the sidebar. */
const UNDER_SETTINGS = new Set<View>(["log", "setup"]);
const TITLES: Partial<Record<View, string>> = { log: "Log", setup: "Set up FreeSeries" };

const STATUS_TEXT: Record<Status["state"], string> = { stopped: "Stopped", running: "Running", reconnecting: "Reconnecting…" };

export default function App(p: {
  engine: EngineApi;
  win: WindowApi;
  log: LogApi;
  startup: StartupApi;
  setup: SetupApi;
  info: InfoApi;
  hotkeys: HotkeyApi;
  apps: AppsApi;
  agent: AgentApi;
  headset: HeadsetApi;
  autoEq: AutoEqApi;
  settings: SettingsStore;
  /** A development build sits next to the installed app, so it must not start playing by itself. */
  autoStart: boolean;
}) {
  const { engine, settings } = p;
  const [saved] = useState(() => settings.load());
  const [view, setView] = useState<View>(saved.outputs.length ? "equalizer" : "devices");
  const [devices, setDevices] = useState<Device[]>([]);
  const [source, setSource] = useState<string | null>(saved.source);
  const [outputs, setOutputs] = useState<SavedOutput[]>(saved.outputs);
  const [running, setRunning] = useState(saved.running && p.autoStart);
  const [autoSwitch, setAutoSwitch] = useState(saved.autoSwitch);
  const [hotkeysOn, setHotkeysOn] = useState(saved.hotkeys);
  const [mixer, setMixer] = useState(saved.channels);
  const [balance, setBalance] = useState(saved.balance);
  const [editingKey, setEditingKey] = useState("main");
  const [refused, setRefused] = useState<string[]>([]);
  const [bypass, setBypass] = useState(saved.bypass);
  const [library, setLibrary] = useState(saved.library);
  const [assignments, setAssignments] = useState(saved.assignments);
  const [dialOn, setDialOn] = useState(saved.dial);
  const [headset, setHeadset] = useState<HeadsetState>();
  const [features, setFeatures] = useState<HeadsetFeatures | null>(null);
  const [displayOn, setDisplayOn] = useState(saved.display);
  const [screen, setScreen] = useState(saved.screen);
  const [headsetSettings, setHeadsetSettings] = useState(saved.headsetSettings);
  const [agentOn, setAgentOn] = useState(saved.agent);
  const [agentToken, setAgentToken] = useState(saved.agentToken);
  const [agentUrl, setAgentUrl] = useState<string | null>(null);
  const [agentError, setAgentError] = useState<string>();
  const [status, setStatus] = useState<Status>({ state: "stopped" });
  const [entries, setEntries] = useState<(LogEntry & { time: string })[]>([]);

  // Subscribed for the whole life of the app, so the Log page has history whenever it is opened.
  useEffect(() => {
    const stop = p.log.subscribe((entry) =>
      setEntries((old) => [...old.slice(-499), { ...entry, time: new Date().toLocaleTimeString([], { hour12: false }) }]),
    );
    return () => void stop.then((unsubscribe) => unsubscribe());
  }, [p.log]);

  useEffect(() => {
    engine.setBypass(saved.bypass);
    engine.listOutputs().then((found) => {
      setDevices(found);
      // Nothing to play into yet: this PC hasn't been set up (unless the user picked their own virtual cable).
      if (!findVirtualDevice(found) && !saved.source) setView("setup");
    });
    const poll = setInterval(() => engine.snapshot().then((s) => setStatus(s.status)), 1000);
    return () => clearInterval(poll);
  }, [engine]);

  useEffect(
    () => settings.save({ source, outputs, running, autoSwitch, hotkeys: hotkeysOn, channels: mixer, balance, bypass, library, assignments, dial: dialOn, display: displayOn, screen, headsetSettings, agent: agentOn, agentToken }),
    [source, outputs, running, autoSwitch, hotkeysOn, mixer, balance, bypass, library, assignments, dialOn, displayOn, screen, headsetSettings, agentOn, agentToken],
  );
  useEffect(() => void p.hotkeys.setEnabled(hotkeysOn).then(setRefused, () => {}), [p.hotkeys, hotkeysOn]);

  // The virtual device is found by name; `source` only matters when that fails and the user picked one.
  const virtual = findVirtualDevice(devices)?.id ?? source;

  // Every channel owns its sound; an output only carries the correction for the device itself.
  // A plan whose devices are unchanged retunes the running session, so editing a curve is just a new plan.
  // One channel per freeseries virtual device. A manually picked cable (no freeseries device) is one channel too.
  const found = virtualChannels(devices);
  const channels = found.length ? found : virtual ? [{ key: "main", label: "Everything", device: devices.find((d) => d.id === virtual) ?? { id: virtual, name: "", channels: 2, sampleRate: 48000 } }] : [];
  // Sliders are positions; gains are their square, which is closer to how loudness is heard.
  const gainOf = (key: string) => {
    const c = mixer[key] ?? DEFAULT_CHANNEL;
    // The balance sits between the main channel and the second one, and only ever turns one side down.
    const pull = key === channels[1]?.key ? Math.min(0, balance) : key === "main" ? Math.min(0, -balance) : 0;
    return c.muted ? 0 : (c.volume * (1 + pull)) ** 2;
  };
  const sounds = [...BUILTIN, ...library];
  const plannedChannels = channels.map((c) => ({
    id: c.key,
    from: c.device.id,
    volume: gainOf(c.key),
    preset: (mixer[c.key] ?? DEFAULT_CHANNEL).preset,
  }));

  // With switching off the engine only ever hears about the first output, so it can't move.
  const planned = autoSwitch ? outputs : outputs.slice(0, 1);
  const latest = useRef({ planned, plannedChannels });
  latest.current = { planned, plannedChannels };
  const activeNow = status.state === "running" ? status.output : undefined;
  const idle = planned.filter((o) => o.id !== activeNow).map((o) => o.correction?.preset);
  const shape = running && plannedChannels.length && planned.length ? `${JSON.stringify(plannedChannels)}|${planned.map((o) => o.id).join("|")}|${JSON.stringify(idle)}` : null;
  useEffect(() => {
    const now = latest.current;
    engine.setPlan(shape ? { channels: now.plannedChannels, outputs: now.planned.map((o) => ({ id: o.id, preset: o.correction?.preset ?? FLAT })) } : null);
  }, [engine, shape]);

  // Windows keeps an app pinned to a channel's device whether FreeSeries runs or not, and without the
  // engine that device is a dead end. So quitting hands every app back to the Windows default
  // (`release_apps` in lib.rs), and this puts them where they belong whenever audio is running:
  // apps that were there before the quit, and apps that start later.
  const placing = useRef({ assignments, channels, adopted: false });
  placing.current.assignments = assignments;
  placing.current.channels = channels;
  const audioOn = status.state === "running";
  useEffect(() => {
    if (!audioOn) return;
    const place = async () => {
      const found = await p.apps.list().catch(() => []);
      const now = placing.current;
      const extra = now.channels.slice(1);
      // Pins from before FreeSeries kept track (or made in Windows' own volume mixer) are taken over once.
      if (!now.adopted && found.length) {
        now.adopted = true;
        const adopted = Object.fromEntries(found.flatMap((a) => (extra.find((c) => c.device.id === a.device) && !now.assignments[a.name] ? [[a.name, extra.find((c) => c.device.id === a.device)!.key]] : [])));
        if (Object.keys(adopted).length) setAssignments((old) => ({ ...adopted, ...old }));
      }
      for (const app of found) {
        const target = extra.find((c) => c.key === now.assignments[app.name])?.device.id;
        if (target && app.device !== target) p.apps.assign(app.pid, target).catch(() => {}); // not ready yet: the next round tries again
      }
    };
    place();
    const poll = setInterval(place, 5000);
    return () => clearInterval(poll);
  }, [p.apps, audioOn]);

  // The headset's own dial is the balance, for as long as there is a second channel to balance against.
  const hasBalance = channels.length > 1;
  useEffect(() => void p.headset.setDial(dialOn && hasBalance).catch(() => {}), [p.headset, dialOn, hasBalance]);
  useEffect(() => {
    p.headset.state().then(setHeadset, () => {});
    const stop = p.headset.subscribe(setHeadset);
    return () => void stop.then((unsubscribe) => unsubscribe());
  }, [p.headset]);
  // A headset appeared (or another one): learn what it can do, and send it the user's choices again, since it doesn't report them.
  const model = headset?.model ?? null;
  const chosen = useRef(headsetSettings);
  chosen.current = headsetSettings;
  useEffect(() => {
    if (!model) return setFeatures(null);
    p.headset.features().then((found) => {
      setFeatures(found);
      for (const s of found?.settings ?? []) {
        const value = chosen.current[`${found!.model}:${s.key}`];
        if (value !== undefined) p.headset.set(s.key, value).catch(() => {});
      }
    }, () => {});
  }, [p.headset, model]);

  const [dialGame, dialChat] = headset?.dial ?? [null, null];
  useEffect(() => {
    if (dialGame === null || dialChat === null || !dialOn) return;
    const turned = dialChat - dialGame; // both sides full in the middle; turning it takes one side down
    setBalance(Math.abs(turned) < 0.03 ? 0 : turned);
  }, [dialGame, dialChat, dialOn]);

  const second = channels[1]?.label;
  useScreen({
    headset: p.headset,
    idle: p.info.idleSeconds,
    features,
    state: headset,
    settings: screen,
    balance: dialOn && displayOn && second ? { value: balance, left: "Game", right: second } : null,
    sound: (mixer.main ?? DEFAULT_CHANNEL).presetName,
  });

  // Stable identities: the graph restarts its analyser loop when these change.
  const spectrum = useCallback(() => engine.spectrum(96), [engine]);
  const setAnalysing = useCallback((on: boolean) => void engine.setAnalysing(on), [engine]);

  // The output whose correction is shown and changed: the one that is playing, or the preferred one while stopped.
  const activeId = status.state === "running" ? status.output : undefined;
  const editing = outputs.find((o) => o.id === activeId) ?? outputs[0];
  // The channel the Equalizer and the Presets page work on.
  const editingChannel = channels.find((c) => c.key === editingKey) ?? channels[0];
  const sound = (editingChannel && mixer[editingChannel.key]) ?? DEFAULT_CHANNEL;

  const applyPreset = (next: Preset, name?: string) => {
    if (!editingChannel) return;
    const was = sound.presetName;
    const presetName = !next.bands.length && !next.preampDb ? "Flat" : (name ?? (was.endsWith(" (edited)") || was === "Flat" ? was : `${was} (edited)`));
    setMixer({ ...mixer, [editingChannel.key]: { ...sound, preset: next, presetName } });
  };

  /** An agent's write. A curve tuned by hand that was never saved goes into the presets first, so it can't be lost. */
  const replaceSound = (key: string, name: string, preset: Preset) => {
    const channel = mixer[key] ?? DEFAULT_CHANNEL;
    if (channel.presetName.endsWith(" (edited)")) {
      const backup = `${channel.presetName.replace(/ \(edited\)$/, "")} (before agent)`;
      setLibrary([...library.filter((n) => n.name !== backup), { name: backup, preset: channel.preset }]);
    }
    setMixer({ ...mixer, [key]: { ...channel, presetName: name, preset } });
  };
  const viewRef = useRef(view);
  viewRef.current = view;
  const analyserOff = useRef(0);

  const applyCorrection = (correction: SavedOutput["correction"]) => {
    if (!editing) return;
    setOutputs(outputs.map((o) => (o.id === editing.id ? { ...o, correction } : o)));
    if (editing.id === activeId) engine.setPreset(correction?.preset ?? FLAT);
  };

  // Shortcuts arrive while a game has the screen, so they act on the latest state without the
  // subscription being torn down and rebuilt on every render.
  const onHotkey = useRef<(action: string) => void>(() => {});
  onHotkey.current = (action) => {
    if (action === "bypass") {
      setBypass(!bypass);
      engine.setBypass(!bypass);
    } else if (editingChannel) {
      const at = sounds.findIndex((n) => n.name === sound.presetName.replace(/ \(edited\)$/, ""));
      const next = sounds[(at + (action === "next-sound" ? 1 : sounds.length - 1)) % sounds.length];
      applyPreset(next.preset, next.name);
    }
  };
  useEffect(() => {
    const stop = p.hotkeys.subscribe((action) => onHotkey.current(action));
    return () => void stop.then((unsubscribe) => unsubscribe());
  }, [p.hotkeys]);

  // An agent's tools act on whatever the state is when they are called, through the same setters as the UI.
  const agentHost = useRef<AgentHost>(null!);
  agentHost.current = {
    state: () => ({
      running: activeId !== undefined,
      bypass,
      channels: channels.map((c) => ({ key: c.key, label: c.label, ...(mixer[c.key] ?? DEFAULT_CHANNEL) })),
      output: editing ? { name: nameOf(editing.id), correction: editing.correction ?? null } : null,
      presets: sounds.map((n) => n.name),
    }),
    setSound: (key, name, preset) => replaceSound(key, name, preset),
    usePreset: (key, name) => {
      const named = name === "Flat" ? { name, preset: FLAT } : sounds.find((n) => n.name === name);
      if (named) replaceSound(key, named.name, named.preset);
      return !!named;
    },
    savePreset: (key, name) => {
      const channel = mixer[key] ?? DEFAULT_CHANNEL;
      setLibrary([...library.filter((n) => n.name !== name), { name, preset: channel.preset }]);
      setMixer({ ...mixer, [key]: { ...channel, presetName: name } });
    },
    // The analyser only runs while someone looks at it; the Equalizer page keeps it on for itself.
    spectrum: async (bands) => {
      engine.setAnalysing(true);
      clearTimeout(analyserOff.current);
      analyserOff.current = window.setTimeout(() => viewRef.current !== "equalizer" && engine.setAnalysing(false), 1500);
      return engine.spectrum(bands);
    },
    searchHeadphones: async (query) => {
      const words = query.toLowerCase().split(/\s+/).filter(Boolean);
      return (await p.autoEq.index()).filter((e) => words.every((w) => e.name.toLowerCase().includes(w))).slice(0, 20).map((e) => e.name);
    },
    useCorrection: async (name) => {
      if (name === null) return applyCorrection(null), true;
      const entry = (await p.autoEq.index()).find((e) => e.name === name);
      if (entry) applyCorrection({ name: entry.name, preset: await engine.parsePreset(await p.autoEq.fetchPreset(entry)) });
      return !!entry;
    },
  };
  useEffect(() => {
    const version = p.info.about().then((a) => a.version, () => "unknown");
    const answer = async (message: string) => createAgent(agentHost.current, await version)(message);
    const stop = p.agent.subscribe(answer);
    return () => void stop.then((unsubscribe) => unsubscribe());
  }, [p.agent, p.info]);
  useEffect(() => {
    setAgentError(undefined);
    p.agent.setEnabled(agentOn && !!agentToken, agentToken).then(setAgentUrl, (e) => (setAgentUrl(null), setAgentError(String(e))));
  }, [p.agent, agentOn, agentToken]);

  const nameOf = (id: string) => shortName(devices.find((d) => d.id === id)?.name ?? outputs.find((o) => o.id === id)?.name ?? "Unknown device");
  const statusDetail =
    status.state === "running" ? `Playing on ${nameOf(status.output)}`
    : status.state === "reconnecting" ? status.error
    : outputs.length ? "Press Start in Devices" : "Pick your outputs to start";

  return (
    <div className="window">
      <aside className="sidebar">
        <h1 className="brand" data-tauri-drag-region>
          <svg className="brand-mark" viewBox="0 0 28 28" aria-hidden="true">
            <rect width="28" height="28" rx="7" />
            <path d="M4 18h3c3.9 0 4-9 7-9s3.1 9 7 9h3" />
            <circle cx="14" cy="9" r="3" />
          </svg>
          FreeSeries
          {import.meta.env.DEV && <small>dev</small>}
        </h1>
        {/* What you use every day on top; the rest sits out of the way above the status. */}
        {[NAV.filter((n) => !SECONDARY.has(n.view) && (n.view !== "plugins" || features)), NAV.filter((n) => SECONDARY.has(n.view))].map((items, group) => (
          <nav key={group} className={group === 0 ? "nav-primary" : undefined} aria-label={group === 0 ? "Main" : "More"}>
            {items.map((n) => (
              <button
                key={n.view}
                className="nav"
                aria-current={view === n.view || (n.view === "settings" && UNDER_SETTINGS.has(view)) ? "page" : undefined}
                onClick={() => setView(n.view)}
              >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d={n.icon} />
                </svg>
                {n.label}
              </button>
            ))}
          </nav>
        ))}
        <button className="status" onClick={() => setView("devices")} title="Open Devices">
          <span className={`status-dot is-${status.state}`} />
          <span>
            <strong>{STATUS_TEXT[status.state]}</strong>
            <small>{statusDetail}</small>
          </span>
        </button>
      </aside>

      <main className="main">
        <header className="toolbar" data-tauri-drag-region>
          <h2 className="toolbar-title" data-tauri-drag-region>
            {view === "equalizer" && editingChannel ? sound.presetName : (TITLES[view] ?? NAV.find((n) => n.view === view)?.label)}
          </h2>
          {(view === "equalizer" || view === "presets") && channels.length > 1 && (
            <div className="segmented" role="radiogroup" aria-label="Channel">
              {channels.map((c) => (
                <button key={c.key} role="radio" aria-checked={c === editingChannel} onClick={() => setEditingKey(c.key)}>
                  {c.label}
                </button>
              ))}
            </div>
          )}
          {view === "equalizer" && editingChannel && (
            <>
              <div className="segmented" role="radiogroup" aria-label="Compare">
                {[false, true].map((off) => (
                  <button
                    key={String(off)}
                    role="radio"
                    aria-checked={bypass === off}
                    onClick={() => {
                      setBypass(off);
                      engine.setBypass(off);
                    }}
                  >
                    {off ? "Bypass" : "EQ on"}
                  </button>
                ))}
              </div>
              <button className="button button-quiet" onClick={() => applyPreset(FLAT)} disabled={!sound.preset.bands.length && !sound.preset.preampDb}>
                Reset
              </button>
            </>
          )}
          <div className="window-controls">
            <button onClick={p.win.minimize} aria-label="Minimize">
              <svg viewBox="0 0 10 10"><path d="M1 5h8" /></svg>
            </button>
            <button onClick={p.win.toggleMaximize} aria-label="Maximize">
              <svg viewBox="0 0 10 10"><rect x="1.5" y="1.5" width="7" height="7" /></svg>
            </button>
            <button className="is-close" onClick={p.win.close} aria-label="Close to tray" title="Close to tray. Audio keeps running.">
              <svg viewBox="0 0 10 10"><path d="M1.5 1.5l7 7m0-7l-7 7" /></svg>
            </button>
          </div>
        </header>

        {view === "equalizer" &&
          (editing && editingChannel ? (
            <Equalizer
              preset={sound.preset}
              correction={editing.correction?.preset}
              bypass={bypass}
              morphKey={`${editingChannel.key}|${sound.presetName.replace(/ \(edited\)$/, "")}`}
              onPreset={applyPreset}
              spectrum={spectrum}
              onAnalysing={setAnalysing}
              onBrowsePresets={() => setView("presets")}
            />
          ) : (
            <div className="page">
              <p className="hint">Add the headphones or speakers you listen on, then come back to tune them.</p>
              <div className="actions">
                <button className="button button-primary" onClick={() => setView("devices")}>
                  Open Devices
                </button>
              </div>
            </div>
          ))}
        {view === "mixer" && (
          <Mixer
            apps={p.apps}
            levels={engine.levels}
            presets={sounds}
            output={editing && { name: nameOf(activeId ?? editing.id), correction: editing.correction?.name ?? "No correction", playing: activeId !== undefined }}
            headset={headset?.online && headset.model && nameOf(activeId ?? editing?.id ?? "").includes(headset.model.replace(/ Wireless$/, "")) ? headset : undefined}
            channels={channels.map((c) => ({ key: c.key, label: c.label, deviceId: c.device.id, device: c.device.name.replace(/ \(.*\)$/, ""), ...(mixer[c.key] ?? DEFAULT_CHANNEL) }))} // the name to look for in an app's device list
            balance={balance}
            setup={p.setup}
            onBalance={setBalance}
            onChannel={(key, next) => setMixer({ ...mixer, [key]: next })}
            onAssign={(name, key) => setAssignments(({ [name]: _, ...rest }) => (key === "main" ? rest : { ...rest, [name]: key }))}
            onEdit={(key) => {
              setEditingKey(key);
              setView("equalizer");
            }}
            onRefresh={() => engine.listOutputs().then(setDevices)}
          />
        )}
        {view === "presets" && (
          <Presets
            autoEq={p.autoEq}
            library={library}
            current={sound.presetName}
            target={editingChannel && (channels.length > 1 ? editingChannel.label : "everything you play")}
            output={editing && nameOf(editing.id)}
            onLoad={(named) => applyPreset(named.preset, named.name)}
            sound={sound.preset}
            correction={editing?.correction ?? null}
            onCorrection={applyCorrection}
            onLoadAutoEq={async (entry) => applyCorrection({ name: entry.name, preset: await engine.parsePreset(await p.autoEq.fetchPreset(entry)) })}
            onImport={async (name, text) => applyPreset(await engine.parsePreset(text), name)}
            onSave={(name) => {
              if (!editingChannel) return;
              setLibrary([...library.filter((n) => n.name !== name), { name, preset: sound.preset }]);
              setMixer({ ...mixer, [editingChannel.key]: { ...sound, presetName: name } });
            }}
            onDelete={(name) => setLibrary(library.filter((n) => n.name !== name))}
          />
        )}
        {view === "plugins" && (
          <Plugins
            headset={features}
            state={headset}
            hasBalance={hasBalance}
            dial={dialOn}
            onDial={setDialOn}
            display={displayOn}
            onDisplay={setDisplayOn}
            screen={screen}
            onScreen={setScreen}
            headsetSettings={headsetSettings}
            onHeadsetSetting={(key, value) => {
              if (!features) return;
              setHeadsetSettings({ ...headsetSettings, [`${features.model}:${key}`]: value });
              p.headset.set(key, value).catch(() => {});
            }}
          />
        )}
        {view === "settings" && (
          <Settings
            startup={p.startup}
            info={p.info}
            problems={entries.filter((e) => e.level !== "info").length}
            hotkeys={hotkeysOn}
            refusedHotkeys={refused}
            onHotkeys={setHotkeysOn}
            agent={{ on: agentOn, url: agentUrl, token: agentToken, error: agentError }}
            onAgent={(on) => {
              if (on && !agentToken) setAgentToken(crypto.randomUUID().replace(/-/g, ""));
              setAgentOn(on);
            }}
            onSetup={() => setView("setup")}
            onLog={() => setView("log")}
            onOpenLogFolder={p.log.openFolder}
          />
        )}
        {view === "setup" && (
          <Setup
            setup={p.setup}
            detected={findVirtualDevice(devices)}
            hasOutputs={outputs.length > 0}
            onRefresh={() => engine.listOutputs().then(setDevices)}
            onDone={() => setView("devices")}
          />
        )}
        {view === "log" && <Log entries={entries} onOpenFolder={p.log.openFolder} onBack={() => setView("settings")} />}
        {view === "devices" && (
          <Devices
            devices={devices}
            detected={findVirtualDevice(devices)}
            source={source}
            outputs={outputs}
            activeId={activeId}
            running={running}
            autoSwitch={autoSwitch}
            onAutoSwitch={setAutoSwitch}
            status={status}
            onSource={setSource}
            onOutputs={setOutputs}
            onRunning={(next) => {
              setRunning(next);
              if (next) setView("equalizer"); // just started: go tune it
            }}
            onRefresh={() => engine.listOutputs().then(setDevices)}
            onSetup={() => setView("setup")}
            setup={p.setup}
          />
        )}
      </main>
    </div>
  );
}
