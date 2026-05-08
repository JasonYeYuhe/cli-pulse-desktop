# VM Smoke Instructions — cli-pulse-desktop

Self-contained step-by-step for verifying a new release on
`clipulse-win-test`. Designed so a fresh Claude Code session (or a
human in RDP) can run it without any prior context.

**Goal of this smoke**: confirm the new build LAUNCHES and STAYS
ALIVE for 60 s on Windows. That's it. We're not checking features —
just survival, because the v0.8.0 incident proved the launch path is
where things break first.

**Target version**: replace `<TAG>` everywhere below with the version
under test, e.g. `v0.8.2`. The test driver should pass `<TAG>` as the
first argument when invoking these instructions.

---

## Step 1 — uninstall any pre-existing CLI Pulse

```powershell
# Find an installed version (multiple names possible across releases)
$installed = Get-ItemProperty HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\* `
  -ErrorAction SilentlyContinue `
  | Where-Object { $_.DisplayName -like "*CLI Pulse*" }

if ($installed) {
  foreach ($app in $installed) {
    if ($app.UninstallString) {
      $u = $app.UninstallString -replace '"', ''
      # NSIS uninstaller — /S = silent
      Start-Process -Wait -FilePath $u -ArgumentList "/S"
    }
  }
  Start-Sleep -Seconds 5
}

# Confirm uninstall
Get-ItemProperty HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\* `
  -ErrorAction SilentlyContinue `
  | Where-Object { $_.DisplayName -like "*CLI Pulse*" } `
  | Select-Object DisplayName, DisplayVersion
# Expected output: nothing
```

## Step 2 — download v0.8.2 (or whatever `<TAG>` is)

```powershell
$tag = "<TAG>"  # e.g. "v0.8.2"
$installerPath = "$env:USERPROFILE\Downloads\CLI.Pulse_$($tag.TrimStart('v'))_x64-setup.exe"
$url = "https://github.com/JasonYeYuhe/cli-pulse-desktop/releases/download/$tag/CLI.Pulse_$($tag.TrimStart('v'))_x64-setup.exe"

Invoke-WebRequest -Uri $url -OutFile $installerPath
(Get-Item $installerPath).Length  # Should be roughly 3 MB (between 2.5M and 4M)
```

## Step 3 — install v0.8.2 silently

```powershell
Start-Process -Wait -FilePath $installerPath -ArgumentList "/S"
Start-Sleep -Seconds 5

# Confirm installation
$exe = "C:\Program Files\CLI Pulse\cli-pulse-desktop.exe"
(Get-Item $exe).VersionInfo.FileVersion  # Should match <TAG> minus 'v'
```

## Step 4 — download and run the launch-survival smoke script

```powershell
$scriptPath = "$env:USERPROFILE\Downloads\vm-smoke-launch.ps1"
Invoke-WebRequest `
  -Uri "https://raw.githubusercontent.com/JasonYeYuhe/cli-pulse-desktop/main/scripts/vm-smoke-launch.ps1" `
  -OutFile $scriptPath

powershell -ExecutionPolicy Bypass -File $scriptPath
```

The script:
- Records baseline (kills any pre-existing `cli-pulse-desktop` for clean start)
- Launches the GUI binary
- Waits **60 seconds** for the stability window
- Checks: original PID alive + zero new WER `Application Error` events
- Prints `PASS`, `DEGRADED`, or `FAIL` on the **last line** of output

## Step 5 — report

Copy the entire output of step 4 (it's short — about 20 lines) into
the chat. The Mac-side maintainer will read the verdict and either:
- **PASS** → flip `<TAG>` to Latest, you're done
- **FAIL** → revert Latest, investigate, ship a fix

If you cannot complete a step, say which one and paste the error.
That's still useful — partial info beats no info.

---

## Privacy reminder

Everything in this script's output is safe to paste:
- File version numbers, PIDs, file sizes — all non-sensitive
- WER message excerpts (first 3 lines per event) — exception code +
  faulting module, no user data
- The script does NOT touch oauth tokens, helper_secret, device_id,
  or anything in the user's keychain

If the script output looks weird, share it anyway — when in doubt,
debug-mode + paste-everything is faster than guessing what's safe.

---

## What if a step fails?

| Step | Failure | Fix |
|---|---|---|
| 1 | Uninstall hangs | Open Task Manager, kill any `Un_A.exe` from `C:\Program Files\CLI Pulse`, rerun |
| 2 | 404 on download URL | Tag may not be promoted to public yet — wait 30 s, retry, OR ask Mac-side whether the release was tagged + made non-draft |
| 3 | Install hangs | Same as step 1 — kill installer, rerun |
| 4 | Script can't download | Use `(New-Object System.Net.WebClient).DownloadFile($url, $path)` as fallback |
| 4 | `FAIL` verdict | **STOP — paste output to chat. Do not retry.** Failure is the data. |
