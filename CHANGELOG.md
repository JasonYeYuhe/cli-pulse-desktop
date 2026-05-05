# Changelog

All notable changes to CLI Pulse Desktop (Windows + Linux).

## [0.4.21] — 2026-05-05

### Fixed
- **v0.4.20 Item 1 fix-to-the-fix.** v0.4.20 added an mpsc channel so a
  manual "Refresh now" click resets the 120 s background-sync
  countdown — but the loop still ran a `background_tick` at the top
  of the next iteration after the wake, producing a redundant
  `quota::collect_all` call ~2 s after every manual click. VM Block A
  measured this exactly: manual click at 16:42:05, spurious extra
  `collect_all` at 16:42:07 (+2 s), then the next regular tick at
  16:44:09 (T_click + 120 s, which IS correct). v0.4.21 changes
  `wait_for_next_tick` to return a `WaitOutcome` enum (`Elapsed` /
  `Reset`) and the loop uses
  `while wait_for_next_tick(...).await == WaitOutcome::Reset {}` so a
  manual-refresh wake re-enters the wait without triggering an
  intermediate tick. Net behavior: clicking Refresh in the idle window
  now produces exactly ONE `collect_all` (the manual one) and shifts
  the next background tick to T_click + 120 s, instead of producing
  two close-together calls.

### Changed
- **Providers tab "Refresh now" → "Refresh quota now"**
  (`立即刷新配额` / `クォータを今すぐ更新`). Disambiguates from the
  global `Rescan` / `重新扫描` / `再スキャン` button in the top-right,
  which scans local CLI usage data — not provider quotas. Spotted as
  a UX nit in the v0.4.20 VM verify report.

### Notes
- **Gemini 3.1 Pro v0.4.21 review caught two more issues** beyond the
  primary +2 s tick:
  - **P1 (channel-close busy loop)**: the bare `_ = rx.recv()` select
    arm matches both `Some(())` and `None`. If
    `MANUAL_REFRESH_TX.set(tx)` ever fails (e.g. duplicate
    `spawn_background_sync` from a hot-reload), the local `tx` is
    dropped, closing the channel — `recv()` then returns `None`
    instantly, the arm fires, the new `while … == Reset` pattern
    busy-loops at 100 % CPU. Fixed by changing the arm to
    `Some(_) = rx.recv()` so the arm is disabled (not fired) on
    channel close, leaving the sleep arm to run normally.
  - **P2 (shutdown latency under repeated clicks)**: the new inner
    `while … == Reset {}` doesn't check `stop`. If the user keeps
    clicking Refresh faster than 120 s, the loop can outlive a
    shutdown request indefinitely. Fixed with `&& !stop.load(...)`
    on the while condition at both call sites.
- 1 new test: `manual_refresh_does_not_cause_extra_tick` simulates the
  loop with a counter standing in for `background_tick` and asserts
  the count stays at 1 after a manual refresh fires mid-idle, then
  bumps to 2 only after a full `interval` has elapsed since the
  click. Test cleanup uses `drop(tx)` directly (not `loop_handle.abort()`)
  to validate the P1 fix end-to-end. The two existing v0.4.20 mpsc
  tests now also assert the `WaitOutcome` discriminant.
- Both call sites of `wait_for_next_tick` in `spawn_background_sync`
  use the `while … == Reset && !stop {}` pattern — the
  post-`device_status` cleared-credentials path also benefits (a
  manual click after a pairing wipe no longer fires a useless tick).
- 207 tests green (163 backend, +1 new; 44 frontend, +0).

## [0.4.20] — 2026-05-05

### Added
- **Per-provider error badge on the Providers tab.** When a collector
  (Claude / Codex / Cursor / Gemini / Copilot / OpenRouter) fails — bad
  credentials file, expired refresh, HTTP 5xx — the affected card now
  renders a red "error" badge with the failure reason in the tooltip.
  Closes the gap between v0.4.15's amber "stale" badge (which only
  fires after 6 minutes of stale `updated_at`) and the user noticing
  something's wrong. Worse: a provider that was NEVER successfully
  collected (just signed in, refresh broken) had no row, so "stale"
  never fired either — only "error" surfaces that case. Backend
  refactored each `<provider>::collect()` from `Option<QuotaSnapshot>`
  to `Result<Option<QuotaSnapshot>, CollectorError>` with three
  discriminant variants (`Http`, `SchemaOrIo`, `RefreshFailed`); `Ok(None)`
  preserves the silent "user not signed in" skip. Per Gemini 3.1 Pro
  v0.4.20 review: the typed enum (vs raw `String`) keeps the door open
  for future retry-policy work without revisiting collector signatures.
- **Settings → Integrations now shows the active credentials backend.**
  A "Storage: OS keychain" line in emerald, or "Storage: file (keyring
  unavailable) ⚠" in amber with a tooltip explaining how to enable the
  keychain on Linux. v0.4.16 already routed this state through Copy
  Diagnostic, but a Linux user without `libsecret` who never copied
  diagnostics silently stayed on file storage. Per Gemini 3.1 Pro
  v0.4.20 review: pair every silent fallback with a discoverable
  surface; the diagnostic copy alone is too easy to miss.

### Changed
- **"Refresh now" button now interrupts the background sync sleep.**
  When the user clicks Refresh at second 118 of a 120 s background
  cycle, the upcoming background tick used to fire 2 s later — a
  redundant sync. v0.4.20 wires a `tokio::sync::mpsc` channel between
  `sync_now` and the loop's idle window so the next 120 s countdown
  restarts from "now" instead. Per Gemini 3.1 Pro v0.4.20 review:
  the initial `tokio::sync::Notify` proposal had the exact bug we were
  trying to fix — permits earned during the active tick get buffered,
  then consumed by the next `select!`, firing a redundant background
  tick right after the manual one. Fixed by drain-before-select on the
  mpsc receiver so signals that arrive *during* the active tick are
  discarded, and only signals that arrive *during* the idle window
  cause a reset.

### Notes
- The user-invoked Tauri `sync_now` and the background loop now share
  an inner `perform_sync` helper. `sync_now` calls it then pokes the
  manual-refresh channel; `background_tick` calls it directly so the
  loop doesn't poke itself.
- 11 new tests (162 backend total, was 151; 44 frontend unchanged):
  two `tokio::test(start_paused = true)` tests pin the
  drain-before-select contract, three pin the `CollectorError` wire
  format + helper, six pin the credentials-file read seam returns
  `Ok(None)` on missing files and `Err` on malformed JSON across the
  three OAuth-file collectors.

## [0.4.19] — 2026-05-05

### Added
- **"Refresh now" button on the Providers tab.** Click to run all 6
  provider collectors (Claude/Codex/Cursor/Gemini/Copilot/OpenRouter)
  + reload server-side quota data immediately, instead of waiting up
  to 120s for the next background sync cycle. Disabled while a manual
  sync is in flight (per Gemini 3.1 Pro review of the v0.4.19 plan —
  spam-clicks would fire concurrent sync_now calls against provider
  rate limits).

### Changed
- **Proactive Claude/Gemini OAuth refresh** — refresh now fires when
  the token has < 5 minutes of life remaining (was: only after expiry,
  with a 60s safety margin for Claude / 0s for Gemini). With the
  120-second background sync cycle, a 5-minute buffer absorbs ~2
  missed ticks before the token actually expires. The shared constant
  `quota::PRE_EXPIRY_BUFFER_MS` keeps Claude and Gemini consistent —
  pinned via `pre_expiry_buffer_pinned_at_5_minutes` test so future
  drift is caught.

### Fixed
- **Removed v0.4.16's `provider_creds.json` breadcrumb.** v0.4.16
  rewrote the file with `version: 2` + zeroed values as a one-release
  rollback safety net during the keychain migration. v0.4.18 is now
  stable in production; the breadcrumb is no longer needed.
  v0.4.19 deletes it on a background thread at app startup (per
  Gemini review: filesystem I/O during init must not block the main
  thread). The deletion is gated on `version >= 2` AND keychain being
  the active backend — v1 files (where the file IS the storage) are
  never touched.

### Notes
- The deferral note in the v0.4.14-v0.4.16 dev plan claiming
  "Codex auth model is different (long-lived session + cookie). Not a
  refresh-token flow" was wrong. `quota/codex.rs::refresh_tokens` has
  shipped active OAuth refresh against `auth.openai.com/oauth/token`
  with the public PKCE client_id since v0.4.3 — it just uses an
  8-day `last_refresh` staleness gate instead of `expiresAt`. No work
  needed; deferral deleted from internal notes.

## [0.4.18] — 2026-05-05

### Fixed
- **OpenRouter custom endpoint URL row now has a Clear button**, matching
  the parity of the 3 secret rows above (Cursor cookie / Copilot token /
  OpenRouter API key). Previously the only way to clear the URL was to
  manually empty the field and save — VM verification of v0.4.17 flagged
  the inconsistency. The URL isn't a secret so we skip the confirm modal
  (which exists for "expensive-to-recreate token" semantics) and clear
  directly.

## [0.4.17] — 2026-05-05

### Fixed
- **"Copy diagnostic snapshot" now includes the provider-creds backend
  line** ("OS keychain" / "file (keyring unavailable)"). v0.4.16 wired
  the backend into the Rust `DiagnosticSnapshot` struct but missed
  adding the formatter line in `App.tsx::diagText`, so the field was
  silently dropped on its way to the clipboard. VM E2E verification
  of v0.4.16 (D-block) caught this — the data was already in the IPC
  response, just not rendered. Pure-rendering fix.

## [0.4.16] — 2026-05-04

### Changed
- **Cursor / Copilot / OpenRouter credentials now stored in the OS
  keychain** (macOS Keychain / Windows Credential Manager / Linux
  Secret Service via `libsecret`). Replaces the v0.4.6 plaintext-
  with-mode-0600 storage. On first launch after upgrade, existing
  credentials migrate automatically — the plaintext file is then
  rewritten with `version: 2` + zeroed values. (v0.4.17 will delete
  the file entirely; the two-step gives one release of rollback room
  in case migration goes wrong.)
- Linux installs without a running keyring service (headless server,
  minimal container) gracefully fall back to the v0.4.6 file storage
  with a one-time INFO log line. The active backend is surfaced via
  `diagnostic_snapshot.provider_creds_backend` so security-conscious
  users can verify they're on the OS keychain rather than the file.
  Per Gemini 3.1 Pro review: silent fallback can mislead users; the
  diagnostic copy makes it visible.

### Implementation notes
- `keychain.rs` gained a generic `store_at(account, value)` /
  `read_at(account)` / `delete_at(account)` API alongside the
  existing v0.3.0 OTP refresh-token wrappers. Same `KeychainError`
  with `NotAvailable` discriminant.
- `provider_creds.rs` runs the v1 → v2 migration in
  `tauri::Builder::setup` at app startup, NOT on first `save()`.
  Per Gemini review (P1): tying it to `save()` would leave users
  who never edit creds on the plaintext file forever, fragmenting
  the user base.
- 8 tests in `provider_creds.rs` (was 6) — added Backend enum
  serialization pin + v2-file idempotency guard.

## [0.4.15] — 2026-05-04

### Added
- **Stale-data indicator on provider cards.** Each provider row now
  shows a small amber "stale" badge when the cached server data is
  more than 6 minutes old. Hover for an exact "Last updated N min/hr/d
  ago" tooltip. Helps users distinguish "Gemini hasn't synced yet"
  from "Gemini sync failed and the values are old". Threshold is 6
  minutes (not 5) so it doesn't flap right before each 2-min sync
  cycle — per Gemini 3.1 Pro's review of the v0.4.14-v0.4.16 dev
  plan. Threshold pinned in a vitest test.

### Fixed (backend)
- **OpenRouter balance no longer truncates above ~$21,474.**
  Supabase migration v0.43 (`v0_43_provider_quotas_bigint_and_updated_at`)
  bumped `provider_quotas.{quota, remaining}` from `integer` (i32) to
  `bigint`. Rust side has been i64 since v0.4.0; the cast happened on
  INSERT inside the helper_sync RPC, so any user with an OpenRouter
  balance ≥ ~$21k saw truncated values until now. Migration is
  data-preserving (every existing row's value fits in both i32 and
  bigint by definition).

### Backend changes (server-side, no client action needed)
- Migration `v0_43_provider_quotas_bigint_and_updated_at` applied to
  prod Supabase (project `gkjwsxotmwrgqsvfijzs`):
  - `ALTER TABLE provider_quotas ALTER COLUMN quota TYPE bigint`
  - `ALTER TABLE provider_quotas ALTER COLUMN remaining TYPE bigint`
  - `provider_summary()` RPC now projects `updated_at` per row
- `ProviderSummaryRow` (Rust + TS) gains `updated_at: Option<String>`
  / `updated_at: string | null`. Older clients still work — Supabase
  returns the new field, old clients ignore unknown fields.

## [0.4.14] — 2026-05-04

### Added
- **Active OAuth refresh for Claude tokens.** When `~/.claude/.credentials.json`
  has an expired `accessToken` but a present `refreshToken`, we now POST
  to Anthropic's OAuth endpoint (`console.anthropic.com/v1/oauth/token`,
  PKCE public client, no client_secret) and atomically write the
  refreshed tokens back to disk. Mirrors the v0.4.7-v0.4.12 Gemini
  refresh work. Previously expired Claude tokens silently skipped
  collection until the user re-launched `claude` CLI to refresh,
  which gave a stale Claude card on the Providers tab if the user
  hadn't opened a Claude session in ~8h.

### Changed
- Promoted all `[Claude]` collector log lines from DEBUG to INFO
  (matching the v0.4.10 Gemini pattern), so users + future debugging
  can see which exit path was taken without the "silent half-fix"
  failure mode.
- `ClaudeCredentialsFile` and `ClaudeOAuthInner` now `flatten extra`
  unknown fields so atomic write-back from the refresh path
  preserves keys like `subscriptionType` (which `claude` CLI itself
  consumes) instead of silently dropping them.

### Notes
- **Anthropic Acceptable Use:** the read-only quota fetch (`/api/oauth/usage`)
  this app has been doing since v0.4.0 is unchanged. v0.4.14 only
  refreshes the access_token to keep that read-only fetch working
  past the ~8h token expiry — we do not proxy any inference traffic.

## [0.4.13] — 2026-05-04

### Fixed
- **Header app icon now renders the real CLI Pulse logo instead of
  a placeholder green-cyan gradient square.** The `<div>` placeholder
  in `App.tsx`'s top-left header was originally added during the
  v0.1 scaffold and was never replaced with the actual icon. The
  app icon at `src-tauri/icons/icon.png` (and its size variants)
  was correctly bundled by Tauri for the OS-level window title bar
  and tray, but the in-app header was rendering a Tailwind gradient
  div instead. v0.4.13 imports `src/assets/app-icon.png` (copied
  from `src-tauri/icons/128x128.png`) and renders it as an `<img>`
  in the header.

## [0.4.12] — 2026-05-03

### Fixed
- **Gemini OAuth refresh now ships a guaranteed-fallback hardcoded
  client_id+secret from upstream gemini-cli.** v0.4.11's recursive
  bundle walk found the chunks but still couldn't extract the
  client_id/client_secret pair from modern @google/gemini-cli
  releases — esbuild's code-splitting + property-assignment
  minification produces shapes that no realistic regex can match
  reliably. VM substring count confirmed `client_id` is present in
  14 chunk files and `client_secret` in 10, but they're emitted as
  property keys on imported config objects rather than as
  recognizable named constants OR co-located literal value pairs.
  Rather than chase another regex iteration that the next esbuild
  flag will break, v0.4.12 hardcodes the upstream public values
  from `packages/core/src/code_assist/oauth2.ts` (Apache-2.0).
  These are "installed application" OAuth credentials per Google's
  own documentation — the secret is intentionally checked into
  gemini-cli's open-source repo and is "obviously not treated as a
  secret" (see developers.google.com/identity/protocols/oauth2#installed).
  Local extraction still runs first so any future upstream rotation
  is picked up automatically; the hardcoded values only kick in
  when extraction returns None (which is the normal case for
  modern bundled npm installs).

### Added
- `fallback_oauth_client_values_match_upstream_shape` test —
  pins format invariants (apps.googleusercontent.com suffix on the
  client_id, GOCSPX- prefix and minimum length on the secret) so a
  typo at copy time can't silently 401 against Google.

### Notes
- License: gemini-cli is Apache-2.0. The two literal constants
  themselves are not copyrightable (they're identifiers / facts);
  the source-of-truth comment in `gemini_refresh.rs` cites the
  upstream file path so attribution is clear.
- Stability: in 9+ months on npm, gemini-cli has not rotated these
  values — rotation would break every existing installed CLI's
  refresh path simultaneously, so they're as stable as installed-app
  credentials get.

## [0.4.11] — 2026-05-03

### Fixed
- **Gemini OAuth refresh now reaches `dist/src/code_assist/oauth2.js`
  in modern monorepo @google/gemini-cli installs.** v0.4.10's
  diagnostic logging surfaced that on Windows VM, discovery walked
  76 .js files in `<root>/bundle/` (the bundled chunks) but matched
  none — the actual unbundled source `oauth2.js` lives at
  `<root>/dist/src/code_assist/oauth2.js`, which v0.4.10's scan
  didn't find because the scan was non-recursive (one level only).
  Three fixes:
  1. **Recursive .js walk** under each `<root>/bundle`, `<root>/dist`,
     `<root>/lib` subdir, capped at depth 4 (covers the
     `dist/src/code_assist/oauth2.js` path) and skipping nested
     `node_modules/` to avoid expanding into transitive deps.
  2. **Per-root legacy-path probing** — the `gemini-cli-core` install
     root used to be joined with a relative path that already
     contained `node_modules/@google/gemini-cli-core/...`, producing
     a doubly-nested path that never existed. v0.4.11 probes both
     `<root>/dist/src/code_assist/oauth2.js` (sibling/hoisted layout
     in modern npm) and the deep-nested form in order.
  3. **Multiline-assignment test pinned.** Upstream gemini-cli-core
     emits `const OAUTH_CLIENT_ID =\n    '...';` after TS→JS compile.
     Rust regex `\s*` already matches across newlines, but a new test
     locks in that contract so a future regex tweak can't silently
     break it.

### Added
- 4 new Vitest tests in `quota/gemini_refresh.rs`:
  - `extract_credentials_from_multiline_assignment` — confirms regex
    handles upstream's split `const X =\n  '...';` shape.
  - `scan_dir_recursively_finds_creds_three_levels_deep` — synthetic
    `dist/src/code_assist/oauth2.js` layout, asserts recursive walk
    reaches it.
  - `scan_dir_recursive_skips_node_modules` — defensive: vendored
    deps' `node_modules/` are skipped to avoid pathological scans.
  - `scan_dir_recursive_returns_none_for_missing_dir` — probing a
    non-existent root must not panic (common: half the candidate
    roots are missing on every refresh).

### Notes
- v0.4.10's INFO-level logging is preserved unchanged. After the
  recursive walk hits the OAuth pair, you'll see one
  `[Gemini] refresh: found OAuth pair via legacy direct path ...`
  (or `... after scanning N .js file(s) recursively under ...`)
  followed by the existing
  `[Gemini] OAuth token refreshed ...` success line and the
  `[Gemini] refresh wrote new tokens to ... (atomic, mode 0600)`
  write-back confirmation.

## [0.4.10] — 2026-05-03

### Changed
- **Gemini collector now logs every branch at INFO level.** v0.4.9 VM
  verification produced a "silent half-fix": the Providers tab kept
  rendering Gemini, but no `[Gemini]`-prefixed log line ever appeared
  in the running v0.4.9 process, the `oauth_creds.json` mtime was
  unchanged, AND `quota::collect_all` reported Gemini in the populated
  list every cycle. The contradiction was that several `collect()`
  exit paths logged at DEBUG (filtered by the global INFO filter from
  v0.3.4) and the "token still valid" path logged nothing at all, so
  any of those branches looked identical in the log file. v0.4.10
  promotes every branch to INFO and adds an entry-point line that
  prints the raw `expiry_date` and `now_ms` it's comparing, plus
  whether `refresh_token` and `access_token` are present. With this,
  the next VM run will tell us exactly which exit path is firing
  (token still valid / no refresh token / refresh attempted-and-failed
  / refresh attempted-and-succeeded) instead of leaving us inferring
  it from server-side state.
- `gemini_refresh::refresh()` now logs the OAuth client creds path it
  discovered (or which root list it probed when it found nothing),
  and `scan_dir_for_credentials()` reports the .js scan count per dir.
  This pinpoints whether v0.4.9's bundle-walking is even reaching the
  user's install or skipping it because the candidate root path is
  wrong on their box.

### Notes
- No behavior change beyond logging — the actual refresh flow,
  discovery order, and atomic write-back are byte-identical to v0.4.9.
  This is a pure-diagnostics ship to debug v0.4.9 in the field.

## [0.4.9] — 2026-05-04

### Fixed
- **Gemini OAuth refresh now finds credentials in modern bundled
  @google/gemini-cli releases.** v0.4.7 introduced active OAuth
  refresh by mirroring CodexBar's discovery: look for a literal
  `oauth2.js` source file at known npm/Homebrew/Nix paths. That
  works for Homebrew installs and older gemini-cli versions, but
  modern @google/gemini-cli npm releases are esbuild-bundled —
  there's no standalone `oauth2.js`; the OAuth code lives inside
  hashed chunks like `bundle/gemini-3OZCG3O2.js`. VM verification
  of v0.4.7 caught this gap (Windows user with gemini-cli installed
  at `%APPDATA%\npm\node_modules\@google\gemini-cli\` — refresh
  failed because the v0.4.7 path search looked for a file that
  doesn't exist in the bundled release).
  v0.4.9 expands `find_oauth_credentials()`:
  1. Try the legacy direct `oauth2.js` path first (Homebrew /
     source layouts, unchanged).
  2. Walk `<gemini-cli-root>/bundle/*.js` chunks (modern npm
     releases — primary expected match).
  3. Walk `<gemini-cli-root>/dist/*.js` and `<root>/lib/*.js` as
     additional fallbacks.
  Plus a value-pattern regex fallback for when minification has
  stripped the named constants `OAUTH_CLIENT_ID` /
  `OAUTH_CLIENT_SECRET`. The fallback matches Google's canonical
  formats:
  - Client ID: `<9-12 digit project>-<random>.apps.googleusercontent.com`
  - Client secret: `GOCSPX-<22+ chars>`
  Named-constant regex still runs first; value-pattern only kicks
  in when minification has obscured the names.

### Added
- 4 new Vitest tests in `quota/gemini_refresh.rs`:
  - `extract_credentials_from_minified_bundle_chunk` — verifies the
    value-pattern fallback works on real esbuild output shape.
  - `value_fallback_rejects_too_short_secret` — defensive: the
    `{20,}` length floor on GOCSPX-... avoids false matches on
    GOCSPX-prefixed substrings shorter than a real client secret.
  - `value_fallback_rejects_non_googleusercontent_domain` — the
    `.apps.googleusercontent.com` suffix is required.
  - `named_constant_takes_priority_over_value_fallback` — when
    both forms are present (e.g. comments documenting other OAuth
    apps), the named constants win.
- 4 KB JS file size cap on bundle scans to avoid pathological I/O
  on unrelated huge JS bundles.

### Changed
- Discovery roots expanded to also include `@google/gemini-cli-core`
  (sometimes installed alongside gemini-cli rather than nested),
  Windows `%PROGRAMFILES%\nodejs\node_modules\@google\gemini-cli`,
  and Mac/Linux `~/.nvm/versions/node` for NVM users.

### Notes
- Best-effort behavior unchanged: if no candidate path matches OR
  the refresh API rejects, fall back to v0.4.6 silent-skip. No
  regression for users without gemini-cli installed.
- Total: **131 Rust lib tests** (was 127 in v0.4.7) + 33 Vitest
  tests.
- iOS / Android / Mac unaffected. Server-side schema unchanged.
- v0.4.x desktops on auto-update pick this up automatically once
  v0.4.9 promotes to Latest.

## [0.4.8] — 2026-05-04

### Fixed
- **Provider card visibility no longer gated on local scan cache.**
  v0.4.3 through v0.4.7 only rendered Provider cards when the local
  cost-usage scan had at least one entry for that provider. A user
  who'd just paired the desktop and had a populated server-side
  `provider_quotas` row (from another paired device, or from active
  Gemini OAuth refresh in v0.4.7) but no recent local activity for
  that provider got an empty Providers tab. VM verification of
  v0.4.7 confirmed the gap (Gemini server row populated, no card
  rendered).
  v0.4.8 extends the card aggregation to backfill any
  server-known provider absent from local scan with empty aggregate
  values. Tier bars + plan badge still render from server data; the
  card subtitle distinguishes the no-local-scan case with a
  dedicated copy line ("No local activity yet — quota from your
  account" / "暂无本地活动 — 配额来自账号" /
  "ローカル使用履歴なし — クォータはアカウントから取得").

### Added
- New i18n key `providers.no_local_scan_yet` in all 3 locales
  (en / zh-CN / ja).

### Notes
- Pure frontend fix. No backend / collector / schema changes.
- Local-scan-based numbers (active days, msgs, models, cost) stay
  zero for server-only cards; tier bars + plan badge show real
  quota state.
- Sort order: cost desc, then provider name asc as a tie-breaker
  (so server-only zero-cost entries have a stable order).
- v0.4.x desktops on auto-update pick this up automatically once
  v0.4.8 is published.

### v0.4.9+ backlog
- **Stale indicator on provider cards** when last-server-update is
  > N hours old. Requires backend `provider_summary` to expose
  `updated_at` (currently shipped omitted) — schema change pending
  user flag per autonomy rules.
- **Claude active OAuth refresh.** Different mechanism from Gemini
  (Anthropic doesn't expose a simple POST /token; Claude CLI
  rotates via its own internal flow). Investigation pending.
- **OS keychain / `tauri-plugin-stronghold`** migration for
  `provider_creds.json`.
- **OpenRouter i32 overflow → bigint migration**. Pending user flag.

## [0.4.7] — 2026-05-04

### Added
- **Active Gemini OAuth refresh.** Gemini's access token expires
  ~8 hours after issue. v0.4.6 silently skipped collection past expiry,
  forcing the user to re-run `gemini` CLI to get fresh quota data.
  v0.4.7 now refreshes automatically by:
  - Locating the user's Gemini CLI installation's bundled `oauth2.js`
    (npm / Homebrew / Nix paths covered).
  - Regex-extracting `OAUTH_CLIENT_ID` + `OAUTH_CLIENT_SECRET` (these
    are the values Gemini CLI uses internally; not secrets per
    RFC 6749 §2.2 — already shipped in the user's local CLI binary).
  - POSTing to `https://oauth2.googleapis.com/token` with
    `grant_type=refresh_token`.
  - Atomically writing the new tokens back to
    `~/.gemini/oauth_creds.json` (mode 0600 set BEFORE rename).
  Mirrors macOS CodexBar `GeminiStatusProbe.swift:520-600` (commit
  82bbcde) — same scrape paths, same Google OAuth flow.
- **Best-effort fallback chain.** If `oauth2.js` can't be located OR
  the refresh API rejects, fall back to v0.4.6 silent-skip. No
  regression; expired-token + missing-CLI users see the same empty
  state they did before.
- **Structured success log.** `[Gemini] OAuth token refreshed via
  gemini-cli local credentials (expires in <N>s)` at INFO level — first
  evidence in `cli-pulse.log` that the refresh path is firing on
  real-user systems.

### Changed
- **Settings → Integrations panel position.** Moved from above
  Updates section to truly below Updates, per v0.4.6 dev plan §3
  ("dedicated section at the bottom"). VM verification of v0.4.6
  flagged the discrepancy. No functionality change.
- `quota/gemini.rs` `CredsFile` now retains `refresh_token` + `id_token`
  fields (was previously read-and-discarded). Required for the active
  refresh path; backwards-compatible for legacy file shapes that omit
  these fields.

### Tests
- 6 new Rust tests in `quota/gemini_refresh.rs`: `OAUTH_CLIENT_ID` /
  `OAUTH_CLIENT_SECRET` regex extraction (single/double quote, missing
  pair refused, empty content), Google refresh response parse
  (minimal + rotated tokens), candidate path collection smoke.
- Total: **127 Rust lib tests** (was 121 in v0.4.6) + 33 Vitest tests.

### CI infrastructure
- First v0.4.x tag built under the streamlined matrix
  (`f2eed29` 2026-05-04 dropped Win ARM64 + Linux ARM64 from the
  default matrix — public-repo Standard runners are FREE; ARM larger
  runners were billed for 0 real-user downloads). Tag-push CI cost
  drops from ~50 quota-min to ~25 quota-min per tag, $0 spend.

### Notes
- iOS / Android / Mac unaffected. Server-side schema unchanged.
- v0.4.x desktops on auto-update pick this up automatically once
  v0.4.7 promotes to Latest.
- v0.4.8+ candidates: OS keychain / `tauri-plugin-stronghold`
  migration for `provider_creds.json`; OpenRouter i32 overflow
  bigint migration (needs user backend-schema flag).

## [0.4.6] — 2026-05-04

### Added
- **Settings → Integrations panel for Cursor / Copilot / OpenRouter
  credentials.** v0.4.3 introduced these three providers but only read
  credentials from environment variables — a non-technical user had no
  in-app way to configure them. v0.4.6 adds a dedicated "Integrations"
  section at the bottom of the Settings tab with three rows:
  - Cursor session cookie
  - GitHub Copilot token
  - OpenRouter API key (with optional custom endpoint behind an
    "Advanced settings" toggle)
  Each row shows status (Configured / Not set) and an env-override
  warning banner when the corresponding env var is set, since the env
  takes priority over the saved value (backwards-compatible read order:
  env → file → none). Save is per-row; Clear opens a confirmation
  modal. Once saved, the raw value is never re-displayed — the UI only
  shows status. Replace by typing a new value or click Clear.
- **Locale-aware number formatting via i18next.** v0.4.5 plural-aware
  message keys (`{{count}} msgs`) interpolated raw integer strings
  ("2782 msgs"), losing the thousands separator that v0.4.4 and earlier
  rendered ("2,782 msgs"). v0.4.6 adds an i18next `number` formatter
  that routes numbers through `Intl.NumberFormat` with the active
  language; locale strings opt in via `{{count, number}}`. en / zh-CN
  / ja all use comma per CLDR.
- **Atomic credential persistence.** New `provider_creds.json` lives in
  the same per-user config dir as the existing `config.json`. Same
  security model: file mode 0600 on Unix (set BEFORE rename, so no
  permission window), per-user `%APPDATA%` ACL default on Windows.
  Atomic write via `tempfile::NamedTempFile` + persist; in-memory
  read-side cache invalidated on every save so live edits take effect
  on the next sync cycle without re-reading disk per-collector. Schema
  versioned (`version: 1`) for v0.4.7+ stronghold migration.

### Changed
- `quota/cursor.rs` / `quota/copilot.rs` / `quota/openrouter.rs`
  credential read priority: env var → `provider_creds.json` → none.
  Existing v0.4.5 env-var users keep working identically; new users go
  through the UI.

### Reviews
- **Gemini 3.1 Pro (2026-05-04)** — UX / product / i18n review of v0.4.6
  spec. 10 findings: 4 FAILs resolved inline in spec (textarea →
  password input, dedicated Integrations section vs Account/Budget
  sandwich, 4-state save → 2-state, clear-confirmation modal), 4
  ship-it-with-nits (HTTP 401 friendly copy mapping, zh-CN/ja
  translation specifics, env-override banner copy, OpenRouter base URL
  behind Advanced toggle), 2 ship-its (no-peek decision, zh-CN comma
  format).

### Tests
- 6 new Rust unit tests in `provider_creds.rs` (round-trip empty/
  populated, missing version defaults to 1, malformed JSON surfaces
  Err, unknown fields ignored, empty-string credential semantics).
- 3 new Vitest tests in `i18n.test.ts` for the number formatter
  (en/zh-CN/ja messages key with count=2782 renders "2,782 msgs"
  variant in each locale).
- Total: **121 Rust lib tests + 33 Vitest tests** (was 115 + 30 in v0.4.5).

### Deferred to v0.4.7+
- **CI dynamic matrix** for tag-push Windows-only optimization. v0.4.5
  attempt at job-level `if: matrix.platform` broke the workflow; v0.4.6
  ships with the same 4-platform build to avoid stacking pipeline risk
  with feature work. v0.4.7 will do this as a focused CI sprint with a
  throwaway rc tag for validation.
- **OS keychain / `tauri-plugin-stronghold`** migration. Plaintext
  mode-0600 storage is the v0.4.6 baseline; v0.4.7+ migrates to true
  cross-platform keychain.
- **Active Gemini OAuth refresh.** Still requires the user to run
  `gemini` CLI periodically to refresh `oauth_creds.json`.
- **OpenRouter i32 overflow** at $21k+ balance. Backend bigint
  migration pending user flag per autonomy rules.

### Notes
- Existing env-var users (CURSOR_COOKIE / COPILOT_API_TOKEN /
  OPENROUTER_API_KEY / OPENROUTER_API_URL) continue to work
  unchanged; env values take priority over saved file values, with a
  banner in the Settings UI explaining the override.
- No server-side schema changes. iOS / Android / Mac unaffected.
- v0.4.x desktops on auto-update pick this up automatically once
  v0.4.6 is published.

## [0.4.5] — 2026-05-04

### Fixed
- **Tier bar visual direction inverted vs text label.** v0.3.4–v0.4.4
  rendered each tier bar's fill width as `used%` (consumption) while
  the text label said `X/Y left` (remaining). The two halves of the
  same row pointed at opposite metrics — text "85 left" rendered
  alongside a 15%-filled bar. v0.4.5 flips the bar to fill from left
  to right with the **remaining** percentage, so 85 left = 85% green
  bar, matching the text. Color heat thresholds invert too:
  - >40% remaining → green (safe)
  - 10–40% remaining → amber/orange (warning)
  - ≤10% remaining → red (critical)
  Caught by user inspection of v0.4.4 Providers tab screenshot;
  applies to per-tier bars (`App.tsx:792-820`) and the singleton
  fallback bar for non-Claude providers with flat quota
  (`App.tsx:822-842`).
- **Pluralization for "active days", "msgs", "models".** Provider
  card subtitle hardcoded "{{count}} active days · {{msgs}} msgs"
  even when count was 1 ("1 active days · 0 msgs · 1 models"). The
  `models` count was also a hardcoded English string with no i18n
  routing. v0.4.5 splits into i18next plural-aware keys
  (`active_days_one/_other`, `messages_one/_other`,
  `models_one/_other`) for all three locales (en / zh-CN / ja). zh-CN
  and ja use the single CLDR form (`_other`); en uses both.

### Tests
- 5 new Vitest tests in `i18n.test.ts` covering plural forms across
  the 3 supported locales (en singular/plural toggle, zh-CN single-
  form invariance, ja single-form invariance, models + messages
  variants).

### Notes
- Pure frontend UX polish. No collector / backend / schema changes.
  Per-provider quota fetch path unchanged from v0.4.4 — the live
  values being visualized are the same.
- v0.4.4 desktops on auto-update pick this up automatically once
  v0.4.5 is published.

## [0.4.4] — 2026-05-03

### Fixed
- **Claude collector schema mismatch.** v0.4.3 `ClaudeCredentials` struct
  expected flat top-level `{accessToken, expiresAt, ...}`, but real
  `claude` CLI ≥2.x writes nested `{claudeAiOauth: {accessToken, ...}}`.
  Every sync silently parsed to `None` (debug-level log) — Claude
  collector was effectively dead code in v0.4.3 for all real users.
  v0.4.4 nested-only struct, mirrors CodexBar upstream commit `82bbcde`
  (`Sources/CodexBarCore/Providers/Claude/ClaudeOAuth/ClaudeOAuthCredentialModels.swift:65-78`).
  No flat-shape fallback — CodexBar upstream never accepted flat top-level
  either; matching upstream avoids divergent schema drift. `expiresAt`
  parsed strictly as epoch milliseconds (drops the v0.4.3 ISO-8601
  branching code path that was based on incorrect docstring assumption).
  Caught during VM E2E 2026-05-03 JST — Mac side never had real
  `~/.claude/.credentials.json` to validate against.
- **Codex `/wham/usage` parse error.** v0.4.3 `Credits.balance`
  deserialized as `Option<f64>`, but the real ChatGPT API returns
  `balance` as a JSON STRING (e.g. `"5.43"`) — verified via
  `wham_inspect.py` against a live ChatGPT Plus account 2026-05-03 JST.
  Every cycle's `resp.json::<UsageResponse>()` therefore failed with
  `parse: error decoding response body`, and Codex collector returned
  `None` despite valid creds + 200 OK response. v0.4.4 adds a string|
  number custom deserializer accepting both forms (back-compat for any
  account where the field was historically a number).
- **Per-collector log level for schema drift.** Claude / Codex / Gemini
  `read_*()` helpers now distinguish file-absent (`debug!` — user not
  signed in, expected) from JSON parse failure (`warn!` — schema drift,
  surface immediately). v0.4.3 collapsed both into `Option<None>` with
  `debug!`, which is why the Claude bug above went silent for the
  entire v0.4.3 era. Future Anthropic / OpenAI / Google response shape
  changes will now appear in `cli-pulse.log` on the very first cycle.

### Tests
- 5 new fixture tests in `claude.rs`: nested happy path with
  `subscriptionType` (silently ignored per upstream), flat-shape yields
  `oauth=None`, empty `accessToken` preserved through parse, missing
  `expiresAt` defensively returns false from `is_token_fresh`,
  past-`expiresAt` returns false. Removed 3 ISO-8601 expiry tests
  obsoleted by nested-only schema (real claude CLI never writes
  ISO-8601 there).
- 2 new fixture tests in `codex.rs`: real `/wham/usage` shape with
  string `balance` + 9 unknown top-level fields (`account_id`, `email`,
  `spend_control`, etc. — all silently ignored per default serde
  semantics); `balance` deserializer accepts string, number, null,
  empty-string, and absent-field forms.

### Notes
- Gemini token-expiry behavior unchanged (still v0.4.3 documented
  limitation per `gemini.rs:10-14`). Explicit `CollectorStatus::Expired`
  UI warning tracked for v0.4.5+ frontend sprint.
- helper_sync RPC wiring (`lib.rs::sync_now` building
  `p_provider_tiers` from `quota::collect_all`) confirmed correct in
  VM E2E 2026-05-03 JST — server `provider_quotas` row updates
  correctly as soon as any collector returns `Some(snapshot)`.
- No server-side schema changes. iOS / Android / Mac unaffected.
- v0.4.x desktops on auto-update pick this up automatically once
  v0.4.4 is published.

## [0.4.3] — 2026-05-02

### Added
- **Multi-provider quota collection.** v0.4.0–0.4.2 only ported the
  Claude OAuth collector. v0.4.3 adds 5 more providers, matching what
  the Mac menu-bar app already collects:
  - **Codex (OpenAI)** — reads `~/.codex/auth.json` (or
    `$CODEX_HOME/auth.json`), refreshes OAuth access token via
    `auth.openai.com/oauth/token` if `last_refresh` > 8 days, hits
    `chatgpt.com/backend-api/wham/usage`. Emits "5h Window", "Weekly",
    "Credits" tiers. Mirrors `CodexCollector.swift`.
  - **Cursor** — reads env `CURSOR_COOKIE`, hits
    `cursor.com/api/usage-summary`. Emits "Plan" + "On-Demand" tiers
    (cents-scaled). Mirrors `CursorCollector.swift`.
  - **Gemini** — reads `~/.gemini/oauth_creds.json` (file-only path,
    matches Mac's secondary fallback). Hits
    `cloudcode-pa.googleapis.com:loadCodeAssist` + `:retrieveUserQuota`.
    Groups buckets by model family, emits "Pro" / "Flash" /
    "Flash Lite" tiers. Active OAuth refresh deferred to v0.4.5+.
  - **GitHub Copilot** — reads env `COPILOT_API_TOKEN`, hits
    `api.github.com/copilot_internal/user` with the editor headers
    GitHub Copilot's internal API requires. Emits "Premium" + "Chat"
    tiers. Mirrors `CopilotCollector.swift`.
  - **OpenRouter** — reads env `OPENROUTER_API_KEY` (optional
    `OPENROUTER_API_URL` override), hits `/credits` (required) and
    `/key` (optional, 3s timeout). Emits "Credits" + "Key Limit"
    tiers, dollar-scaled $1=100,000. Mirrors `OpenRouterCollector.swift`.
- All 6 providers run **concurrently** via `tokio::spawn` per arm with
  panic isolation (NOT `tokio::join!` — Codex review caught that
  `join!` shares a task with the parent and would unwind `sync_now`
  on any provider panic). Per-arm `JoinError::is_panic()` check logs
  panics at ERROR level but doesn't kill the sync.
- Per-provider structured logging: each `[Provider]`-prefixed
  WARN/DEBUG log makes `grep '\[Codex\]' cli-pulse.log` a triage
  tool. Per Gemini 3.1 Pro review.
- Provider name contract test: a checked-in snapshot of
  `Models.swift:10-37` `ProviderKind` raw values asserts against Rust
  constants in `quota/mod.rs`. Drift between Mac and Win/Linux
  provider names would land writes on different
  `(user_id, provider)` PKs and break dual-writer convergence.
- Sentry scrubber regex coverage extended for the new providers'
  token formats: OpenAI (`sk-proj-*`, `sk-svcacct-*`, legacy `sk-*`),
  GitHub legacy (`gh[pousr]_*`), GitHub PAT new (`github_pat_*`,
  47-char body), OpenRouter (`sk-or-*`), Google OAuth (`ya29.*`),
  generic Cookie + Authorization Bearer header redaction. 8 new
  Sentry tests.

### Changed
- **Module restructure**: existing v0.4.2 `quota.rs` moved to
  `quota/claude.rs`. New `quota/mod.rs` orchestrator owns the
  shared `QuotaSnapshot` / `TierEntry` types and provides
  `collect_all()`. Sibling modules: `codex.rs`, `cursor.rs`,
  `gemini.rs`, `copilot.rs`, `openrouter.rs`.
- `lib.rs::sync_now` now builds a multi-provider `p_provider_tiers`
  payload from `quota::collect_all()`. helper_sync's
  `jsonb_object_keys()` loop already handles multi-key maps —
  verified in v0.4.2 audit.

### Tests
- 35 new unit tests across the 5 new collectors (+ 8 Sentry, +
  existing 18 quota = **111 lib tests** total). Per-provider tests
  cover JSON parsing, tier emission, plan_type bucketing, error
  fallbacks, and provider-specific quirks (Codex `reset_at` epoch
  vs ISO, Copilot snake_case vs camelCase, Gemini launch-window
  null semantics, OpenRouter scaling).

### Reviews
- **Codex GPT-5.4 (2026-05-02)** — full spec audit + 5 Mac source
  files. 4 FIX-FIRST + 3 ship-it. Resolutions inline in the dev plan
  (`PROJECT_DEV_PLAN_2026-05-02_v0.4.3_multi_provider_quota.md` §10):
  panic isolation switched from `tokio::join!` to spawn-per-arm,
  Sentry regex extended for `github_pat_*` + generic Bearer,
  provider-name contract test added. **Deferred**: OpenRouter i32
  overflow at $21k+ balance is an inherited Mac bug requiring backend
  schema migration; tracked as v0.4.4+.
- **Gemini 3.1 Pro (2026-05-02)** — UX/product/i18n review. 4 FAIL.
  Per-provider WARN logs added (§4.6). Other UX gaps (env-var-only
  config, per-provider empty-state copy, token-expired UI warning)
  acknowledged as v0.4.4 frontend sprint scope.

### Known limitations (deferred to v0.4.4)
- Cursor / Copilot / OpenRouter credentials read from env vars only.
  Settings UI for credential entry pending.
- Gemini token refresh requires the user to run `gemini` CLI
  periodically. Active OAuth refresh + cross-platform Keychain pending.
- Token-expired silent-skip leaves stale provider_quotas row in
  place (no UI warning state). Explicit `CollectorStatus::Expired`
  + UI warning pending.
- Per-provider on/off toggle UI pending.

### Notes
- No server-side schema changes. iOS / Android / Mac unaffected.
- v0.4.x desktops on auto-update pick this up automatically.

## [0.4.2] — 2026-05-02

### Fixed
- **Dual-writer payload alignment with Mac scanner.** `provider_quotas`
  is keyed `(user_id, provider)` and helper_sync does a full-replace
  upsert on `tiers` / `plan_type` / `reset_time`. v0.4.0 quota.rs
  diverged from Mac's `ClaudeOAuthStrategy.swift` /
  `ClaudeSourceStrategy.swift` in four places, so when both clients
  were active for the same account, the row flickered every time the
  alternate writer polled. v0.4.2 closes the gaps:
  - **Sonnet/Opus fallback.** Mac emits the "Sonnet only" tier using
    `seven_day_sonnet` OR `seven_day_opus`. v0.4.x previously only
    read `seven_day_sonnet` and skipped the tier whenever Anthropic
    served opus instead. quota.rs now adds a `seven_day_opus` field
    and falls back to it (sonnet wins when both present).
  - **Launch-window null semantics.** Mac's `parseLaunchWindow`
    distinguishes "key absent" (skip the tier) from "key present
    but null" (rolled out, unused — emit at 100% remaining). v0.4.x
    used `Option<UsageWindow>` which collapses both to `None`.
    Replaced with a `LaunchWindow` three-state enum + custom
    deserializer mirroring the Swift semantics; applied to
    `iguana_necktie` (Designs) and `seven_day_omelette` (Daily
    Routines).
  - **Plan-type formatting buckets.** Mac normalizes via lowercase
    substring match across 8 buckets ("Max 20x", "Max 5x", "Ultra",
    "Pro", "Team", "Enterprise", "Free", "Unknown"); v0.4.x used
    exact equality + verbatim fallback. Re-implemented `format_plan`
    to match Mac line-for-line.
  - **Outer `reset_time` field.** Mac's helper payload includes a
    top-level `reset_time` keyed off the 5h Window reset
    (`ClaudeSourceStrategy.swift:217`); v0.4.x lib.rs::sync_now
    omitted it. helper_sync writes whatever it sees to
    `provider_quotas.reset_time`, so the absence flipped the column
    NULL on every Win sync. quota.rs now exposes `session_reset` on
    `QuotaSnapshot` and lib.rs threads it into the upload body.

### Added
- 6 new unit tests covering: opus fallback when sonnet absent, sonnet
  precedence when both present, launch-window present-null at 100%,
  launch-window absent skip, outer session_reset taken from 5h, outer
  session_reset None when 5h missing. Existing tests updated for the
  new plan-type bucket logic ("garbage_tier" → "Unknown" not verbatim,
  None → "Unknown" not "Claude").

### Reviews
- **Codex GPT-5.4 (architecture review, 2026-05-02)** — independent
  audit of dual-writer correctness vs Mac Swift collector. Verdict:
  INV-1 PASS (tier names match), INV-3 PASS (0–100 percentage scale
  matches), INV-2/4/5 FAIL with concrete file:line evidence. All
  three FAILs resolved in this release. Without these fixes,
  promoting v0.4.1 to Latest would have caused row flicker on any
  account where Mac Swift menu-bar app and Win/Linux Tauri desktop
  ran simultaneously.

### Notes
- No server-side schema changes. iOS / Android / Mac unaffected.
- v0.4.x desktops on auto-update pick this up automatically.

## [0.4.1] — 2026-05-02

### Added
- **Quota success log line.** v0.4.0's `quota.rs` only emitted log
  output on the failure paths (debug for missing creds / expired
  token, warn on API error) — the success branch was silent, so
  `cli-pulse.log` had no evidence the new collector was running on
  paired desktops with valid `.credentials.json`. Now writes
  `Claude quota updated: plan=<>, tiers=<n>, remaining=<n>` at info
  level once per successful fetch (every ~2-min sync cycle), so VM
  E2E and real-user diagnostics can confirm the collector is firing.

### Notes
- No behavior change. Only a single new `log::info!` call in
  `collect_claude()`'s `Ok` branch.
- iOS / Android / Mac unaffected.
- v0.4.0 desktops on auto-update: pick this up automatically.

## [0.4.0] — 2026-05-02

### Added
- **Local Claude quota collection on Win / Linux / Mac.** The desktop
  now scrapes the Anthropic OAuth `/api/oauth/usage` endpoint on its
  own and uploads the result via `helper_sync`'s
  `p_provider_remaining` / `p_provider_tiers` parameters (which had
  been shipping as `{}` since v0.3.0). A signed-in desktop sees real
  Claude tier bars (`5h Window`, `Weekly`, `Sonnet only`, `Designs`,
  `Daily Routines`) within ~2 minutes of starting Claude Code,
  regardless of whether a Mac is online for the same account.
  - Reads `~/.claude/.credentials.json` — same path Claude Code
    writes on every OS.
  - Best-effort: missing creds, expired token, or API failure
    silently skip the quota upload without breaking session/alert
    sync.
  - Plan-type detection from `rateLimitTier` field: `max_20x` →
    "Max 20x", `max_5x` → "Max 5x", `pro` → "Pro", custom values
    pass through verbatim.
  - Token freshness check with 60-second safety margin. Both
    ISO-8601 and epoch-millisecond `expiresAt` formats supported.
  - Pure-Win / Linux users (the v0.3.0 OTP-onboarding target) now
    see real tier bars without needing a Mac in the loop.

### Fixed
- **Codex gpt-5.5 cost rendered as $0.00.** v0.3.5 VM E2E found
  pricing.rs only went up to gpt-5.4, so any account running Codex
  with `gpt-5.5` returned a null cost and aggregated to $0. Added
  `gpt-5.5`, `gpt-5.5-codex`, `gpt-5.5-mini`, `gpt-5.5-nano`,
  `gpt-5.5-pro` entries with rates mirroring gpt-5.4 (OpenAI hasn't
  published official 5.5 billing yet — flagged in source as
  approximate, replace when public).

### Privacy
- **Sentry scrubber extended for Anthropic tokens.** The new OAuth
  usage path uses `Authorization: Bearer sk-ant-oat...` headers; if
  these ever leak into error messages or breadcrumb URLs, the
  `before_send` hook now redacts them with the new
  `<anthropic-token-redacted>` marker. Permissive regex
  (`sk-ant-(oat|api|sid)\d{0,3}-...`) handles current and future
  version formats including the rumored `sid` prefix. 4 new tests
  cover oat / api / Bearer header / unversioned variants.

### Reviews
- Codex GPT-5.4 (SQL/security/correctness, 2026-05-02): caught two
  FIX-FIRSTs on the original spec — the Anthropic regex was too
  strict on version digits (`\d{2}` only matched 2-digit versions;
  permissive `\d{0,3}` handles 1/2/3-digit and unversioned), and
  noted scrubber coverage for stacktrace/request-metadata strings
  was contingent on the regex matching real tokens. Both resolved
  before code.

### Notes
- Server-side schema unchanged. iOS / Android / Mac unaffected.
- v0.3.x desktops on auto-update: unaffected. Existing helper_sync
  payload accepted both empty and non-empty quota maps from day 1.
- Codex / Cursor / OpenAI API quota collection deferred to v0.4.1+.

## [0.3.5] — 2026-05-02

### Fixed
- **Copy-diagnostics block missing the `Logs:` line.** v0.3.4 added
  `log_dir` to the Rust `DiagnosticSnapshot` but the TypeScript
  diagText() never rendered it. Found during v0.3.4 Win VM E2E.
- **Log file appeared empty for unpaired desktops.** All Info-level
  log calls in the v0.3.4 code path were gated on a successful sync,
  which requires pairing — so a fresh-install unpaired user saw 0 KB
  in `cli-pulse.log`. Now writes a guaranteed startup banner from
  `.setup()` after all plugins install (so the logger is live):
  `CLI Pulse Desktop vX.Y.Z starting on <os> (<arch>)`,
  `Log directory: <path>`,
  `Paired (device 12345678…)` or `Not paired — sign in via Settings`,
  `Background sync loop started — first tick in 20s, then every 120s`.
  Users now have evidence-of-life in the log file regardless of
  paired state.

## [0.3.4] — 2026-05-02

### Added
- **Server-side dashboard parity** — when signed in, the Providers
  and Overview tabs now display the same plan/quota/tier/cross-device
  metrics that iOS / Android show for the account.
  - **Providers tab** picks up plan badge, quota bar, and tier bars
    (e.g. Claude Max's "5h Window 80/100", "Weekly 66/100", "Sonnet
    only 98/100", "Designs", "Daily Routines"). When a provider has
    no server-side quota data, an honest "Quota data unavailable"
    line appears instead of a fake bar.
  - **Overview tab** gains a 6-tile "All devices — today" grid above
    the existing local-scan section: today_cost, today_usage,
    today_sessions, active_sessions, online_devices,
    unresolved_alerts. The local-scan tiles are now labeled "This
    device" so users can tell server-aggregated from local.
  - All read paths use the existing v0.3.0 OTP infrastructure:
    refresh_token from the OS keychain, lazy/on-demand refresh, 30s
    in-memory cache scoped by user_id and explicitly cleared at
    every auth transition.
- **Single-instance enforcement** (user-flagged 2026-05-02). Launching
  CLI Pulse twice now focuses the existing window instead of spawning
  a duplicate. Uses `tauri-plugin-single-instance` (named-mutex on
  Windows, Unix domain socket on macOS/Linux).
- **File logging** (audit-flagged "no on-disk logs" gap). All app logs
  now write to:
  - Windows: `%LOCALAPPDATA%\dev.clipulse.desktop\logs\cli-pulse.log`
  - macOS: `~/Library/Logs/dev.clipulse.desktop/cli-pulse.log`
  - Linux: `~/.local/share/dev.clipulse.desktop/logs/cli-pulse.log`
  Rotation: 5 MB per file, KeepAll. The path is included in the
  Copy-diagnostics block on the About panel so support tickets can
  point at it directly.
- **Server-side unpair**. `Unpair this device` now actually removes
  the device row from Supabase (via the new strictly-additive
  `unregister_desktop_helper` RPC), instead of leaving an orphan that
  accumulated each re-pair. Best-effort: on transient network errors
  the local clear still proceeds (the next sign-in supersedes the
  orphan via `register_desktop_helper`).

### Server-side
Strictly-additive RPC deployed via
`migrate_v0.38_unregister_desktop_helper.sql` in the main repo:

- `unregister_desktop_helper(p_device_id, p_helper_secret)` — anon-
  callable but secret-gated. Returns `{deleted: true,
  remaining_devices}` on success, `{deleted: false, reason:
  'not_found'}` for both genuinely-missing rows and hash-mismatch
  (privacy invariant — same shape as `device_status`).
- Recomputes `profiles.paired = (count post-DELETE > 0)` in the same
  RPC tx so a multi-device account whose Laptop A unregisters while
  Laptop B is still active does NOT get its paired flag flipped to
  false (Codex review fix).

### Privacy
- Refresh-token rotation is now persisted to the keychain BEFORE the
  dashboard RPC call. A process crash between refresh and RPC no
  longer loses the rotated token (Codex review fix).
- Cache scoping is anchored by user_id with belt-and-suspenders
  mismatch checks on read; all auth transitions (sign-in, sign-out,
  unpair, refresh-failure, helper_sync error classifier) explicitly
  invalidate the cache so a re-sign-in as a different user can never
  see the previous account's tile data.

### Translation review
Gemini 3.1 Pro reviewed the new `auth.signin.*`, `overview.tile_*`,
and `providers.*` keys for zh-CN + ja. Caught:
- "Unresolved alerts" — `通知` (notification) → `告警` (zh-CN, matches
  iOS/Mac convention) and `通知` → `アラート` (ja).
- `剩 X/Y` (zh-CN) → `剩余 X/Y` (more polished UI copy).
- `计划` → `套餐` (zh-CN; "plan" in SaaS context).
- `本日のトークン` (ja) → `本日のトークン使用量` (clarifies that it's a
  metric, not a literal token list).
- `クォータ` retained for ja (matches iOS/Mac existing localization).

### Reviews
- VM Claude broader audit (2026-05-02) surfaced the zero-call-sites
  parity gap, the no-on-disk-logs gap, and the orphan-device-row
  cleanup gap — all addressed.
- Codex GPT-5.4 reviewed this spec and surfaced four FIX-FIRSTs:
  multi-device race on `paired = false`, unpair flow's "call server
  then clear local" being unsafe on transient errors, refresh-token
  rotation not persisted in the wrapper, and 30s cache leaking
  across sign-out boundaries. All resolved before execution.

### Notes
- v0.3.3 desktops on auto-update will land on v0.3.4 within minutes.
  Existing pairing survives the upgrade; the v0.3.4 plugins
  (single-instance, log) initialize on first launch with no
  user-visible migration.
- The new dashboard reads only fire when paired AND the user has
  signed in via OTP. Pure-Win/Linux users without a paired Mac will
  see "Quota data unavailable" — honest empty state, not a fake bar.

## [0.3.3] — 2026-05-02

### Fixed
- **About panel didn't reactively update on sign-in.** The Account
  section flipped to "Paired" with the right device_id immediately,
  but the About panel (and its Copy diagnostics block) kept showing
  "Not paired: -" until the next launch. Found during v0.3.2 Win VM
  E2E. AboutSection now refetches the diagnostic snapshot whenever
  the paired state flips.
- **OTP form vanished after unpair until tab switch.** The OTP-flow
  stage state stayed at `signed-in` after a successful sign-out, so
  neither the email-input nor code-input block rendered — the section
  showed only the heading + hint + legacy disclosure. Now reset to
  the email stage on unpair so re-pairing on the same screen works
  without a tab round-trip.
- **Unpair confirmation dialog claimed "new 6-digit code".** That's
  only accurate for the legacy Mac-pair path; on Windows / Linux the
  re-establish flow is the variable-length email OTP. Updated en /
  zh-CN / ja `unpair_confirm` strings to neutral wording covering
  both paths.

## [0.3.2] — 2026-05-02

### Fixed
- **OTP code field truncated to 6 digits, blocking sign-in (P0,
  found during v0.3.1 Win VM E2E).** The Supabase Auth project this
  desktop talks to is configured for 8-digit OTPs (verified against
  the iOS + Android sign-in views, both of which accept variable
  length). The desktop client hardcoded `maxLength={6}` and a
  `.slice(0, 6)` truncation in the OTP input handler, so the last 2
  digits of every code were silently dropped and verification failed.
  Removed the hardcoded length cap on the OTP path; input now accepts
  any digit-only string with a minimum of 4 to enable Verify. Updated
  `auth.signin.code_label` / `auth.signin.hint` strings (en / zh-CN
  / ja) to drop the "6-digit" wording.
  - The legacy "pair from Mac menu bar" path keeps its 6-digit cap;
    that code is server-minted by `generatePairingCode()` on the Mac
    and is genuinely fixed-length.

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
    post-build verification step now asserts NSIS ≥ 1.5 MB
    (LZMA-compressed; healthy build is ~2.7 MB, broken was
    ~0.6 MB), `.deb`/`.rpm` ≥ 3 MB, AppImage ≥ 30 MB, AND
    inspects each archive for the GUI binary by name (`7z l`
    for NSIS, `dpkg-deb -c` for .deb). Failure makes the matrix
    job red so the human un-draft gate notices.
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
