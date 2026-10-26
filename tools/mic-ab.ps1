# Records the headset microphone and "FreeSeries Mic" (what Discord hears) at the same time, so
# two people can listen to both and say what the mic path does to a voice. Talk while it runs.
#   pwsh tools/mic-ab.ps1 [-Mic 1] [-Cable 0] [-Seconds 10]      (numbers: the list it prints first)
# Writes mic-ab\headset.wav and mic-ab\freeseries-mic.wav in the repo. Try it at three noise-removal
# strengths (Mic page) and with Discord's own noise suppression off; the pair that sounds right wins.
param([int]$Mic = 1, [int]$Cable = 0, [int]$Seconds = 10)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
cargo build --release --example micspike --manifest-path "$root\Cargo.toml"
$exe = "$root\target\release\examples\micspike.exe"
& $exe
$out = "$root\mic-ab"
foreach ($d in 'headset', 'cable') { New-Item -ItemType Directory -Force "$out\$d" | Out-Null }
"recording $Seconds s: talk now"
$a = Start-Process -FilePath $exe -ArgumentList "$Mic $Seconds" -WorkingDirectory "$out\headset" -NoNewWindow -PassThru
$b = Start-Process -FilePath $exe -ArgumentList "$Cable $Seconds" -WorkingDirectory "$out\cable" -NoNewWindow -PassThru
$a.WaitForExit(); $b.WaitForExit()
Move-Item -Force "$out\headset\mic-raw.wav" "$out\headset.wav"
Move-Item -Force "$out\cable\mic-raw.wav" "$out\freeseries-mic.wav"
Remove-Item -Recurse -Force "$out\headset", "$out\cable"
"headset:        $out\headset.wav"
"what Discord hears: $out\freeseries-mic.wav"
