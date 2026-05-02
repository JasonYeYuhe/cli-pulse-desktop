# Changelog

All notable changes to CLI Pulse Desktop (Windows + Linux).

## [0.3.1] — 2026-05-02

### Fixed
- **Multi-device daily-usage clobbering (P0)**. The previous schema
  keyed `daily_usage_metrics` on `(user_id, metric_date, provider,
  model)` — per-user, no device dimension. With v0.3.0 onboarding new
  pure-Win/Linux users alongside their existing Mac scanner, every
  device's 2-minute sync would race-clobber the same row, so dashboard
  totals reflected whichever device synced last instead of the sum.
  v0.3.1 adds `device_id` to the schema PK and aggregates across devices
  in the dashboard read paths.

### Added
- **Tauri daily-usage upload restored** via a new
  `helper_sync_daily_usage` RPC (sibling to `helper_sync` —
  helper-credential auth, 200-row cap, per-row sub-transactions for
  fault isolation). Reports `metrics_synced` / `metrics_errored`
  counts in the manual-sync confirmation message.
- **`get_daily_usage_by_device`** server-side RPC for a future
  per-device breakdown UI ("Mac $5.20 + Win $1.80 = $7.00"). The
  RPC is in place; the UI surface ships separately when ready.

### Server-side migration
Deployed via `migrate_v0.37_daily_usage_device_id.sql` in the main repo:
- Add `device_id uuid` column with the nil-UUID sentinel
  (`'00000000-0000-0000-0000-000000000000'`) as default; existing
  rows backfill to the sentinel without a table rewrite.
- Swap PK to `(user_id, device_id, metric_date, provider, model)`.
- Add `devices_id_not_nil_uuid` check constraint so no real device
  row can ever take the sentinel value (Codex review: explicit
  INSERTs could supply nil despite the `gen_random_uuid()` default).
- Replace `upsert_daily_usage` with a 2-arg version (`metrics`,
  `p_device_id` default null). Validates device ownership against
  `auth.uid()` when an explicit device_id is supplied (closes Codex's
  device-name-leak vector through the new `get_daily_usage_by_device`
  join).
- Update `get_daily_usage` to SUM across `device_id` so the public
  JSON shape stays unchanged for iOS/Android consumers.
- Add `get_daily_usage_by_device(days)` RPC, joined on
  `(id, user_id)` so the JOIN cannot leak foreign device names.
- Old 1-arg `upsert_daily_usage(jsonb)` is explicitly DROPed before
  the new 2-arg version is created (Codex review:
  `CREATE OR REPLACE FUNCTION` with extra args creates a new overload
  alongside the old, leaving a broken function callable in production).

### macOS scanner
- `APIClient.syncDailyUsage` now passes `HelperConfig.load()?.deviceId`
  as `p_device_id` so the Mac contributes its own row alongside any
  Tauri devices on the same account. Pre-pair / nil cases fall through
  to the sentinel UUID for backward compatibility.

### Architecture decision (recorded in spec)
- Original spec proposed extending `helper_sync` with `p_daily_usage`.
  When pulling the live `helper_sync` body to write the migration, the
  existing function turned out to be much richer than the spec
  assumed (per-device `pg_advisory_xact_lock`, sophisticated session
  column shapes, future-date clamps, two-loop provider-quota model).
  Replacing that body wholesale to add a parameter would carry
  regression risk for v0.3.1. v0.3.1 uses a **sibling RPC**
  (`helper_sync_daily_usage`) instead — 2 RPCs/cycle for Tauri
  (negligible at 2-min cadence). Future v0.4.0 cleanup may unify
  paths once both are stable.

### Reviews
- Gemini 3.1 Pro (product/UX): caught broken rollback strategy when
  multi-device rows exist; spec now has a SUM-collapse rollback
  script.
- Codex GPT-5.4 (SQL/security): caught three FIX-FIRSTs (overload
  semantics, nil-UUID device insertion, foreign device-name leak via
  JOIN). All resolved before deploy.

## [0.3.0] — 2026-05-02

### Added
- **Direct email sign-in** — pure Windows / Linux users can now onboard
  without owning a Mac. Enter your email in Settings → Sign in to CLI
  Pulse, receive a 6-digit code, and the desktop mints its own helper
  credentials against your Supabase account. New users get auto-signed
  up; existing Mac/iOS account holders sign in to the same account.
  The legacy "pair from a Mac menu bar" 6-digit-code flow is preserved
  under an "Advanced" disclosure.
- **OS-native refresh-token storage** via the `keyring` crate:
  - macOS → Keychain
  - Windows → Credential Manager
  - Linux → Secret Service / libsecret
  Linux without libsecret fails closed with a clear install-hint error
  rather than falling back to weak machine-id-derived encryption.
- **helper_sync error classifier** — when sync hits an auth-shaped
  error (HTTP 401/403, "Device not found"), the desktop now asks the
  server whether the device or account is still healthy via a new
  `device_status` RPC. If the device or account is gone, the desktop
  clears local credentials and shows a "sign in again" notification
  instead of looping on 401s indefinitely.
- **Localization**: new `auth.signin.*` and `messages.signed_in_as`
  keys for English, Simplified Chinese, and Japanese (translation
  reviewed by Gemini 3.1 Pro).

### Server-side
- Two new strictly-additive RPCs deployed via
  `migrate_v0.36_desktop_otp.sql` in the main repo:
  - `register_desktop_helper(p_device_name, ...)` — auth.uid()-based
    mirror of `register_helper`. Mints a `device_id + helper_secret`
    against the user's session JWT. Includes per-user 20-device cap
    enforced via `pg_advisory_xact_lock` (race-safe), and
    `set search_path = pg_catalog, public, extensions` to mirror
    existing `register_helper` hardening.
  - `device_status(p_device_id, p_helper_secret)` — anon-callable but
    secret-gated. Returns `'healthy' | 'device_missing' |
    'account_missing'`. Returns `device_missing` for both genuinely
    missing devices and hash-mismatches so it cannot be used to
    enumerate device UUIDs.

### Privacy
- **Sentry scrubber tightened** — the `before_send` hook now redacts
  JWTs, helper secrets (`helper_<64hex>`), and `refresh_token` /
  `access_token` / `helper_secret` / `pairing_code` / `Authorization`
  query parameters embedded in error messages, breadcrumb URLs, and
  request bodies. Org-level field-name scrubbing still applies; this
  is belt-and-suspenders for content-shaped tokens.

### Notes
- v0.3.0 is the first release where Tauri can sign up new accounts
  directly. Mac users with an existing pairing keep working unchanged.
- Refresh strategy is **lazy / on-demand only**: the desktop never
  background-refreshes user JWTs. helper_sync uses device credentials
  exclusively. Refresh runs only when a user-scoped action needs the
  user JWT (rare today; will see more use as future features land).
- v0.3.1 (next release) restores per-device daily-usage syncing for
  the desktop with a multi-device-aware schema.

## [0.2.14] — 2026-05-02

### Fixed
- **Sync no longer reports failure after a successful sessions+alerts
  upload (P0).** A cross-platform audit on 2026-05-02 found that every
  paired Windows/Linux desktop was hitting an auth-shape mismatch on the
  daily-usage upload step (the `upsert_daily_usage` RPC required a user
  JWT but Tauri only has the helper's anon-key credentials). Sessions
  and alerts had been landing in Supabase correctly, but the daily-usage
  step bubbled up as a hard error and the entire sync surfaced as
  failed in the manual-sync UI. We've removed the broken call. Per-device
  daily usage is intentionally absent from the desktop until v0.3.1, which
  introduces a multi-device-aware path so Mac and Windows scanners stop
  race-clobbering the same row.
- **Watch auth tokens no longer linger in unprotected UserDefaults
  (P1).** Pre-v0.2.14 builds wrote the access + refresh tokens to both
  the Keychain (canonical) and watchOS UserDefaults (unencrypted at rest).
  v0.2.14 removes the UserDefaults write sites and adds a one-shot
  launch-time migration: any stranded values are adopted into Keychain
  (only when the Keychain is empty for that key — never overwriting a
  fresh value with a stale one) and the UserDefaults entries are cleared.
  No re-authentication required for the common case.
- **Misleading SQL doc comment on `register_helper` (P3).** The block
  comment claimed authentication required `auth.uid()` to match. The
  function actually validates the supplied pairing code and doesn't
  read `auth.uid` at all. Updated the comment to match the implementation
  and forward-reference the `auth.uid()`-based `register_desktop_helper`
  RPC coming in v0.3.0.

### Coming next
- v0.3.0: direct email sign-in for Tauri desktop (no more "find a Mac
  to pair from"). Spec lives at
  `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md`. Pure Windows / Linux
  users get a single-step OTP onboarding.
- v0.3.1: per-device `daily_usage_metrics` so Mac + Windows + Linux all
  contribute correctly to dashboard cost totals across devices.

## [0.2.13] — 2026-05-02

### Fixed
- **Misleading pairing instructions (P2 UX).** The Settings tab told users
  to "open CLI Pulse on iOS → Settings → Add device" and copy the 6-digit
  code from the phone. The iOS app has no such UI — `generatePairingCode()`
  is only ever called from the macOS menu bar app
  (`PairingSection.swift:46`), confirmed by repo-wide grep. The instruction
  was a dead-end for everyone, and especially confusing for Windows / Linux
  users who don't own a Mac (those users currently cannot onboard at all).
  - Fix: rewrite `pair_heading` and `pair_hint` in `src/locales/{en,zh-CN,ja}.json`.
    New text points users to the macOS menu bar app (the actual code source)
    and previews v0.3.0's email-based sign-in for Mac-less users.
  - This is a stop-gap. The architectural fix — direct email sign-in on
    Tauri desktop — is tracked in
    `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md` and slated for
    v0.3.0. After v0.3.0 ships, both the new copy and the pairing-code UI
    will be replaced with email + OTP.

### Notes
- v0.2.13 is purely a string change. No Rust changes; no schema changes;
  no behavior changes. The pairing flow itself works correctly when the
  user has a Mac to source the code from.

## [0.2.12] — 2026-05-02

### Fixed
- **Opus 4.7 missing from pricing table (P1).** Real-world test on Windows
  surfaced three symptoms that all traced to the same root cause:
  per-row Cost in the Today's Detail table rendered as `—`, the 7-day
  cost trend chart drew an empty frame with no bars, and the Provider
  quota bar in the Providers tab collapsed to invisible. Each pathway
  reads `cost_usd` (or aggregates of it) from the scan result; for any
  unrecognized model, `pricing::claude_cost_usd` returns `None` →
  frontend gets `null` → Cost cell shows `—`, daily totals are 0,
  `maxCost` is 1, all bar heights are 0%. The `claude-opus-4-7` model
  ID was missing from `CLAUDE_MODELS` (the table only covered Opus
  4.5 / 4.6 / 4 / 4.1).
  - Fix: `src-tauri/src/pricing.rs` — add `claude-opus-4-7` entry
    using the same per-token rates as Opus 4.5 / 4.6 (Anthropic's
    pricing pattern for the 4.5+ generation).
  - Test: `claude_cost_opus_4_7_priced_like_4_5_4_6` regression test
    locks the rate in. 8/8 pricing tests pass; 53/53 total Rust tests.
  - User-visible impact: anyone running Claude Code on Opus 4.7 (the
    current default) saw $0.00 / dash everywhere. After upgrading,
    Cost / chart / quota bar all populate from the same scan data
    that was already being collected — no rescan needed; the cache
    holds raw token counts and cost is recomputed on read.

### Notes
- v0.2.11 was held in `prerelease` and never promoted to `latest` once
  this issue surfaced during VM verification. v0.2.12 is the
  first-promotable build of the Sessions-tab fix line.
- Swift / iOS / macOS apps in the main `cli pulse` repo also lack
  Opus 4.7 in their pricing table — tracked separately for the next
  Mac release. Not a desktop blocker.

## [0.2.11] — 2026-05-01

### Fixed
- **Sessions tab white-screen on Windows (P1).** Clicking the Sessions tab
  unmounted the entire React tree, leaving only the Tauri window chrome
  visible. No log trail, no crash dump — initial diagnosis (Rust panic /
  WebView2 renderer crash / OS kill) all came up empty during multi-hour
  forensic on the test VM. Real cause was a frontend `TypeError`:
  `App.tsx` rendered `{s.cpu_usage.toFixed(1)}%`, but the backend marked
  `LiveSession::cpu_usage` and `memory_mb` as `#[serde(skip_serializing)]`
  (intent: strip them from the supabase `helper_sync` payload). That
  attribute also stripped them from the Tauri IPC response, so the
  frontend received `undefined` for these fields. `undefined.toFixed()`
  threw at React render time, and React 18's default behavior with no
  `ErrorBoundary` is to unmount the whole tree.
  - Latent since v0.1.0. Not surfaced because the Windows GUI never
    started in any prior release (v0.2.10 was the first version where
    the bundle actually contained the GUI binary). On Linux the bug
    would also fire if the Sessions tab found a matched process; the
    test VM happened to be running Claude Code which the regex picked up.
  - Backend fix (`src-tauri/src/sessions.rs`): split the data model.
    `LiveSession` is now fully serializable (used for Tauri IPC, frontend
    sees all fields). A new `SyncableSession` view is built only when
    constructing the `helper_sync.p_sessions` payload — same fields
    stripped as before, but via an explicit struct boundary instead of
    a serde attribute that affected both consumers.
  - Defense-in-depth #1: NaN sanitization on `cpu_usage`. `sysinfo` on
    Windows can return NaN for short-lived / protected processes where
    the CPU% delta isn't computable. `serde_json` refuses to serialize
    NaN; downstream arithmetic taints with NaN. Floor non-finite values
    to `0.0`.
  - Defense-in-depth #2: `(s.cpu_usage ?? 0).toFixed(1)` in `App.tsx` so
    a missing field becomes `0.0%` rather than a render-time crash.
  - Defense-in-depth #3: new `src/ErrorBoundary.tsx` wraps the App root
    in `main.tsx`. Future render-time exceptions show a structured
    fallback panel (error message + stack + component stack) instead of
    silently unmounting the tree. Click a button to attempt recovery.
  - Tooling fix: enable the Tauri `devtools` feature in `Cargo.toml` so
    Ctrl+Shift+I / F12 opens DOM/console devtools in release builds.
    The v0.2.10 diagnostic was harder than it needed to be because
    devtools were disabled and we couldn't see the JS exception live.
  - Forensic write-up:
    [`PROJECT_FIX_2026-05-01_v0.2.11_sessions_white_screen.md`](PROJECT_FIX_2026-05-01_v0.2.11_sessions_white_screen.md)

## [0.2.10] — 2026-05-01

### Fixed
- **CATASTROPHIC PACKAGING REGRESSION (P0): all v0.1.0–v0.2.9 NSIS,
  `.deb`, and `.rpm` installers shipped without the GUI binary.**
  They contained only `scan_cli` (a sidecar diagnostic CLI tool)
  instead of the main `cli-pulse-desktop` Tauri app. Sizes were
  ~600 KB instead of the expected ~7 MB. Symptom: installer ran
  successfully, registered Start Menu / `.desktop` entries, but
  launching produced a flash of console and no GUI.
  - Root cause: `src-tauri/src/bin/{scan_cli,sessions_smoke}.rs`
    auto-register as cargo bins. Without `default-run` in
    `[package]`, `cargo tauri build` had no canonical "main"
    binary and silently picked `scan_cli` (alphabetically first
    among the auto-detected bins) for the Linux/Windows
    bundlers. AppImage bundler resolves binaries differently and
    was unaffected — that's why no automated test caught it for
    14 versions and the AppImage's 70+ MB size hid the others'
    breakage.
  - Fix: added `default-run = "cli-pulse-desktop"` to
    `src-tauri/Cargo.toml`. Forces a single canonical default
    binary across `cargo build`, `cargo tauri build`, and the
    bundler.
  - **CI guard added** (`.github/workflows/release.yml`):
    post-build verification step now asserts NSIS ≥ 3 MB,
    `.deb`/`.rpm` ≥ 3 MB, AppImage ≥ 30 MB, AND inspects each
    archive for the GUI binary by name (`7z l` for NSIS,
    `dpkg-deb -c` for .deb). Failure makes the matrix job red
    so the human un-draft gate notices.
  - Caught by first real human Windows GUI test on Azure VM —
    13 prior releases passed CI matrix because CI only built,
    never installed-and-launched. Adopting "real-VM smoke gate
    before un-drafting" as part of the release contract.
  - v0.2.9 was yanked to draft; `latest.json` redirects to v0.2.8
    until v0.2.10 is verified.
  - Forensic write-up:
    [`PROJECT_FIX_2026-05-01_v0.2.10_default_run.md`](PROJECT_FIX_2026-05-01_v0.2.10_default_run.md)

## [0.2.9] — 2026-04-27 (YANKED — broken NSIS/.deb/.rpm, see v0.2.10)

### Fixed
- **CRLF byte-offset drift in incremental scan cache.** On Windows,
  JSONL files written with `\r\n` line terminators caused the
  `parsed_bytes` cache to under-count by 1 byte per line. After N
  scans, the next incremental resumption would seek N bytes too
  early — into the middle of the next line — and silently drop its
  first event. Replaced `BufRead::lines()` (which loses the
  terminator's exact byte count) with `read_until(b'\n', &mut buf)`
  + explicit CR/LF stripping. macOS and Linux LF-only users were
  unaffected.
  - Caught by Codex deep review of v0.2.8: *"Next sprint
    recommendation: fix the incremental scanner offset bookkeeping
    first; it's the highest-value correctness risk in the shipped
    product."*
  - 2 new regression tests in `scanner_integration.rs`
    (`crlf_codex_jsonl_parses_identically_to_lf`,
    `crlf_incremental_resume_does_not_drop_lines`) — both fail
    pre-fix, pass post-fix.
  - Forensic write-up:
    [`PROJECT_FIX_2026-04-27_v0.2.9_crlf_offset.md`](PROJECT_FIX_2026-04-27_v0.2.9_crlf_offset.md)

### Added
- **Sentry crash reporting is now LIVE.** Created `desktop` project
  in the existing `jason-yeyuhe.sentry.io` org (alongside `apple-ios`,
  `apple-macos`, `android`). DSN baked into release builds via
  `CLI_PULSE_SENTRY_DSN` GitHub Actions secret. Privacy stance
  unchanged from v0.2.4: `sendDefaultPii=false`,
  `tracesSampleRate=0`, client-side `$HOME` path scrubbing,
  org-level Data Scrubber + Default Scrubbers active. Dev builds
  with no DSN env var continue to be a clean no-op (verified by
  `install_without_dsn_is_a_noop` test).
- Tagged events: `platform=desktop`, `os={windows|linux|macos}`,
  `arch={x86_64|aarch64}`, `app_version=0.2.9` so the dashboard can
  filter cleanly.

### Numbers
- 90 tests now (53 Rust + 25 frontend + 12 integration, was 78).
- Sentry crate adds ~1 MB to release binary; was already in v0.2.4
  but unused. Now actually emits events for crashes / panics on
  release builds.

## [0.2.8] — 2026-04-26

### Added
- **"Update available" banner in the header.** On every app launch
  the frontend silently calls the updater and, if a newer version
  is published, shows a small green pill in the top-right that
  reads `⬆ v0.2.X is available · Update`. Clicking it switches to
  *Settings → Updates* where the user can confirm + install. The
  download is **never** triggered automatically — same consent
  model as v0.1.0, just made discoverable.
- Failure of the update check is silent (offline, GitHub Releases
  edge cache lag, etc.) — no scary error toasts on startup.
- New `updater.banner_available` / `updater.banner_action` keys in
  en / zh-CN / ja.

### Why
Before: the "auto-update" feature required users to remember to
poke *Settings → Updates → Check for updates*. Many users never
will. Now: the visual nudge surfaces whenever a release is available
without breaking the "no surprise installs" privacy stance.

## [0.2.7] — 2026-04-26

### Fixed
- **`setLang` no longer fires-and-forgets the i18next promise.**
  `i18next.changeLanguage()` returns a Promise that we previously
  invoked without awaiting or handling rejection — works in
  practice because all locales are bundled at build time, but a
  rejected promise would surface as an unhandled-rejection warning
  in the console. Now returns a Promise (caller can await if it
  cares about completion) and converts any error into a
  `console.warn` so it never crashes the app. Caught by Codex
  review of v0.2.6: *"src/i18n.ts:52: changeLanguage() is not
  awaited, and the tests assume sync language flips."*
- localStorage persistence now happens **before** `changeLanguage`
  so a thrown error during the language switch still leaves the
  user's choice remembered for the next launch.

### Polished
- Empty-state Overview now reads "Scanning ~/.claude and ~/.codex
  for the past 30 days…" while the first scan is in flight, instead
  of showing four mute pulsing rectangles. New `misc.scanning_hint`
  key added in en / zh-CN / ja.
- `sentry_init.rs` doc comment trimmed: was claiming the
  `before_send` filter scrubs `token / secret / password / api_key
  / supabase / claude_api / anthropic / codex / openai / gemini /
  dsn / keychain / pairing / ...` field names client-side, but the
  actual implementation only scrubs `$HOME` paths. Field-name
  scrubbing is delegated to the Sentry org-level Data Scrubber
  settings (matches Swift / Kotlin arrangement, per
  `reference_sentry.md`). Doc now accurately reflects the
  implementation. No behavior change.

## [0.2.6] — 2026-04-26

### Added
- **Settings → About** panel. Shows app version, platform family +
  arch, paired status (with truncated device id, no secret leakage),
  and a one-click "Copy diagnostics" button that puts a structured
  text block in your clipboard — paste it into a GitHub issue when
  reporting a bug. Includes a link to the repo.
- New Tauri command `diagnostic_snapshot` returns the structured
  data the About panel renders. Sensitive fields (helper_secret,
  user_id, full device_id) are deliberately not exposed.
- `about_*` translation keys added to en / zh-CN / ja.

### Why
First-line support friction: when a user reports an issue I have no
way to confirm what version they're on, what arch / OS, whether
they're paired, etc. without a back-and-forth. The About panel is
also the natural home for any "what's this app" UX a new user
needs.

## [0.2.5] — 2026-04-26

### Added
- **Frontend test suite** via Vitest + jsdom + Testing Library. 25
  tests covering pure presentation helpers (USD / int formatters, CSV
  escape, RFC-4180 row rendering) and i18n behaviour (localStorage
  persistence, fallback when stored code is unsupported, every required
  UI key resolves non-empty across en / zh-CN / ja).
- `src/lib/format.ts` extracted from `App.tsx` so the formatters are
  importable and testable. App.tsx behaviour byte-identical.
- `npm test` script wired to the CI `frontend` job + `pre-push` hook.
  Frontend regressions now caught before they hit `main`, matching
  the bar Rust already cleared.

### Internal
- `src/test/setup.ts` ships an in-memory localStorage shim so tests
  don't depend on jsdom version quirks (Vitest 2.x + jsdom 25
  occasionally exposes incomplete Storage).
- 53 Rust tests + 25 frontend tests = **78 total tests** across 5
  CI runners.

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
