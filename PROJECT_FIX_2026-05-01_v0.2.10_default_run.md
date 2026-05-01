# PROJECT FIX — v0.2.10 — multi-bin default-run / packaging regression

**Severity:** P0 / catastrophic
**Discovered:** 2026-05-01 by first real human Windows GUI test on Azure VM
**Affects:** all 14 prior releases (v0.1.0 through v0.2.9) — Windows NSIS,
Linux `.deb`, Linux `.rpm`. AppImage was unaffected.
**Fix shipped in:** v0.2.10
**Yanked:** v0.2.9 (set to draft 2026-05-01, `latest.json` redirected to
v0.2.8). Earlier versions left as-is — they were equally broken but
auto-update had nothing to refresh from since no GUI ever launched.

## Symptom

Running `*-setup.exe` on Windows or installing `*.deb` on Linux:

1. Installer runs cleanly, no errors.
2. Start Menu shortcut / `.desktop` entry appears.
3. Launching the shortcut produces a flash of console window. No GUI ever
   appears.
4. Windows Event Viewer / Linux journal show no errors — the binary that
   ran (`scan_cli`) is *supposed* to be a one-shot CLI tool that exits 0.

The diagnostic on Azure Windows Server 2025 VM (the very first time anyone
launched the installed product on a real Windows machine):

```
Desktop GUI 不存在。快捷方式跑的是 scan_cli.exe,一闪而过的控制台,
没有窗口。
```

## What was actually shipped

`v0.2.9` `.deb` payload (`dpkg-deb -c CLI.Pulse_0.2.9_amd64.deb`):

```
usr/share/icons/hicolor/128x128/apps/scan_cli.png
usr/share/icons/hicolor/32x32/apps/scan_cli.png
usr/share/icons/hicolor/256x256@2/apps/scan_cli.png
usr/share/applications/CLI Pulse.desktop
usr/bin/scan_cli                          ← THE WRONG BINARY
```

Note the `.desktop` file is named "CLI Pulse" (matching `productName`) but
the executable is `scan_cli`. The bundler honored two different sources of
truth and they diverged.

NSIS sizes across all releases (sanity-check the regression is universal):

| Version | x64-setup.exe | Verdict |
|---------|---------------|---------|
| v0.1.0  | 0.56 MB       | broken  |
| v0.1.3  | 0.58 MB       | broken  |
| v0.2.0  | 0.58 MB       | broken  |
| v0.2.1  | 0.58 MB       | broken  |
| v0.2.2  | 0.58 MB       | broken  |
| v0.2.3  | 0.58 MB       | broken  |
| v0.2.4  | 0.58 MB       | broken  |
| v0.2.6  | 0.58 MB       | broken  |
| v0.2.7  | 0.58 MB       | broken  |
| v0.2.8  | 0.59 MB       | broken  |
| v0.2.9  | 0.61 MB       | broken  |

Expected: ~7 MB (matches local `cargo build --bin cli-pulse-desktop` output
of 6.6 MB stripped + Tauri WebView2 host shim). AppImage was correct
(~70 MB self-contained) which is why nobody noticed.

## Root cause

`src-tauri/src/bin/scan_cli.rs` and `src-tauri/src/bin/sessions_smoke.rs`
were added in v0.1.0 as developer diagnostic tools. Cargo auto-registers
any `src/bin/*.rs` as bin targets. Combined with the implicit "main"
binary from `src-tauri/src/main.rs`, this gives a multi-bin crate with
three bins:

- `cli-pulse-desktop` (from `src/main.rs`, name from `[package].name`)
- `scan_cli`
- `sessions_smoke`

When `cargo tauri build --target X --bundles Y` runs without an explicit
`--bin`, cargo builds **all** bins. The Tauri bundler then picks one to
package by querying cargo metadata. With no `default-run` declared, cargo
returns the bins in implementation-defined order, and Tauri's
NSIS/.deb/.rpm bundlers grabbed `scan_cli` (alphabetically before
`cli-pulse-desktop` after the package-name normalization step that turns
hyphens into underscores under the hood — `cli_pulse_desktop` vs
`scan_cli`, the latter wins by Cargo's metadata ordering for these
specific bundler paths).

The AppImage bundler uses a different code path that resolves the binary
via `[package].name` directly, so it always picked the right bin — which
is why AppImage was 70+ MB and worked, masking the issue.

## Why CI never caught it

The full release matrix (4 platforms × build + sign + upload) was green
for all 14 versions:

- `cargo build` succeeded.
- `cargo clippy` was clean.
- `cargo test` ran 90+ tests, all green.
- `cargo tauri build --bundles ...` exited 0 and produced files.
- Files were signed and uploaded to GitHub Releases.

But CI never **installed and launched** the resulting installer. Every
unit and integration test ran against the source tree, not the packaged
artifact. The bundler quietly produced a `.deb` that contained the wrong
binary, with no way for any pre-existing test to detect it.

## Fix

### 1. `src-tauri/Cargo.toml`

```toml
[package]
name = "cli-pulse-desktop"
version = "0.2.10"
# ...
default-run = "cli-pulse-desktop"
```

`default-run` is the canonical Cargo mechanism for disambiguating which
binary is "the default" in a multi-bin crate. It propagates to:

- `cargo run` (no `--bin` arg) → runs `cli-pulse-desktop`.
- `cargo tauri build` → bundles `cli-pulse-desktop`.
- The bundler's binary-resolution logic on every platform.

Verified locally with `cargo build --release --bin cli-pulse-desktop` →
6.6 MB stripped binary (matches the expected packaged size minus
WebView2 shims).

### 2. `.github/workflows/release.yml` — bundle verification step

Added a **post-build regression guard** that runs after `tauri-action`
inside the same matrix job:

- Asserts NSIS `*-setup.exe` ≥ 3 MB.
- Asserts Linux `.deb` ≥ 3 MB, `.rpm` ≥ 3 MB, AppImage ≥ 30 MB.
- Inspects NSIS via `7z l` and asserts `cli-pulse-desktop.exe` is
  present.
- Inspects `.deb` via `dpkg-deb -c` and asserts `usr/bin/cli-pulse-desktop`
  is present.
- Failure makes the matrix job red (with structured `::error::` output).
  The release stays as draft (per `releaseDraft: true`); the human
  un-draft gate now sees red CI and refuses to publish.

This guard is intentionally narrow:

- It catches *this* class of bug (wrong binary bundled, or no binary
  bundled).
- It does NOT catch crashes-on-launch, missing assets, missing icons,
  unsigned binaries, or signature-verification failures. Those need a
  human-VM smoke test (see Process Change).

## Process change

The `feedback_desktop_autonomy.md` release contract previously said:

> After CI green: `git tag vX.Y.Z` and let the release workflow produce
> 17 signed artifacts. Verify a `.deb` signature with minisign before
> publishing the draft. Then `gh release edit vX.Y.Z --draft=false
> --latest` to push live.

Updated to:

> After CI green: `git tag vX.Y.Z`. Wait for the release workflow's
> matrix to go full-green (now with bundle-content guards). Before
> un-drafting:
>
> 1. Verify `.deb` signature with minisign.
> 2. **Spin up the test Azure Windows VM (`clipulse-win-test` in the
>    `cli-pulse-test-rg` resource group, Japan East) and install +
>    launch the new NSIS installer. Verify the GUI window opens, the
>    five tabs render, the tray icon appears, Settings → About shows
>    the new version, and `Event Viewer Application` log has no
>    errors after 60 seconds.** Take one screenshot, save under
>    `releases/screenshots/vX.Y.Z-windows.png` for the forensic record.
> 3. Same dance for Ubuntu (UTM ARM VM locally) — install `.deb`,
>    launch, screenshot.
> 4. Only then `gh release edit vX.Y.Z --draft=false --latest`.
> 5. Stop the Azure VM (deallocate) immediately after to stop billing.

This adds ~10 minutes of human time per release. Cheap insurance against
shipping another silent packaging regression.

## Independent observations from the diagnostic VM session

These came up during the v0.2.9 inspection but are unrelated to the
packaging bug. Filed for future fix, none P0:

- `scan_cli.exe` printed `Total cost: $-0.0000`. Negative-zero float
  representation when `cost_nanos` accumulates to exactly 0 over an
  empty path scan. Cosmetic but wrong-looking. Fix when scanner code
  next touches.
- `scan_cli` reported "1 file, 9460 tokens" on a fresh Windows VM where
  `~/.claude` and `~/.codex` definitively do not exist. Some other
  scanner-discoverable location is being read — possibly Claude Code
  CLI's own session log under `%LOCALAPPDATA%`. Worth investigating —
  if real, it changes the documented "scanner only reads the two well-
  known providers" contract.
- WebView2 Runtime was preinstalled on Windows Server 2025 (147.0.3912.98).
  The bug was 100% in packaging; the runtime side is fine.

## Verification (post-fix)

After v0.2.10's matrix completes green:

1. `gh release view v0.2.10 --json assets` should show:
   - `*-setup.exe` ≥ 5 MB (target ~6–8 MB)
   - `*.deb` ≥ 5 MB
   - `*.rpm` ≥ 5 MB
   - `*.AppImage` ~70 MB (unchanged)
2. Local sanity check:
   ```bash
   gh release download v0.2.10 -p "*amd64.deb"
   dpkg-deb -c CLI.Pulse_0.2.10_amd64.deb | grep cli-pulse-desktop
   # should print: usr/bin/cli-pulse-desktop
   ```
3. RDP into Azure VM, install x64 NSIS, launch — must see GUI window
   with 5 tabs.
4. Sentry: a fresh session should appear in the `desktop` project for
   the first time ever (since this is the first version where the GUI
   actually launches on Windows).

## Lesson

A `.deb` 10× smaller than the AppImage for the same product is a
screaming red flag. Visual inspection of asset sizes after every
release should be a 5-second habit. The new size-assertion step
formalizes that into CI so it's never forgotten.

Codex review caught the previous three production bugs (timezone,
i18n Promise, CRLF byte-offset). It did not catch this one because
its read-only sandbox can't observe the *output* of the build — only
the source. Human-in-the-VM testing is irreplaceable for packaging
regressions.
