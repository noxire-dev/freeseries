import { useEffect, useMemo, useRef, useState } from "react";
import type { About, AppsApi, AudioApp, AutoEqApi, AutoEqEntry, Band, Device, InfoApi, Kind, LogEntry, Preset, SetupApi, StartupApi, Status } from "./api";
import EqGraph, { bandColor } from "./EqGraph";
import { DB_RANGE, F_MAX, F_MIN, clamp, combine, formatDb, formatPreset, frequencies, totalGainDb } from "./eqmath";
import { BUILTIN } from "./builtin";
import { isVirtualDevice } from "./devices";
import { FLAT } from "./store";
import type { NamedPreset, SavedChannel, SavedOutput } from "./store";

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

export function Equalizer(p: {
  preset: Preset;
  /** The headphone correction under this sound preset, drawn for reference. Not editable here. */
  correction: Preset | undefined;
  bypass: boolean;
  morphKey: string;
  spectrum(): Promise<number[] | null>;
  onAnalysing(on: boolean): void;
  onPreset(preset: Preset): void;
  onBrowsePresets(): void;
}) {
  const { preset, onPreset } = p;
  // The engine only taps the audio for the analyser while this page is up.
  useEffect(() => {
    p.onAnalysing(true);
    return () => p.onAnalysing(false);
  }, [p.onAnalysing]);
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
  // Headroom is about everything that plays: the correction and the sound on top of it.
  const peak = useMemo(() => {
    const all = combine(p.correction, preset);
    return Math.max(...PEAK_FREQS.map((hz) => totalGainDb(all, hz)));
  }, [preset, p.correction]);

  return (
    <div className="eq">
      <div className="eq-stage">
        <EqGraph
          preset={morphing ? shown : preset}
          base={p.correction}
          selected={selected}
          bypass={p.bypass}
          interactive={!morphing}
          spectrum={p.spectrum}
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
            Loudest point is {formatDb(peak)} above full scale. The limiter catches it, but loud passages will sound squashed.
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
  /** The channel a chosen sound goes to. Undefined before the virtual device exists. */
  target: string | undefined;
  /** The output the correction belongs to. Undefined when the user hasn't added any yet. */
  output: string | undefined;
  sound: Preset | undefined;
  correction: NamedPreset | null;
  onLoad(named: NamedPreset): void;
  onCorrection(correction: NamedPreset | null): void;
  onLoadAutoEq(entry: AutoEqEntry): Promise<void>;
  onImport(name: string, text: string): Promise<void>;
  onSave(name: string): void;
  onDelete(name: string): void;
}) {
  const [name, setName] = useState("");
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState<AutoEqEntry[]>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState<string>();
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (query.trim().length < 2 || index) return;
    p.autoEq.index().then(setIndex, (e) => setError(String(e.message ?? e)));
  }, [query, index, p.autoEq]);

  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const results = useMemo(
    () => (index && query.trim().length >= 2 ? index.filter((e) => words.every((w) => e.name.toLowerCase().includes(w))).slice(0, 40) : []),
    [index, query],
  );

  const soundRow = (named: NamedPreset, removable: boolean) => (
    <li key={named.name} className={`row${p.current === named.name ? " is-current" : ""}`}>
      <svg className="check" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.5 5 9l4.5-5.5" /></svg>
      <span className="row-title">{named.name}</span>
      <span className="row-detail">
        {named.preset.bands.length} {named.preset.bands.length === 1 ? "band" : "bands"}
      </span>
      {p.current === named.name ? <span className="row-state">In use</span> : <button className="link" onClick={() => p.onLoad(named)}>Use</button>}
      {removable && (
        <button className="icon-button is-danger" onClick={() => p.onDelete(named.name)} aria-label={`Delete ${named.name}`} title="Delete">
          <svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" /></svg>
        </button>
      )}
    </li>
  );

  if (!p.target) {
    return (
      <div className="page">
        <p className="hint">Presets show up once the freeseries audio device is installed.</p>
      </div>
    );
  }

  return (
    <div className="page">
      <h2>Sound for {p.target}</h2>
      <ul className="group">{BUILTIN.map((named) => soundRow(named, false))}</ul>
      <ul className="group group-below">
        {p.library.map((named) => soundRow(named, true))}
        <li className="row">
          <form
            className="row-form"
            onSubmit={(e) => {
              e.preventDefault();
              if (name.trim()) p.onSave(name.trim());
              setName("");
            }}
          >
            <input className="text" placeholder="Name the current sound to save it" value={name} onChange={(e) => setName(e.target.value)} aria-label="Preset name" />
            <button className="button" disabled={!name.trim()}>
              Save preset
            </button>
          </form>
        </li>
      </ul>
      <div className="actions actions-tight">
        <label className="button">
          Import from file
          <input
            type="file"
            accept=".txt,text/plain"
            className="visually-hidden"
            onChange={(e) => {
              const file = e.target.files?.[0];
              e.target.value = ""; // so picking the same file again still fires
              if (!file) return;
              setError(undefined);
              file.text().then((text) => p.onImport(file.name.replace(/(\s*ParametricEQ)?\.txt$/i, ""), text)).catch((err) => setError(String(err.message ?? err)));
            }}
          />
        </label>
        <button
          className="button"
          disabled={!p.sound}
          onClick={() => navigator.clipboard.writeText(formatPreset(p.sound!)).then(() => (setCopied(true), setTimeout(() => setCopied(false), 2000)), (err) => setError(String(err.message ?? err)))}
        >
          {copied ? "Copied" : "Copy current sound as text"}
        </button>
        <span className="hint">Equalizer APO format, the one Peace and AutoEq use.</span>
      </div>

      {p.output && (
        <>
      <h2>Headphone correction for {p.output}</h2>
      <ul className="group">
        <li className="row">
          <span className="row-title">{p.correction ? p.correction.name : "None"}</span>
          {p.correction && (
            <>
              <span className="row-detail">{p.correction.preset.bands.length} bands</span>
              <button className="link" onClick={() => p.onCorrection(null)}>
                Remove
              </button>
            </>
          )}
        </li>
      </ul>
      <p className="hint hint-below">
        Measured corrections from the AutoEq project, downloaded when you pick one. They assume your headset's own EQ is flat, so switch that off
        first or the two will stack.
      </p>
      <input className="text text-search" type="search" placeholder="Search by model, e.g. Arctis Nova Pro" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search headphones" />
      {error && <p className="notice">{error}</p>}
      {query.trim().length >= 2 && !index && !error && <p className="hint">Loading the headphone list…</p>}
      {index && query.trim().length >= 2 && results.length === 0 && <p className="hint">No model matches “{query}”. Try fewer words.</p>}
      {results.length > 0 && (
        <ul className="group">
          {results.map((entry) => (
            <li key={entry.path} className={`row${p.correction?.name === entry.name ? " is-current" : ""}`}>
              <svg className="check" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 6.5 5 9l4.5-5.5" /></svg>
              <span className="row-title">{entry.name}</span>
              <span className="row-detail">{entry.source}</span>
              {p.correction?.name === entry.name && loading !== entry.path ? (
                <span className="row-state">In use</span>
              ) : (
                <button
                  className="link"
                  disabled={loading === entry.path}
                  onClick={() => {
                    setLoading(entry.path);
                    setError(undefined);
                    p.onLoadAutoEq(entry)
                      .catch((e) => setError(`Couldn't download that correction (${e.message ?? e}). Check your connection and try again.`))
                      .finally(() => setLoading(undefined));
                  }}
                >
                  {loading === entry.path ? "Loading…" : "Use"}
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
        </>
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
  onSetup(): void;
}) {
  const virtual = p.detected?.id ?? p.source;
  const chosen = new Set(p.outputs.map((o) => o.id));
  const available = p.devices.filter((d) => d.id !== virtual && !chosen.has(d.id) && !isVirtualDevice(d));
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
          <p className="notice">
            The freeseries audio device isn't installed, so there's nothing for your apps to play into yet.
            <button className="link" onClick={p.onSetup}>
              Set it up
            </button>
          </p>
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
              {o.correction && <span className="row-detail">{o.correction.name}</span>}
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
              <button className="link" onClick={() => p.onOutputs([...p.outputs, { id: d.id, name: d.name }])}>
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

export function Log(p: { entries: (LogEntry & { time: string })[]; onOpenFolder(): void; onBack(): void }) {
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
        <span className="actions-end">
          <button className="button button-quiet" onClick={p.onBack}>
            Back to Settings
          </button>
          <button className="button" onClick={p.onOpenFolder}>
            Open log folder
          </button>
        </span>
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

export function Settings(p: {
  startup: StartupApi;
  info: InfoApi;
  problems: number;
  hotkeys: boolean;
  refusedHotkeys: string[];
  onHotkeys(enabled: boolean): void;
  onSetup(): void;
  onLog(): void;
  onOpenLogFolder(): void;
}) {
  const [enabled, setEnabled] = useState(false);
  const [about, setAbout] = useState<About>();
  useEffect(() => void p.info.about().then(setAbout, () => {}), [p.info]);
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

      <h2>Keyboard shortcuts</h2>
      <ul className="group">
        <li>
          <label className="row">
            <span className="row-title">Work in any app, including games</span>
            <input type="checkbox" role="switch" className="switch" checked={p.hotkeys} onChange={(e) => p.onHotkeys(e.target.checked)} />
          </label>
        </li>
        {[
          ["Next sound preset", "Ctrl+Shift+F10"],
          ["Previous sound preset", "Ctrl+Shift+F9"],
          ["EQ on / bypass", "Ctrl+Shift+F11"],
        ].map(([what, keys]) => (
          <li key={keys} className={`row${p.hotkeys ? "" : " is-unused"}`}>
            <span className="row-title">{what}</span>
            {p.refusedHotkeys.includes(keys) && <span className="row-detail">Taken by another app</span>}
            <kbd>{keys}</kbd>
          </li>
        ))}
      </ul>

      <h2>Audio device</h2>
      <ul className="group">
        <li className="row">
          <span className="row-title">Install or repair the freeseries audio device</span>
          <button className="button" onClick={p.onSetup}>
            Open setup
          </button>
        </li>
      </ul>

      <h2>Closing the window</h2>
      <p className="hint">
        Closing freeseries hides it in the tray next to the clock and your audio keeps running. To stop it completely, right-click the tray icon
        and choose Quit.
      </p>

      <h2>Log</h2>
      <ul className="group">
        <li className="row">
          <span className="row-title">{p.problems === 0 ? "No glitches or errors since the app started" : `${p.problems} ${p.problems === 1 ? "problem" : "problems"} since the app started`}</span>
          <button className="link" onClick={p.onOpenLogFolder}>
            Open folder
          </button>
          <button className="button" onClick={p.onLog}>
            View log
          </button>
        </li>
      </ul>

      <h2>About</h2>
      <ul className="group">
        <li className="row">
          <span className="row-title">Version</span>
          <span className="row-detail row-selectable">
            {about ? `${about.version} (${about.commit})${about.dev ? ", development build" : ""}` : ""}
          </span>
        </li>
        <li className="row">
          <span className="row-title">License</span>
          <span className="row-detail">Free software, AGPL-3.0</span>
        </li>
      </ul>
      <p className="hint hint-below">
        No account and no telemetry. freeseries only goes online to fetch a headphone preset when you search for one.
      </p>
    </div>
  );
}

export function Setup(p: { setup: SetupApi; detected: Device | undefined; hasOutputs: boolean; onRefresh(): void; onDone(): void }) {
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string>();
  const [defaultId, setDefaultId] = useState<string | null>(null);

  // The second step happens in Windows' own settings, so watch for it rather than ask.
  useEffect(() => {
    const check = () => p.setup.defaultOutput().then(setDefaultId, () => {});
    check();
    const poll = setInterval(check, 1500);
    return () => clearInterval(poll);
  }, [p.setup]);

  const installed = p.detected !== undefined;
  const isDefault = installed && defaultId === p.detected!.id;
  const install = () => {
    setInstalling(true);
    setError(undefined);
    p.setup
      .installDevice()
      .then(p.onRefresh, (e) => setError(String(e)))
      .finally(() => setInstalling(false));
  };

  const step = (done: boolean, title: string, body: React.ReactNode) => (
    <li className={`step${done ? " is-done" : ""}`}>
      <span className="step-mark" aria-hidden="true">
        {done && <svg viewBox="0 0 12 12"><path d="M2.5 6.5 5 9l4.5-5.5" /></svg>}
      </span>
      <div>
        <h3>
          {title}
          {done && <span className="visually-hidden"> (done)</span>}
        </h3>
        {body}
      </div>
    </li>
  );

  return (
    <div className="page">
      <p className="hint">
        freeseries sits between your apps and your headphones. Three steps get it there; the first one is the only one that needs your
        permission.
      </p>
      <ol className="steps">
        {step(
          installed,
          "Install the freeseries audio device",
          <>
            <p className="hint">
              Your apps play into this device, and freeseries passes the sound on to your headphones with your EQ applied. It's a driver signed
              by Microsoft, so Windows will ask for permission once.
            </p>
            {installed ? (
              <p className="step-state">Installed as {p.detected!.name}</p>
            ) : (
              <button className="button button-primary" onClick={install} disabled={installing}>
                {installing ? "Waiting for Windows…" : "Install"}
              </button>
            )}
            {error && <p className="notice">{error}</p>}
          </>,
        )}
        {step(
          isDefault,
          "Make it your default output",
          <>
            <p className="hint">In Sound settings, under Output, choose the freeseries device. This page notices when you've done it.</p>
            {isDefault ? (
              <p className="step-state">Windows is playing into freeseries</p>
            ) : (
              <button className="button" onClick={() => p.setup.openSoundSettings()} disabled={!installed}>
                Open Sound settings
              </button>
            )}
          </>,
        )}
        {step(
          p.hasOutputs,
          "Choose what you listen on",
          <>
            <p className="hint">
              Pick your headphones and anything else you listen on. For surround in games, turn spatial sound on for the freeseries device
              and off for your headphones, so it's only applied once. Dolby Atmos and DTS Headphone:X work if you own them; Windows Sonic
              for Headphones is built in and free.
            </p>
            <button className={`button${isDefault && !p.hasOutputs ? " button-primary" : ""}`} onClick={p.onDone} disabled={!installed}>
              Open Devices
            </button>
          </>,
        )}
      </ol>
    </div>
  );
}

const CHANNEL_COLORS: Record<string, string> = { main: "#0A84FF", chat: "#30D158", media: "#BF5AF2" };
const channelColor = (key: string) => CHANNEL_COLORS[key] ?? "#FF9F0A";
const METER_FLOOR_DB = -60;
const THUMB_FREQS = frequencies(56);

/** Fader position (0 to 1) as the gain it stands for. The engine squares the position. */
const faderDb = (position: number) => (position <= 0 ? "−∞ dB" : formatDb(40 * Math.log10(position)));

// Icons are fetched once per executable and turned into an image the browser can cache.
const iconCache = new Map<string, Promise<string | null>>();
function iconUrl(apps: AppsApi, path: string): Promise<string | null> {
  if (!iconCache.has(path)) {
    iconCache.set(
      path,
      apps.icon(path).then(
        (icon) => {
          if (!icon) return null;
          const canvas = Object.assign(document.createElement("canvas"), { width: icon.width, height: icon.height });
          canvas.getContext("2d")!.putImageData(new ImageData(new Uint8ClampedArray(icon.rgba), icon.width, icon.height), 0, 0);
          return canvas.toDataURL();
        },
        () => null,
      ),
    );
  }
  return iconCache.get(path)!;
}

function AppIcon(p: { apps: AppsApi; name: string; path: string }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    iconUrl(p.apps, p.path).then((u) => live && setUrl(u));
    return () => void (live = false);
  }, [p.apps, p.path]);
  return url ? <img className="app-icon" src={url} alt="" draggable={false} /> : <span className="app-icon app-icon-letter" aria-hidden="true">{p.name[0]?.toUpperCase()}</span>;
}

/** The channel's EQ as a small curve: enough to tell "flat" from "footsteps" at a glance. */
function EqThumb(p: { preset: Preset | undefined }) {
  const d = THUMB_FREQS.map((hz, i) => {
    const db = p.preset ? clamp(totalGainDb({ ...p.preset, preampDb: 0 }, hz), -12, 12) : 0;
    return `${i ? "L" : "M"}${((i / (THUMB_FREQS.length - 1)) * 100).toFixed(1)},${(18 - (db / 12) * 15).toFixed(1)}`;
  }).join("");
  return (
    <svg className="eq-thumb" viewBox="0 0 100 36" preserveAspectRatio="none" aria-hidden="true">
      <path className="eq-thumb-zero" d="M0,18H100" />
      <path className="eq-thumb-fill" d={`${d}L100,18L0,18Z`} />
      <path className="eq-thumb-line" d={d} />
    </svg>
  );
}

type Strip = { key: string; label: string; deviceId: string; device: string } & SavedChannel;

/** The mixing console: a strip per channel with its EQ, its apps, a fader and a live meter. */
export function Mixer(p: {
  apps: AppsApi;
  levels(): Promise<Record<string, number>>;
  channels: Strip[];
  presets: NamedPreset[];
  output: { name: string; correction: string; playing: boolean } | undefined;
  balance: number;
  setup: SetupApi;
  onBalance(balance: number): void;
  onChannel(key: string, next: SavedChannel): void;
  /** Open that channel's sound in the Equalizer. */
  onEdit(key: string): void;
  onRefresh(): void;
}) {
  const [apps, setApps] = useState<AudioApp[]>([]);
  const [error, setError] = useState<string>();
  const [adding, setAdding] = useState(false);
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);

  const refresh = () => p.apps.list().then(setApps, (e) => setError(String(e)));
  useEffect(() => {
    refresh();
    const poll = setInterval(refresh, 3000); // apps come and go
    return () => clearInterval(poll);
  }, [p.apps]);

  // Meters move 20 times a second, so they are painted straight onto their elements, outside React.
  // Driven by animation frames: they stop by themselves while the window sits in the tray.
  const meters = useRef(new Map<string, HTMLElement>());
  useEffect(() => {
    const shown = new Map<string, number>();
    let frame = 0;
    let last = 0;
    let waiting = false;
    let live = true;
    const loop = (now: number) => {
      frame = requestAnimationFrame(loop);
      if (waiting || now - last < 50) return;
      last = now;
      waiting = true;
      p.levels()
        .then((levels) => {
          if (!live) return;
          for (const [key, bar] of meters.current) {
            const db = Math.max(METER_FLOOR_DB, 20 * Math.log10(Math.max(levels[key] ?? 0, 1e-6)));
            const held = Math.max(db, (shown.get(key) ?? METER_FLOOR_DB) - 2.2); // rises at once, falls ~44 dB/s
            shown.set(key, held);
            bar.style.clipPath = `inset(${(held / METER_FLOOR_DB) * 100}% 0 0 0)`; // hide the part of the scale above the level
            bar.classList.toggle("is-hot", held > -1);
          }
        }, () => {})
        .finally(() => (waiting = false));
    };
    frame = requestAnimationFrame(loop);
    return () => {
      live = false;
      cancelAnimationFrame(frame);
    };
  }, [p.levels]);
  const meter = (key: string) => (
    <>
      <span className="meter-scale" aria-hidden="true">
        {[0, -12, -24, -36, -48].map((db) => (
          <span key={db} style={{ top: `${(db / METER_FLOOR_DB) * 100}%` }}>
            {-db}
          </span>
        ))}
      </span>
      <span className="meter" aria-hidden="true">
        <span className="meter-bar" ref={(el) => void (el ? meters.current.set(key, el) : meters.current.delete(key))} />
      </span>
    </>
  );

  if (p.channels.length === 0) {
    return (
      <div className="page">
        <p className="hint">The mixer shows up once the freeseries audio device is installed.</p>
      </div>
    );
  }

  // Discord, browsers and games are several processes each: the user thinks in apps, so they move together.
  const names = [...new Set(apps.map((a) => a.name))];
  const main = p.channels[0];
  const stripOf = (name: string) => {
    const pinned = apps.find((a) => a.name === name && a.device)?.device;
    return p.channels.find((c) => c.deviceId === pinned) ?? main; // unpinned, or pinned elsewhere: it plays with everything else
  };
  const move = (name: string, to: Strip) => {
    setError(undefined);
    // The main channel is the Windows default device: moving an app there means un-pinning it.
    Promise.all(apps.filter((a) => a.name === name).map((a) => p.apps.assign(a.pid, to === main ? null : to.deviceId)))
      .catch((e) => setError(String(e)))
      .then(refresh);
  };
  const addChannel = () => {
    setAdding(true);
    setError(undefined);
    p.setup
      .addChannelDevice()
      .then(p.onRefresh, (e) => setError(String(e)))
      .finally(() => setAdding(false));
  };
  const hasChat = p.channels.some((c) => c.key === "chat");
  const nextChannel = !hasChat ? "chat" : !p.channels.some((c) => c.key === "media") ? "media" : null;

  return (
    <div className="console">
      <div className="strips">
        {p.channels.map((strip) => {
          const { key, label, device, deviceId: _, ...c } = strip;
          const mine = names.filter((name) => stripOf(name) === strip);
          return (
            <section
              key={key}
              className={`strip-col${c.muted ? " is-muted" : ""}${over === key && dragging && stripOf(dragging) !== strip ? " is-over" : ""}`}
              style={{ "--channel": channelColor(key) } as React.CSSProperties}
              aria-label={`${label} channel`}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setOver(key);
              }}
              onDragLeave={(e) => e.currentTarget.contains(e.relatedTarget as Node) || setOver(null)}
              onDrop={(e) => {
                e.preventDefault();
                if (dragging && stripOf(dragging) !== strip) move(dragging, strip);
                setDragging(null);
                setOver(null);
              }}
            >
              <div className="strip-eq">
                <button className="eq-thumb-button" onClick={() => p.onEdit(key)} title={`Edit the ${label} sound in the Equalizer`} aria-label={`Edit the ${label} sound`}>
                  <EqThumb preset={c.preset} />
                </button>
                <select
                  value={c.presetName}
                  onChange={(e) => {
                    const named = p.presets.find((n) => n.name === e.target.value) ?? { name: "Flat", preset: FLAT };
                    p.onChannel(key, { ...c, presetName: named.name, preset: named.preset });
                  }}
                  aria-label={`Sound for ${label}`}
                >
                  {/* A curve tuned by hand is in no list until it is saved. */}
                  {c.presetName !== "Flat" && !p.presets.some((n) => n.name === c.presetName) && <option>{c.presetName}</option>}
                  <option>Flat</option>
                  {p.presets.map((n) => (
                    <option key={n.name}>{n.name}</option>
                  ))}
                </select>
              </div>

              <ul className="strip-apps">
                {mine.map((name) => (
                  <li
                    key={name}
                    className={`app${dragging === name ? " is-dragging" : ""}`}
                    draggable
                    // The same move without a mouse: focus the app, then left or right.
                    tabIndex={0}
                    aria-label={`${name}, in ${label}. Arrow keys move it to another channel.`}
                    onKeyDown={(e) => {
                      const to = p.channels[p.channels.indexOf(strip) + (e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0)];
                      if (to && to !== strip) move(name, to);
                    }}
                    onDragStart={(e) => {
                      setDragging(name);
                      e.dataTransfer.effectAllowed = "move";
                      e.dataTransfer.setData("text/plain", name);
                    }}
                    onDragEnd={() => {
                      setDragging(null);
                      setOver(null);
                    }}
                  >
                    <AppIcon apps={p.apps} name={name} path={apps.find((a) => a.name === name)!.path} />
                    <span className="app-name">{name}</span>
                  </li>
                ))}
                {mine.length === 0 && <li className="strip-apps-empty">{p.channels.length > 1 ? "Drop apps here" : "Apps playing sound show up here"}</li>}
              </ul>

              <div className="strip-fader">
                {meter(key)}
                <input
                  type="range"
                  className="fader"
                  min={0}
                  max={1}
                  step={0.01}
                  value={c.volume}
                  onChange={(e) => p.onChannel(key, { ...c, volume: e.target.valueAsNumber })}
                  onDoubleClick={() => p.onChannel(key, { ...c, volume: 1 })}
                  aria-label={`${label} volume`}
                  title="Double-click for full volume"
                />
              </div>

              <div className="strip-foot">
                <output>{c.muted ? "Muted" : faderDb(c.volume)}</output>
                <button className="mute" aria-pressed={c.muted} onClick={() => p.onChannel(key, { ...c, muted: !c.muted })} title={c.muted ? "Unmute" : "Mute"}>
                  M
                </button>
              </div>
              <h3 className="strip-plate">
                {label}
                <small>{device}</small>
              </h3>
            </section>
          );
        })}

        {nextChannel && (
          <button className="strip-col strip-add" onClick={addChannel} disabled={adding}>
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
            <strong>{adding ? "Waiting for Windows…" : `Add a ${nextChannel} channel`}</strong>
            <span>
              {nextChannel === "chat"
                ? "Its own volume and EQ for voice chat, and a Game and chat balance."
                : "A third device, for music and video."}{" "}
              Windows asks for permission once.
            </span>
          </button>
        )}

        {p.output && (
          <section className="strip-col strip-out" aria-label="Output">
            <div className="strip-fader">{meter("out")}</div>
            <h3 className="strip-plate">
              {p.output.playing ? "Output" : "Stopped"}
              <small>{p.output.name}</small>
              <small>{p.output.correction}</small>
            </h3>
          </section>
        )}
      </div>

      {hasChat && (
        <label className="crossfade">
          <span style={{ color: channelColor("main") }}>Game</span>
          <input
            type="range"
            min={-1}
            max={1}
            step={0.02}
            value={p.balance}
            onChange={(e) => p.onBalance(Math.abs(e.target.valueAsNumber) < 0.06 ? 0 : e.target.valueAsNumber)}
            onDoubleClick={() => p.onBalance(0)}
            aria-label="Game and chat balance"
            title="Towards one side, the other gets quieter. Double-click to centre."
          />
          <span style={{ color: channelColor("chat") }}>Chat</span>
        </label>
      )}
      {error && <p className="notice">{error}</p>}
    </div>
  );
}
