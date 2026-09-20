import { useEffect, useRef, useState } from "react";
import type { AutoEqApi, Device, EngineApi, Preset, Route, Status, WindowApi } from "./api";
import type { SettingsStore } from "./store";
import { Devices, Equalizer, Presets } from "./views";

type View = "equalizer" | "presets" | "devices";

const NAV: { view: View; label: string; icon: string }[] = [
  { view: "equalizer", label: "Equalizer", icon: "M4 7h9m4 0h3M4 17h3m4 0h9M15 4.5v5M9 14.5v5" },
  { view: "presets", label: "Presets", icon: "M5 6h14M5 12h14M5 18h9" },
  { view: "devices", label: "Devices", icon: "M5 18v-5a7 7 0 0 1 14 0v5M5 14.5h2.5v5H5zM16.5 14.5H19v5h-2.5z" },
];

const STATUS_TEXT: Record<Status["state"], string> = { stopped: "Stopped", running: "Running", reconnecting: "Reconnecting…" };

export default function App(p: { engine: EngineApi; win: WindowApi; autoEq: AutoEqApi; settings: SettingsStore }) {
  const { engine, settings } = p;
  const [saved] = useState(() => settings.load());
  const [view, setView] = useState<View>(saved.route ? "equalizer" : "devices");
  const [devices, setDevices] = useState<Device[]>([]);
  const [route, setRoute] = useState<Route | null>(saved.route);
  const [preset, setPreset] = useState<Preset>(saved.preset);
  const [presetName, setPresetName] = useState(saved.presetName);
  const [bypass, setBypass] = useState(saved.bypass);
  const [library, setLibrary] = useState(saved.library);
  const [status, setStatus] = useState<Status>({ state: "stopped" });

  // The engine starts empty on every launch; hand it what was in use last time.
  useEffect(() => {
    engine.setPreset(saved.preset);
    engine.setBypass(saved.bypass);
    engine.setRoute(saved.route);
    engine.listOutputs().then(setDevices);
    const poll = setInterval(() => engine.snapshot().then((s) => setStatus(s.status)), 1000);
    return () => clearInterval(poll);
  }, [engine]);

  useEffect(() => settings.save({ route, preset, presetName, bypass, library }), [route, preset, presetName, bypass, library]);

  // Dragging a band fires on every pointer move; the engine only needs the latest per frame.
  const pending = useRef<number>(0);
  const applyPreset = (next: Preset, name = presetName.endsWith(" (edited)") || presetName === "Flat" ? presetName : `${presetName} (edited)`) => {
    setPreset(next);
    setPresetName(next.bands.length || next.preampDb ? name : "Flat");
    cancelAnimationFrame(pending.current);
    pending.current = requestAnimationFrame(() => engine.setPreset(next));
  };

  // "Speakers (FxSound Audio Enhancer)" -> "FxSound Audio Enhancer": the part in brackets is the one that tells devices apart.
  const deviceName = (id: string) => {
    const name = devices.find((d) => d.id === id)?.name ?? "Unknown device";
    return name.match(/\((?:\d+- )?(.+)\)$/)?.[1] ?? name;
  };

  return (
    <div className="window">
      <aside className="sidebar" data-tauri-drag-region>
        <h1 className="brand" data-tauri-drag-region>
          freeseries
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
            <small>{route ? `${deviceName(route.from)} to ${deviceName(route.to)}` : "Pick your devices to start"}</small>
          </span>
        </button>
      </aside>

      <main className="main">
        <header className="toolbar" data-tauri-drag-region>
          <h2 className="toolbar-title" data-tauri-drag-region>
            {view === "equalizer" ? presetName : NAV.find((n) => n.view === view)!.label}
          </h2>
          {view === "equalizer" && (
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
              <button className="button button-quiet" onClick={() => applyPreset({ preampDb: 0, bands: [] }, "Flat")} disabled={!preset.bands.length && !preset.preampDb}>
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
            <button className="is-close" onClick={p.win.close} aria-label="Close">
              <svg viewBox="0 0 10 10"><path d="M1.5 1.5l7 7m0-7l-7 7" /></svg>
            </button>
          </div>
        </header>

        {view === "equalizer" && <Equalizer preset={preset} bypass={bypass} onPreset={applyPreset} />}
        {view === "presets" && (
          <Presets
            autoEq={p.autoEq}
            library={library}
            current={presetName}
            onLoad={(named) => {
              applyPreset(named.preset, named.name);
              setView("equalizer");
            }}
            onLoadAutoEq={async (entry) => {
              applyPreset(await engine.parsePreset(await p.autoEq.fetchPreset(entry)), entry.name);
              setView("equalizer");
            }}
            onSave={(name) => {
              setLibrary([...library.filter((n) => n.name !== name), { name, preset }]);
              setPresetName(name);
            }}
            onDelete={(name) => setLibrary(library.filter((n) => n.name !== name))}
          />
        )}
        {view === "devices" && (
          <Devices
            devices={devices}
            route={route}
            status={status}
            onRefresh={() => engine.listOutputs().then(setDevices)}
            onRoute={(next) => {
              setRoute(next);
              engine.setRoute(next);
              if (next && !route) setView("equalizer"); // just started: go tune it
            }}
          />
        )}
      </main>
    </div>
  );
}
