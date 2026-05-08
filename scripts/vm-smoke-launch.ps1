# vm-smoke-launch.ps1 — single-script launch survival smoke for cli-pulse-desktop.
#
# Run on the test VM (clipulse-win-test) AFTER installing a new release.
# No Claude session needed — RDP in, copy-paste, run, copy output back.
#
#   powershell -ExecutionPolicy Bypass -File vm-smoke-launch.ps1
#
# Outputs:
#   PASS or FAIL on the last line. If FAIL, the lines above show why.
#
# What it does:
#   1. Records baseline (current installed version + WER pre-launch state)
#   2. Launches cli-pulse-desktop.exe
#   3. Waits 60 s
#   4. Checks: process still alive + no new BEX64 / 0xc0000409 in WER
#   5. Prints PASS/FAIL summary
#
# Lessons baked in from the v0.8.0 incident:
#   - Don't trust "GUI window appears" — it can appear briefly then crash
#   - Don't trust "process exists" alone — supervisor may auto-restart
#   - Always cross-check WER for crash events in the test window
#
# Privacy: the script does NOT read or print any oauth tokens, helper_secret,
# or device_id. If you copy the output into chat, it's safe.

$ErrorActionPreference = "Stop"

Write-Host "=== cli-pulse-desktop launch survival smoke ==="
Write-Host ""

# 1. Baseline ----------------------------------------------------------------
$exe = "C:\Program Files\CLI Pulse\cli-pulse-desktop.exe"
if (-not (Test-Path $exe)) {
    Write-Host "FAIL: $exe not found — installer didn't put the binary in the expected location."
    exit 1
}
$installedVersion = (Get-Item $exe).VersionInfo.FileVersion
Write-Host "Installed binary: $exe"
Write-Host "FileVersion:      $installedVersion"

# Kill any pre-existing instance so we get a clean baseline
Get-Process cli-pulse-desktop -ErrorAction SilentlyContinue | ForEach-Object {
    Write-Host "Stopping pre-existing PID $($_.Id) for clean baseline"
    Stop-Process -Id $_.Id -Force
}
Start-Sleep -Seconds 2

$baselineTime = Get-Date
Write-Host "Baseline timestamp: $baselineTime"
Write-Host ""

# 2. Launch ------------------------------------------------------------------
Write-Host "Launching $exe ..."
$proc = Start-Process -FilePath $exe -PassThru
$launchedPid = $proc.Id
Write-Host "Launched PID: $launchedPid"

# 3. Wait + observe ----------------------------------------------------------
Write-Host "Waiting 60 s for stability window ..."
Start-Sleep -Seconds 60

# 4. Checks ------------------------------------------------------------------
$checks = @{}

# 4a. Original PID alive?
$origAlive = $null -ne (Get-Process -Id $launchedPid -ErrorAction SilentlyContinue)
$checks["original_pid_alive"] = $origAlive
Write-Host "Original PID $launchedPid alive after 60 s: $origAlive"

# 4b. Any cli-pulse-desktop process at all? (handles supervisor-restart case)
$anyProc = Get-Process cli-pulse-desktop -ErrorAction SilentlyContinue
$checks["any_proc_running"] = ($null -ne $anyProc)
if ($anyProc) {
    $procIds = ($anyProc | ForEach-Object { $_.Id }) -join ", "
    Write-Host "cli-pulse-desktop PID(s) currently running: $procIds"
} else {
    Write-Host "cli-pulse-desktop PID(s) currently running: NONE"
}

# 4c. New crash events in WER since baseline?
$crashEvents = @()
try {
    $crashEvents = Get-WinEvent -LogName Application -MaxEvents 50 -ErrorAction SilentlyContinue `
        | Where-Object { $_.TimeCreated -gt $baselineTime -and $_.ProviderName -eq "Application Error" -and $_.Message -match "cli-pulse-desktop" }
} catch {}
$crashEventCount = ($crashEvents | Measure-Object).Count
$checks["zero_crash_events"] = ($crashEventCount -eq 0)
Write-Host "WER 'Application Error' events for cli-pulse-desktop since baseline: $crashEventCount"
if ($crashEventCount -gt 0) {
    Write-Host "--- Crash events ---"
    $crashEvents | ForEach-Object {
        Write-Host "  $($_.TimeCreated): $($_.LevelDisplayName) [$($_.Id)]"
        # First 3 lines of message — usually the exception code + faulting module
        $_.Message -split "`n" | Select-Object -First 3 | ForEach-Object { Write-Host "    $_" }
    }
}

# 5. Verdict -----------------------------------------------------------------
Write-Host ""
Write-Host "=== Verdict ==="
foreach ($k in $checks.Keys) {
    $v = if ($checks[$k]) { "OK" } else { "FAIL" }
    Write-Host "  $k : $v"
}

# Original PID alive AND no crash events = PASS
# Any other combo = FAIL or DEGRADED
if ($checks["original_pid_alive"] -and $checks["zero_crash_events"]) {
    Write-Host ""
    Write-Host "PASS"
    exit 0
} elseif ($checks["any_proc_running"] -and $checks["zero_crash_events"]) {
    # Supervisor restart but no crashes — still suspicious
    Write-Host ""
    Write-Host "DEGRADED: original PID died but a process is running (supervisor restart?). Investigate."
    exit 2
} else {
    Write-Host ""
    Write-Host "FAIL"
    exit 1
}
