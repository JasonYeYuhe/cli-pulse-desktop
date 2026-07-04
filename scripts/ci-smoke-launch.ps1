<#
.SYNOPSIS
  v0.11.0 — Windows GUI launch-smoke. Runs headlessly in CI (windows-latest)
  and is the automated form of the mandatory pre-publish smoke gate.

.DESCRIPTION
  Launches the freshly-built RELEASE binary in smoke mode and asserts the two
  historical launch-incident classes cannot regress unnoticed:

    HARD gates (non-zero exit → job fails):
      1. process survives the launch window          → catches crash-on-launch
                                                         (v0.8.0 BEX64 /
                                                         STATUS_STACK_BUFFER_OVERRUN)
      2. the frontend-ready marker file appears       → catches white-screen
         (React mounted + IPC round-trip works)         (v0.2.11)
      3. built exe is multi-MB (not a tiny sidecar)   → belt-and-braces vs the
                                                         v0.2.10 wrong-binary class
                                                         (release.yml has the
                                                         authoritative bundle check)

    SOFT checks (warn only — a non-interactive CI desktop can render oddly;
    the marker already proves the frontend mounted):
      4. a top-level "CLI Pulse" window exists (EnumWindows)
      5. a full-screen screenshot (CopyFromScreen) → uploaded as an artifact

  RELEASE (not debug) is deliberate: v0.8.0's crash and v0.8.2's stderr-pipe
  panic were release-only (windows_subsystem="windows", no console attached).

.EXITCODES
  0 = PASS   1 = crash-on-launch   2 = white-screen (no marker)   3 = bad binary
#>
[CmdletBinding()]
param(
  [string]$Exe = "src-tauri/target/release/cli-pulse-desktop.exe",
  [int]$TimeoutSec = 45,
  [string]$OutDir = "smoke-artifacts"
)

$ErrorActionPreference = "Stop"
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Capture-Screenshot {
  param([string]$Name)
  try {
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $path = Join-Path $OutDir $Name
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host "screenshot: $path"
  } catch {
    Write-Host "::warning::screenshot failed: $($_.Exception.Message)"
  }
}

function Collect-Logs {
  # Best-effort: copy the app log dir so a failure is diagnosable from the
  # uploaded artifact (matches where the in-app 'Copy diagnostic' reads).
  $logDir = Join-Path $env:LOCALAPPDATA "dev.clipulse.desktop\logs"
  if (Test-Path $logDir) {
    Copy-Item (Join-Path $logDir "*") $OutDir -ErrorAction SilentlyContinue
    Write-Host "collected app logs from $logDir"
  }
}

if (-not (Test-Path $Exe)) {
  Write-Host "::error::built exe not found at $Exe"
  exit 3
}
$exePath = (Resolve-Path $Exe).Path
$marker  = Join-Path ([System.IO.Path]::GetTempPath()) "clipulse-frontend-ready.marker"
if (Test-Path $marker) { Remove-Item $marker -Force }

Write-Host "== launch-smoke =="
Write-Host "exe:    $exePath"
Write-Host "marker: $marker"

# (3) GUI-binary sanity — the sidecar diagnostic binaries are ~sub-MB; the GUI
# binary is multi-MB. Cheap first line of defence vs the wrong-binary class.
$exeSize = (Get-Item $exePath).Length
Write-Host "exe size: $exeSize bytes"
if ($exeSize -lt 1000000) {
  Write-Host "::error::exe < 1MB ($exeSize B) — looks like a sidecar, not the GUI binary"
  exit 3
}

# Smoke env: marker path + disable the remote agent loop (determinism — no
# network in CI; the device is unpaired anyway, this is belt-and-braces).
$env:CLI_PULSE_SMOKE_MARKER          = $marker
$env:CLI_PULSE_DISABLE_REMOTE_AGENT  = "1"

$proc = Start-Process -FilePath $exePath -PassThru
Write-Host "launched pid $($proc.Id)"

# Poll for the marker while asserting the process stays alive (1).
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$mounted = $false
while ((Get-Date) -lt $deadline) {
  if ($proc.HasExited) {
    Write-Host "::error::process exited early (exit $($proc.ExitCode)) before frontend-ready — crash-on-launch class (cf. v0.8.0)"
    Capture-Screenshot "crash-screenshot.png"
    Collect-Logs
    exit 1
  }
  if (Test-Path $marker) { $mounted = $true; break }
  Start-Sleep -Milliseconds 500
}

# (2) White-screen gate.
if (-not $mounted) {
  Write-Host "::error::frontend-ready marker never appeared within ${TimeoutSec}s — white-screen class (cf. v0.2.11). process-alive=$(-not $proc.HasExited)"
  Capture-Screenshot "whitescreen-screenshot.png"
  Collect-Logs
  Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
  exit 2
}
Write-Host "PASS(2): frontend-ready marker present — React mounted"
Get-Content $marker | ForEach-Object { Write-Host "  marker> $_" }

# Let the window finish first paint before enumerating / screenshotting.
Start-Sleep -Seconds 3

# (1) Re-assert alive after mount (didn't mount-then-die).
if ($proc.HasExited) {
  Write-Host "::error::process exited right after mounting (exit $($proc.ExitCode))"
  Capture-Screenshot "postmount-crash.png"
  Collect-Logs
  exit 1
}
Write-Host "PASS(1): process alive after mount (pid $($proc.Id))"

# (4) SOFT — enumerate top-level windows owned by our pid for a CLI Pulse title.
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class WinEnum {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
"@
$titles = New-Object System.Collections.ArrayList
$targetPid = [uint32]$proc.Id
$cb = [WinEnum+EnumProc] {
  param($h, $l)
  $sb = New-Object System.Text.StringBuilder 256
  [void][WinEnum]::GetWindowText($h, $sb, 256)
  $t = $sb.ToString()
  if ($t.Length -gt 0) {
    $wpid = [uint32]0
    [void][WinEnum]::GetWindowThreadProcessId($h, [ref]$wpid)
    if ($wpid -eq $targetPid) { [void]$titles.Add($t) }
  }
  return $true
}
[void][WinEnum]::EnumWindows($cb, [IntPtr]::Zero)
Write-Host "windows for pid ${targetPid}: $(if ($titles.Count) { $titles -join ' | ' } else { '(none)' })"
if (($titles | Where-Object { $_ -match "CLI Pulse" }).Count -gt 0) {
  Write-Host "PASS(4): top-level 'CLI Pulse' window present"
} else {
  Write-Host "::warning::no 'CLI Pulse' window enumerated for pid $targetPid — CI desktop render may be limited; the marker gate already proved the frontend mounted"
}

# (5) SOFT — screenshot for the artifact.
Capture-Screenshot "launch-screenshot.png"
Copy-Item $marker (Join-Path $OutDir "frontend-ready.marker") -ErrorAction SilentlyContinue
Collect-Logs

Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Write-Host "== launch-smoke PASS =="
exit 0
