# v0.2.14 — Quick fixes (slim release)

**Status:** spec — pending sign-off.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-02).
**Tracks:** PROJECT_AUDIT_2026-05-02_cross_platform_auth (subset).
**Parent:** post-v0.2.13 (locale fix landed at `2d955fe`); pre-v0.3.0 OTP work
(spec at `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md`).
**Supersedes:** `PROJECT_DEV_PLAN_2026-05-02_v0.2.14_helper_sync_completeness.md`
(v1, marked obsolete — see §1.1).

## 1. Problem

The cross-platform audit on 2026-05-02 surfaced four bugs. v0.2.14 ships the
**three quick wins**; the fourth (Tauri provider quotas) waits for v0.3.0 OTP
because it requires user JWT to read `user_provider_keys`.

| # | Severity | Fix | Where |
|---|---|---|---|
| 1 | **P0** | Tauri's `upsert_daily_usage` call hard-fails (anon-key vs server's `auth.uid()` requirement). Every paired Win/Linux user gets a sync `Err` even when sessions/alerts succeeded. | Tauri client only |
| 2 | **P1** | Watch stores auth tokens in BOTH UserDefaults (unprotected) AND Keychain (canonical). UserDefaults copy is exfiltratable. | watchOS only |
| 3 | **P3** | `register_helper` SQL doc comment claims "requires auth.uid()". Implementation actually validates the pairing code; auth.uid is unused. Misleads schema readers. | Backend doc |

P2 ("Tauri Windows helper_secret ACL hardening") is deferred to v0.3.0 alongside
the keychain migration for `refresh_token` (cleaner to do both in one pass).
P0 ("Tauri provider quotas") is deferred to v0.3.0+ (gated on user JWT access).

### 1.1 Why v1 of this plan was scrapped

v1 proposed extending `helper_sync` with a `p_daily_usage` parameter so Tauri
could write daily metrics through the helper credential path. Gemini 3.1 Pro's
review caught a fundamental issue: `daily_usage_metrics` has primary key
`(user_id, metric_date, provider, model)` — **per-user, not per-device**. With
both Mac and Windows scanning every 2 minutes, the two devices would race-write
the same row, each overwriting the other. The final row would reflect whichever
device synced last, not the sum. Cost accuracy across devices would silently
break for any user with both Mac and Windows.

The right fix is a `device_id` column + dashboard-side aggregation, which is
~3.5 days of work and touches the Mac scanner, iOS dashboard, and Android
dashboard. That work is now `PROJECT_DEV_PLAN_2026-05-03_v0.3.1_multi_device_daily_usage.md`
(to be drafted), executing after v0.3.0.

For v0.2.14, the right thing to do is **stop making the broken call**. Tauri's
daily metrics will stay empty (same observable state as today, since the call
already fails silently in the audit) but sync stops returning `Err`. v0.3.1
properly populates them.

## 2. Decisions

**Fix #1 — delete the broken call, don't extend helper_sync.** Rationale: any
schema-extending fix collides with the multi-device race that v0.3.1 has to
solve anyway. v0.2.14 stays a no-schema-change release.

**Fix #2 — migrate-then-delete pattern on Watch app launch.** Even though no
read site references the leaky UserDefaults keys today (canonical reads come
from Keychain via `WatchAppState`), an upgrading user could in theory have
hit a Keychain write failure in the leaky build. So: on launch, if a stale
UserDefaults token exists, copy to Keychain (idempotent — overwrite OK), verify
by read, then `removeObject`. After ~2 weeks of v0.2.14 population, the
migration code can be removed in v0.3.x.

**Fix #3 — doc-only. 5 minutes.**

**iOS PhoneSessionManager already migrated to Keychain.** Verified by repo-wide
grep: zero `UserDefaults.standard.set` calls under `CLI Pulse Bar/CLI Pulse
Bar iOS/` for token-named keys. No iOS change needed.

## 3. Tauri changes

### 3.1 `cli-pulse-desktop/src-tauri/src/lib.rs`

**Delete**: lines 295-304 (the entire daily-usage block) — the local metrics
build + `upsert_daily_usage` call + error propagation.

**Update SyncReport struct (lib.rs:245-256)**: drop `metrics_uploaded: usize`.

**Update background-sync log line (lib.rs:374-380)**: drop the `{} metrics`
specifier and the `report.metrics_uploaded` arg. Keep the sessions + alerts
counters.

```rust
// Before:
struct SyncReport {
    sessions_synced: i64,
    alerts_synced: i64,
    metrics_uploaded: usize,
    total_cost_usd: f64,
    ...
}

// After:
struct SyncReport {
    sessions_synced: i64,
    alerts_synced: i64,
    total_cost_usd: f64,
    ...
}
```

### 3.2 `cli-pulse-desktop/src-tauri/src/supabase.rs`

**Delete** (now dead after the lib.rs change):
- `UpsertDailyUsageRequest` struct (lines 157-160)
- `DailyUsageMetric` struct (lines 162-171)
- `upsert_daily_usage` function (lines 173-182)
- `impl DailyUsageMetric { from_entry }` block (lines 207-225)
- The two `daily_usage_metric_*` tests (lines 251-282)

**Keep** (still used): `helper_sync`, `helper_heartbeat`, `register_helper`,
`unpair_device`, all surrounding non-daily-usage code.

v0.3.1 will re-introduce a refined `DailyUsageMetric` (with `device_id`) when
it wires daily_usage through `helper_sync`.

### 3.3 `cli-pulse-desktop/src/App.tsx`

**Drop `metrics_uploaded` from the TS SyncReport type (lines 39-48)** to match
the new Rust struct shape.

**Drop the two `metrics_uploaded` UI surfaces** (lines 761, 850 — both pass it
into `metrics:` props for display). Those props/components either no longer
need a metrics field, or the field is dropped entirely. Choose the simpler
path during implementation.

Search for any other `metrics_uploaded` reference and clean up (the grep at
spec time shows only the three sites: type def + two UI passes).

### 3.4 Tests

- `cargo test` in `src-tauri/` — must pass after dead-code removal.
- `npm test` (Vitest) in `src/` — must pass after type change.
- Pre-push hook gates the push.
- Manual: pair desktop on Win VM, run sync once, assert `SyncReport` no longer
  contains `metrics_uploaded` and the call returns `Ok` (was `Err` from the
  upsert path).

## 4. Watch UserDefaults migration

### 4.1 The leak

`CLI Pulse Bar/CLI Pulse Bar Watch/WatchSessionManager.swift:170-172`:

```swift
UserDefaults.standard.set(token, forKey: "cli_pulse_watch_auth_token")
if let refresh { UserDefaults.standard.set(refresh, forKey: "cli_pulse_watch_refresh_token") }
```

Triggered when the iPhone pushes auth via `didReceiveUserInfo`. The Keychain
write happens in parallel via `WatchAppState` (`WatchAppState.swift:47, 52`),
so Keychain is the canonical store. UserDefaults is dead-write — there are
zero read sites for these keys outside the existing logout-cleanup at lines
155-156.

### 4.2 Phase 1 — v0.2.14 (this sprint)

**Remove writes**: delete lines 170-172 (the `UserDefaults.standard.set`
calls). Keep the Keychain path untouched.

**Add launch-time migration**: at Watch app launch, before any auth dispatch:
1. Read any existing UserDefaults `cli_pulse_watch_auth_token` /
   `cli_pulse_watch_refresh_token`.
2. If present, write to Keychain via `KeychainHelper.save` (idempotent —
   overwriting an existing key is a no-op semantically).
3. Verify via `KeychainHelper.load` — if read-back succeeds, call
   `UserDefaults.standard.removeObject` on both keys. If read-back fails,
   leave the UserDefaults entries in place and log; user will continue with
   the existing Keychain copy on next session.

The launch hook lives in WatchAppState's `init` (or its `restoreSession`
helper). Adding ~15 lines.

**Keep the logout cleanup at lines 155-156** — defense-in-depth for the rare
case where the migration didn't run (e.g., user signs out before opening the
Watch app). It's a safe no-op when the keys are already absent.

### 4.3 Phase 2 — v0.3.x (later)

After ~2 weeks of v0.2.14 telemetry showing the migration ran on the installed
base, remove the migration code. Hard-strip any reference to the legacy
UserDefaults keys.

### 4.4 Tests

- Unit (where feasible — watchOS test rigs are limited): inject a fake
  UserDefaults containing the legacy key, run migration, assert Keychain has
  the value AND UserDefaults no longer does.
- Force-quit-mid-migration scenario: copy lands first, verify-read succeeds,
  then forcibly skip the remove. On next launch, same migration runs again
  (idempotent) and completes the delete.
- Manual on a real Watch device with v0.2.13 installed: upgrade to v0.2.14,
  open Watch app, inspect via Xcode Console that migration log fired and the
  UserDefaults entries are gone.

## 5. SQL doc comment fix

`cli pulse/backend/supabase/helper_rpc.sql:5-9` — replace the misleading block:

```sql
-- register_helper: security definer — requires auth.uid() to match
--   the pairing code owner; called by the authenticated user.
-- helper_heartbeat / helper_sync: security definer — helpers call
--   via anon key, so RLS would block them.  Internal auth is done
--   by validating (device_id, helper_secret) inside the function.
```

with:

```sql
-- register_helper: security definer. Authentication is via the pairing code:
--   the function looks up pairing_codes.user_id and creates a device row
--   under that user_id. Does NOT require or check auth.uid() — callers use
--   the anon key. (For an auth.uid()-based desktop sign-in path, see
--   register_desktop_helper, planned for v0.3.0.)
-- helper_heartbeat / helper_sync: security definer — helpers call
--   via anon key, so RLS would block them.  Internal auth is done
--   by validating (device_id, helper_secret) inside the function.
```

Doc-only edit. No deploy needed (PostgreSQL function comments are descriptive
text inside the SQL file; the live function definitions remain unchanged).
The committed file becomes the up-to-date reference for whoever reads schema
next.

## 6. CHANGELOG entry

`cli-pulse-desktop/CHANGELOG.md` — append:

```
## v0.2.14 — 2026-05-XX

### Fixed
- Sync no longer reports failure after a successful sessions+alerts
  upload. The prior daily-usage path was hard-failing on every sync due
  to an auth shape mismatch and surfacing as a hard error in the
  manual-sync UI; we've removed that path. Per-device daily usage
  metrics are coming back properly in v0.3.1.
- watchOS auth tokens no longer linger in UserDefaults alongside the
  canonical Keychain copy. Existing leaked tokens are migrated to
  Keychain on first launch and the UserDefaults copies are cleared.

### Coming next
- v0.3.0: direct email sign-in for Tauri desktop (no more "find a Mac
  to pair from"). Spec:
  PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md
- v0.3.1: per-device daily usage metrics so Mac + Windows + Linux all
  contribute correctly to your dashboard cost totals.
```

## 7. Version bumps

Bump from 0.2.13 → 0.2.14 in all four:
- `src-tauri/Cargo.toml` (`version = ...`)
- `src-tauri/Cargo.lock` (regenerate via `cargo update -p cli-pulse-desktop`
  or whichever package name; if the lock isn't auto-regenerated by the toml
  bump, run `cargo build` once)
- `src-tauri/tauri.conf.json` (`"version": "..."`)
- `package.json` (`"version": "..."`)

## 8. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Watch users with no UserDefaults leakage hit the migration anyway | Migration is a no-op (`UserDefaults.standard.string(forKey:)` returns nil → skip). Zero risk. |
| Migration deletes a token before successfully copying it to Keychain | Sequence is copy → verify-read → delete. If verify-read fails, delete is skipped and the user keeps the existing Keychain copy from previous session. |
| `metrics_uploaded` removal breaks an external consumer | This field is internal to Tauri (Rust↔TS only). No external consumer. Verified by grep — no references outside `lib.rs`, `App.tsx`, and `supabase.rs`. |
| Linux x64 CI hits the transient binary-releases 502 during AppImage bundling | `gh run rerun --failed` after the green Mac/Win/Linux-arm runs land. Same playbook as v0.2.10, v0.2.12. |
| User on v0.2.13 sees a "metrics_uploaded missing" deserialization error | Tauri client + UI both ship in the same release. Server unchanged. No version skew. |

## 9. Milestones (1.5 working days target)

| Half-day | Work |
|---|---|
| 0.5 | Tauri lib.rs + supabase.rs + App.tsx changes; cargo test + npm test green; manual sync verify on Win VM (sync returns Ok). |
| 0.5 | Watch UserDefaults migration: WatchAppState.swift hook + delete writes in WatchSessionManager + force-quit-mid-migration smoke. |
| 0.25 | SQL doc fix; CHANGELOG; version bumps. |
| 0.25 | Commit, tag v0.2.14, push, monitor CI for all 4 platforms; gh release edit --latest after green. |

## 10. Backward compatibility

- v0.2.13 desktop users continue to work after v0.2.14 ships (server is
  unchanged).
- Watch users on the prior leaky build pick up the migration on next launch
  with the new build.
- iOS / Android / Mac scanner: zero change.

## 11. Out of scope (explicitly deferred)

- Tauri provider quotas (`p_provider_remaining`, `p_provider_tiers`) → v0.3.0
  (gated on OTP for `user_provider_keys` read access).
- Tauri Windows helper_secret ACL hardening → v0.3.0 (alongside refresh_token
  keychain migration).
- Per-device `daily_usage_metrics` schema redesign → v0.3.1.
- Watch UserDefaults migration code removal → v0.3.x (after population window).

## 12. References

- v0.2.14 v1 (obsolete): `PROJECT_DEV_PLAN_2026-05-02_v0.2.14_helper_sync_completeness.md`
- v0.3.0 OTP spec: `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md`
- Watch leak site: `cli pulse/CLI Pulse Bar/CLI Pulse Bar Watch/WatchSessionManager.swift:170-172`
- Watch canonical Keychain: `cli pulse/CLI Pulse Bar/CLI Pulse Bar Watch/WatchAppState.swift:43-56`
- Tauri broken call: `cli-pulse-desktop/src-tauri/src/lib.rs:295-304`
- SQL doc: `cli pulse/backend/supabase/helper_rpc.sql:1-10`
