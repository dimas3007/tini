# Measures RAM and CPU for each pill variant in two scenarios:
#   idle   : pill at rest (collapsed), no interaction
#   active : pill expands/collapses automatically every $CycleMs ms
# Results are written to results.md in this folder.
param(
    [int]$Warmup = 8,
    [int]$Sample = 20,
    [int]$CycleMs = 700,
    [string]$Bin = $(if ($env:CARGO_TARGET_DIR) { "$env:CARGO_TARGET_DIR\release" } else { "$PSScriptRoot\target\release" })
)

$variants = @(
    @{ Name = 'Composition (Win32 + DWM)'; Exe = "$Bin\pill-composition.exe"; Env = @{} },
    @{ Name = 'Slint femtovg (OpenGL)'; Exe = "$Bin\pill-slint.exe"; Env = @{ SLINT_BACKEND = 'winit-femtovg' } },
    @{ Name = 'Slint software'; Exe = "$Bin\pill-slint.exe"; Env = @{ SLINT_BACKEND = 'winit-software' } }
)
$cores = [Environment]::ProcessorCount

# dwm.exe CPU time (ms) via the raw performance counter; Get-Process dwm needs admin.
function Get-DwmCpu {
    try {
        $c = Get-CimInstance Win32_PerfRawData_PerfProc_Process -Filter "Name='dwm'" -ErrorAction Stop | Select-Object -First 1
        [double]$c.PercentProcessorTime / 10000
    } catch { $null }
}

function Measure-Variant($v, $mode) {
    foreach ($k in $v.Env.Keys) { Set-Item "env:$k" $v.Env[$k] }
    $argList = if ($mode -eq 'active') { @('--cycle', "$CycleMs") } else { @() }
    $p = if ($argList) { Start-Process -FilePath $v.Exe -ArgumentList $argList -PassThru } else { Start-Process -FilePath $v.Exe -PassThru }
    foreach ($k in $v.Env.Keys) { Remove-Item "env:$k" }

    Start-Sleep -Seconds $Warmup
    if ($p.HasExited) { return [pscustomobject]@{ Variant = $v.Name; Scenario = $mode; Notes = "crash, exit 0x{0:X}" -f $p.ExitCode } }

    $p.Refresh()
    $cpu0 = $p.TotalProcessorTime.TotalMilliseconds
    $dwm0 = Get-DwmCpu
    $t0 = [Diagnostics.Stopwatch]::StartNew()
    $wsPriv = @(); $privBytes = @(); $ws = @()
    for ($i = 0; $i -lt $Sample; $i++) {
        Start-Sleep -Seconds 1
        $c = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process -Filter "IDProcess=$($p.Id)"
        $p.Refresh()
        $wsPriv += [double]$c.WorkingSetPrivate
        $privBytes += [double]$p.PrivateMemorySize64
        $ws += [double]$p.WorkingSet64
    }
    $elapsed = $t0.Elapsed.TotalMilliseconds
    $p.Refresh()
    $cpu = ($p.TotalProcessorTime.TotalMilliseconds - $cpu0) / $elapsed * 100
    $dwm1 = Get-DwmCpu
    $threads = $p.Threads.Count
    Stop-Process -Id $p.Id -Force
    Start-Sleep -Seconds 2

    $median = { param($a) $s = $a | Sort-Object; $s[[int][math]::Floor($s.Count / 2)] }
    [pscustomobject]@{
        Variant             = $v.Name
        Scenario            = $mode
        'Private WS (MB)'   = [math]::Round((& $median $wsPriv) / 1MB, 1)
        'Working set (MB)'  = [math]::Round((& $median $ws) / 1MB, 1)
        'Private bytes (MB)'= [math]::Round((& $median $privBytes) / 1MB, 1)
        'CPU app (% 1 core)'= [math]::Round($cpu, 2)
        'CPU dwm (% 1 core)'= if ($null -ne $dwm0 -and $null -ne $dwm1) { [math]::Round(($dwm1 - $dwm0) / $elapsed * 100, 2) } else { 'n/a' }
        Thread              = $threads
        Notes               = ''
    }
}

$baselineDwm = $null
$d0 = Get-DwmCpu
if ($null -ne $d0) { $sw = [Diagnostics.Stopwatch]::StartNew(); Start-Sleep -Seconds $Sample; $baselineDwm = [math]::Round(((Get-DwmCpu) - $d0) / $sw.Elapsed.TotalMilliseconds * 100, 2) }

$rows = foreach ($v in $variants) { foreach ($m in 'idle', 'active') { Measure-Variant $v $m } }
$rows | Format-Table -AutoSize | Out-String -Width 220

$sizes = $variants | ForEach-Object { $_.Exe } | Select-Object -Unique | ForEach-Object { "- ``$(Split-Path $_ -Leaf)``: $([math]::Round((Get-Item $_).Length / 1MB, 2)) MB" }
$cols = 'Variant', 'Scenario', 'Private WS (MB)', 'Working set (MB)', 'Private bytes (MB)', 'CPU app (% 1 core)', 'CPU dwm (% 1 core)', 'Thread', 'Notes'
$md = @("# Notch pill spike results", "", "Measured $(Get-Date -Format 'yyyy-MM-dd HH:mm') on $((Get-CimInstance Win32_Processor).Name.Trim()), $cores logical threads. Warmup $Warmup s, sample $Sample s, active scenario toggles every $CycleMs ms.", "")
$md += "| " + ($cols -join ' | ') + " |"
$md += "|" + (($cols | ForEach-Object { '---' }) -join '|') + "|"
foreach ($r in $rows) { $md += "| " + (($cols | ForEach-Object { $r.$_ }) -join ' | ') + " |" }
$md += ""
$md += "Baseline dwm CPU without a pill: $(if ($null -ne $baselineDwm) { "$baselineDwm% 1 core" } else { 'n/a' })"
$md += ""
$md += "Binary size:"
$md += $sizes
$md | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'results.md')
"baseline dwm: $baselineDwm"
