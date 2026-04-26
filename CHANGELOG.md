# Changelog

All notable changes to CLI Pulse Desktop (Windows + Linux).

## [0.2.4] — 2026-04-26

### Added
- **Brand icons.** Replaced the Tauri scaffold default icons with the
  proper CLI Pulse 1024×1024 brand mark (sourced from the iOS app's
  `AppIcon.appiconset`). Tauri regenerated the per-platform variants
  (NSIS / .icns / Windows tiles / Android mipmaps).
- **Sentry crash + error reporting** wired (`src-tauri/src/sentry_init.rs`).
  No-op when `CLI_PULSE_SENTRY_DSN` is unset (default), so privacy stance
  is "opt-in only." Privacy filter matches the Swift / Kotlin
  counterparts: `sendDefaultPii = false`, `tracesSampleRate = 0`,
  `before_send` scrubs `$HOME` paths. See README → "Optional: Sentry."
- **Pre-push git hook** (`scripts/git-hooks/pre-push`) that runs the
  same gates as CI (rustfmt + clippy + tests + frontend build) before
  every push. One-time install: `scripts/install-git-hooks.sh`. Skip
  with `--no-verify`. Motivation: the v0.2.3 host-TZ-dependent test
  bug should never have hit CI.
- README rewritten — full layout map + sprint history + Sentry setup.

### Fixed
- N/A — no production bugs reported since v0.2.3.

### Internal
- `PROJECT_FIX_2026-04-26_v0.2.3_test_tz_dependency.md` archives the
  v0.2.3 test-harness host-TZ-dependency bug (per the project's
  "every fix gets a write-up" policy).

## [0.2.3] — 2026-04-25

### Build / internals (no user-facing changes)
- **Integration test framework** in `src-tauri/tests/scanner_integration.rs`.
  10 fixture-based end-to-end tests that build synthetic JSONL files in
  a temp dir and assert the scanner emits the expected `DailyEntry`
  shapes. Coverage:
  - Codex cumulative `total_token_usage` delta math (3-turn case)
  - Codex pricing applied at the right granularity
  - **Claude per-message tiered pricing** (the bug we caught back in
    Sprint 0 — two 150K-token Sonnet messages must price as 2× $0.45,
    NOT as 300K aggregate which would cross the 200K tier)
  - Streaming-chunk token dedup via `(message.id, requestId)` while the
    `__claude_msg__` synthetic bucket counts every event
  - **Timezone date-range filter** with explicit `today_override`
    (would have caught the v0.2.2 bug)
  - Out-of-range files excluded from the result
  - Cache makes repeat scans idempotent (cold → warm transition)
  - Multi-day events grouped correctly by local date
- `ScanOptions` gained 3 test-only fields: `codex_roots_override`,
  `claude_roots_override`, `today_override`. Production code passes
  `None` and behavior is unchanged. Frontend types untouched.
- 52 / 52 Rust tests pass on macOS (4 platforms × CI matrix similar).
  Up from 42 in v0.2.2.

## [0.2.2] — 2026-04-25

### Fixed
- **Timezone scan-range bug.** Non-UTC users (especially JST and other
  UTC+ timezones) saw today's usage stuck at 0 between local 00:00 and
  ~09:00. Per-event day classification was in local time but the scan
  range was anchored on UTC, so today's events got tagged with a
  later date than the filter allowed and were silently dropped from
  the Overview, chart, daily-budget alerts, and helper_sync upload.
  Fixed by anchoring `today`, `since`, `until_key`, and `today_key`
  all on `chrono::Local::now()`. Caught by Codex independent review.
  See [PROJECT_FIX_2026-04-25_v0.2.2_timezone.md](PROJECT_FIX_2026-04-25_v0.2.2_timezone.md).
- 4 new regression tests in `scanner.rs::tests` cover today_key /
  range consistency and `parse_day_key_local` edge cases. 42/42 Rust
  tests pass (was 38).

## [0.2.1] — 2026-04-25

### Added
- **Providers tab: expandable per-model breakdown.** Click any provider
  row to see the top 10 models contributing to its spend, with input /
  output tokens and per-model cost. Provider rows also show a small
  progress bar relative to the top spender — quick visual ranking.
- **Export scan data.** *Settings → Export* buttons download the last
  30 days of local scan data as CSV (for Excel / Google Sheets) or JSON
  (full `ScanResult` shape, useful for scripting).
  - CSV columns: `date, provider, model, input_tokens, cached_tokens,
    output_tokens, cost_usd, message_count`.
  - Client-side only — no server round-trip.

### Not in this release
- **Server-side `dashboard_summary` on desktop** was considered but
  skipped: the existing RPC requires a user JWT (iOS / macOS / Android
  get one from OAuth signin), while the desktop app authenticates as a
  paired *device* with `helper_secret`. Surfacing server aggregates
  here would require a new `get_daily_usage_for_device` RPC on the
  shared Supabase backend — a cross-project schema change that
  shouldn't be made without an explicit plan.

## [0.2.0] — 2026-04-25

### Added
- **🌏 Internationalization.** UI now ships in English, **简体中文**, and
  **日本語**. Choice persists in `localStorage` and respects the OS
  language on first launch. Switch any time from *Settings → Language*.
  Infra is `i18next` + `react-i18next` (~62 KB gz added to bundle).
- **🖥️ ARM64 builds.** Release + CI workflows now matrix-build on four
  platforms: Windows x64, **Windows ARM64**, Linux x64, **Linux ARM64**.
  Native builds (no QEMU / cross-compile), so the runtime is as fast as
  x64 on equivalent silicon. Latest.json includes all four signatures.

### Build
- CI matrix additions: `windows-11-arm`, `ubuntu-24.04-arm`. Rust cache
  is partitioned by OS key so parallel matrix jobs don't trample each
  other's target directories.
- Release artifacts grow from 4 to 8 installers + 8 .sig files + 1
  latest.json = 17 assets per release.

### Notes
- **Minor version bump (0.1 → 0.2)** because i18n is a substantive new
  user-facing surface. Auto-update path from any 0.1.x continues to
  work — the signing key is unchanged.

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
