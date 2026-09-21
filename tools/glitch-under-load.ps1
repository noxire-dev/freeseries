# Does audio glitch while the PC is busy building? Read docs/roadmap.md first: the "installed" line counts
# what the installed app logs, and builds before the wake-up fix log this script's own test tone starting
# and stopping as glitches. With the fix in, anything it still counts under load is real.
#   pwsh tools/glitch-under-load.ps1 [-Channels 1] [-Output 2] [-Seconds 60]      (device numbers: cargo run)
# Needs: cargo build --release; cargo build --release --example soak. Plays a quiet test tone for that long.
# Runs a dev engine Media -> monitor with a test tone through it, a heavy parallel compile as the
# load, and reports: glitches the dev engine logged, what the tone measured, and what the installed
# engine logged in the same window (its own channel, its own output: a second witness).
param([string]$Channels = '1', [string]$Output = '2', [int]$Seconds = 60, [string]$Engine = "C:\Users\noxire\falke\Projects\freeseries_radar\target\release\freeseries.exe", [string]$Label = "current code")
$root = "C:\Users\noxire\falke\Projects\freeseries_radar"
Set-Location $root
$t = $env:TEMP
$prodLog = "$env:LOCALAPPDATA\dev.freeseries.app\logs\FreeSeries.log"
$started = @()
try {
    $engine = Start-Process $Engine -ArgumentList 'run', $Channels, $Output -RedirectStandardError "$t\fs-repro-engine.log" -RedirectStandardOutput "$t\fs-repro-engine.out" -PassThru -NoNewWindow
    $started += $engine
    Start-Sleep 3
    $soak = Start-Process "$root\target\release\examples\soak.exe" -ArgumentList '1', $Output, "$Seconds" -RedirectStandardOutput "$t\fs-repro-soak.log" -RedirectStandardError "$t\fs-repro-soak.err" -PassThru -NoNewWindow
    $started += $soak
    $from = Get-Date
    # The load: a from-scratch debug build of the Tauri app's dependency tree, all cores, lots of disk.
    if (Test-Path "$root\target\loadgen") { Remove-Item "$root\target\loadgen" -Recurse -Force -ErrorAction SilentlyContinue }
    $load = Start-Process "$env:USERPROFILE\.cargo\bin\cargo.exe" -ArgumentList 'build', '-q', '--manifest-path', "$root\app\src-tauri\Cargo.toml", '--target-dir', "$root\target\loadgen" -RedirectStandardError "$t\fs-repro-load.err" -RedirectStandardOutput "$t\fs-repro-load.out" -PassThru -NoNewWindow
    $started += $load
    $soak.WaitForExit()
    $to = Get-Date
}
finally {
    # Stop the load and everything it spawned, then the engine. By id, never by name.
    Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*target\loadgen*' -or $_.CommandLine -like '*target/loadgen*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Get-Process | Where-Object { $_.Path -like '*\freeseries_radar\target\release\*' } | ForEach-Object { Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue }
}
Start-Sleep 1
$e = Get-Content "$t\fs-repro-engine.log"
$count = { param($lines, $pattern) @($lines | Select-String $pattern).Count }
$s = Get-Content "$t\fs-repro-soak.log"
$stamp = { param($line) if ($line -match '^\[(\d{4}-\d\d-\d\d)\]\[(\d\d:\d\d:\d\d)\]') { [datetime]::ParseExact($Matches[1] + ' ' + $Matches[2], 'yyyy-MM-dd HH:mm:ss', $null) } }
$prod = Get-Content $prodLog | Where-Object { $at = & $stamp $_; $at -and $at -ge $from.AddSeconds(-1) -and $at -le $to.AddSeconds(1) }
$dev = (& $count $e 'capture glitch') + (& $count $e 'ran dry')
"== $Label, $Seconds s under build load"
"dev engine : {0} capture glitches, {1} dry warnings, {2} trims" -f (& $count $e 'capture glitch'), (& $count $e 'ran dry'), (& $count $e 'dropped \d+ ms')
"tone       : {0}" -f ((($s | Select-String 'dropouts|phase jumps').Line | ForEach-Object { ($_ -split ' ')[0..2] -join ' ' }) -join ' | ')
"installed  : {0} capture glitches, {1} dry warnings, {2} trims (its own route, same window)" -f (& $count $prod 'capture glitch'), (& $count $prod 'ran dry'), (& $count $prod 'dropped \d+ ms')
if ($dev -gt 0) { "VERDICT: RED" } else { "VERDICT: green" }

"--- seconds with a capture glitch:"
"dev       : " + ((($e | Select-String 'capture glitch').Count -gt 0) ? "(the CLI log has no timestamps: see counts)" : "none")
"installed : " + (($prod | Select-String 'capture glitch' | ForEach-Object { $_.Line.Substring(13, 8) } | Select-Object -Unique) -join ' ')

