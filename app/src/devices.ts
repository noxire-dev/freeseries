import type { Device } from "./api";

// freeseries only works through a virtual device apps can play into: the FxSound driver it
// installs, or VB-Cable for people who already have that. Found by name so nobody has to pick it.
const VIRTUAL = /fxsound|cable input/i;

export const findVirtualDevice = (devices: Device[]): Device | undefined => devices.find((d) => VIRTUAL.test(d.name));

/** "Headphones (8- Arctis Nova Pro Wireless)" -> "Arctis Nova Pro Wireless": the bracketed part tells devices apart. */
export const shortName = (name: string): string => name.match(/\((?:\d+- )?(.+)\)$/)?.[1] ?? name;
