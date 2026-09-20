import type { Device } from "./api";

// freeseries only works through a virtual device apps can play into: the FxSound driver it
// installs, or VB-Cable for people who already have that. Found by name so nobody has to pick it.
const VIRTUAL = /fxsound|cable input/i;

/** Something apps play into, not something you can listen on. */
export const isVirtualDevice = (device: Device): boolean => VIRTUAL.test(device.name);

// With more than one instance of the driver installed (channels), the main one is the endpoint
// setup named plain "freeseries"; "freeseries Chat" and the like are not what apps play into by default.
const MAIN = /^freeseries \(/i;

export const findVirtualDevice = (devices: Device[]): Device | undefined =>
  devices.find((d) => MAIN.test(d.name)) ?? devices.find((d) => VIRTUAL.test(d.name) && !/^freeseries /i.test(d.name));

/** "Headphones (8- Arctis Nova Pro Wireless)" -> "Arctis Nova Pro Wireless": the bracketed part tells devices apart. */
export const shortName = (name: string): string => name.match(/\((?:\d+- )?(.+)\)$/)?.[1] ?? name;

export interface VirtualChannel {
  /** Stable key settings are stored under: "main", "chat", "media". */
  key: string;
  /** What the user calls it. The main device is where games go. */
  label: string;
  device: Device;
}

/** The channels that exist on this PC: one per freeseries virtual device, the main one first. */
export function virtualChannels(devices: Device[]): VirtualChannel[] {
  const main = findVirtualDevice(devices);
  if (!main) return [];
  const extras = devices
    .filter((d) => d.id !== main.id && isVirtualDevice(d))
    .map((device) => {
      const label = device.name.match(/^freeseries (\w+) \(/i)?.[1];
      return { key: label?.toLowerCase() ?? device.id, label: label ?? shortName(device.name), device };
    });
  return [{ key: "main", label: extras.length ? "Game" : "Everything", device: main }, ...extras];
}
