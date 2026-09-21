# Builds the installer and runs it over the installed app the way Tauri's updater would: no pages,
# no uninstall, settings and shortcuts stay, and the app starts again when it is done.
#
#   /P       passive: a progress bar, no questions
#   /UPDATE  skips the "already installed" page and the uninstall hook (windows/hooks.nsh)
#   /NS      leaves the shortcuts alone (passive mode would add one to the desktop)
#   /R       starts the app afterwards
#
# The installer closes every freeseries-app.exe first, a running dev build included. Audio is
# gone for the few seconds in between.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)
npm run tauri build
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$setup = Get-ChildItem src-tauri/target/release/bundle/nsis/*-setup.exe | Sort-Object LastWriteTime | Select-Object -Last 1
Write-Host "Installing $($setup.Name)"
Start-Process $setup.FullName -ArgumentList '/P', '/UPDATE', '/NS', '/R' -Wait
