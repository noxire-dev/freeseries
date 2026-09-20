# Third-party material

freeseries is licensed under the GNU Affero General Public License v3.0 (see `LICENSE`). It ships or relies on the following work by others.

## Shipped in the repository and the installer

- **FxSound virtual audio driver** (`app/src-tauri/resources/fxvad/`: `fxvad.sys`, `fxvad.inf`, `fxvadntamd64.cat`). Copyright FxSound LLC, AGPL-3.0, derived from Microsoft's SYSVAD sample (MS-LPL). Unmodified binaries from <https://github.com/fxsound2/fxsound-app> (`Installer/Drivers/Version14/win10/x64`), signed by the Microsoft Windows Hardware Compatibility Publisher. Source: <https://github.com/fxsound2/fxsound-driver>. The files must stay byte-identical: rebuilding or re-saving them loses the signature. The INF in FxSound's git repository is LF-normalised; the copy here has its original CRLF line endings restored so it matches the signed catalog.
- **Inter** typeface, via `@fontsource-variable/inter`. Copyright The Inter Project Authors, SIL Open Font License 1.1.
- **Settings icon** path from Lucide (<https://lucide.dev>), ISC License.

## Used at run time, not shipped

- **AutoEq** headphone presets (<https://github.com/jaakkopasanen/AutoEq>), MIT License for the project; the underlying measurements belong to their authors (oratory1990, Rtings, crinacle and others). freeseries downloads a preset only when the user picks one and does not redistribute the database.
- **Dolby Atmos for Headphones, DTS Headphone:X, Windows Sonic**: the user's own spatial renderer, used through Windows. freeseries contains none of it.

## Knowledge, not code

- The Arctis Nova Pro Wireless base-station protocol (`src/arctis.rs`) follows what HeadsetControl (GPL-3.0), Dakumodo/Nova_pro_reloaded and hardtekpt/arctis_nova_pro_hid document. No code was copied.
- The PSense presets come from the zhelper fork of G-Helper by this project's author.

Rust and npm dependencies carry their own licenses; `cargo tree` and `npm ls` list them.
