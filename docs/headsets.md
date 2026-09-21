# Headset hardware: research notes (2026-09-21)

Read out of other projects' source, nothing here has been run against hardware by FreeSeries yet
except the online/offline byte (`src/arctis.rs`). Sources: Arctis Sound Manager
(loteran/Arctis-Sound-Manager, GPL-3.0-or-later, itself built on elegos/Linux-Arctis-Manager,
GPL-3.0), HeadsetControl (Sapd/HeadsetControl, GPL-3.0), ggoled (GPL-3.0, Rust, the OLED).
There is no permissively licensed upstream.

## Licence position

GPLv3 section 13 lets GPL-3.0 code be combined with AGPL-3.0 code; copied or ported files stay
GPL-3.0-or-later, keep their copyright lines, get a dated "modified" notice, and need the GPLv3
text plus a `THIRD-PARTY.md` entry. Byte-level protocol facts are not copyrightable: a table
written by us from those facts carries no obligation, credit is courtesy. Do that, and credit all
three projects. Do NOT copy ASM's newer spec-derived files or comments: they come from decrypting
SteelSeries GG's own spec files with a key recovered from its executable.

## How ASM defines devices

Declarative YAML, 21 hardware profiles on one schema (`src/arctis_sound_manager/devices/*.yaml`,
schema in `docs/device_configuration_file_specs.md`), six status-parser types, no branching on
product id in the core. OLED and hardware EQ are code. The schema has grown escape hatches
(transport kind, report id, extra requests, sleep tokens). It uses libusb directly, not hidraw.

## Arctis Nova Pro Wireless (vendor 0x1038, products 0x12e0, 0x12e5, 0x225d), interface 4

Every command: report id `06`, opcode, value, zero-padded to 64 bytes. On Windows, hidapi shows
interface 4 as separate collections; we already match `mi_04&col01`.

Status: send `06 b0`, reply starts `06 b0`:

| Offset | Field | Values |
|---|---|---|
| 6 | headset battery | 0-8 (nine steps, not a percentage) |
| 7 | battery in the charge slot | 0-8 |
| 8 | transparency level | 0-10 |
| 9 | mic | 01 muted, 00 live |
| 0x0a | ANC | 00 off, 01 transparent, 02 on |
| 0x0e | pairing | 01 not paired, 04 paired offline, 08 connected |
| 0x0f | headset power | 01 offline, 02 charging by cable, 08 online |

Unsolicited input reports, prefix `07`:
- `07 45 <game> <chat>`: the ChatMix dial, each side 0-100. Only flows after `06 8d 01`
  ("Sonar is here") and `06 49 01` (software ChatMix on). On exit send `06 49 00` then `06 8d 00`,
  or the base station is left in software-ChatMix mode with nothing driving it. SteelSeries GG
  running at the same time fights over `06 49`.
- `07 25 <vol>`: base station volume, raw 56..0 for 0..100 %.
- `07 bd` ANC, `07 b9` transparency level, `07 43` line-out.
- ASM ignores dial moves of 1 as jitter except at 0 and 100, paces commands 50 ms apart and
  waits 5 s after the device appears.

Writes, all `06 <opcode> <value>`: sidetone `39` 0-3 (write-only, not in the status reply), mic
volume `37` 1-10, ANC `bd` 0-2, transparency `b9` 1-10, mic LED `bf` 1-10, auto-off `c1` 0-6,
gain `27`, EQ preset `2e` (4 = custom), EQ bands `33` + 10 bytes (0x14 = 0 dB). HeadsetControl
sends `06 09` to save after a set; ASM replays settings at start instead.

OLED (from ggoled): bitmap `06 93 x 0 w h` + column-major LSB data in strips up to 64 px as a
1024-byte feature report, 128x64; brightness `06 85 <0-10>`; back to the stock UI `06 95`. Needs
`send_feature_report`, possibly on another collection: unverified on Windows.

HeadsetControl covers battery, online, sidetone, lights, inactive time, EQ and save for this
model, but not ChatMix, ANC or mic mute.

## Design to build

One Rust file per headset exporting a `const` table, one registry line; no YAML (a parser
dependency, and ASM shows where that schema ends up). The table: name, vendor, product ids,
Windows interface match, report id and length, status request, reply maps (prefix -> field ->
offset, with a decoder: steps, map, on/off, percent), settings (name, opcode, range), init and
shutdown sequences with pacing, optional save command, optional OLED block. All I/O behind one
`HidTransport` trait (write, read with timeout, feature report) so a table is tested against
canned replies. Poll `06 b0` slowly, one reader thread for the `07` events.

First use: the ChatMix dial drives the Mixer's Game / Media balance.
