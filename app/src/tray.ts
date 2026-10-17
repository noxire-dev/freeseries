// The tray icon's right-click menu: the same switches as the pages, for when the window is away.

/** One entry. `items` makes a submenu, `checked` a check item, a label of "-" a separator. */
export interface TrayItem {
  id: string;
  label: string;
  checked?: boolean;
  items?: TrayItem[];
}

export interface TrayState {
  outputs: { id: string; name: string }[];
  /** The output in use, or the one that would be. */
  listening?: string;
  sounds: string[];
  /** The main channel's preset, "(edited)" and all. */
  sound: string;
  bypass: boolean;
  running: boolean;
  /** null when there is no cable or no microphone picked: nothing to switch on. */
  micOn: boolean | null;
}

/** The items between Open and Quit. A click comes back as the id; `trayPick` reads it. */
export function trayMenu(s: TrayState): TrayItem[] {
  const items: TrayItem[] = [];
  if (s.outputs.length) items.push({ id: "outputs", label: "Play on", items: s.outputs.map((o) => ({ id: `output:${o.id}`, label: o.name, checked: o.id === s.listening })) });
  if (s.sounds.length) items.push({ id: "sounds", label: "Sound", items: s.sounds.map((n) => ({ id: `sound:${n}`, label: n, checked: n === s.sound.replace(/ \(edited\)$/, "") })) });
  items.push({ id: "bypass", label: "EQ on", checked: !s.bypass });
  if (s.micOn !== null) items.push({ id: "mic", label: "Mic on", checked: s.micOn });
  items.push({ id: "running", label: "Audio processing", checked: s.running });
  return items;
}

/** "output:wasapi:{...}" is kind "output", value "wasapi:{...}": only the first colon splits. */
export function trayPick(id: string): [kind: string, value: string] {
  const at = id.indexOf(":");
  return at < 0 ? [id, ""] : [id.slice(0, at), id.slice(at + 1)];
}
