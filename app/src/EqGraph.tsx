import { useEffect, useMemo, useRef, useState } from "react";
import type { Band, Preset } from "./api";
import { DB_RANGE, F_MAX, F_MIN, bandGainDb, clamp, formatDb, formatHz, frequencies, totalGainDb } from "./eqmath";

// One Apple system hue per band, low to high across the spectrum.
export const BAND_COLORS = ["#FF453A", "#FF9F0A", "#FFD60A", "#30D158", "#64D2FF", "#0A84FF", "#BF5AF2", "#FF375F"];
export const bandColor = (i: number) => BAND_COLORS[i % BAND_COLORS.length];

// A log grid the way analysers draw it: every step of the leading digit, decades emphasised.
const GRID_MINOR = [1, 2, 3, 4].flatMap((decade) => [1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => n * 10 ** decade)).filter((hz) => hz > F_MIN && hz < F_MAX);
const DECADES = new Set([100, 1000, 10000]);
const LABEL_HZ = [20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000];
const GRID_DB = [-12, -6, 0, 6, 12];
const PAD = { left: 36, right: 14, top: 14, bottom: 24 };
const FREQS = frequencies(240);

interface Props {
  preset: Preset;
  selected: number | null;
  bypass: boolean;
  /** False while a preset change is animating: what's drawn isn't what an edit would change. */
  interactive: boolean;
  onSelect(index: number | null): void;
  onChange(index: number, band: Band): void;
  onAdd(band: Band): void;
  onRemove(index: number): void;
}

export default function EqGraph({ preset, selected, bypass, interactive, onSelect, onChange, onAdd, onRemove }: Props) {
  const box = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 800, h: 360 });
  useEffect(() => {
    const observer = new ResizeObserver(([e]) => setSize({ w: e.contentRect.width, h: e.contentRect.height }));
    observer.observe(box.current!);
    return () => observer.disconnect();
  }, []);

  const plot = { w: size.w - PAD.left - PAD.right, h: size.h - PAD.top - PAD.bottom };
  const x = (hz: number) => PAD.left + (Math.log(hz / F_MIN) / Math.log(F_MAX / F_MIN)) * plot.w;
  const y = (db: number) => PAD.top + (0.5 - clamp(db, -DB_RANGE, DB_RANGE) / (2 * DB_RANGE)) * plot.h;
  const hzAt = (px: number) => F_MIN * (F_MAX / F_MIN) ** clamp((px - PAD.left) / plot.w, 0, 1);
  const dbAt = (py: number) => (0.5 - clamp((py - PAD.top) / plot.h, 0, 1)) * 2 * DB_RANGE;

  const line = (gain: (hz: number) => number) => FREQS.map((hz, i) => `${i ? "L" : "M"}${x(hz).toFixed(1)},${y(gain(hz)).toFixed(1)}`).join("");
  const toZero = `L${x(F_MAX)},${y(0)}L${x(F_MIN)},${y(0)}Z`;
  // The curve leaves the preamp out so every handle sits on it; the preamp has its own fader.
  const total = useMemo(() => line((hz) => totalGainDb({ ...preset, preampDb: 0 }, hz)), [preset.bands, size]);
  const fills = useMemo(() => preset.bands.map((b) => line((hz) => bandGainDb(b, hz)) + toZero), [preset.bands, size]);

  const local = (e: { clientX: number; clientY: number }) => {
    const r = box.current!.getBoundingClientRect();
    return { px: e.clientX - r.left, py: e.clientY - r.top };
  };
  const round = (b: Band): Band => ({ ...b, fc: Math.round(b.fc), gainDb: Math.round(b.gainDb * 10) / 10, q: Math.round(b.q * 100) / 100 });

  const drag = (i: number) => (e: React.PointerEvent) => {
    if (!(e.buttons & 1)) return;
    const { px, py } = local(e);
    onChange(i, round({ ...preset.bands[i], fc: hzAt(px), gainDb: dbAt(py) }));
  };
  const nudge = (i: number) => (e: React.KeyboardEvent) => {
    const b = preset.bands[i];
    const step = e.shiftKey ? 5 : 1;
    const moves: Record<string, Band> = {
      ArrowUp: { ...b, gainDb: clamp(b.gainDb + 0.5 * step, -DB_RANGE, DB_RANGE) },
      ArrowDown: { ...b, gainDb: clamp(b.gainDb - 0.5 * step, -DB_RANGE, DB_RANGE) },
      ArrowRight: { ...b, fc: clamp(b.fc * 1.02 ** step, F_MIN, F_MAX) },
      ArrowLeft: { ...b, fc: clamp(b.fc / 1.02 ** step, F_MIN, F_MAX) },
    };
    if (moves[e.key]) onChange(i, round(moves[e.key]));
    else if (e.key === "Delete" || e.key === "Backspace") onRemove(i);
    else return;
    e.preventDefault();
  };

  const focused = selected !== null ? preset.bands[selected] : undefined;

  return (
    <div
      ref={box}
      className={`graph${bypass ? " is-bypassed" : ""}${interactive ? "" : " is-morphing"}`}
      onPointerDown={(e) => (e.target as Element).closest(".handle") || onSelect(null)}
      onDoubleClick={(e) => {
        if ((e.target as Element).closest(".handle")) return;
        const { px, py } = local(e);
        onAdd(round({ kind: "peak", fc: hzAt(px), gainDb: dbAt(py), q: 1.41 }));
      }}
    >
      <svg width={size.w} height={size.h} role="group" aria-label="Frequency response. Double-click to add a band.">
        {GRID_MINOR.map((hz) => (
          <line key={hz} className={DECADES.has(hz) ? "grid grid-major" : "grid"} x1={x(hz)} x2={x(hz)} y1={PAD.top} y2={PAD.top + plot.h} />
        ))}
        {LABEL_HZ.map((hz) => (
          <text key={hz} className="tick" x={x(hz)} y={size.h - 7} textAnchor={hz === F_MIN ? "start" : hz === F_MAX ? "end" : "middle"}>
            {hz >= 1000 ? `${hz / 1000}k` : hz}
          </text>
        ))}
        {GRID_DB.map((db) => (
          <g key={db}>
            <line className={db === 0 ? "grid grid-zero" : "grid grid-major"} x1={PAD.left} x2={PAD.left + plot.w} y1={y(db)} y2={y(db)} />
            <text className="tick" x={PAD.left - 8} y={y(db) + 3.5} textAnchor="end">
              {db > 0 ? `+${db}` : db}
            </text>
          </g>
        ))}
        {fills.map((d, i) => (
          <path key={i} className="band-fill" d={d} fill={bandColor(i)} opacity={selected === i ? 0.34 : 0.13} />
        ))}
        <path className="curve-body" d={total + toZero} />
        <path className="curve" d={total} />
        {preset.bands.map((b, i) => (
          <g
            key={i}
            className={`handle${selected === i ? " is-selected" : ""}`}
            transform={`translate(${x(b.fc)},${y(b.gainDb)})`}
            style={{ color: bandColor(i) }}
            tabIndex={0}
            role="slider"
            aria-label={`Band ${i + 1}`}
            aria-valuetext={`${formatHz(b.fc)}, ${formatDb(b.gainDb)}, Q ${b.q}`}
            onFocus={() => onSelect(i)}
            onPointerDown={(e) => {
              e.currentTarget.setPointerCapture(e.pointerId);
              onSelect(i);
            }}
            onPointerMove={drag(i)}
            onKeyDown={nudge(i)}
            onDoubleClick={() => onRemove(i)}
            onWheel={(e) => onChange(i, round({ ...b, q: clamp(b.q * (e.deltaY < 0 ? 1.1 : 1 / 1.1), 0.1, 10) }))}
          >
            <circle className="handle-hit" r="16" />
            <circle className="handle-ring" r="12.5" />
            <circle className="handle-dot" r="8" />
            <text className="handle-num" y="3.5" textAnchor="middle">
              {i + 1}
            </text>
          </g>
        ))}
      </svg>

      {focused && interactive && (
        <output
          className={`readout${y(focused.gainDb) < 64 ? " is-below" : ""}`}
          style={{ left: clamp(x(focused.fc), 86, size.w - 86), top: y(focused.gainDb), color: bandColor(selected!) }}
        >
          <b>{formatHz(focused.fc)}</b>
          <span>{formatDb(focused.gainDb)}</span>
          <span>Q {focused.q.toFixed(2)}</span>
        </output>
      )}
    </div>
  );
}
