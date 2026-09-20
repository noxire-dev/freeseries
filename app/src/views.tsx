import { useEffect, useMemo, useRef, useState } from "react";
import type { AutoEqApi, AutoEqEntry, Band, Device, Kind, LogEntry, Preset, StartupApi, Status } from "./api";
import EqGraph, { bandColor } from "./EqGraph";
import { DB_RANGE, F_MAX, F_MIN, clamp, formatDb, frequencies, totalGainDb } from "./eqmath";
import { BUILTIN } from "./builtin";
import type { NamedPreset, SavedOutput } from "./store";

const KINDS: { kind: Kind; label: string }[] = [
  { kind: "lowShelf", label: "Low shelf" },
  { kind: "peak", label: "Peak" },
  { kind: "highShelf", label: "High shelf" },
];
const PEAK_FREQS = frequencies(120);

function NumberField(p: { label: string; unit: string; value: number; min: number; max: number; step: number; onChange(v: number): void }) {
  return (
    <label className="field">
      <span className="field-label">{p.label}</span>
      <span className="field-input">
        <input
          type="number"
          value={p.value}
          min={p.min}
          max={p.max}
          step={p.step}
          onChange={(e) => Number.isFinite(e.target.valueAsNumber) && p.onChange(clamp(e.target.valueAsNumber, p.min, p.max))}
        />
        <span className="field-unit">{p.unit}</span>
      </span>
    </label>
  );
}

const PREAMP_TICKS = [18, 12, 6, 0, -6, -12, -18];
const MORPH_MS = 360;

const mix = (a: number, b: number, t: number) => a + (b - a) * t;
const mixLog = (a: number, b: number, t: number) => Math.exp(mix(Math.log(a), Math.log(b), t));

/** Blend between two presets. A band that exists on one side only grows from, or shrinks to, 0 dB. */
function blend(from: Preset, to: Preset, t: number): Preset {
  const count = Math.max(from.bands.length, to.bands.length);
  const bands = Array.from({ length: count }, (_, i): Band => {
    const a = from.bands[i] ?? { ...to.bands[i], gainDb: 0 };
    const b = to.bands[i] ?? { ...from.bands[i], gainDb: 0 };
    return { kind: b.kind, fc: mixLog(a.fc, b.fc, t), gainDb: mix(a.gainDb, b.gainDb, t), q: mixLog(a.q, b.q, t) };
  });
  return { preampDb: mix(from.preampDb, to.preampDb, t), bands };
}

/**
 * What the graph draws. Follows `preset` instantly while the user edits, but when `key` changes
 * (another preset or another output was loaded) the curve travels there, so the change is visible.
 */
function useMorph(preset: Preset, key: string): { shown: Preset; morphing: boolean } {
  const [shown, setShown] = useState(preset);
  const [morphing, setMorphing] = useState(false);
  const state = useRef({ key, shown: preset });
  state.current.shown = shown;

  useEffect(() => {
    const still = key === state.current.key || matchMedia("(prefers-reduced-motion: reduce)").matches;
    state.current.key = key;
    if (still) {
      setShown(preset);
      return;
    }
    const from = state.current.shown;
    const began = performance.now();
    let frame = 0;
    setMorphing(true);
    const step = (now: number) => {
      const t = Math.min(1, (now - began) / MORPH_MS);
      if (t < 1) {
        setShown(blend(from, preset, 1 - 2 ** (-10 * t))); // exponential ease-out
        frame = requestAnimationFrame(step);
      } else {
        setShown(preset);
        setMorphing(false);
      }
    };
    frame = requestAnimationFrame(step);
    // A hidden window (freeseries lives in the tray) gets no animation frames, and the engine can
    // switch outputs while it's hidden. The timer makes sure the morph always ends.
    const deadline = setTimeout(() => {
      cancelAnimationFrame(frame);
      setShown(preset);
      setMorphing(false);
    }, MORPH_MS + 80);
    return () => {
      cancelAnimationFrame(frame);
      clearTimeout(deadline);
      setMorphing(false);
    };
  }, [preset, key]);

  return { shown, morphing };
}

/** The middle (on a log scale) of the widest stretch of spectrum that has no band yet. */
function emptiestFrequency(bands: Band[]): number {
  const marks = [F_MIN, ...bands.map((b) => b.fc).sort((a, b) => a - b), F_MAX];
  let best = { ratio: 0, fc: 1000 };
  for (let i = 1; i < marks.length; i++) {
    const ratio = marks[i] / marks[i - 1];
    if (ratio > best.ratio) best = { ratio, fc: Math.round(Math.sqrt(marks[i] * marks[i - 1])) };
  }
  return bands.length ? best.fc : 1000;
}

export function Equalizer(p: { preset: Preset; bypass: boolean; morphKey: string; onPreset(preset: Preset): void; onBrowsePresets(): void }) {
  const { preset, onPreset } = p;
  const [selected, setSelected] = useState<number | null>(null);
  const { shown, morphing } = useMorph(preset, p.morphKey);
  useEffect(() => setSelected(null), [p.morphKey]);

  const band = selected !== null ? preset.bands[selected] : undefined;
  const setBand = (i: number, b: Band) => onPreset({ ...preset, bands: preset.bands.map((old, j) => (j === i ? b : old)) });
  const add = (b: Band) => {
    onPreset({ ...preset, bands: [...preset.bands, b] });
    setSelected(preset.bands.length);
  };
  const remove = (i: number) => {
    onPreset({ ...preset, bands: preset.bands.filter((_, j) => j !== i) });
    setSelected(null);
  };
  // Boosts eat headroom. AutoEq presets ship a negative preamp for exactly this reason.
  const peak = useMemo(() => Math.max(...PEAK_FREQS.map((hz) => totalGainDb(preset, hz))), [preset]);

  return (
    <div className="eq">
      <div className="eq-stage">
        <EqGraph
          preset={morphing ? shown : preset}
          selected={selected}
          bypass={p.bypass}
          interactive={!morphing}
          onSelect={setSelected}
          onChange={setBand}
          onRemove={remove}
          onAdd={add}
        />
        {preset.bands.length === 0 && !morphing && (
          <div className="graph-empty">
            <p>Flat. Nothing is being changed yet.</p>
            <div>
              <button className="button button-primary" onClick={() => add({ kind: "peak", fc: 1000, gainDb: 3, q: 1.41 })}>
                Add a band
              </button>
              <button className="button" onClick={p.onBrowsePresets}>
                Find a preset for your headphones
              </button>
            </div>
          </div>
        )}
        <label className="preamp">
          <span className="field-label">Preamp</span>
          <span className="preamp-track">
            <span className="preamp-scale" aria-hidden="true">
              {PREAMP_TICKS.map((db) => (
                <i key={db} className={db === 0 ? "is-zero" : ""} style={{ top: `${((DB_RANGE - db) / (2 * DB_RANGE)) * 100}%` }}>
                  <b>{db === 0 ? "0" : Math.abs(db) === 12 ? (db > 0 ? "+12" : "−12") : ""}</b>
                </i>
              ))}
            </span>
            <input
              type="range"
              min={-DB_RANGE}
              max={DB_RANGE}
              step={0.1}
              value={preset.preampDb}
              onChange={(e) => onPreset({ ...preset, preampDb: e.target.valueAsNumber })}
              onDoubleClick={() => onPreset({ ...preset, preampDb: 0 })}
              title="Double-click to reset"
            />
          </span>
          <output>{formatDb(preset.preampDb)}</output>
        </label>
      </div>

      {/* Always takes its row, so the graph doesn't resize under the pointer mid-drag. */}
      <div className="eq-notice">
        {peak > 0.05 && (
          <p className="notice">
            Loudest point is {formatDb(peak)} above full scale, so loud passages can clip.
            <button className="link" onClick={() => onPreset({ ...preset, preampDb: Math.round((preset.preampDb - peak) * 10) / 10 })}>
              Lower preamp to {formatDb(preset.preampDb - peak)}
            </button>
          </p>
        )}
      </div>

      <div className="strip">
        <div className="strip-head">
          <div className="chips" role="tablist" aria-label="Bands">
            {preset.bands.map((_, i) => (
              <button key={i} role="tab" aria-selected={selected === i} className="chip" style={{ color: bandColor(i) }} onClick={() => setSelected(i)}>
                <span>{i + 1}</span>
              </button>
            ))}
          </div>
          <button className="button" onClick={() => add({ kind: "peak", fc: emptiestFrequency(preset.bands), gainDb: 0, q: 1.41 })}>
            Add band
          </button>
        </div>
        {band && selected !== null ? (
          <div className="band">
            <div className="segmented" role="radiogroup" aria-label="Band type">
              {KINDS.map((k) => (
                <button key={k.kind} role="radio" aria-checked={band.kind === k.kind} onClick={() => setBand(selected, { ...band, kind: k.kind })}>
                  {k.label}
                </button>
              ))}
            </div>
            <NumberField label="Frequency" unit="Hz" value={band.fc} min={F_MIN} max={F_MAX} step={1} onChange={(fc) => setBand(selected, { ...band, fc })} />
            <NumberField label="Gain" unit="dB" value={band.gainDb} min={-DB_RANGE} max={DB_RANGE} step={0.1} onChange={(gainDb) => setBand(selected, { ...band, gainDb })} />
            <NumberField label="Width (Q)" unit="" value={band.q} min={0.1} max={10} step={0.01} onChange={(q) => setBand(selected, { ...band, q })} />
            <button className="button button-quiet" onClick={() => remove(selected)}>
              Remove band
            </button>
          </div>
        ) : (
          <p className="hint">
            {preset.bands.length
              ? "Select a band to edit it. Drag it to move, scroll over it to change its width, double-click it to remove. Double-click empty space to add one there."
              : "Bands you add show up here."}
          </p>
        )}
      </div>
    </div>
  );
}

export function Presets(p: {
  autoEq: AutoEqApi;
  library: NamedPreset[];
  current: string;
  /** The output a chosen preset will be applied to. Undefined when the user hasn't added any yet. */
  target: string | undefined;
  onLoad(named: NamedPreset): void;
  onLoadAutoEq(entry: AutoEqEntry): Promise<void>;
  onSave(name: string): void;
  onDelete(name: string): void;
}) {
  const [name, setName] = useState("");
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState<AutoEqEntry[]>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState<string>();

  useEffect(() => {
    if (query.trim().length < 2 || index) return;
    p.autoEq.index().then(setIndex, (e) => setError(String(e.message ?? e)));
  }, [query, index, p.autoEq]);

  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const results = useMemo(
    () => (index && query.trim().length >= 2 ? index.filter((e) => words.every((w) => e.name.toLowerCase().includes(w))).slice(0, 40) : []),
    [index, query],
  );

  return (
    <div className="page">
      <p className="hint">{p.target ? `A preset you pick here is applied to ${p.target}.` : "Add an output in Devices first, then pick its preset here."}</p>
      <h2>Included</h2>
      <ul className="group">
        {BUILTIN.map((named) => (
          <li key={named.name} className="row">
            <span className="row-title">{named.name}</span>
            <span className="row-detail">{named.preset.bands.length} bands</span>
            {p.current === named.name ? <span className="row-detail">In use</span> : <button className="link" onClick={() => p.onLoad(named)}>Use</button>}
          </li>
        ))}
      </ul>

      <h2>Your presets</h2>
      <ul className="group">
        {p.library.map((named) => (
          <li key={named.name} className="row">
            <span className="row-title">{named.name}</span>
            <span className="row-detail">
              {named.preset.bands.length} {named.preset.bands.length === 1 ? "band" : "bands"}
            </span>
            {p.current === named.name ? <span className="row-detail">In use</span> : <button className="link" onClick={() => p.onLoad(named)}>Use</button>}
            <button className="icon-button is-danger" onClick={() => p.onDelete(named.name)} aria-label={`Delete ${named.name}`} title="Delete">
              <svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" /></svg>
            </button>
          </li>
        ))}
        <li className="row">
          <form
            className="row-form"
            onSubmit={(e) => {
              e.preventDefault();
              if (name.trim()) p.onSave(name.trim());
              setName("");
            }}
          >
            <input className="text" placeholder="Name the current EQ to save it" value={name} onChange={(e) => setName(e.target.value)} aria-label="Preset name" />
            <button className="button" disabled={!name.trim()}>
              Save preset
            </button>
          </form>
        </li>
      </ul>

      <h2>Find your headphones</h2>
      <p className="hint">
        Measured corrections from the AutoEq project, downloaded when you pick one. They assume your headset's own EQ is flat, so switch that off
        first or the two will stack.
      </p>
      <input className="text text-search" type="search" placeholder="Search by model, e.g. Arctis Nova Pro" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search headphones" />
      {error && (
        <p className="notice">
          Couldn't reach the AutoEq database ({error}). Check your connection and type again to retry.
        </p>
      )}
      {query.trim().length >= 2 && !index && !error && <p className="hint">Loading the headphone list…</p>}
      {index && query.trim().length >= 2 && results.length === 0 && <p className="hint">No model matches “{query}”. Try fewer words.</p>}
      {results.length > 0 && (
        <ul className="group">
          {results.map((entry) => (
            <li key={entry.path} className="row">
              <span className="row-title">{entry.name}</span>
              <span className="row-detail">{entry.source}</span>
              <button
                className="link"
                disabled={loading === entry.path}
                onClick={() => {
                  setLoading(entry.path);
                  p.onLoadAutoEq(entry).catch((e) => setError(String(e.message ?? e))).finally(() => setLoading(undefined));
                }}
              >
                {loading === entry.path ? "Loading…" : "Use"}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export function Devices(p: {
  devices: Device[];
  detected: Device | undefined;
  source: string | null;
  outputs: SavedOutput[];
  activeId: string | undefined;
  running: boolean;
  autoSwitch: boolean;
  status: Status;
  onAutoSwitch(enabled: boolean): void;
  onSource(id: string): void;
  onOutputs(outputs: SavedOutput[]): void;
  onRunning(running: boolean): void;
  onRefresh(): void;
}) {
  const virtual = p.detected?.id ?? p.source;
  const chosen = new Set(p.outputs.map((o) => o.id));
  const available = p.devices.filter((d) => d.id !== virtual && !chosen.has(d.id));
  // Reordering: drag a row, or use its arrow buttons (keyboard and screen readers).
  const [drag, setDrag] = useState<number | null>(null);
  const [over, setOver] = useState<number | null>(null);
  const move = (from: number, to: number) => {
    const next = [...p.outputs];
    next.splice(to, 0, ...next.splice(from, 1));
    p.onOutputs(next);
  };

  return (
    <div className="page">
      <h2>Apps play into</h2>
      {p.detected ? (
        <>
          <ul className="group">
            <li className="row">
              <span className="row-title">{p.detected.name}</span>
              <span className="row-detail">Found</span>
            </li>
          </ul>
          <p className="hint hint-below">Make this your default output in Windows, and turn spatial sound on for it if you use Atmos or DTS.</p>
        </>
      ) : (
        <>
          <p className="notice">The freeseries virtual device isn't installed, so there's nothing for your apps to play into yet.</p>
          <p className="hint hint-below">Already use another virtual cable? Pick it here.</p>
          <ul className="group" role="radiogroup" aria-label="Virtual device">
            {p.devices.map((d) => (
              <li key={d.id}>
                <label className="row row-choice">
                  <input type="radio" name="source" checked={p.source === d.id} disabled={chosen.has(d.id)} onChange={() => p.onSource(d.id)} />
                  <span className="row-title">{d.name}</span>
                </label>
              </li>
            ))}
          </ul>
        </>
      )}

      <h2>You listen on</h2>
      <p className="hint">
        {p.autoSwitch
          ? "Drag them into the order you prefer. freeseries plays on the highest one that's switched on: when it goes off, audio moves down the list, and when it comes back, audio moves back up."
          : "freeseries only plays on the first one. Drag another to the top to use that instead."}{" "}
        Each keeps its own EQ. Leave spatial sound off on these so it isn't applied twice.
      </p>
      {p.outputs.length > 0 && (
        <ol className="group" onDragLeave={(e) => e.currentTarget.contains(e.relatedTarget as Node) || setOver(null)}>
          {p.outputs.map((o, i) => (
            <li
              key={o.id}
              className={`row row-draggable${drag === i ? " is-dragging" : ""}${over === i && drag !== null && drag !== i ? (drag < i ? " is-over-below" : " is-over-above") : ""}${!p.autoSwitch && i > 0 ? " is-unused" : ""}`}
              draggable
              onDragStart={(e) => {
                setDrag(i);
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", o.id);
              }}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setOver(i);
              }}
              onDrop={(e) => {
                e.preventDefault();
                if (drag !== null && drag !== i) move(drag, i);
              }}
              onDragEnd={() => {
                setDrag(null);
                setOver(null);
              }}
            >
              <svg className="grip" viewBox="0 0 10 16" aria-hidden="true">
                <path d="M3 3h.01M7 3h.01M3 8h.01M7 8h.01M3 13h.01M7 13h.01" />
              </svg>
              <span className="row-order">{i + 1}</span>
              <span className="row-title">{p.devices.find((d) => d.id === o.id)?.name ?? (o.name || "Disconnected device")}</span>
              {o.id === p.activeId ? <span className="badge">Playing</span> : !p.devices.some((d) => d.id === o.id) && <span className="row-detail">Not connected</span>}
              <span className="row-detail">{o.presetName}</span>
              <button className="icon-button" onClick={() => move(i, i - 1)} disabled={i === 0} aria-label={`Move ${o.name} up`} title="Prefer this one more">
                <svg viewBox="0 0 12 12"><path d="M2.5 7.5 6 4l3.5 3.5" /></svg>
              </button>
              <button className="icon-button" onClick={() => move(i, i + 1)} disabled={i === p.outputs.length - 1} aria-label={`Move ${o.name} down`} title="Prefer this one less">
                <svg viewBox="0 0 12 12"><path d="M2.5 4.5 6 8l3.5-3.5" /></svg>
              </button>
              <button className="icon-button is-danger" onClick={() => p.onOutputs(p.outputs.filter((x) => x.id !== o.id))} aria-label={`Remove ${o.name}`} title="Remove">
                <svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" /></svg>
              </button>
            </li>
          ))}
        </ol>
      )}
      {p.outputs.length > 1 && (
        <ul className="group group-below">
          <li>
            <label className="row">
              <span className="row-title">Switch outputs automatically</span>
              <input type="checkbox" role="switch" className="switch" checked={p.autoSwitch} onChange={(e) => p.onAutoSwitch(e.target.checked)} />
            </label>
          </li>
        </ul>
      )}
      {available.length > 0 && (
        <ul className="group group-below">
          {available.map((d) => (
            <li key={d.id} className="row">
              <span className="row-title">{d.name}</span>
              <span className="row-detail">
                {d.channels} ch, {d.sampleRate / 1000} kHz
              </span>
              <button className="link" onClick={() => p.onOutputs([...p.outputs, { id: d.id, name: d.name, presetName: "Flat", preset: { preampDb: 0, bands: [] } }])}>
                Add
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="actions">
        {p.running ? (
          <button className="button" onClick={() => p.onRunning(false)}>
            Stop
          </button>
        ) : (
          <button className="button button-primary" disabled={!virtual || p.outputs.length === 0} onClick={() => p.onRunning(true)}>
            Start
          </button>
        )}
        <button className="button button-quiet" onClick={p.onRefresh}>
          Refresh devices
        </button>
        {p.status.state === "reconnecting" && <span className="hint">{p.status.error}</span>}
      </div>
    </div>
  );
}

export function Log(p: { entries: (LogEntry & { time: string })[]; onOpenFolder(): void }) {
  const problems = p.entries.filter((e) => e.level !== "info").length;
  return (
    <div className="page page-wide">
      <div className="actions actions-top">
        <p className="hint">
          {p.entries.length === 0
            ? "Nothing logged yet. Engine events show up here as they happen."
            : problems === 0
              ? "No glitches or errors since the app started."
              : `${problems} ${problems === 1 ? "problem" : "problems"} since the app started. The full history is in the log files.`}
        </p>
        <button className="button" onClick={p.onOpenFolder}>
          Open log folder
        </button>
      </div>
      {p.entries.length > 0 && (
        <ol className="group log">
          {p.entries.map((e, i) => (
            <li key={i} className={`log-line is-${e.level}`}>
              <time>{e.time}</time>
              <span>{e.message}</span>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

export function Settings(p: { startup: StartupApi }) {
  const [enabled, setEnabled] = useState(false);
  const [error, setError] = useState<string>();
  useEffect(() => {
    p.startup.startsWithWindows().then(setEnabled, (e) => setError(String(e)));
  }, [p.startup]);

  return (
    <div className="page">
      <h2>Startup</h2>
      <ul className="group">
        <li>
          <label className="row">
            <span className="row-title">Start with Windows</span>
            <input
              type="checkbox"
              role="switch"
              className="switch"
              checked={enabled}
              onChange={(e) => {
                const next = e.target.checked;
                setError(undefined);
                p.startup.setStartsWithWindows(next).then(() => setEnabled(next), (err) => setError(String(err)));
              }}
            />
          </label>
        </li>
      </ul>
      <p className="hint hint-below">Starts in the tray and picks up your devices and EQ from last time, so your audio works before you open anything.</p>
      {error && <p className="notice">{error}</p>}

      <h2>Closing the window</h2>
      <p className="hint">
        Closing freeseries hides it in the tray next to the clock and your audio keeps running. To stop it completely, right-click the tray icon
        and choose Quit.
      </p>
    </div>
  );
}
