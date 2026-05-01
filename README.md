# CLI Pulse — Desktop (Windows + Linux)

The Windows + Linux build of **CLI Pulse**. Tracks usage, tokens, and cost for
26 AI command-line tools (Claude Code, Codex, Gemini CLI, Cursor, Kimi, GLM,
and more) from a single native desktop app, and syncs aggregated history with
the iPhone / Apple Watch / Android / macOS apps via a shared Supabase backend.

## Part of CLI Pulse

| Platform                        | Source                                                                      | Distribution        |
| ------------------------------- | --------------------------------------------------------------------------- | ------------------- |
| macOS · iOS · iPadOS · watchOS  | [JasonYeYuhe/cli-pulse](https://github.com/JasonYeYuhe/cli-pulse) (`CLI Pulse Bar/`) | App Store           |
| Android                         | [JasonYeYuhe/cli-pulse](https://github.com/JasonYeYuhe/cli-pulse) (`android/`) | Google Play         |
| **Windows · Linux**             | **this repo** (Rust + Tauri 2)                                              | GitHub Releases     |

This repo is separate from the main one because it shares no client code with
the Apple/Android apps (Rust + Tauri vs Swift/Kotlin) and has its own CI
matrix + release channel. The on-device JSONL scanner here is bit-exact
parity with the Swift implementation in the main repo.

**Latest release**: see
[Releases](https://github.com/JasonYeYuhe/cli-pulse-desktop/releases).
Auto-update is signed and runs on every launch (Settings → Updates).

## Stack

- **Backend**: Rust 1.85+ / Tauri 2 / sysinfo / serde_json / chrono / reqwest
- **Frontend**: React 19 + TypeScript + Tailwind CSS v4 / i18next (en/zh-CN/ja)
- **Targets**: Windows 10/11 (NSIS) + Linux (.deb / .rpm / .AppImage). x64 + ARM64.
- **Backend integration**: Supabase (paired via 6-digit code from iPhone)

## Quick start

```bash
npm install
scripts/install-git-hooks.sh   # one-time: enables pre-push gate
npm run tauri dev              # dev with hot reload
```

### Linux build dependencies (Ubuntu/Debian)

```bash
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libssl-dev patchelf xdg-utils
```

### Run tests

```bash
(cd src-tauri && cargo test)   # 52+ Rust tests, ~1 s
npm run build                  # frontend type-check + bundle
```

The pre-push hook (`scripts/git-hooks/pre-push`) runs all of these
before each `git push`. Skip a single push with `git push --no-verify`.

## Layout

```
src/                          React + TypeScript frontend
src/locales/{en,zh-CN,ja}.json    i18n strings
src/i18n.ts                       i18next bootstrap

src-tauri/
  src/
    lib.rs                    Tauri entry + tauri::command surface
    alerts.rs                 Client-side budget + CPU spike alerts
    cache.rs                  Incremental scan cache (mtime+size + parsed_bytes)
    config.rs                 HelperConfig persistence (paired device + thresholds)
    creds.rs                  Supabase URL / anon key (env-overridable)
    notify.rs                 Native notification helpers
    paths.rs                  Cross-platform ~/.claude, ~/.codex resolution
    pricing.rs                Per-token rates for Codex + Claude
    scanner.rs                JSONL scanner + per-day/model aggregation
    sentry_init.rs            Sentry wiring (no-op when DSN unset)
    sessions.rs               Live process collector (sysinfo + 27 patterns)
    supabase.rs               REST + RPC client
    tray.rs                   System tray (Win first-class, Linux fallback)
  tests/
    scanner_integration.rs    10 fixture-based end-to-end tests
  tauri.conf.json
  Cargo.toml

.github/workflows/
  ci.yml                      4-platform matrix: rustfmt + clippy + tests + tauri build
  release.yml                 4-platform matrix: signed bundle + draft release on tag

scripts/
  install-git-hooks.sh        One-time setup
  git-hooks/pre-push          Run CI gates locally before push

PROJECT_FIX_*.md              Forensic write-ups for individual fixes
CHANGELOG.md
RELEASE.md                    How to cut a release (tag → CI → publish)
```

## Optional: Sentry

Crash + error reporting is wired but disabled by default. To enable:

```bash
# Build-time DSN (baked into the binary)
CLI_PULSE_SENTRY_DSN="https://...@sentry.io/..." npm run tauri build

# Or runtime — set env var before launching
CLI_PULSE_SENTRY_DSN="https://...@sentry.io/..." ./CLI Pulse
```

Empty / unset DSN is a clean no-op — zero events leave the machine.
Privacy stance matches the iOS / macOS apps: `sendDefaultPii = false`,
`tracesSampleRate = 0`, and a `before_send` filter scrubs `$HOME` paths
from event payloads. See `src-tauri/src/sentry_init.rs`.

## Sprint history

See [`CHANGELOG.md`](CHANGELOG.md) for what shipped in each release.
The summary as of v0.2.3:

- **Sprint 0**: scaffold + local JSONL scan with bit-exact Swift parity (v0.1.0)
- **Sprint 1**: Supabase pair + helper_sync + upsert_daily_usage (v0.1.0–v0.1.1)
- **Sprint 2**: live sessions + system tray + native notifications (v0.1.x)
- **Sprint 3**: signed auto-update + GitHub Releases automation (v0.1.0)
- **Sprint 4**: Alerts tab + budget thresholds (v0.1.1)
- **Sprint 5**: 7-day chart + sessions filter (v0.1.2)
- **Sprint 6**: 27× incremental scan cache (v0.1.3)
- **Sprint 7**: i18n (en/zh-CN/ja) + ARM64 builds (v0.2.0)
- **Sprint 8**: Providers per-model + CSV/JSON export (v0.2.1)
- **Sprint 9**: integration test framework (v0.2.3) + critical TZ hotfix (v0.2.2)
- **Sprint 10**: brand icons + Sentry + pre-push hook (v0.2.4)
