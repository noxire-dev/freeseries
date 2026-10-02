# What a stalled audio thread costs. Runs a dev engine Media -> monitor with the soak tone through
# it, freezes the engine process twice (as a busy PC or a stalling USB controller would), and prints
# what the engine logged and what the tone measured.
#   tools\stall.ps1 [-Stalls 60,30] [-Seconds 14]
# The tone goes into the Media device, so it is also heard, quietly, through the installed app.
param([int[]]$Stalls = @(60, 30), [int]$Seconds = 14, [string]$Channel = '1', [string]$Output = '2')
$root = Split-Path $PSScriptRoot -Parent
$t = $env:TEMP
Add-Type -Namespace Nt -Name Proc -MemberDefinition @'
[DllImport("ntdll.dll")] public static extern int NtSuspendProcess(IntPtr handle);
[DllImport("ntdll.dll")] public static extern int NtResumeProcess(IntPtr handle);
'@
$engine = $null
try {
    $engine = Start-Process "$root\target\release\freeseries.exe" -ArgumentList 'run', $Channel, $Output -RedirectStandardError "$t\fs-stall-engine.log" -RedirectStandardOutput "$t\fs-stall-engine.out" -PassThru -NoNewWindow
    Start-Sleep 3
    $soak = Start-Process "$root\target\release\examples\soak.exe" -ArgumentList $Channel, $Output, "$Seconds" -RedirectStandardOutput "$t\fs-stall-soak.log" -RedirectStandardError "$t\fs-stall-soak.err" -PassThru -NoNewWindow
    Start-Sleep 4
    foreach ($ms in $Stalls) {
        $watch = [Diagnostics.Stopwatch]::StartNew()
        [void][Nt.Proc]::NtSuspendProcess($engine.Handle)
        while ($watch.Elapsed.TotalMilliseconds -lt $ms) { }
        [void][Nt.Proc]::NtResumeProcess($engine.Handle)
        "froze the engine for {0:n0} ms" -f $watch.Elapsed.TotalMilliseconds
        Start-Sleep 3
    }
    $soak.WaitForExit()
}
finally {
    if ($engine) { Stop-Process -Id $engine.Id -Force -ErrorAction SilentlyContinue }
}
Start-Sleep 1
"--- engine"
Get-Content "$t\fs-stall-engine.log" | Where-Object { $_ -match 'glitch|dry|late|buffer|channel|cushion' }
"--- tone"
Get-Content "$t\fs-stall-soak.log"
