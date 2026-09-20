import { useEffect, useRef, useState } from "react";
import type { AutoEqApi, Device, EngineApi, LogApi, LogEntry, Preset, StartupApi, Status, WindowApi } from "./api";
import { findVirtualDevice, shortName } from "./devices";
import type { SavedOutput, SettingsStore } from "./store";
import { Devices, Equalizer, Log, Presets, Settings } from "./views";

type View = "equalizer" | "presets" | "devices" | "settings" | "log";

const NAV: { view: View; label: string; icon: string }[] = [
  { view: "equalizer", label: "Equalizer", icon: "M4 7h9m4 0h3M4 17h3m4 0h9M15 4.5v5M9 14.5v5" },
  { view: "presets", label: "Presets", icon: "M5 6h14M5 12h14M5 18h9" },
  { view: "devices", label: "Devices", icon: "M5 18v-5a7 7 0 0 1 14 0v5M5 14.5h2.5v5H5zM16.5 14.5H19v5h-2.5z" },
  { view: "settings", label: "Settings", icon: "M12 8.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7zM12 3v2.5M12 18.5V21M3 12h2.5M18.5 12H21M5.6 5.6l1.8 1.8M16.6 16.6l1.8 1.8M18.4 5.6l-1.8 1.8M7.4 16.6l-1.8 1.8" },
  { view: "log", label: "Log", icon: "M7 4h10a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1zM9 9h6M9 13h6M9 17h3" },
];

const STATUS_TEXT: Record<Status["state"], string> = { stopped: "Stopped", running: "Running", reconnecting: "Reconnecting…" };
const FLAT: Preset = { preampDb: 0, bands: [] };

export default function App(p: {
  engine: EngineApi;
  win: WindowApi;
  log: LogApi;
  startup: StartupApi;
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
  const [bypass, setBypass] = useState(saved.bypass);
  const [library, setLibrary] = useState(saved.library);
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
    engine.listOutputs().then(setDevices);
    const poll = setInterval(() => engine.snapshot().then((s) => setStatus(s.status)), 1000);
    return () => clearInterval(poll);
  }, [engine]);

  useEffect(() => settings.save({ source, outputs, running, bypass, library }), [source, outputs, running, bypass, library]);

  // The virtual device is found by name; `source` only matters when that fails and the user picked one.
  const virtual = findVirtualDevice(devices)?.id ?? source;

  // The engine gets a new plan when its shape changes, not on every EQ tweak: live edits go
  // through setPreset, and the engine keeps them with the output that was playing.
  const latest = useRef(outputs);
  latest.current = outputs;
  const activeNow = status.state === "running" ? status.output : undefined;
  const idle = outputs.filter((o) => o.id !== activeNow).map((o) => o.preset);
  const shape = running && virtual && outputs.length ? `${virtual}|${outputs.map((o) => o.id).join("|")}|${JSON.stringify(idle)}` : null;
  useEffect(() => {
    engine.setPlan(shape && virtual ? { from: virtual, outputs: latest.current.map(({ id, preset }) => ({ id, preset })) } : null);
  }, [engine, shape]);

  // What the editor shows: the output that is playing, or the preferred one while stopped.
  const activeId = status.state === "running" ? status.output : undefined;
  const editing = outputs.find((o) => o.id === activeId) ?? outputs[0];

  // Dragging a band fires on every pointer move; the engine only needs the latest per frame.
  const pending = useRef<number>(0);
  const applyPreset = (next: Preset, name?: string) => {
    if (!editing) return;
    const was = editing.presetName;
    const presetName = !next.bands.length && !next.preampDb ? "Flat" : (name ?? (was.endsWith(" (edited)") || was === "Flat" ? was : `${was} (edited)`));
    setOutputs(outputs.map((o) => (o.id === editing.id ? { ...o, preset: next, presetName } : o)));
    if (editing.id === activeId) {
      cancelAnimationFrame(pending.current);
      pending.current = requestAnimationFrame(() => engine.setPreset(next));
    }
  };

  const nameOf = (id: string) => shortName(devices.find((d) => d.id === id)?.name ?? outputs.find((o) => o.id === id)?.name ?? "Unknown device");
  const statusDetail =
    status.state === "running" ? `Playing on ${nameOf(status.output)}`
    : status.state === "reconnecting" ? status.error
    : outputs.length ? "Press Start in Devices" : "Pick your outputs to start";

  return (
    <div className="window">
      <aside className="sidebar" data-tauri-drag-region>
        <h1 className="brand" data-tauri-drag-region>
          freeseries
          {import.meta.env.DEV && <small>dev</small>}
        </h1>
        <nav>
          {NAV.map((n) => (
            <button key={n.view} className="nav" aria-current={view === n.view ? "page" : undefined} onClick={() => setView(n.view)}>
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d={n.icon} />
              </svg>
              {n.label}
            </button>
          ))}
        </nav>
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
            {view === "equalizer" ? (editing?.presetName ?? "Equalizer") : NAV.find((n) => n.view === view)!.label}
            {view === "equalizer" && editing && <small> for {nameOf(editing.id)}</small>}
          </h2>
          {view === "equalizer" && editing && (
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
              <button className="button button-quiet" onClick={() => applyPreset(FLAT)} disabled={!editing.preset.bands.length && !editing.preset.preampDb}>
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
          (editing ? (
            <Equalizer preset={editing.preset} bypass={bypass} onPreset={applyPreset} />
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
        {view === "presets" && (
          <Presets
            autoEq={p.autoEq}
            library={library}
            current={editing?.presetName ?? ""}
            target={editing ? nameOf(editing.id) : undefined}
            onLoad={(named) => {
              applyPreset(named.preset, named.name);
              setView("equalizer");
            }}
            onLoadAutoEq={async (entry) => {
              applyPreset(await engine.parsePreset(await p.autoEq.fetchPreset(entry)), entry.name);
              setView("equalizer");
            }}
            onSave={(name) => {
              if (!editing) return;
              setLibrary([...library.filter((n) => n.name !== name), { name, preset: editing.preset }]);
              setOutputs(outputs.map((o) => (o.id === editing.id ? { ...o, presetName: name } : o)));
            }}
            onDelete={(name) => setLibrary(library.filter((n) => n.name !== name))}
          />
        )}
        {view === "settings" && <Settings startup={p.startup} />}
        {view === "log" && <Log entries={entries} onOpenFolder={p.log.openFolder} />}
        {view === "devices" && (
          <Devices
            devices={devices}
            detected={findVirtualDevice(devices)}
            source={source}
            outputs={outputs}
            activeId={activeId}
            running={running}
            status={status}
            onSource={setSource}
            onOutputs={setOutputs}
            onRunning={(next) => {
              setRunning(next);
              if (next) setView("equalizer"); // just started: go tune it
            }}
            onRefresh={() => engine.listOutputs().then(setDevices)}
          />
        )}
      </main>
    </div>
  );
}
