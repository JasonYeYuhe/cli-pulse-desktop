# Dev Plan — v0.5.4 + v0.5.5 + v0.5.6 (post-parity polish trio)

**Date:** 2026-05-05 (late JST, rolls into 2026-05-06)
**Replaces / supersedes:** None (new sprint after v0.5 wave closed)
**Reviewers (requested):** Codex + Gemini 3.1 Pro
**Trigger:** v0.5.3 VM verify came back PASS on B/C/D, "PARTIAL-by-proxy" on A (banner-click fix is in v0.5.3 code but only live-testable from inside v0.5.3 once a v0.5.4+ ships). User explicitly approved three small post-parity ships: **A** = tray popover with mini-metrics, **B** = Activity Timeline chart on Sessions tab, **C** = Settings → Danger Zone (delete account + clear cache).

## Context

v0.5 wave closed cleanly: cost forecast / risk signals / top projects / auto-updater click / time-unit L10n all verified. The Mac-Overview parity surface is functionally complete. What's left, ranked by autonomy memory's WATCHING-mode posture:

- Real-user signals: **none** (Sentry 14 d = 0 events, ~9 unique page-views, 1 star, 0 forks)
- Mac-sibling parity gaps still applicable:
  - Tray popover with rich content (Mac has `MenuBarView.swift`; desktop tray has only context-menu actions since v0.2.x)
  - Activity timeline visualization (Mac has `ActivityTimelineChart.swift`)
  - Danger Zone — delete-account + clear-cache (Mac has `DangerZoneSection.swift`)

User directed all three. Concession that this is closer to "make-work" than "user-pain-driven" (per autonomy memory's WATCHING-mode comment "react to actual signals, don't fabricate work") — but the user's autonomy contract makes the call. We ship.

## Sequencing rationale

Three contained ships, easiest-first so each enables the next's testing pattern:

1. **v0.5.4 — Danger Zone** (1–2 h): smallest scope, all backend pieces already exist. **Bonus**: ships first → opens a live-test window for the v0.5.3 auto-updater banner click bug fix that VM Block A couldn't verify in-place.
2. **v0.5.5 — Activity Timeline** (2–3 h): pure-frontend SVG, no new deps, no platform fragmentation.
3. **v0.5.6 — Tray popover** (4–6 h): biggest scope, highest cross-platform risk (Linux AppIndicator vs Windows shell tray API differ). Do last when context is freshest.

VM verify cadence: one consolidated verify after v0.5.6 lands rather than three separate ones. Each ship's pre-push gates + Gemini review hold the per-ship quality bar; VM batches them for the user-visible E2E pass.

## v0.5.4 — Settings → Danger Zone

### Backend additions

`delete_user_account` RPC already exists server-side (verified via Supabase MCP RPC list 2026-05-05). It deletes the user's row from `auth.users` cascading to `alerts`, `sessions`, `daily_usage_metrics`, etc. The RPC returns `{success: true}` on completion.

Two new Tauri commands:

```rust
#[tauri::command]
async fn delete_account_and_unpair() -> Result<(), String> {
    // 1. Call delete_user_account RPC (server-side row deletion)
    // 2. Clear OS keychain (refresh + provider creds)
    // 3. Clear app config file (config.json)
    // 4. Clear local caches (scan + daily_usage + provider_summary +
    //    collector_status + risk-signals + cost-forecast — anything
    //    user-data-flavored)
    // 5. Return Ok — frontend reloads to unpaired state
}

#[tauri::command]
async fn clear_local_caches() -> Result<(), String> {
    // Same cache invalidation as #4 above, BUT does NOT touch
    // keychain or config. User stays signed in; next sync re-fetches.
}
```

The cache layer in `lib.rs` already has `cache_invalidate()` — we extend that to a per-cache `invalidate_*` set, then have `clear_local_caches` call all of them. Probably touches: scan cache, daily_usage cache, provider_summary cache, collector_status cache, top_projects cache (if any), forecast cache (if any).

### Frontend

New section at the bottom of `Settings`, BELOW Integrations (matches the visual "danger" reading from being last + visually-isolated):

```
┌─ Danger Zone ──────────────────────────────────┐
│                                                 │
│  Clear local caches                            │
│  Wipes scan + provider summary + forecast      │
│  caches. You stay signed in; next sync         │
│  re-fetches everything.                        │
│  [ Clear caches ]                              │
│                                                 │
│  Delete cloud account                          │
│  Permanently deletes your account, all         │
│  paired devices, and all server-side data.     │
│  This cannot be undone.                        │
│  [ Delete account ]                            │
│                                                 │
└─────────────────────────────────────────────────┘
```

Both buttons trigger a confirmation dialog:
- Clear caches: simple "Are you sure? Cancel / Clear" dialog (reversible by next sync)
- Delete account: type-to-confirm. User must type the literal string `DELETE` (or localized: `删除` / `削除`) to enable the destructive button. Hard gate against accident.

The Danger Zone section itself: red-tinted border (`border-red-900/40`) + small "⚠ Danger Zone" header. Visual distinct from the rest of Settings.

### i18n keys (3 langs, ~14 new keys)

```
settings.danger_heading: "Danger Zone" / "危险操作" / "危険な操作"
settings.danger_clear_caches_title: "Clear local caches"
settings.danger_clear_caches_body: "Wipes scan + provider summary + forecast caches. You stay signed in; next sync re-fetches everything."
settings.danger_clear_caches_button: "Clear caches"
settings.danger_clear_caches_confirm: "Clear all local caches? This is reversible — next sync re-fetches data."
settings.danger_delete_account_title: "Delete cloud account"
settings.danger_delete_account_body: "Permanently deletes your account, all paired devices, and all server-side data. This cannot be undone."
settings.danger_delete_account_button: "Delete account"
settings.danger_delete_account_confirm: "Type {{phrase}} to confirm:"
settings.danger_delete_account_phrase: "DELETE" / "删除" / "削除"
settings.danger_delete_account_processing: "Deleting…"
settings.danger_delete_account_done: "Account deleted. Reloading…"
settings.danger_caches_cleared: "Caches cleared."
settings.danger_action_failed: "Operation failed: {{err}}"
```

### Tests

- Backend: 2 new tests covering the cache invalidation set + the keychain-clear path. (No live-call test against Supabase — the RPC call is mocked.)
- Frontend: 3 new vitest cases covering type-to-confirm gate state machine.

Total: 5 new tests, ~189 backend / ~57 frontend.

### Risks

- **Race during account-delete:** if the RPC succeeds server-side but the keychain-clear locally fails (e.g. Linux libsecret unavailable), user is in an inconsistent state — local app thinks they're paired but server has no row. Mitigation: clear keychain BEFORE calling the RPC; if RPC fails, user can re-pair to recover. Trade-off: brief window where keychain is gone but server row exists. Acceptable — rare and recoverable.
- **`delete_user_account` RPC behavior on devices that are still online:** other devices (Mac, phone) will start failing their next sync with 401/404. They should detect via the existing `device_status` probe (per `feedback_desktop_autonomy.md`). No desktop-side action needed.

## v0.5.5 — Activity Timeline chart on Sessions tab

### Frontend (no backend changes)

Sessions tab today renders a flat list of active sessions: one row per session with provider / project / status / metrics. v0.5.5 adds a horizontal SVG timeline ABOVE the list, visualizing the last 24 h of session activity.

Algorithm:
- Read sessions from existing `list_sessions` Tauri command (already paginated by 10 s polling per `feedback_desktop_autonomy.md`).
- For each session: compute `[started_at, last_active_at]` interval.
- Bucket into a horizontal SVG: x-axis = last 24 h, y-axis = one row per provider (Claude / Codex / Gemini / Cursor / Copilot / OpenRouter).
- Color each session bar by provider. Width proportional to session duration.
- Hover any bar → tooltip with `project · started Xh ago · Y messages · Z tokens`.

```tsx
function ActivityTimelineChart({ sessions }: { sessions: SessionRow[] }) {
  // SVG, no chart library. Reuses existing useMemo() pattern.
  // Layout: 240 px tall × 100% wide. 6 horizontal lanes (one per provider).
  // X-axis: 24 h with hourly tick marks every 4 h.
  // Y-axis: provider lanes with provider logo + label.
}
```

### Edge cases

- Sessions older than 24 h (anything outside the visible window): clip + render a small `+N` overflow indicator at the left edge.
- Provider with zero sessions: lane still rendered (greyed-out) so the layout doesn't shift between time-windows.
- DST / timezone shift mid-window: use UTC internally, render local time on x-axis ticks.

### Tests

- Vitest snapshot of the chart with mocked sessions.
- Vitest interaction test: hover a session bar → tooltip appears with correct content.

Total: 2 new tests.

### Risks

- **SVG re-render cost on 10 s polling:** Sessions tab refreshes every 10 s. If the chart re-renders the entire SVG every poll, on machines with 50+ sessions over 24 h the DOM diff could feel sluggish. Mitigation: memoize the bar-layout computation in `useMemo` keyed on `sessions.length + sessions[0]?.last_active_at`; only recompute when a session genuinely changed.

## v0.5.6 — Tray popover with mini-metrics

### Cross-platform reality (P1 risk)

Tauri 2's `tray-icon` plugin is well-tested on Windows; Linux support is **AppIndicator-based** which is **menu-only** by design — clicking the indicator shows a menu, not an arbitrary window. For a "rich popover" Linux is significantly harder than Windows.

**Two design options:**

**(α) Rich popover (frameless window).** Click tray → spawn an undecorated Tauri window near the cursor with a mini React UI showing today's cost / sync status / open-app. Works cleanly on Windows. **On Linux, requires hacks**: capture cursor position via `gdk_display_get_default()` (X11 only — broken on Wayland), position window manually, listen for blur to hide. Cross-platform fragmentation is the cost.

**(β) Enhanced tray menu (no window).** Same content, but rendered as menu items. Tauri's `MenuItem` supports `text` + `accelerator` + `enabled` + nested submenus. Show "Today: $X.XX" / "Active sessions: N" / "Synced 12s ago" / "Refresh now" / "Open" / "Quit" as menu rows. **Works everywhere** without platform forks. Less visual richness — text-only.

**Recommendation: ship (β) as v0.5.6, defer (α) until there's user signal asking for richer.** Reasoning:
1. The autonomy memory's WATCHING mode says "react to real signals" — no signal demands a rich popover specifically.
2. (β) gets the same data into the tray surface with zero platform fragmentation.
3. Tauri's `MenuItem::set_text()` supports live updates → we update menu text from a 30 s background task. No popover lifecycle to manage.

If user pushes back wanting (α): split into v0.5.6 (β menu) + v0.5.7 (α popover Windows-first).

### Implementation outline (β path)

`src-tauri/src/tray.rs` already exists (since v0.2.x). Extend it:

```rust
struct TrayMenuState {
    today_cost: f64,
    active_sessions: u32,
    synced_seconds_ago: u32,
}

fn build_dynamic_menu(state: &TrayMenuState) -> Menu {
    Menu::new()
        .item(&MenuItem::new("CLI Pulse").enabled(false))
        .separator()
        .item(&MenuItem::new(&format!("Today: {}", format_usd(state.today_cost))))
        .item(&MenuItem::new(&format!("{} active sessions", state.active_sessions)))
        .item(&MenuItem::new(&format!("Synced {} ago", format_relative_short(state.synced_seconds_ago))))
        .separator()
        .item(&MenuItem::new("Refresh now").id("refresh"))
        .item(&MenuItem::new("Open").id("open"))
        .item(&MenuItem::new("Quit").id("quit"))
        .build()
}

// Background task — every 30 s, re-fetch the data + rebuild the menu:
async fn tray_menu_refresh_loop(stop: Arc<AtomicBool>) {
    loop {
        let state = collect_tray_state().await;
        if let Some(tray) = TRAY_HANDLE.get() {
            tray.set_menu(Some(build_dynamic_menu(&state)));
        }
        wait_for_next_tick(&mut rx, Duration::from_secs(30), &stop).await;
    }
}
```

Reuses the v0.4.23 `wait_for_next_tick` helper for stop-responsive sleep — same pattern as the main background sync loop.

### Wire-up points

- `today_cost`: from `get_dashboard_summary().today_cost`
- `active_sessions`: from `get_dashboard_summary().active_sessions`
- `synced_seconds_ago`: time since `LAST_OUTCOMES`'s most recent successful timestamp

### i18n

Tray menu text in the user's selected app language. Read from `i18n` runtime via a JS→Rust bridge? OR read from `config::load().language` field. The desktop already has language-switcher in Settings; whichever path it persists is what tray reads.

### Tests

- Backend: 1 unit test on `build_dynamic_menu()` — given a known `TrayMenuState`, assert the formatted strings come out right.
- No frontend tests (tray UI is native, not React).

Total: 1 new test.

### Risks

- **Linux AppIndicator vs StatusNotifierItem:** distros vary. If `libayatana-appindicator3` isn't installed (already a desktop runtime requirement per `reference_desktop_repo.md`), tray doesn't render. v0.5.6 doesn't change this — same baseline.
- **Tray menu refresh contention:** if the 30 s tray-refresh and the main 120 s background sync overlap their `dashboard_summary` calls, we double the network hit. Mitigation: tray reads from cache (existing `cache_get_dashboard_summary`) rather than triggering a fresh fetch. The 30 s tray cycle becomes a "render the cached value" loop, only doing real work when cache is stale.

## Out of scope (this trio)

- **Tray popover (α path):** deferred per recommendation above. Re-cut as v0.5.7 if user signal demands.
- **Yield score, PDF export, demo mode, subscription/team UI, remote control:** still on the autonomy-memory deferred list. Wait for user signal.
- **Onboarding wizard:** Codex's v0.5.3+0.5.4 plan-review explicit defer stands.
- **Memory updates:** none in this trio (last set went out with v0.5.3).

## Tests / metrics target

- v0.5.4: 234 → ~239 (180 backend, +2 cache invalidation; 54 frontend, +3 type-to-confirm).
- v0.5.5: 239 → ~241 (180 backend, +0; 56 frontend, +2 chart snapshots/interactions).
- v0.5.6: 241 → ~242 (181 backend, +1 menu builder; 56 frontend, +0).

## Risks (cross-cutting)

1. **Three ships in one session is a lot.** Today already has 8. Context exhaustion is the primary failure mode. Mitigation: each ship gets the full pre-push gates + Gemini review pass (no skipping); if Gemini catches any P1 in v0.5.4 or v0.5.5, FIX before moving to next ship.
2. **VM verify backlog.** No verify between v0.5.4/5/6 means a regression in v0.5.4 only surfaces during the consolidated v0.5.6 verify — by which point three commits have layered on top. Mitigation: keep each ship's diff narrow + reviewable; verify report is granular enough to attribute bugs back to each version.
3. **Tray UX variance Linux vs Windows:** v0.5.6 (β menu path) avoids most of this but Linux AppIndicator's menu-redraw latency can be sluggish on KDE Plasma 6. Acceptable for v0.5.6; can revisit if a Linux user reports it.

## Review questions for Codex

1. **`delete_user_account` RPC ordering:** clear-keychain-then-call-RPC vs call-RPC-then-clear-keychain — which is the safer ordering against partial-failure recovery? The user-data-loss case is the same either way (RPC succeeds, RPC fails — the keychain state is the same as before either side runs). The cross-device-sync corruption case differs: if RPC fires first and keychain-clear fails, this device retains its session keys but server says the user is gone — next sync 401s and triggers `device_status` cleanup automatically. Vs keychain-first: device is unsigned-in even if RPC errors out (recoverable by re-OTP). Recommend?

2. **Tray-state cache reuse:** the v0.5.6 tray-menu refresh wants `today_cost` / `active_sessions` / `synced_seconds_ago` every 30 s. The main app's Overview refreshes the same data every 30 s on its own poll (per v0.4.22). Should they share a `OnceLock<RwLock<DashboardCache>>` rather than each fetching independently? Code complexity vs network savings.

3. **SVG vs Canvas for v0.5.5 timeline:** for ≤ ~50 session bars over 24 h, SVG with React is fine. For 200+, Canvas + manual rendering wins. Should v0.5.5 just default to SVG and let some future high-volume user trigger the rewrite, or bake in a 50-bar threshold up front?

## Review questions for Gemini 3.1 Pro

1. **Type-to-confirm UX for delete account:** my plan asks the user to literally type `DELETE` (or `删除` / `削除`) to enable the destructive button. Is that overkill for desktop apps where the user is already signed in? Some platforms (Discord, Vercel) require typing the username; some (Slack, Notion) just have a 5 s timer + checkbox. What pattern matches user trust posture for a CLI / dev-tool audience?

2. **Activity Timeline aesthetic — bar height:** 240 px tall canvas split across 6 provider lanes = 40 px per lane. With 1–3 sessions per provider over 24 h, a 40 px bar might look cartoonish. Should I drop to 24 px lanes (smaller, cleaner) or keep 40 px for hover-target affordance (mobile-style 44 px target heuristic)?

3. **Tray menu — show today's cost OR predicted month-end?** Showing `Today: $0.50` is very small-number for most users. Showing `Month so far: $X / forecast $Y` is more useful but doubles menu items. Pick one.

4. **Cross-cutting at P1/P2:** be aggressive on plan oversights — three ships in a row after a long session today is exactly the moment for a fresh-eyes catch.

## Files this plan would touch

**v0.5.4:**
- `src-tauri/src/lib.rs` (new commands `delete_account_and_unpair` + `clear_local_caches`)
- `src-tauri/src/cache.rs` or wherever the existing `cache_invalidate` lives (extend per-cache invalidation set)
- `src-tauri/src/keychain.rs` (no change — `delete_refresh_token` exists)
- `src-tauri/src/supabase.rs` (one new wrapper `delete_user_account()` if not already present)
- `src/App.tsx` (new `DangerZoneSection` component + Settings invocation)
- `src/locales/{en,zh-CN,ja}.json` (~14 new keys × 3)
- `src/i18n.test.ts` (critical-labels list)
- 3 manifests + 2 lock files

**v0.5.5:**
- `src/components/ActivityTimelineChart.tsx` (NEW, ~150 lines)
- `src/App.tsx` (Sessions tab integration)
- `src/locales/*` (~3 i18n keys for axis labels / hover tooltip)
- 3 manifests + 2 lock files

**v0.5.6:**
- `src-tauri/src/tray.rs` (extend with dynamic menu + refresh loop)
- `src-tauri/src/lib.rs` (wire tray refresh task spawn)
- `src/locales/*` (~6 i18n keys for menu items)
- 3 manifests + 2 lock files

— end of plan —
