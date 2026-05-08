# vm-smoke-full.ps1 — END-TO-END VM smoke for cli-pulse-desktop.
#
# One-shot script: uninstalls existing version, downloads + installs
# the target release, runs launch-survival check, prints PASS/FAIL.
# No Claude interpretation needed — RDP in, one command, get verdict.
#
# Usage (on the VM):
#   $url = "https://raw.githubusercontent.com/JasonYeYuhe/cli-pulse-desktop/main/scripts/vm-smoke-full.ps1"
#   iwr $url -OutFile $env:TEMP\smoke.ps1
#   powershell -ExecutionPolicy Bypass -File $env:TEMP\smoke.ps1 -Tag v0.8.2
#
# Or as a single line (no file):
#   $url = "https://raw.githubusercontent.com/JasonYeYuhe/cli-pulse-desktop/main/scripts/vm-smoke-full.ps1"
#   iex "& { $(iwr $url) } -Tag v0.8.2"
#
# Last line of output is the verdict — copy entire output to chat.

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Tag,

    # Override the default GitHub repo if you need to test against a fork
    [string]$Repo = "JasonYeYuhe/cli-pulse-desktop",

    # Stability window after launch (seconds)
    [int]$StabilitySeconds = 60
)

$ErrorActionPreference = "Stop"
$VersionNoV = $Tag.TrimStart('v')

Write-Host "=== cli-pulse-desktop FULL smoke (Tag=$Tag) ==="
Write-Host ""

# -----------------------------------------------------------------------------
# Step 1 — uninstall any pre-existing CLI Pulse
# -----------------------------------------------------------------------------
Write-Host "[1/4] Uninstalling any pre-existing CLI Pulse..."
$installed = Get-ItemProperty HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\* `
    -ErrorAction SilentlyContinue `
    | Where-Object { $_.DisplayName -like "*CLI Pulse*" }

if ($installed) {
    foreach ($app in $installed) {
        Write-Host "  Found: $($app.DisplayName) $($app.DisplayVersion)"
        if ($app.UninstallString) {
            $u = $app.UninstallString -replace '"', ''
            Write-Host "  Running: $u /S"
            try {
                Start-Process -Wait -FilePath $u -ArgumentList "/S" -ErrorAction Stop
            } catch {
                Write-Host "  Uninstall produced an error (continuing): $_"
            }
        }
    }
    Start-Sleep -Seconds 5
} else {
    Write-Host "  None found, nothing to uninstall."
}

# Verify uninstall
$still = Get-ItemProperty HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\* `
    -ErrorAction SilentlyContinue `
    | Where-Object { $_.DisplayName -like "*CLI Pulse*" }
if ($still) {
    Write-Host "  WARNING: uninstall did not remove all entries:"
    $still | Select-Object DisplayName, DisplayVersion | Format-Table | Out-String | Write-Host
}
Write-Host ""

# -----------------------------------------------------------------------------
# Step 2 — download installer
# -----------------------------------------------------------------------------
Write-Host "[2/4] Downloading $Tag installer..."
$installerName = "CLI.Pulse_${VersionNoV}_x64-setup.exe"
$installerPath = "$env:TEMP\$installerName"
$installerUrl = "https://github.com/$Repo/releases/download/$Tag/$installerName"

try {
    Invoke-WebRequest -Uri $installerUrl -OutFile $installerPath -UseBasicParsing
} catch {
    Write-Host "  ERROR: failed to download $installerUrl"
    Write-Host "  Detail: $_"
    Write-Host ""
    Write-Host "FAIL"
    exit 1
}

$size = (Get-Item $installerPath).Length
Write-Host "  Downloaded: $installerPath ($size bytes)"
if ($size -lt 1500000) {
    Write-Host "  ERROR: installer is suspiciously small (< 1.5 MB) — bundle regression?"
    Write-Host ""
    Write-Host "FAIL"
    exit 1
}
Write-Host ""

# -----------------------------------------------------------------------------
# Step 3 — install silently
# -----------------------------------------------------------------------------
Write-Host "[3/4] Installing $Tag silently..."
Start-Process -Wait -FilePath $installerPath -ArgumentList "/S"
Start-Sleep -Seconds 5

$exe = "C:\Program Files\CLI Pulse\cli-pulse-desktop.exe"
if (-not (Test-Path $exe)) {
    Write-Host "  ERROR: $exe not present after install — installer didn't put binary in expected location"
    Write-Host ""
    Write-Host "FAIL"
    exit 1
}
$installedVer = (Get-Item $exe).VersionInfo.FileVersion
Write-Host "  Installed: $exe"
Write-Host "  FileVersion: $installedVer (expected $VersionNoV)"
if ($installedVer -notlike "$VersionNoV*") {
    Write-Host "  WARNING: installed version doesn't match target tag — proceeding anyway"
}
Write-Host ""

# -----------------------------------------------------------------------------
# Step 4 — launch + 60s stability check
# -----------------------------------------------------------------------------
Write-Host "[4/4] Launch survival check (${StabilitySeconds}s window)..."

# Clean baseline
Get-Process cli-pulse-desktop -ErrorAction SilentlyContinue | ForEach-Object {
    Write-Host "  Stopping pre-existing PID $($_.Id) for clean baseline"
    Stop-Process -Id $_.Id -Force
}
Start-Sleep -Seconds 2

$baselineTime = Get-Date
Write-Host "  Baseline timestamp: $baselineTime"

Write-Host "  Launching $exe ..."
$proc = Start-Process -FilePath $exe -PassThru
$launchedPid = $proc.Id
Write-Host "  Launched PID: $launchedPid"

Write-Host "  Waiting ${StabilitySeconds}s for stability window..."
Start-Sleep -Seconds $StabilitySeconds

# Checks
$origAlive = $null -ne (Get-Process -Id $launchedPid -ErrorAction SilentlyContinue)
$anyProc = Get-Process cli-pulse-desktop -ErrorAction SilentlyContinue
$anyAlive = ($null -ne $anyProc)

# WER: any new "Application Error" events for cli-pulse-desktop since baseline?
$crashEvents = @()
try {
    $crashEvents = Get-WinEvent -LogName Application -MaxEvents 50 -ErrorAction SilentlyContinue `
        | Where-Object {
            $_.TimeCreated -gt $baselineTime `
                -and $_.ProviderName -eq "Application Error" `
                -and $_.Message -match "cli-pulse-desktop"
        }
} catch {}
$crashEventCount = ($crashEvents | Measure-Object).Count

Write-Host "  Original PID $launchedPid alive after ${StabilitySeconds}s: $origAlive"
if ($anyProc) {
    $procIds = ($anyProc | ForEach-Object { $_.Id }) -join ", "
    Write-Host "  cli-pulse-desktop PID(s) currently running: $procIds"
} else {
    Write-Host "  cli-pulse-desktop PID(s) currently running: NONE"
}
Write-Host "  WER 'Application Error' events for cli-pulse-desktop since baseline: $crashEventCount"
if ($crashEventCount -gt 0) {
    Write-Host "  --- Crash events ---"
    $crashEvents | ForEach-Object {
        Write-Host "    $($_.TimeCreated): [$($_.Id)] $($_.LevelDisplayName)"
        $_.Message -split "`n" | Select-Object -First 3 | ForEach-Object { Write-Host "      $_" }
    }
}
Write-Host ""

# -----------------------------------------------------------------------------
# Verdict
# -----------------------------------------------------------------------------
Write-Host "=== Verdict ==="
Write-Host "  Tag:                   $Tag"
Write-Host "  Installed version:     $installedVer"
Write-Host "  Original PID alive:    $origAlive"
Write-Host "  Any process running:   $anyAlive"
Write-Host "  Zero crash events:     $($crashEventCount -eq 0)"
Write-Host ""

if ($origAlive -and ($crashEventCount -eq 0)) {
    Write-Host "PASS"
    exit 0
} elseif ($anyAlive -and ($crashEventCount -eq 0)) {
    Write-Host "DEGRADED: original PID died but a process is running (supervisor restart?). Investigate."
    exit 2
} else {
    Write-Host "FAIL"
    exit 1
}
