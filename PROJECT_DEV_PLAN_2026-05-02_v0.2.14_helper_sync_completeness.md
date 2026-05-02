# v0.2.14 — Helper sync completeness + cross-cutting fixes

> **⚠️ OBSOLETE (2026-05-02).** Superseded by
> `PROJECT_DEV_PLAN_2026-05-02_v0.2.14_quick_fixes.md`.
>
> This plan extended `helper_sync` with a `p_daily_usage` parameter, but
> Gemini 3.1 Pro's review caught that `daily_usage_metrics` has a
> per-user PK `(user_id, metric_date, provider, model)` — Mac and
> Windows scanners both writing every 2 minutes would race-clobber the
> same row. The proper fix needs a `device_id` column + dashboard-side
> aggregation, deferred to v0.3.1. v0.2.14 now ships as a slim
> 3-fix release that just **stops** making the broken call rather than
> patching it through helper_sync.
>
> Kept on disk for the multi-device-finding write-up only. Do not
> implement from this file.

---

**Status:** spec — pending sign-off.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-02), with input from Codex GPT-5.4 + Gemini 3.1 Pro.
**Tracks:** PROJECT_AUDIT_2026-05-02_cross_platform_auth (4 issues surfaced).
**Parent:** post-v0.2.13 (locale fix landed at `2d955fe`); pre-v0.3.0 OTP work (specced separately at `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md`).

## 1. Problem

Three-way audit (own + Codex + Gemini) of CLI Pulse's auth + sync architecture surfaced four bugs with this severity ordering:

| # | Severity | Bug | Detected by |
|---|---|---|---|
| 1 | **P0** | Tauri's `upsert_daily_usage` call hard-fails (anon-key vs server's `auth.uid()` requirement). Every paired Win/Linux user has 0 rows in `daily_usage_metrics`; iOS/Android viewers see incomplete cost data for their account. Plus the failure makes the entire sync command return `Err`, masking the partial helper_sync success. | own + Codex + Gemini |
| 2 | **P0** | Tauri sends empty `p_provider_remaining: {}` and `p_provider_tiers: {}` to helper_sync. Provider quota sync from desktop is dead. | Codex |
| 3 | **P1** | Watch stores auth tokens in BOTH UserDefaults (unprotected) AND Keychain (canonical). UserDefaults copy is a real exfil surface. | Codex |
| 4 | **P3** | `register_helper` SQL doc comment claims "requires auth.uid()" but the implementation only validates the pairing code. Misleads anyone reading the schema. | Codex |

P2 ("Tauri Windows helper_secret ACL hardening") is **explicitly deferred** to v0.3.0 because it'll be cleaner to migrate `helper_secret` to OS keychain (via the `keyring` crate) at the same time as `refresh_token`, rather than do an interim ACL-only pass now.

## 2. Decision (with rationale)

**Fix #1 + #2 by extending the existing `helper_sync` RPC with a new `p_daily_usage` parameter, instead of creating a sibling `helper_upsert_daily_usage` RPC.**

Three independent reviews on the fix shape:

| Reviewer | Initial vote | Final |
|---|---|---|
| Codex | new RPC `helper_upsert_daily_usage` | accepted with caveat |
| Gemini 3.1 Pro | extend `helper_sync` | extend |
| Claude Opus 4.7 | extend `helper_sync` (after weighing both) | extend |

**Reasons to extend:**
1. **One round-trip per cycle**: the helper sync loop runs every 2 minutes. A single combined RPC saves ~262K HTTP calls/year/device versus a parallel call. Aggregate at fleet scale that's meaningful.
2. **Atomic semantics**: the audit found a real UX bug — current Tauri sends helper_sync(✓) → upsert_daily_usage(✗), client reports failure even though sessions/alerts landed. Atomic helper_sync(p_daily_usage) makes the result honest: all-or-nothing per call.
3. **API surface stays small**: helper_sync already accepts 6 parameters across 3 data types (sessions, alerts, provider_quotas). Adding daily_usage is a natural extension; spawning a parallel RPC duplicates auth/transaction boilerplate.
4. **Backward-compatible**: `p_daily_usage jsonb default '[]'::jsonb` — old clients (v0.2.13 and earlier) calling with the existing arg list still work because Postgres function defaults fill in the missing parameter.

**Codex's caveat addressed:** "atomic could mean daily failure rolls back sessions/alerts." We solve this with **per-row PL/pgSQL subtransactions** (each `insert ... on conflict ...` wrapped in `begin ... exception when others then ... end`), so a single bad metric row doesn't unwind the parent transaction or affect other rows. Counts of (succeeded, errored) returned in the response.

**Fix #2 (provider quotas) is partially addressed in this sprint and partially deferred:**
- Tauri still doesn't have user API keys, so it cannot populate `provider_remaining` / `provider_tiers` for paid providers (Anthropic API, OpenAI API, etc.). Those would require user JWT to read `user_provider_keys` table → gated on v0.3.0 OTP.
- What Tauri CAN populate: provider quota state derived from local JSONL scan (e.g., model usage breakdown, total tokens). For v0.2.14 we keep `p_provider_*` empty and document the gap; v0.3.x will fill it once OTP + user_provider_keys read access lands.

**Fix #3 (Watch UserDefaults) handled with a migration helper**: on first launch after upgrade, copy any existing UserDefaults token to Keychain, then delete the UserDefaults copy. After ~2 weeks of population, the migration code can be removed in v0.3.x.

**Fix #4 is a doc-only edit**: 5 minutes.

## 3. Server-side changes

### 3.1 Extended `helper_sync` signature

`backend/supabase/helper_rpc.sql` — diff (full new function shown for clarity):

```sql
create or replace function public.helper_sync(
  p_device_id uuid,
  p_helper_secret text,
  p_sessions jsonb default '[]'::jsonb,
  p_alerts jsonb default '[]'::jsonb,
  p_provider_remaining jsonb default '{}'::jsonb,
  p_provider_tiers jsonb default '{}'::jsonb,
  p_daily_usage jsonb default '[]'::jsonb   -- NEW (v0.2.14)
)
returns jsonb as $$
declare
  v_user_id uuid;
  v_session jsonb;
  v_alert jsonb;
  v_metric jsonb;
  v_provider text;
  v_remaining integer;
  v_session_count integer := 0;
  v_alert_count integer := 0;
  v_metric_count integer := 0;          -- NEW
  v_metric_error_count integer := 0;    -- NEW
  v_synced_ids text[] := '{}';
begin
  -- [existing auth block: SHA-256 secret check → v_user_id]
  -- [existing payload size guards: max 500 sessions, max 500 alerts]

  -- NEW: payload size guard for daily_usage
  if jsonb_array_length(p_daily_usage) > 200 then
    raise exception 'Too many daily usage metrics (max 200)';
  end if;

  -- [existing: device.last_seen_at = now()]
  -- [existing: sessions upsert loop → v_session_count]
  -- [existing: stale-session sweep]
  -- [existing: alerts upsert loop → v_alert_count]
  -- [existing: provider_tiers / provider_remaining loops]

  -- NEW: daily_usage upsert with per-row subtransaction
  for v_metric in select * from jsonb_array_elements(p_daily_usage) loop
    begin
      insert into public.daily_usage_metrics (
        user_id, metric_date, provider, model,
        input_tokens, cached_tokens, output_tokens, cost, updated_at
      )
      values (
        v_user_id,
        (v_metric->>'metric_date')::date,
        v_metric->>'provider',
        v_metric->>'model',
        coalesce((v_metric->>'input_tokens')::bigint, 0),
        coalesce((v_metric->>'cached_tokens')::bigint, 0),
        coalesce((v_metric->>'output_tokens')::bigint, 0),
        coalesce((v_metric->>'cost')::numeric, 0),
        now()
      )
      on conflict (user_id, metric_date, provider, model) do update set
        input_tokens = excluded.input_tokens,
        cached_tokens = excluded.cached_tokens,
        output_tokens = excluded.output_tokens,
        cost = excluded.cost,
        updated_at = now();
      v_metric_count := v_metric_count + 1;
    exception when others then
      -- Per-row subtransaction: bad row (malformed date, null model, etc.)
      -- doesn't unwind the parent transaction or affect other rows.
      v_metric_error_count := v_metric_error_count + 1;
    end;
  end loop;

  return jsonb_build_object(
    'sessions_synced', v_session_count,
    'alerts_synced', v_alert_count,
    'metrics_synced', v_metric_count,        -- NEW
    'metrics_errored', v_metric_error_count  -- NEW
  );
end;
$$ language plpgsql security definer set search_path = pg_catalog, public, extensions;
```

**Conflict target**: `(user_id, metric_date, provider, model)` — same as the legacy `upsert_daily_usage` to ensure either RPC writes to the same row.

**Subtransaction notes** (PL/pgSQL semantics): a `begin ... exception when others then ... end` inside a function creates a subtransaction. If the inner block raises, only that subtransaction rolls back; the function's outer state (counters, prior loop iterations) is preserved. Each subtransaction has a small allocation cost (XID, savepoint slot). For typical batches (1-30 metric rows per 2-min sync), this is negligible. We cap at 200 to keep worst case bounded.

**Backward compatibility**: the new param has `default '[]'::jsonb`. Existing v0.2.13 desktop calls 6-arg helper_sync; Postgres fills in `p_daily_usage = '[]'` and the new loop runs zero iterations. Zero behavior change for old clients.

### 3.2 `register_helper` doc comment fix

`backend/supabase/helper_rpc.sql:5-6` — replace:
```sql
-- register_helper: security definer — requires auth.uid() to match
```
with:
```sql
-- register_helper: security definer. Authentication is via the pairing code:
-- the function looks up pairing_codes.user_id and creates a device row under
-- that user_id. Does NOT require or check auth.uid(); callers use anon key.
-- (For an auth.uid()-based path see register_desktop_helper, v0.3.0.)
```

### 3.3 Tests

For the extended `helper_sync`:
- **Backwards-compat test**: call helper_sync with 6 args (no p_daily_usage); assert success, `metrics_synced = 0`.
- **Happy path**: call with 5 valid metric rows; assert all 5 land in `daily_usage_metrics`, returned `metrics_synced = 5`.
- **Per-row resilience**: include 1 malformed row (e.g. `metric_date = "not-a-date"`) among 4 good rows; assert `metrics_synced = 4`, `metrics_errored = 1`, the 4 good rows landed, the 1 bad row is missing.
- **DoS cap**: pass 201 rows; assert error 'Too many daily usage metrics'.
- **Conflict / re-sync idempotency**: same payload sent twice; assert second call updates rows (not duplicates), counts return 5+5.
- **Schema-compatibility test**: read inserted row back via direct SELECT; assert all columns populated correctly.
- **Auth test**: call with wrong helper_secret; assert 'Device not found or unauthorized' (existing auth path unchanged).

### 3.4 Migration safety

- Strictly additive: no existing column drop, no existing function replaced (only extended).
- Old `upsert_daily_usage` RPC is left in place; macOS scanner still uses it via user JWT. We don't touch it.
- Rollback: revert helper_sync to 6-arg form. Old desktop clients calling without p_daily_usage continue to work; new desktop clients reverting (re-deploying old build) also continue to work.

## 4. Tauri client changes

### 4.1 `src-tauri/src/supabase.rs`

```rust
// Extend HelperSyncRequest to include p_daily_usage.
#[derive(Debug, Clone, Serialize)]
pub struct HelperSyncRequest<'a> {
    pub p_device_id: &'a str,
    pub p_helper_secret: &'a str,
    pub p_sessions: Value,
    pub p_alerts: Value,
    pub p_provider_remaining: Value,
    pub p_provider_tiers: Value,
    pub p_daily_usage: Value,  // NEW — array of DailyUsageMetric
}

// Extend HelperSyncResponse with new metric counters.
#[derive(Debug, Clone, Deserialize)]
pub struct HelperSyncResponse {
    pub sessions_synced: i32,
    pub alerts_synced: i32,
    pub metrics_synced: i32,        // NEW
    pub metrics_errored: i32,       // NEW (default 0 for backwards-compat parsing of legacy responses)
}

// REMOVE: pub async fn upsert_daily_usage(metrics) → no longer used by Tauri.
// Keep the DailyUsageMetric struct + from_entry conversion (used to build the
// payload for helper_sync now).
```

### 4.2 `src-tauri/src/lib.rs`

In the existing `manual_sync` flow (around line 280-310):

```rust
// Build daily_usage payload (was passed separately to upsert_daily_usage)
let daily_usage_metrics: Vec<_> = scan
    .entries
    .iter()
    .filter_map(supabase::DailyUsageMetric::from_entry)
    .collect();
let metrics_count_local = daily_usage_metrics.len();
let daily_usage_json = serde_json::to_value(&daily_usage_metrics)
    .unwrap_or(serde_json::json!([]));

// Single combined helper_sync call (was: 2 calls — helper_sync + upsert_daily_usage)
let hs = supabase::helper_sync(&supabase::HelperSyncRequest {
    p_device_id: &cfg.device_id,
    p_helper_secret: &cfg.helper_secret,
    p_sessions: sessions::sessions_payload(&snapshot),
    p_alerts: alerts_payload,
    p_provider_remaining: serde_json::json!({}),
    p_provider_tiers: serde_json::json!({}),
    p_daily_usage: daily_usage_json,  // NEW
})
.await
.map_err(friendly)?;

// REMOVE the separate upsert_daily_usage call.
// REMOVE the metrics_uploaded field that previously came from a count of
// what we sent; replace it with hs.metrics_synced (returned from server).

Ok(SyncReport {
    sessions_synced: hs.sessions_synced,
    alerts_synced: hs.alerts_synced,
    metrics_synced: hs.metrics_synced,         // NEW (replaces metrics_uploaded)
    metrics_errored: hs.metrics_errored,       // NEW
    metrics_attempted: metrics_count_local,    // NEW (what we sent vs what server accepted)
    total_cost_usd: scan.total_cost_usd,
    total_tokens: scan.total_tokens,
    files_scanned: scan.files_scanned,
    live_sessions_sent: snapshot.sessions.len(),
    live_processes_seen: snapshot.total_processes_seen,
})
```

### 4.3 SyncReport / UI updates

The `SyncReport` returned to the frontend gains `metrics_synced`, `metrics_errored`, `metrics_attempted`. The Settings tab's manual-sync button result display should surface a warning if `metrics_errored > 0`. For typical happy-path users, errored is 0 and the UI doesn't change.

### 4.4 Tauri tests

- Unit: serialize a HelperSyncRequest with p_daily_usage populated; assert JSON shape.
- Integration (against staging Supabase): paired test device runs full sync, asserts metrics_synced > 0 in the response.
- Manual: pair desktop on Win VM, run sync, query `select count(*) from daily_usage_metrics where user_id = ...` → expect non-zero.

## 5. Watch token leakage fix

### 5.1 The leak
`CLI Pulse Bar/CLI Pulse Bar Watch/WatchSessionManager.swift:170-172` writes:
```swift
UserDefaults.standard.set(token, forKey: "cli_pulse_watch_auth_token")
```
in addition to the canonical Keychain write at `WatchAppState.swift:43-56`. UserDefaults is unprotected on watchOS — readable by other tasks in the same app sandbox and exfiltrable via routine debug/backup tools.

### 5.2 Fix in two phases

**Phase 1 — v0.2.14 (this sprint):**
1. On Watch app launch: read any existing `cli_pulse_watch_auth_token` from UserDefaults. If present:
   - Verify Keychain has a copy (or copy it across).
   - `UserDefaults.standard.removeObject(forKey: "cli_pulse_watch_auth_token")`.
2. Remove all WRITE sites for `cli_pulse_watch_auth_token` in WatchSessionManager.
3. Keep the Keychain code path unchanged. WatchAppState becomes the only source of truth.

**Phase 2 — v0.3.x:**
After ~2 weeks of v0.2.14 population (long enough for installed-base migration), remove the migration code. Hard-strip any code that even references the old UserDefaults key.

Equivalent migration on iOS PhoneSessionManager if it has the same dual-write — needs verification before sprint start (closed in §11).

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| Old desktop clients (v0.2.13) crash on the new helper_sync return shape (extra fields) | Decoder uses `serde(default)` on `metrics_synced` / `metrics_errored` — old clients parse new responses without errors. Verified in Tauri test. |
| Per-row subtransaction overhead at scale | DoS cap of 200 rows + measurement on staging (a synthetic 200-row sync should complete <100ms based on similar PL/pgSQL benchmarks; will measure during Day 1 testing) |
| `daily_usage_metrics` rows from device A overwrite rows from device B for same user | Conflict target `(user_id, metric_date, provider, model)` is **per-user**, not per-device. Same as existing `upsert_daily_usage` semantics. So a user with Mac + Windows synthesizes their token usage at the user level, not double-counted per device. **This matches the existing macOS scanner behavior** — verified by reading `helper_rpc.sql` carefully. **Open: does the existing schema support a per-device breakdown for the future "see token usage by device" feature?** Logged in §11. |
| Tauri stops calling old `upsert_daily_usage`; that RPC sits unused | Acceptable. Leave it for now; the macOS scanner still uses it. Don't drop it as part of this sprint. |
| Watch UserDefaults migration runs on a user who never had the leaky build | Migration is a no-op (`UserDefaults.standard.string(forKey:)` returns nil → skip). Zero risk. |
| Watch migration deletes a token before successfully copying it to Keychain | Migration logic: copy first, verify by read, only then delete. Test with kill-mid-migration scenario (force-quit Watch app between copy and delete). |

## 7. Milestones

Total est: **1.5–2 working days** for v0.2.14.

| Day | Work |
|---|---|
| 1 (server) | helper_sync extension + 7 tests + register_helper comment fix + staging deploy. Measure subtransaction cost on synthetic 200-row payload. |
| 1 (desktop) | Tauri supabase.rs + lib.rs changes; remove old upsert_daily_usage call; SyncReport fields; manual-sync UI warning for metrics_errored > 0; integration test against staging. |
| 2 | Watch UserDefaults migration: implement Phase 1 + migration test (force-quit between copy and delete). Audit iOS PhoneSessionManager for same dual-write pattern; if found, mirror the migration. |
| 2 | E2E on Windows VM: pair, sync, query staging Supabase to verify daily metrics appear under correct user_id. CHANGELOG. Commit + tag v0.2.14. |

If iOS does NOT have the same UserDefaults issue (verify Day 1), the Watch-side change shrinks to half a day and total drops to 1.5 days.

## 8. Backward compatibility

- v0.2.13 / earlier desktops continue to work after the SQL deploy. They call old 6-arg helper_sync (gets default `p_daily_usage = '[]'`) AND old `upsert_daily_usage` (still fails for them). Net: same broken state as today, but no NEW breakage.
- After v0.2.14 desktop deploy, both server + client are aligned and metrics flow.
- Watch migration is one-time; zero impact on users who never had the leaky build.

## 9. Out of scope (deferred to v0.3.0 or later)

- **Tauri provider quota population** (`p_provider_remaining` / `p_provider_tiers` from desktop). Requires user JWT to read `user_provider_keys` table. Gated on v0.3.0 OTP. Document the gap in v0.2.14 CHANGELOG so users know desktop quota sync is still a no-op.
- **Tauri Windows ACL hardening for helper_secret**. Migrate to OS keychain (via `keyring` crate) alongside refresh_token storage in v0.3.0. Doing both at once is cleaner than an interim ACL pass.
- **Removal of legacy `upsert_daily_usage` RPC**. macOS scanner still uses it. Wait until macOS migrates to helper_sync(p_daily_usage) too — v0.4.0 candidate.
- **Per-device daily_usage breakdown**. Current schema is per-user. If we ever need "show me Mac vs Windows token usage", we'd add a `device_id` column to `daily_usage_metrics` — separate planning item.

## 10. Decisions to close before sprint start

1. **Subtransaction performance on a 200-row payload** — measure on staging Day 0 before locking the cap. If <100ms, ship 200. If higher, lower cap to 100 or batch into multiple calls client-side.
2. **iOS PhoneSessionManager UserDefaults dual-write?** — grep `cli pulse/CLI Pulse Bar/CLI Pulse Bar iOS/PhoneSessionManager.swift` for `UserDefaults.standard.set` paired with token-name strings. If present, scope expands to include iOS migration; if absent, Watch-only.
3. **Sentry scrubber audit** — same as v0.3.0 spec §11 item 4. Day-1 task. Verify `daily_usage_metrics` payload contents won't accidentally land in Sentry breadcrumbs (model names are PII-adjacent for some users? probably fine, but check).
4. **CHANGELOG forward-reference** — should the v0.2.14 release notes name the v0.3.0 OTP work explicitly, or stay scoped to the four fixes? Recommend explicit ("OTP login coming next, see PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md") to set expectations.

## 11. References

- Audit: `PROJECT_AUDIT_2026-05-02_cross_platform_auth.md` (this commit if landing together).
- Existing helper_sync: `cli pulse/backend/supabase/helper_rpc.sql:133-274`.
- Existing upsert_daily_usage: `cli pulse/backend/supabase/schema.sql:430-475`.
- Tauri client: `cli-pulse-desktop/src-tauri/src/supabase.rs:53-200`, `lib.rs:280-310`.
- Watch leak site: `cli pulse/CLI Pulse Bar/CLI Pulse Bar Watch/WatchSessionManager.swift:170-172`.
- Watch canonical Keychain: `cli pulse/CLI Pulse Bar/CLI Pulse Bar Watch/WatchAppState.swift:43-56`.
- v0.3.0 OTP spec (separate, follows this sprint): `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md`.
