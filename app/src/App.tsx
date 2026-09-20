import { useCallback, useEffect, useRef, useState } from "react";
import type { AutoEqApi, Device, EngineApi, LogApi, LogEntry, Preset, SetupApi, StartupApi, Status, WindowApi } from "./api";
import { findVirtualDevice, shortName } from "./devices";
import type { SavedOutput, SettingsStore } from "./store";
import { Devices, Equalizer, Log, Presets, Settings, Setup } from "./views";

type View = "equalizer" | "presets" | "devices" | "settings" | "log" | "setup";

const NAV: { view: View; label: string; icon: string }[] = [
  { view: "equalizer", label: "Equalizer", icon: "M4 7h9m4 0h3M4 17h3m4 0h9M15 4.5v5M9 14.5v5" },
  { view: "presets", label: "Presets", icon: "M5 6h14M5 12h14M5 18h9" },
  { view: "devices", label: "Devices", icon: "M5 18v-5a7 7 0 0 1 14 0v5M5 14.5h2.5v5H5zM16.5 14.5H19v5h-2.5z" },
  { view: "settings", label: "Settings", icon: "M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z" },
  { view: "log", label: "Log", icon: "M7 4h10a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H7a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1zM9 9h6M9 13h6M9 17h3" },
];

const SECONDARY = new Set<View>(["settings", "log"]);

const STATUS_TEXT: Record<Status["state"], string> = { stopped: "Stopped", running: "Running", reconnecting: "Reconnecting…" };
const FLAT: Preset = { preampDb: 0, bands: [] };

export default function App(p: {
  engine: EngineApi;
  win: WindowApi;
  log: LogApi;
  startup: StartupApi;
  setup: SetupApi;
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
    engine.listOutputs().then((found) => {
      setDevices(found);
      // Nothing to play into yet: this PC hasn't been set up (unless the user picked their own virtual cable).
      if (!findVirtualDevice(found) && !saved.source) setView("setup");
    });
    const poll = setInterval(() => engine.snapshot().then((s) => setStatus(s.status)), 1000);
    return () => clearInterval(poll);
  }, [engine]);

  useEffect(() => settings.save({ source, outputs, running, autoSwitch, bypass, library }), [source, outputs, running, autoSwitch, bypass, library]);

  // The virtual device is found by name; `source` only matters when that fails and the user picked one.
  const virtual = findVirtualDevice(devices)?.id ?? source;

  // The engine gets a new plan when its shape changes, not on every EQ tweak: live edits go
  // through setPreset, and the engine keeps them with the output that was playing.
  // With switching off the engine only ever hears about the first output, so it can't move.
  const planned = autoSwitch ? outputs : outputs.slice(0, 1);
  const latest = useRef(planned);
  latest.current = planned;
  const activeNow = status.state === "running" ? status.output : undefined;
  const idle = planned.filter((o) => o.id !== activeNow).map((o) => o.preset);
  const shape = running && virtual && planned.length ? `${virtual}|${planned.map((o) => o.id).join("|")}|${JSON.stringify(idle)}` : null;
  useEffect(() => {
    engine.setPlan(shape && virtual ? { from: virtual, outputs: latest.current.map(({ id, preset }) => ({ id, preset })) } : null);
  }, [engine, shape]);

  // Stable identities: the graph restarts its analyser loop when these change.
  const spectrum = useCallback(() => engine.spectrum(96), [engine]);
  const setAnalysing = useCallback((on: boolean) => void engine.setAnalysing(on), [engine]);

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
      <aside className="sidebar">
        <h1 className="brand" data-tauri-drag-region>
          <svg className="brand-mark" viewBox="0 0 28 28" aria-hidden="true">
            <rect width="28" height="28" rx="7" />
            <path d="M4 18h3c3.9 0 4-9 7-9s3.1 9 7 9h3" />
            <circle cx="14" cy="9" r="3" />
          </svg>
          freeseries
          {import.meta.env.DEV && <small>dev</small>}
        </h1>
        {/* What you use every day on top; the rest sits out of the way above the status. */}
        {[NAV.filter((n) => !SECONDARY.has(n.view)), NAV.filter((n) => SECONDARY.has(n.view))].map((items, group) => (
          <nav key={group} className={group === 0 ? "nav-primary" : undefined} aria-label={group === 0 ? "Main" : "More"}>
            {items.map((n) => (
              <button key={n.view} className="nav" aria-current={view === n.view ? "page" : undefined} onClick={() => setView(n.view)}>
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
            {view === "equalizer" ? (editing?.presetName ?? "Equalizer") : view === "setup" ? "Set up freeseries" : NAV.find((n) => n.view === view)!.label}
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
            <Equalizer
              preset={editing.preset}
              bypass={bypass}
              morphKey={`${editing.id}|${editing.presetName.replace(/ \(edited\)$/, "")}`}
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
        {view === "settings" && <Settings startup={p.startup} onSetup={() => setView("setup")} />}
        {view === "setup" && (
          <Setup
            setup={p.setup}
            detected={findVirtualDevice(devices)}
            hasOutputs={outputs.length > 0}
            onRefresh={() => engine.listOutputs().then(setDevices)}
            onDone={() => setView("devices")}
          />
        )}
        {view === "log" && <Log entries={entries} onOpenFolder={p.log.openFolder} />}
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
          />
        )}
      </main>
    </div>
  );
}
