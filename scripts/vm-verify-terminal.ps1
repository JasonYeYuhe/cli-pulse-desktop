# vm-verify-terminal.ps1 — Windows-VM verification for the terminal epic (T0).
#
# Runs the cross-platform `terminal_smoke` binary on the real Windows VM
# (clipulse-win-test) to confirm the PTY host STREAMS stdout incrementally
# over a genuine ConPTY pseudoconsole (CI's windows-latest runner is the
# primary gate; this is the on-the-real-VM confirmation).
#
# It asserts the LOCAL streamed signal only — the smoke prints a PASS/FAIL
# verdict from what it read off the PTY. There is deliberately NO Sentry
# query here: Sentry ingestion is async, so a fast `age:-1h` query right
# after a run returns "No issues found" BEFORE the event indexes and gives
# false confidence. If you want the Sentry cross-check, poll it 30–60 s
# LATER as a secondary signal, never as the pass condition.
#
# PREREQUISITES on the VM (one-time):
#   * Rust toolchain (rustup + MSVC build tools) — `rustc --version` works
#   * A clone of the repo (default C:\src\cli-pulse-desktop; override -RepoPath)
#
# Usage (on the VM, from an elevated or normal PowerShell):
#   powershell -ExecutionPolicy Bypass -File scripts\vm-verify-terminal.ps1
#   # or against a repo elsewhere / a specific ref:
#   powershell -ExecutionPolicy Bypass -File scripts\vm-verify-terminal.ps1 -RepoPath D:\work\cli-pulse-desktop -Ref main
#
# The last line of output is the verdict — copy the entire output to chat.

[CmdletBinding()]
param(
    # Where the repo is cloned on the VM.
    [string]$RepoPath = "C:\src\cli-pulse-desktop",

    # Git ref to check out + pull before running (empty = use the working
    # tree as-is, no fetch).
    [string]$Ref = "main"
)

$ErrorActionPreference = "Stop"

Write-Host "=== cli-pulse-desktop terminal streaming verify (VM / ConPTY) ==="
Write-Host ""

# ---------------------------------------------------------------------------
# Step 1 — prerequisites
# ---------------------------------------------------------------------------
Write-Host "[1/3] Checking prerequisites..."
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    Write-Host "  ERROR: cargo not found on PATH. Install the Rust toolchain (rustup) first."
    Write-Host ""
    Write-Host "FAIL"
    exit 1
}
Write-Host "  cargo: $((cargo --version) 2>&1)"

if (-not (Test-Path $RepoPath)) {
    Write-Host "  ERROR: repo not found at $RepoPath. Clone it or pass -RepoPath."
    Write-Host ""
    Write-Host "FAIL"
    exit 1
}
Write-Host "  repo:  $RepoPath"
Write-Host ""

# ---------------------------------------------------------------------------
# Step 2 — sync to the target ref (best-effort)
# ---------------------------------------------------------------------------
Set-Location $RepoPath
if ($Ref) {
    Write-Host "[2/3] Syncing to '$Ref'..."
    try {
        git fetch --quiet origin $Ref
        git checkout --quiet $Ref
        git pull --quiet --ff-only origin $Ref
        $sha = (git rev-parse --short HEAD).Trim()
        Write-Host "  at $Ref @ $sha"
    } catch {
        Write-Host "  WARNING: git sync failed (continuing with the current working tree): $_"
    }
} else {
    Write-Host "[2/3] Using the current working tree (no fetch)."
}
Write-Host ""

# ---------------------------------------------------------------------------
# Step 3 — run the streaming smoke, capture + echo output, parse the verdict
# ---------------------------------------------------------------------------
Write-Host "[3/3] Running terminal_smoke (this builds the bin first run)..."
Write-Host ""

$out = & cargo run --quiet --manifest-path (Join-Path $RepoPath "src-tauri\Cargo.toml") --bin terminal_smoke 2>&1
$exit = $LASTEXITCODE
$out | ForEach-Object { Write-Host "  $_" }
Write-Host ""

# The smoke's own last non-empty line is PASS / FAIL / SKIP; trust that AND
# the process exit code together.
$verdictLine = ($out | Where-Object { $_ -match '^(PASS|FAIL|SKIP)$' } | Select-Object -Last 1)

Write-Host "=== Verdict ==="
Write-Host "  smoke exit code:  $exit"
Write-Host "  smoke verdict:    $verdictLine"
Write-Host ""

if ($verdictLine -eq "PASS" -and $exit -eq 0) {
    Write-Host "PASS"
    exit 0
} elseif ($verdictLine -eq "SKIP") {
    Write-Host "SKIP: no PTY could be allocated on this VM — cannot assert streaming here."
    Write-Host "  (This is environmental, not a code regression. Retry from an interactive session.)"
    exit 0
} else {
    Write-Host "FAIL"
    exit 1
}
