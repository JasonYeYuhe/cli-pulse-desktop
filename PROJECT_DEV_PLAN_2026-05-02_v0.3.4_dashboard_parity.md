# v0.3.4 — Dashboard parity + single-instance + file logging

**Status:** spec — pending Codex review.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-02).
**Tracks:** Win VM E2E broader audit (2026-05-02) + user single-instance request.
**Parent:** v0.3.3 (Latest).
**Sibling:** none — this is a single client-side release with one strictly-additive backend RPC.

## 1. Problem

The Win VM broader audit and Mac-parity inventory both surface the same root issue: **v0.3.0 added the OTP infrastructure to mint user JWTs, but no user-scoped READ paths use it.** Every server-side dashboard RPC (`provider_summary`, `dashboard_summary`, `get_daily_usage`, `get_daily_usage_by_device`) has zero call sites in the desktop. The Providers tab shows only local-scan cost rollup; the Overview tab's "today" numbers are single-device-only; v0.3.1's `get_daily_usage_by_device` RPC ships unused. Pure-Win/Linux users sign in successfully and then see a strictly worse dashboard than the iOS users on the same account.

Three orthogonal issues in the same scope-window:
- **Multiple instances spawnable.** User reported launching CLI Pulse multiple times yields multiple windows/trays. Should focus the existing window.
- **No on-disk logs.** `env_logger::init()` writes to stderr, which a Tauri release build on Windows discards. Bug reports have nothing to attach.
- **`unpair_device` is local-only.** Server-side device row stays forever. Re-pairing creates a new row each time; over time a user accumulates 20+ stale rows that hit the v0.3.0 device cap.

## 2. Goal

After v0.3.4, a signed-in user on the desktop sees:

- **Providers tab**: per-provider plan badge, monthly quota, remaining count, and tier bars (5h Window / Weekly / Sonnet only / Designs / Daily Routines for Claude Max). Local-scan cost rollup remains as a secondary detail. Empty state when no server data exists.
- **Overview tab**: server-aggregated 6-tile metrics grid (today_cost, today_usage, active_sessions, online_devices, unresolved_alerts, today_sessions) reflecting all devices on the account. Local-scan numbers stay as fallback when not signed in.
- **One CLI Pulse instance at a time.** Second-launch focuses the existing window and exits.
- **Rolling daily logs** at `%LOCALAPPDATA%\dev.clipulse.desktop\logs\cli-pulse.log` (Win), `~/Library/Logs/dev.clipulse.desktop/cli-pulse.log` (macOS), `~/.local/share/dev.clipulse.desktop/logs/cli-pulse.log` (Linux). Diagnostic snapshot includes the path so support tickets can attach the file.
- **Unpair actually unpairs.** Server-side device row gets deleted; a fresh sign-in mints a clean new row.

## 3. Decisions

**Read paths use the existing OTP refresh-token, refreshed on demand.** v0.3.0's `auth.rs::refresh()` already implements the full cycle. We add one Tauri command per dashboard RPC that:
1. Reads refresh_token from keychain.
2. POSTs to `/auth/v1/token?grant_type=refresh_token` to get a fresh access_token.
3. Persists the rotated refresh_token (Supabase rotates per call).
4. Calls the dashboard RPC with the access_token in `Authorization: Bearer`.
5. On 401 from the refresh: returns a typed `RefreshFailed` error → frontend surfaces "Your sign-in expired" and clears keychain.

We do not cache the access_token. Each user-scoped read does one refresh + one RPC call. At desktop scale (one user, hand-triggered refreshes on tab switches) this is well within Supabase Auth rate limits.

**Empty-state design: honest, not aspirational.** When `provider_summary` returns no rows (account has no Mac populating quotas yet, no local quota scraping in v0.3.4), the Providers card shows "Quota data unavailable — sign into your phone with the same email, or pair a Mac." matching the existing Mac client's pattern. Don't synthesize fake quotas.

**Single-instance: use `tauri-plugin-single-instance`.** Standard plugin. Second-launch raises the existing window via the registered handler.

**File logging: use `tauri-plugin-log` with `RotationStrategy::KeepAll` and a 5-MB max size per file.** Standard plugin. 5 MB × 7 days rotation = ~35 MB upper bound, plenty for support, low enough to not bother users.

**Server-side unpair: add a strictly-additive `unregister_desktop_helper(p_device_id, p_helper_secret)` RPC.** Anon-callable but secret-gated, mirrors `device_status` auth pattern. Deletes the device row. Best-effort from the client: if the call fails (offline, network), client still clears local state — don't trap the user.

**Out of scope for v0.3.4** (deferred to v0.3.5+):
- Cost forecast, Yield Score, Risk Signals, Top Projects on Overview (Mac computes client-side; porting the math is bigger).
- Activity timeline sparkline (Mac uses `dashboard_summary.trend`; not in our schema yet — needs a new RPC).
- Per-device breakdown UI ("Mac $5.20 + Win $1.80 = $7.00") — `get_daily_usage_by_device` exists but the UI rendering is an extra surface.
- Devices list / sign-out-all on Settings.
- Rust-side error string i18n (13 hardcoded English strings surface to UI as Tauri command rejection — fix mechanically in v0.3.5).
- Alerts-tab server-fetch (currently local-only; reading server alerts is a separate flow).
- Polish: aria-live, dev-tools-in-release decision, periodic update re-check, sessions-tab thin-column expansion, sign-in form positioning when unpaired.
- Removing the dead `_json_ref_placeholder` stub.

## 4. Server-side changes

### 4.1 New RPC: `unregister_desktop_helper`

`backend/supabase/migrate_v0.38_unregister_desktop_helper.sql`:

```sql
-- v0.38: server-side unpair for cli-pulse-desktop. Mirrors device_status's
-- helper_secret-gated auth pattern. Strictly additive — does not touch
-- existing register_desktop_helper / register_helper / device_status RPCs.

create or replace function public.unregister_desktop_helper(
  p_device_id uuid,
  p_helper_secret text
) returns jsonb language plpgsql security definer
  set search_path = pg_catalog, public, extensions
as $$
declare
  v_user_id uuid;
  v_stored_hash text;
  v_provided_hash text;
  v_remaining integer;
begin
  v_provided_hash := encode(digest(p_helper_secret, 'sha256'), 'hex');

  select user_id, helper_secret into v_user_id, v_stored_hash
    from public.devices
    where id = p_device_id;

  -- Device row missing → idempotent success (matches local-only unpair UX).
  if v_user_id is null then
    return jsonb_build_object('deleted', false, 'reason', 'not_found');
  end if;

  -- Hash mismatch → reject. Do NOT leak existence — return same shape
  -- as device_status's privacy invariant.
  if v_stored_hash is distinct from v_provided_hash then
    return jsonb_build_object('deleted', false, 'reason', 'not_found');
  end if;

  delete from public.devices where id = p_device_id;

  -- Recompute paired flag from the POST-DELETE device count for this
  -- user. Codex review flagged: blindly setting paired=false would be
  -- wrong if Laptop B is still on the account when Laptop A
  -- unregisters. The DELETE + count happen in the same RPC's
  -- transaction, so v_remaining sees the post-state.
  select count(*) into v_remaining
    from public.devices
    where user_id = v_user_id;
  update public.profiles
    set paired = (v_remaining > 0)
    where id = v_user_id;

  return jsonb_build_object('deleted', true, 'remaining_devices', v_remaining);
end;
$$;

grant execute on function public.unregister_desktop_helper(uuid, text)
  to anon, authenticated;
```

**Tests:**
- Valid (device_id, helper_secret) pair → `{deleted: true}` and the row is gone.
- Wrong helper_secret → `{deleted: false, reason: 'not_found'}` (privacy invariant).
- Unknown device_id → same `{deleted: false, reason: 'not_found'}`.
- Last device removed → `profiles.paired = false`.
- N+ device removed when others remain → `profiles.paired` unchanged.

**Migration safety:** strictly additive. Existing flows untouched. Rollback: `drop function unregister_desktop_helper`.

## 5. Tauri client changes

### 5.1 New deps in `src-tauri/Cargo.toml`

```toml
tauri-plugin-single-instance = "2"
tauri-plugin-log = { version = "2", features = ["colored"] }
```

Wire both in `lib.rs::run()`'s `tauri::Builder` chain.

### 5.2 Single-instance enforcement (`lib.rs`)

```rust
.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
    // Second launch: raise the existing main window.
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.unminimize();
    }
}))
```

The plugin uses an OS-level lock; on Windows it's a named mutex per app identifier (`dev.clipulse.desktop`), on macOS/Linux it's a Unix domain socket / named pipe. Robust across user sessions. Tray clicks on the existing instance still work normally.

### 5.3 File logging (`lib.rs`)

Replace the current `env_logger::Builder::from_env(...).init()` with `tauri-plugin-log`:

```rust
use tauri_plugin_log::{Target, TargetKind, RotationStrategy, TimezoneStrategy};

.plugin(tauri_plugin_log::Builder::default()
    .level(log::LevelFilter::Info)
    .targets([
        Target::new(TargetKind::Stdout),  // dev builds
        Target::new(TargetKind::LogDir { file_name: Some("cli-pulse".into()) }),
    ])
    .rotation_strategy(RotationStrategy::KeepAll)
    .max_file_size(5 * 1024 * 1024) // 5 MB
    .timezone_strategy(TimezoneStrategy::UseLocal)
    .build())
```

Remove the explicit `env_logger` dep. Default log path:
- Win: `%LOCALAPPDATA%\dev.clipulse.desktop\logs\cli-pulse.log`
- macOS: `~/Library/Logs/dev.clipulse.desktop/cli-pulse.log`
- Linux: `~/.local/share/dev.clipulse.desktop/logs/cli-pulse.log`

Add the path to `DiagnosticSnapshot` so it appears in the Copy-diagnostics block:

```rust
struct DiagnosticSnapshot {
    // ... existing fields ...
    log_dir: Option<String>,  // NEW
}
```

`diagText()` in `App.tsx` AboutSection: append a "Logs: <path>" line when present.

### 5.4 New Tauri command: server-side unpair

In `lib.rs`, replace the current `unpair_device` body. Codex review:
"call server, then clear local" is right ONLY for success + known
terminal server states. Transient network failures must not trap the
user as "locally signed out but server still thinks paired" — but they
also must not silently clear local when the server might actually
still own the device row.

Strategy: classify the server response into three buckets:
- **terminal-deleted** (`{deleted: true, ...}`) → clear local.
- **terminal-already-gone** (`{deleted: false, reason: 'not_found'}`) →
  clear local. The row was already removed (e.g., user pruned it via
  another desktop, or the cap-eviction logic removed it). Local should
  catch up.
- **transient** (network error, 5xx, JSON parse error) → clear local
  ANYWAY but surface a warning toast. Rationale: the user has clicked
  "Unpair" with intent to leave this device. Trapping them in a
  zombie-paired state because the network is down is worse UX than
  leaving an orphan server row that the next sign-in supersedes via
  register_desktop_helper. The orphan accumulates only if the device
  also doesn't sign in again (rare). Document in CHANGELOG.

```rust
#[tauri::command]
async fn unpair_device() -> Result<UnpairResult, String> {
    let mut server_status: UnpairServerStatus = UnpairServerStatus::Skipped;
    if let Ok(Some(cfg)) = config::load() {
        let req = supabase::UnregisterDesktopHelperRequest {
            p_device_id: &cfg.device_id,
            p_helper_secret: &cfg.helper_secret,
        };
        match supabase::unregister_desktop_helper(&req).await {
            Ok(resp) if resp.deleted => {
                log::info!("server unregister ok ({} devices remaining)", resp.remaining_devices);
                server_status = UnpairServerStatus::Deleted;
            }
            Ok(_) => {
                log::info!("server row already absent");
                server_status = UnpairServerStatus::AlreadyGone;
            }
            Err(e) => {
                log::warn!("server unregister failed (continuing local clear): {e}");
                server_status = UnpairServerStatus::Transient(format!("{e}"));
            }
        }
    }
    // Local clear runs regardless. The frontend uses server_status to
    // decide whether to show a "device row may still exist server-side"
    // toast (only the Transient branch).
    let _ = keychain::delete_refresh_token();
    config::clear().map_err(|e| e.to_string())?;
    Ok(UnpairResult { server_status })
}

#[derive(Debug, Serialize)]
struct UnpairResult {
    server_status: UnpairServerStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
enum UnpairServerStatus {
    Deleted,
    AlreadyGone,
    Transient(String),
    Skipped,  // no HelperConfig was present
}
```

Frontend (`App.tsx`'s `doUnpair`): when `server_status.kind ===
'transient'`, append a small caveat to the green-banner success
message: "Local sign-out done; server may not have received the
unpair (network issue). It'll catch up next time you sign in."

Plus typed wrapper in `supabase.rs` mirroring the `device_status` shape.

### 5.5 New Tauri commands: dashboard read paths

Three new commands in `lib.rs` that share a common refresh-and-call
helper. Codex review flagged: `auth::refresh()` returns an
`AuthSession` and does NOT persist the rotated refresh_token —
caller MUST. If the wrapper forgets, the next refresh sees a
now-invalid refresh_token (Supabase rotates per call), 401s,
and the user gets locked out of dashboards.

```rust
async fn with_user_jwt<F, Fut, T>(call: F) -> Result<T, String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, supabase::SupabaseError>>,
{
    // 1. Read refresh_token from keychain.
    let refresh_token = match keychain::read_refresh_token() {
        Ok(Some(t)) => t,
        Ok(None) => return Err("Sign in required".into()),
        Err(e) => return Err(format!("Keychain unavailable: {e:?}")),
    };

    // 2. Refresh → fresh AuthSession.
    let session = match auth::refresh(&refresh_token).await {
        Ok(s) => s,
        Err(auth::AuthError::RefreshFailed) => {
            // Refresh token revoked. Clear keychain so the UI can
            // show the sign-in form. HelperConfig stays — the
            // helper-secret data path still works for sync.
            let _ = keychain::delete_refresh_token();
            return Err("Session expired — sign in again to view dashboard.".into());
        }
        Err(e) => return Err(auth_friendly(e)),
    };

    // 3. CRITICAL — persist the rotated refresh token BEFORE the
    //    dashboard RPC. If the RPC takes time and the user closes
    //    the app mid-call, we still have the new refresh_token
    //    saved. If we don't persist, the next refresh on the next
    //    boot sees the OLD refresh_token, which Supabase already
    //    rotated, and the user is locked out.
    if let Err(e) = keychain::store_refresh_token(&session.refresh_token) {
        log::error!("failed to persist rotated refresh_token: {e:?}");
        // Don't fail the whole call — the in-memory access_token
        // still works for THIS RPC. But if subsequent refresh
        // attempts fail, the user will need to sign in again. Log
        // for support.
    }

    // 4. Call the RPC with the access_token.
    call(session.access_token).await.map_err(friendly)
}

#[tauri::command]
async fn get_dashboard_summary() -> Result<DashboardSummary, String>;

#[tauri::command]
async fn get_provider_summary() -> Result<Vec<ProviderSummaryRow>, String>;

#[tauri::command]
async fn get_daily_usage(days: Option<u32>) -> Result<Vec<DailyUsageRow>, String>;
```

Each command:
- Returns the parsed JSON shape from the existing server RPC (zero schema changes).
- Caller is responsible for falling back to local-scan data when these errors out.
- Cached for ~30s in memory at the lib.rs layer to absorb tab switches without hammering refresh.

**Cache scoping (Codex review FIX-FIRST).** The 30s cache must be
keyed by the active `user_id` and explicitly cleared at every account
transition, otherwise a sign-out → sign-in-as-different-user within
30s leaks the previous account's tile data. The cli-pulse-desktop
process is single-instance and survives account transitions, so the
cache lives across them.

```rust
struct DashboardCache {
    user_id: String,                          // anchor — cleared on transition
    dashboard: Option<(Instant, DashboardSummary)>,
    providers: Option<(Instant, Vec<ProviderSummaryRow>)>,
    daily_usage: Option<(Instant, Vec<DailyUsageRow>)>,
}

static CACHE: Lazy<Mutex<Option<DashboardCache>>> = Lazy::new(|| Mutex::new(None));

const CACHE_TTL: Duration = Duration::from_secs(30);

fn cache_invalidate() {
    if let Ok(mut g) = CACHE.lock() { *g = None; }
}
```

`cache_invalidate()` is called from:
- `auth_verify_otp` after successful sign-in (pre-populating user_id
  for the new session).
- `auth_sign_out` (existing command).
- `unpair_device` (any branch — Deleted / AlreadyGone / Transient /
  Skipped).
- `with_user_jwt` when `auth::refresh` returns `RefreshFailed`
  (covers expired-token cases).
- The helper_sync error classifier when it transitions to
  `device_missing` / `account_missing` (existing v0.3.0 path).

Each `get_*` command, on a hit:
- Verifies `cache.user_id == current_helper_config.user_id`. Mismatch
  → invalidate + miss.
- Verifies `Instant::now().duration_since(stored_at) < CACHE_TTL`.

Belt + suspenders: even though every transition explicitly clears,
the user_id check guards against any path we forgot.

Server RPC return shapes already match what iOS / Android consume:

```rust
// dashboard_summary
struct DashboardSummary {
    today_usage: i64,
    today_cost: f64,
    active_sessions: i64,
    online_devices: i64,
    unresolved_alerts: i64,
    today_sessions: i64,
}

// provider_summary — array of:
struct ProviderSummaryRow {
    provider: String,
    today_usage: i64,
    total_usage: i64,             // 7-day rolling
    estimated_cost: f64,          // 7-day cost
    estimated_cost_today: f64,
    estimated_cost_30_day: f64,
    remaining: Option<i64>,
    quota: Option<i64>,
    plan_type: Option<String>,
    reset_time: Option<String>,
    tiers: Vec<TierRow>,
}

struct TierRow {
    name: String,
    quota: i64,
    remaining: i64,
    reset_time: Option<String>,
}
```

Both shapes verified against actual Supabase output during v0.3.1 verification (sample row: `{"plan_type":"Max 20x","remaining":80,"tiers":[{"name":"5h Window","quota":100,"remaining":80,"reset_time":"2026-05-01T08:20:00Z"}, ...]}`).

### 5.6 React UI changes

**`Providers` component (`App.tsx:540-655`)**: 

- On mount and on `paired` flip → invoke `get_provider_summary`. Cache for 30s.
- Merge server rows by provider name into the existing `grouped` Map. When server data exists for a provider, render the new card sections:
  - **Plan badge** next to provider name (small pill: "Max 20x" / "Pro" / "Free").
  - **Tier bars** (Mac-style): one `<UsageBar>` row per `tier`, label = name, value = `(quota - remaining) / quota`, color = tier-usage gradient (green → yellow → red), detail = "X / Y left, resets <relative-time>".
  - When no `tiers` array but `quota > 0` and provider != "Claude": render a single overall quota bar.
  - When `provider_summary` returned no row OR returned no quota fields for this provider: render the existing local-scan card unchanged (current behavior).
- Local-scan model breakdown table stays intact below the new sections — that's per-model detail Mac doesn't have.

**`Overview` component (around the today/30-day summary)**:

- On mount and on `paired` flip → invoke `get_dashboard_summary`. Cache for 30s.
- When server data is present: render a 6-tile metrics grid (today_cost, today_usage, active_sessions, online_devices, unresolved_alerts, today_sessions) above the existing local-scan section. Use `formatUSD` / `formatInt` for values.
- Existing local-scan numbers (today cost, 7-day chart, 30-day est) keep their place. Server tiles supplement — they don't replace.
- When not paired or `get_dashboard_summary` errors: don't render the tiles. No "loading…" infinite spinner.

**Locale strings (en/zh-CN/ja)**: new keys
- `providers.plan_*` (badge labels)
- `providers.tier_left` ("{{remaining}}/{{quota}} left, resets {{when}}")
- `providers.quota_unavailable` ("Quota data unavailable — sign in on your phone or pair a Mac.")
- `overview.tile_today_cost` / `tile_today_usage` / `tile_active_sessions` / `tile_online_devices` / `tile_unresolved_alerts` / `tile_today_sessions`

Gemini translation review on the new keys before commit.

### 5.7 Tests

Rust:
- Existing 47 lib + 12 integration tests pass unchanged.
- New typed-wrapper tests for `unregister_desktop_helper` (mocked rpc).
- New typed-wrapper tests for `get_dashboard_summary` / `get_provider_summary` / `get_daily_usage` JSON shape parsing (canned response strings).

Frontend:
- Existing 25 vitest tests pass.
- Add a smoke test that verifies the Providers card renders the tier bars when a `provider_summary` response is provided via mock.

E2E (Win VM Claude):
- Re-test #A.1 from the audit: sign in, navigate to Providers tab, verify plan badge + tier bars render with the user's actual Max 20x tiers.
- Re-test #A.2: navigate to Overview, verify the 6 server-aggregated tiles render and reflect the cross-device totals.
- New: launch CLI Pulse twice → second instance focuses the existing window instead of opening a new one.
- New: confirm `%LOCALAPPDATA%\dev.clipulse.desktop\logs\cli-pulse.log` exists and is non-empty after first sync.
- New: unpair and verify the device row is gone from `public.devices` server-side.

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| `tauri-plugin-single-instance` clashes with the existing tray icon's "show window" handler | Plugin's handler is invoked from the second-launch process; tray click is in-process. No shared state. Verified by Tauri docs / common pattern. |
| File logging path differs across OS — diagnostic copy-paste leaks user's home path | The plugin uses the OS log dir convention (`Library/Logs` etc.); `<home>` doesn't appear in the *path* itself, only inside the file content. Sentry scrubber's `$HOME → <home>` rewrite (already in `sentry_init.rs`) covers in-content leakage. |
| Refresh-token churn from frequent tab switches | 30s in-memory cache on each get_* command. Tab switch within 30s is a no-op refetch. Refresh rate at steady state: 4 RPC calls per 30s window per active session — well under Supabase Auth's per-IP limit (60/hr default, configurable). |
| `unregister_desktop_helper` race when device sync's mid-call | Postgres row-level lock on the DELETE serializes with any concurrent UPDATE on the same row. helper_sync's update would see the row gone and return "Device not found or unauthorized" — which the helper_sync error classifier (v0.3.0) already handles by clearing local pairing. Net: race is benign. |
| Multi-device clobber on the v0.3.4 cap (20 devices) when users repeatedly install / uninstall | Server-side unregister now actually removes rows on unpair, so the count stays accurate. v0.3.5 device-management UI will let users prune any historical orphans from before v0.3.4. |
| Server-side dashboard RPCs return more fields than we render today | Forwards-compat: serde with `#[serde(default)]` on optional fields. New fields ignored by older clients. |
| Plan badge color for "Free" / "Pro" / "Max 20x" / "Custom" not in Mac convention | Mac uses `StatusBadge(text: plan, color: plan == "Paid" ? .green : .orange)` — we mirror, mapping `Free → orange`, anything else → green. Per-provider quirks (Claude uses "Max 20x") render verbatim. |
| Cost-section regression when server data is present and local-scan is stale | Both numbers can show; server tile is labeled "All devices today", local card is labeled "This device". User sees both, no ambiguity. |

## 7. Milestones (2-2.5 working days)

| Day | Work |
|---|---|
| 1 (server) | `unregister_desktop_helper` migration + 5 tests + deploy to staging. Smoke-verify against the cli-pulse Supabase project. |
| 1 (Tauri) | New deps (single-instance + log) wired in `lib.rs`. Replace `env_logger`. Verify file logging works on Mac dev build first, then push to CI for Win/Linux. Single-instance smoke on macOS (open twice, second focuses first). |
| 1.5 | New Rust commands: `with_user_jwt` helper + `get_dashboard_summary` + `get_provider_summary` + `get_daily_usage` typed wrappers in `supabase.rs`. New `unregister_desktop_helper` wrapper. Replace `unpair_device` body. Cargo test green. |
| 2 | React: Providers tier-bar rendering + Overview tile grid. Locale strings (en + zh-CN + ja). Gemini translation review pass. Diagnostic snapshot includes log_dir. Vitest green. |
| 2 | E2E hand-off to VM Claude: sign in, verify Providers shows real tiers, Overview shows real cross-device totals, second launch focuses existing, log file populated, unpair removes server row. |
| 2.5 | If E2E green: bump version, CHANGELOG, ship as Pre-release. Promote to Latest after VM Claude confirms. |

## 8. Backward compatibility

- v0.3.3 / earlier desktops on auto-update: unaffected. `unregister_desktop_helper` is strictly additive; old desktops never call it. Their local-only unpair continues to leave server rows.
- iOS / Android / Mac: zero change. The new RPC is desktop-targeted (helper-credential auth), and the existing read RPCs they consume (`provider_summary` / `dashboard_summary` / `get_daily_usage`) are unchanged.
- Existing v0.3.0+ desktop users on the `dev.clipulse.desktop` app identifier: the plugin lock works at the OS level — no schema change needed. First user to upgrade hits a fresh lock; subsequent launches are no-ops.

## 9. Out of scope (deferred)

Explicitly punted to v0.3.5 or later. Filed here so they don't get re-scoped mid-sprint:

- **Cost forecast / Yield Score / Risk Signals / Top Projects** on Overview (Mac client-side math).
- **Activity timeline sparkline** (needs a `dashboard_summary.trend` field — not in the current RPC).
- **Per-device breakdown UI** (`get_daily_usage_by_device` exists, render is small but separate).
- **Devices list / sign-out-all** in Settings.
- **Rust error string i18n** (13 hardcoded English strings in Tauri command rejections; mechanical fix).
- **Alerts tab server-fetch** (currently local-only; reading server alerts is a parallel refresh path).
- **Polish**: aria-live on banner, devtools-in-release decision, periodic update re-check, sessions thin-column expansion, sign-in form positioning when unpaired.
- **Dead code**: `_json_ref_placeholder` cleanup.

## 10. Decisions to close before sprint start

1. **Log retention size cap.** Going with 5 MB × KeepAll = unbounded directory growth. Better: cap at 5 files (25 MB) with `RotationStrategy::KeepN(5)`. Confirm before implementation.
2. **Refresh-token reuse window.** Going with 30s in-memory cache per dashboard command. If a user opens Overview, then Providers within the cache window, only one refresh fires. Lengthen to 60s if rate-limit data warrants it (close on Day 1 by checking actual call patterns).
3. **What happens when `get_provider_summary` returns rows for providers that local-scan didn't see?** (e.g., Mac uploaded "Codex" quota but Win has no Codex JSONL.) Going with: render the server card with quota only, no local-scan section underneath. Mac UI does the same.
4. **Plan-badge color mapping for non-standard plans** ("Custom", "Trial", "Enterprise"): green by default, only "Free" / "free" → orange. Match Mac's `plan == "Paid" ? .green : .orange` collapsed to "any plan != Free → green".

## 11. Review history

- **VM Claude (Win VM E2E broader audit, 2026-05-02)** — surfaced the
  parity gap (zero call sites for the user-scoped read RPCs) +
  no-on-disk-logs FAIL + 13 hardcoded English error strings in
  Tauri command rejections + multiple smaller polish findings. This
  spec addresses §A.1 / §A.2 / §A.4 / §A.8 / §B(logs) and the
  user-flagged single-instance gap. Smaller findings deferred to
  v0.3.5 per §9.
- **Codex GPT-5.4 (SQL/security/correctness, 2026-05-02)** — reviewed
  this spec and surfaced four FIX-FIRSTs. All resolved before
  execution:
  1. `unregister_desktop_helper` blindly setting
     `profiles.paired = false` would race multi-device accounts.
     Fixed: now `paired = (count post-DELETE > 0)`, recomputed in
     the same RPC transaction (§4.1).
  2. The unpair flow's "call server then clear local" was unsafe on
     transient network errors (would either trap user or leave
     ambiguous state). Fixed: classifies server response into
     Deleted / AlreadyGone / Transient / Skipped buckets; local
     clear runs in all four with a UI-surfaceable warning toast on
     Transient (§5.4).
  3. `with_user_jwt` would forget to persist the rotated
     refresh_token, locking the user out within 24hrs. Fixed:
     wrapper persists BEFORE the dashboard RPC call, with explicit
     log-on-failure behavior (§5.5).
  4. 30s in-memory cache could leak across sign-out / sign-in
     boundaries since the process survives them (single-instance
     amplifies this). Fixed: cache anchored by user_id + cleared
     explicitly at every auth transition + belt-and-suspenders
     user_id mismatch check on read (§5.5).

  NIT (won't block ship): single-instance second-launch handler
  should also trigger a lightweight auth refresh in case the user
  launched again specifically to pick up auth changes. Filed as a
  v0.3.5 polish item.

## 12. References

- Audit: VM Claude broader audit, 2026-05-02.
- Mac Providers UI: `cli pulse/CLI Pulse Bar/CLI Pulse Bar/ProvidersTab.swift:129+` (`EnhancedProviderCard`).
- iOS Providers UI: `cli pulse/CLI Pulse Bar/CLI Pulse Bar iOS/iOSProvidersTab.swift:152+` (`iOSEnhancedProviderCard`).
- Mac Overview: `cli pulse/CLI Pulse Bar/CLI Pulse Bar/OverviewTab.swift:46-71` (metricsGrid + provider breakdown).
- Server RPCs: `cli pulse/backend/supabase/app_rpc.sql:11+` (dashboard_summary), `app_rpc.sql:62+` (provider_summary), `schema.sql:474+` (get_daily_usage), `migrate_v0.37_daily_usage_device_id.sql` (get_daily_usage_by_device).
- v0.3.0 OTP infrastructure: `cli-pulse-desktop/src-tauri/src/auth.rs` (refresh function), `keychain.rs` (refresh_token storage).
- Single-instance plugin: https://docs.rs/tauri-plugin-single-instance
- Log plugin: https://docs.rs/tauri-plugin-log
- v0.3.0 spec: `PROJECT_DEV_PLAN_2026-05-02_v0.3.0_otp_login.md`.
- v0.3.1 spec: `PROJECT_DEV_PLAN_2026-05-03_v0.3.1_multi_device_daily_usage.md`.
