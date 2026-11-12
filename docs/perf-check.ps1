<#
  NetGlass tray-resident performance probe (read-only, no program changes).
  Usage:
    powershell -ExecutionPolicy Bypass -File "docs\perf-check.ps1" -Minutes 60   -IntervalSec 30
    powershell -ExecutionPolicy Bypass -File "docs\perf-check.ps1" -Minutes 1440 -IntervalSec 60
  Tree membership is resolved by ParentProcessId, so WebView2 processes
  belonging to other applications are not counted.

  NOTE: intentionally ASCII-only. Windows PowerShell 5.1 parses BOM-less
  .ps1 files with the system ANSI codepage; non-ASCII bytes corrupt quotes.
#>
param(
  [int]$Minutes = 60,
  [int]$IntervalSec = 30,
  [string]$Out = "$PSScriptRoot\perf-log.csv"
)
$ErrorActionPreference = 'Stop'

function Get-TreeIds([int]$root) {
  $all = Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId
  $ids = @($root)
  $q = @($root)
  while ($q.Count -gt 0) {
    $cur = $q[0]
    $q = @($q | Select-Object -Skip 1)
    foreach ($k in ($all | Where-Object { $_.ParentProcessId -eq $cur })) {
      $ids += $k.ProcessId
      $q += $k.ProcessId
    }
  }
  return ,$ids
}

$proc = Get-Process -Name 'netglass-native' -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { $proc = Get-Process -Name 'NetGlass' -ErrorAction SilentlyContinue | Select-Object -First 1 }
if (-not $proc) { throw 'No running NetGlass / netglass-native process found' }
$root = $proc.Id
$impl = $proc.ProcessName
$cores = (Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors

"Target: $impl (PID $root) | ${Minutes} min | interval ${IntervalSec}s | CSV: $Out"
"time,impl,proc_count,tree_ws_mb,tree_priv_mb,cpu_delta_s,battery_status,charge_pct" |
  Out-File -FilePath $Out -Encoding utf8

$end = (Get-Date).AddMinutes($Minutes)
$prevCpu = $null

while ((Get-Date) -lt $end) {
  $ps = @()
  foreach ($id in (Get-TreeIds $root)) {
    $p = Get-Process -Id $id -ErrorAction SilentlyContinue
    if ($p) { $ps += $p }
  }
  if ($ps.Count -eq 0) { Write-Warning 'Target process exited; sampling stopped'; break }

  $ws = [math]::Round((($ps | Measure-Object WorkingSet64 -Sum).Sum) / 1MB, 2)
  $pv = [math]::Round((($ps | Measure-Object PrivateMemorySize64 -Sum).Sum) / 1MB, 2)
  $cpu = 0.0
  foreach ($p in $ps) { $cpu += $p.TotalProcessorTime.TotalSeconds }
  $delta = if ($null -eq $prevCpu) { 0.0 } else { [math]::Round($cpu - $prevCpu, 4) }
  $prevCpu = $cpu

  $b = Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | Select-Object -First 1
  $bs = if ($b) { $b.BatteryStatus } else { 'N/A' }
  $ch = if ($b) { $b.EstimatedChargeRemaining } else { 'N/A' }

  Add-Content -Path $Out -Encoding utf8 -Value ("{0},{1},{2},{3},{4},{5},{6},{7}" -f `
    (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'), $impl, $ps.Count, $ws, $pv, $delta, $bs, $ch)
  Start-Sleep -Seconds $IntervalSec
}

$rows = @(Import-Csv $Out)
$samples = @($rows | Select-Object -Skip 1)   # first row cpu_delta is the init value
$minSamples = 3
if ($samples.Count -lt $minSamples) {
  ''
  '=== INSUFFICIENT DATA - assertions skipped ==='
  "Only $($samples.Count) sample(s); need at least $minSamples steady-state samples."
  'Likely cause: target process exited early, or -Minutes is too short for -IntervalSec.'
  'Startup / shutdown CPU spikes are NOT idle CPU; do not judge from a short window.'
  'Re-run, for example: -Minutes 60 -IntervalSec 30'
  "Partial CSV: $Out"
  return
}

$growth = [math]::Round([double]$samples[-1].tree_priv_mb - [double]$samples[0].tree_priv_mb, 2)
$peak = ($samples | ForEach-Object { [double]$_.tree_priv_mb } | Measure-Object -Maximum).Maximum
$avgDelta = ($samples | ForEach-Object { [double]$_.cpu_delta_s } | Measure-Object -Average).Average
$avgCpu = [math]::Round(100 * $avgDelta / $IntervalSec, 3)
$maxProc = ($samples | ForEach-Object { [int]$_.proc_count } | Measure-Object -Maximum).Maximum

function Show-Assert([string]$id, [string]$desc, [bool]$ok, [string]$detail) {
  "{0,-4} {1,-5} {2}  [{3}]" -f $id, $(if ($ok) { 'PASS' } else { 'FAIL' }), $desc, $detail
}

''
"=== Assertions | $($samples.Count) samples / $([math]::Round($samples.Count * $IntervalSec / 60, 1)) min / $cores logical cores ==="
Show-Assert 'A1' 'private memory growth under 10 MB' ($growth -lt 10) "growth $growth MB"
Show-Assert 'A2' 'idle avg CPU under 0.5 pct (one core)' ($avgCpu -lt 0.5) "${avgCpu}% / machine $([math]::Round($avgCpu / $cores, 4))%"
Show-Assert 'A3' 'peak tree private under 60 MB' ($peak -lt 60) "$peak MB"
Show-Assert 'A7' 'no child processes' ($maxProc -eq 1) "max tree processes $maxProc"
''
"Battery status: $($samples[-1].battery_status) (1=DC / 2=AC) | last charge $($samples[-1].charge_pct)%"
"CSV: $Out"
