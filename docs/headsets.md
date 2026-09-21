# Headset hardware

The protocol below was read out of other projects' source; what FreeSeries has checked on real
hardware itself is listed under "What is built". Sources: Arctis Sound Manager
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

## What is built (2026-09-21)

`src/headset.rs` is the generic part: the `Model` table type, `decode`, and `Headset`, a watcher
thread that finds a known headset, asks for its status every 2 s, reads its events, and publishes a
`State` (online, battery, mic muted, dial) whenever it changes. All USB goes through the
`HidTransport` / `HidLink` traits (`Usb` is the hidapi one), so the watcher is tested with a toy
headset and no hardware. `Headset` is also the engine's `Presence`: a headset that says it is
switched off makes the engine move to the next output. On Quit the dial is handed back to the
hardware before the app leaves.

In the app: state arrives as "headset" events; the ChatMix dial sets the balance between the main
channel and the second one (chat side minus game side, snapping to the middle), with a switch in
Settings; the Mixer's output strip shows battery and "mic muted" while the headset is on.

Checked on a real Arctis Nova Pro Wireless: the base station is found, accepts the status request
and the dial on/off commands, and its status reply decodes as documented (seen with the headset
off), and so do battery (2 of 8 steps read as 25 %), mic mute and the dial: `07 45 5b 64` towards
chat, `07 45 64 64` in the middle, `07 45 64 53` towards game. The dial's events arrive on
`mi_04&col02` (usage page ff00), not on `mi_04&col01` (ffc0) where commands and answers go:
Windows splits a HID interface into one collection per report id, hence `event_interfaces` in the
table. There is a third collection, `mi_03` (consumer control), that we don't use.
`cargo run -- headset 20` follows the headset for 20 s and prints every change;
`cargo run --example headset_probe` prints the raw bytes.

## Adding a headset

1. Copy `src/headsets/arctis_nova_pro_wireless.rs`, fill in the table: USB vendor and product ids,
   the part of the Windows device path that picks the right HID interface (list them with any HID
   browser; composite devices show up as `mi_0X&colYY`, and events may arrive on another
   collection than answers do: `headset_probe` listens on all of them and says which), part of
   the audio output's name, the
   report length, the status request, and for every message the device sends: how it starts and
   which byte means what (`Is`, `IsNot`, `Steps(max)`, `Percent`). Leave `dial_on` / `dial_off`
   empty if there is no dial.
2. Add one line to `src/headsets/mod.rs`.
3. Add a test with a reply captured from the real device (`headset_probe` prints them). That
   capture is what lets someone without the hardware review and later refactor the table.
4. Write the table from the protocol facts, in your own words; say where the facts came from. Do
   not paste code or data files from GPL projects without keeping their notices (see above), and
   nothing derived from a vendor's decrypted files.

## What a table can declare, and what the app does with it

A headset has no code of its own and no entry in the UI of its own. Its table says what it can do,
`Model::features()` hands that to the app, and the Settings page builds that headset's section from
it, only while the headset is plugged in. Someone without an Arctis never sees an Arctis setting.

- **A dial** (`dial_on` / `dial_off`): a switch "the dial sets the Game and Media balance".
- **Settings** (`settings`): one command and a value range each, optionally with names for the
  values ("Off", "Transparent", "On"). Shown as a picker. Headsets don't report most of these, so
  the app stores the user's choice (`headsetSettings`, by "model:key"), shows "As it is" until they
  pick, and sends every stored choice again whenever the headset appears. The Nova Pro has sidetone
  (`06 39`, 0-3) and noise cancelling (`06 bd`, 0-2): **neither has been tried on hardware yet.**
- **A display** (`oled`): size, the feature-report length, the strip width, the draw and release
  commands. `headset::frames` packs a row-by-row frame into the reports (tested byte for byte with
  a toy display). The app draws with a canvas, which brings fonts for free, thresholds it to one
  bit and sends it: today one picture, the balance while the dial turns, then the display goes
  back to the headset after two seconds. The Nova Pro's frame format is ggoled's description;
  `examples/oled_probe.rs` draws a test pattern and the base station accepted it.

### Plugins in the app, and the display (2026-09-21)

The sidebar gets a Plugins tab while a known device is plugged in; it lists the plugin with its
status, and the plugin's page (Controls, Display, Burn-in protection) is built from the table.
A plugin is compiled in and arrives with an app update. There is deliberately no in-app
"search and install": a headset table is a list of raw USB commands sent to someone's hardware,
and a pull request reviewed by a person is the safety net for that. What could be downloaded
safely later is data that can't hurt anything: display skins.

`app/src/screen.ts` decides what the display shows. Three modes: the headset's own screen (ours
only while the dial turns), a FreeSeries screen (time, battery, the main channel's sound), or dark.
Burn-in protection, for the modes where we are the one drawing: the screen moves along a small
orbit, one pixel a minute; it goes dark after the user has been away from keyboard and mouse for
a chosen time (`src/idle.rs`). The device's knobs are ours to show: the dial draws the balance, the
volume knob (`07 25`, counted down from 0x38) draws the volume, each for two seconds. Anything
else the device says by itself that the table can't read (`State::activity`) means someone is in
its own menu, which is drawn on the same display, so it gets the screen back for ten seconds.
Battery (`07 b7`) and connection (`07 b5`) events are in the table so they don't count as that. On Quit the watcher releases the display. Identical frames are not sent
twice. Checked in the browser preview (all transitions) and the frame format on hardware;
**not yet seen on the real display: the FreeSeries screen, dark mode, and whether a button press
on the base station really produces a message we can see** (the dial and volume do). A crash
leaves our last frame on the display until the base station is touched or power-cycled: the
watchdog could release it, not built.

Ideas on file: skins (a skin is a draw function on a canvas, so fonts, images and animation come
free; level meters at 10 frames a second are two 1 KB reports each), a screensaver that draws
something instead of nothing.

Only the watcher's thread talks to the device: `set`, `draw` and `release_display` queue an order
for it. A frame nobody has seen yet is replaced by the next one rather than queued behind it.

Still code rather than table: hardware EQ, and anything a device needs that these shapes don't
cover. Add a shape when a second headset needs it, not before.
