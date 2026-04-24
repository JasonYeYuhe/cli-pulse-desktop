# Changelog

All notable changes to CLI Pulse Desktop (Windows + Linux).

## [0.1.3] — 2026-04-25

### Performance
- **Incremental scan cache** — per-provider JSON state at
  `~/Library/Caches/dev.clipulse.desktop/cost-usage/{codex,claude}-v1.json`
  (Linux: `~/.cache/...`, Windows: `%LOCALAPPDATA%\...`). Files whose
  (mtime, size) are unchanged since the last scan are skipped entirely;
  files that grew are parsed only from their previous size forward.
- **27× faster warm scan** on a dev machine with 2711 JSONL files:
  cold 36.2 s → warm 1.34 s. The 2-minute background sync tick goes
  from "noticeable CPU blip" to "invisible."
- ScanResult now reports `files_scanned` (actually touched) vs
  `files_cached` (reused from cache).

### Fixed
- Nothing user-visible since 0.1.2. Claude cost parity with the macOS
  Swift scanner is bit-exact on 04-18 through 04-21 (verified against
  the same week of local data).

### Build / internals
- New `cache.rs` module (450 lines) — schema ported from Swift
  `CostUsageCache.swift`, with explicit per-file state tracking
  (`mtime`, `size`, `parsed_bytes`, Codex `last_totals` + `session_id`).
- 11 new unit tests for cache arithmetic + decision logic + IO
  roundtrip. 38/38 Rust tests pass.
- `scanner.rs` refactored: parsers return per-file packed output
  instead of mutating global agg; outer loop handles cache decisions.

## [0.1.2] — 2026-04-24

### Added
- **7-day cost trend chart on Overview.** Inline SVG, stacked bars by
  provider (Claude green / Codex cyan / Other purple), hover for exact
  per-day breakdown. No new dependencies — <3 KB added to bundle.

### Fixed
- **Sessions project detection no longer surfaces "Library" or "Cellar".**
  Added an explicit filter for OS / toolchain path components
  (Library / Applications / Cellar / Homebrew / node_modules / Program
  Files / AppData / etc.) when extracting project names from cmdlines.
  Strict improvement over v0.1.1 — 5 new tests cover the filter.

## [0.1.1] — 2026-04-24

### Added
- **Alerts tab** — live view of client-computed alerts, 30-second auto-refresh.
- **Daily / weekly budget alerts** — configurable USD thresholds. When today's
  scanned spend exceeds the daily limit, or the rolling 7-day spend exceeds
  the weekly limit, an alert is pushed into `helper_sync` and a native
  notification fires (once per day per budget).
- **CPU spike alerts** — per-session CPU ≥ 80% (tunable in Settings) triggers
  a "Usage Spike" alert row, mirroring the iOS/macOS apps.
- **Budget settings UI** — Settings → Budget section with daily / weekly /
  CPU% inputs. Persists to `HelperConfig.thresholds` (server never sees
  the threshold, only the resulting alerts).
- New Tauri commands: `get_thresholds`, `set_thresholds`, `preview_alerts`.

### Changed
- `HelperConfig` gained a `thresholds` field; old v0.1.0 configs auto-migrate
  on load via serde defaults.
- `sync_now` / background tick now include computed alerts in helper_sync's
  `p_alerts` array.

### Fixed
- Nothing since v0.1.0 — no user-visible bugs reported in the 0 days it's
  been out 🙂.

## [0.1.0] — 2026-04-24

### Added
- Sprint 0: Local JSONL scanner (Codex + Claude) with bit-exact Swift parity.
- Sprint 1: Supabase pairing via 6-digit code, background `helper_sync` +
  `upsert_daily_usage` every 2 minutes.
- Sprint 2: Live sessions collector (sysinfo-based, 27 provider patterns,
  parent+child worker dedup). System tray with Windows first-class
  behavior and Linux graceful fallback.
- Sprint 2.5: Native notifications on pair success and sync failure
  streak (≥3 consecutive failures).
- Sprint 3: Auto-update via `tauri-plugin-updater` (signed releases
  from GitHub Releases). Settings → Updates button.

### Build
- Rust 1.90 + Tauri 2.10
- React 19 + TypeScript + Tailwind v4
- GitHub Actions CI: frontend + rustfmt + clippy + tests + tauri build
  on Windows and Linux.
- GitHub Actions Release: tag-triggered build + sign + draft release
  with `.exe` / `.deb` / `.rpm` / `.AppImage` + `latest.json`.
