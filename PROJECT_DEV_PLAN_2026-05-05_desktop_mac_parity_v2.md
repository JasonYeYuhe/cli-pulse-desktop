# Dev Plan v2 — desktop ↔ Mac parity (post-review revision)

**Date:** 2026-05-05
**Replaces / supersedes:** `PROJECT_DEV_PLAN_2026-05-05_desktop_mac_parity.md` (v1)
**Reviewers (consulted):** Codex + Gemini 3.1 Pro (both reviewed v1; convergent findings — see "Review changelog" at the end)
**Status:** ACTIONABLE. Replaces v1 wherever they conflict.

## What changed from v1

| v1 plan claim | Reality (per Codex + Gemini cross-check) | v2 disposition |
|---|---|---|
| `risk_signals` already in `dashboard_summary`, no new Rust code | Desktop `supabase::DashboardSummary` struct lacks the field. Mac uses `[String]` shape, not `{kind,severity,title,detail}`. | **Need backend struct extension + verify server-side payload first.** |
| `yield_score` is `output_tokens / cost_usd` | Mac reads `yield_score_daily` rows with weighted/raw/ambiguous commit counts; depends on `gitTrackingEnabled` setting + `track_git_activity` user setting. | **Need new RPC read OR full feature deferred** until git-attribution infra is on desktop. Strong push to defer. |
| `top_projects` is a scan-aggregator (local data) | Desktop `DailyEntry` has NO `project` field (`scanner.rs:38`). Mac reads `DashboardSummary.top_projects` server-side. | **Server-side, not local. SQL `GROUP BY` per Gemini.** |
| Forecast: returns `nil` when no data; `< 3` falls back to simple-average | Swift always builds points for `1..dayOfMonth`, treats missing days as zero, returns an unreliable zero forecast. `< 3` still runs regression — only `isReliable` flag flips. | **Description corrected. Algorithm port unchanged but match the actual semantics.** |
| Wave 1 = single bundled v0.5.0 ship | Both reviewers: split. Codex says "split data work from layout"; Gemini says "v0.5.0 backend, v0.5.1 frontend." | **Adopted. Wave 1 splits into v0.5.0 + v0.5.1.** |
| Wave 2 = polish + onboarding (4 ships) | Both reviewers: cut. Codex: "polish pretending to be parity, burns risk budget." Gemini: "feature creep risk." | **Wave 2 cut except onboarding (compressed to 3 steps).** |
| `printpdf = "0.7"` | Stale; current 0.9.1, default features pull HTML/fontconfig. | **Wave 2 cut, so PDF deferred entirely.** |
| Tauri `dialog` plugin already a dep | Not in `Cargo.toml`. | Moot (PDF cut). |
| `tauri.conf.json` minWidth 880 | Actually 840. | **Corrected.** |
| Onboarding 5 steps (matching Mac) | Mac's iter13 fix-comment shows the 5-step trap was internally regretted. Step 4 also renamed "All set" not "Pair Device" (`OnboardingWizardView.swift:450`). | **Compress to 3 steps: Welcome / Privacy / Sign-in+Pair.** |
| `risk_signals` differentiated by color only | Color-only fails WCAG (color-blind users). | **Add lucide icon vocabulary: `Info` / `AlertTriangle` / `AlertCircle`.** |
| `top_projects` aggregation in Rust iterators | Could pull tens of thousands of scan rows into memory. | **SQL `GROUP BY` only. Aggregate at the database/RPC level.** |

## Revised inventory (corrected)

| # | Mac feature | Desktop status (CORRECTED) | v2 disposition |
|---|---|---|---|
| 1 | `CostForecastEngine` | MISSING. Local-only port truly viable — operates on existing `daily_usage` data. | **v0.5.0** (Wave 1 backend) |
| 2 | `top_projects` (server-aggregated) | MISSING. **Not** a local scan port. Comes from server `DashboardSummary.top_projects`. | **v0.5.0** if server payload includes it; otherwise defer until backend extension. **Verify server first.** |
| 3 | `risk_signals` (`[String]`) | MISSING. Server-side; current desktop Rust struct doesn't deserialize the field. | **v0.5.0** — extend struct, verify payload, render array of strings (not objects). |
| 4 | `YieldScore` (commit-count-based) | MISSING. Depends on git-attribution infra desktop doesn't have. | **DEFER to v0.6+** — explicit out-of-scope until git tracking ships on desktop. |
| 5 | Overview tab restructure (2-column) | PARTIAL | **v0.5.1** — frontend only, separate ship. |
| 6 | `OnboardingWizardView` | MISSING | **v0.5.2** — compressed to 3 steps. |
| 7–22 | (PDF, timeline, demo, sub, remote) | various | **CUT from this sprint.** Track for v0.6+ if real-user signal demands. |

## Revised roadmap

### v0.5.0 — Backend foundation (Wave 1a)

| Item | Scope |
|---|---|
| Sign-off pre-flight | Dump current `dashboard_summary` JSON via Supabase MCP / paired-account JWT. Confirm shape of `risk_signals`, presence/absence of `top_projects`, `yield_score`, `cost_forecast`. Lock the Rust struct against this real payload. |
| `quota::cost_forecast` Rust module | Linear-regression port of `CostForecastEngine.swift`. Operates on `Vec<DailyUsageRow>` returned by existing `supabase::get_daily_usage()`. |
| `get_cost_forecast` Tauri command | Async, off main thread. Accepts no args (uses today as reference). Returns `Option<CostForecast>`. |
| Extend `supabase::DashboardSummary` struct | Add `risk_signals: Option<Vec<String>>`, `top_projects: Option<Vec<TopProject>>` based on what the real payload returns. `serde(default)` on each so older payloads don't 500 the deserializer. |
| Tests | 6 forecast tests (port + parity-against-Swift fixtures), 2 dashboard struct tests (deserialize sample JSON, missing-field defaults). |
| **No UI changes.** | Frontend ignores the new fields — that's v0.5.1. |

**Test count target:** 167 → ~175 backend.

### v0.5.1 — Overview UI (Wave 1b)

| Item | Scope |
|---|---|
| `CostForecastCard` React component | Hits `get_cost_forecast`, renders predicted total + bound range + reliability badge. Per-card error-state UI (not whole-Overview-white-screen, per Gemini). |
| `TopProjectsCard` React component | Reads `top_projects` from `dashboard_summary`. Renders top 5 with cost / message count / last-active relative time. |
| `RiskSignalsCard` React component | Renders `risk_signals: string[]`. Each row: lucide icon (`Info` / `AlertTriangle` / `AlertCircle`) + text. Empty array → green "No risk signals" badge. (Severity from string content if Mac encodes it that way; otherwise treat all as `info`.) |
| Overview 2-column restructure | md:+ breakpoint two-column. Single-column < md (840 px main-window minWidth means small-screen users still get the layout). |
| i18n keys (3 langs) | All new strings. |
| Tests | 4 vitest snapshots / interaction tests for the 3 new cards + the empty-state. |

**Test count target:** 50 → ~54 frontend.

### v0.5.2 — Onboarding (Wave 1c, optional)

| Item | Scope |
|---|---|
| 3-step onboarding wizard | Step 0: Welcome + value prop. Step 1: Privacy ("data stays local unless you pair"). Step 2: Sign-in OR Skip. |
| First-launch detection | Sentinel at `app_config_dir()/onboarding_completed.flag`. Plus `is_paired() == true` check (already-paired users skip wizard regardless of sentinel). |
| Permanent skip-X button | Top-right, on every step. Sets sentinel. (Per Mac iter13 lesson.) |
| Settings → "Re-run onboarding" | Deletes sentinel. Dev / re-onboarding path. |
| Tests | 3 vitest tests (each step renders, skip writes sentinel, sign-in path triggers existing OTP flow). |

### Out of scope (this sprint, may revisit later)

- `YieldScoreCard` — gated on git-attribution infra; defer until that ships on desktop.
- PDF report — `printpdf` crate situation messy; defer.
- Activity timeline chart — ornamental vs Wave 1's actual insight value.
- Demo mode — App Store driven, no user need on desktop.
- HowItWorks / LocalModeGuide cards — small UX polish, low signal.
- Subscription / TeamView — separate sprint, IAP infrastructure question.
- Remote session control / approvals — separate sprint, security review needed.
- Tray popover with metrics — separate sprint.
- DangerZone delete-account — small UX polish, defer until paired-user count > 0.

## Per-item design (revised)

### Item 1 — `quota::cost_forecast` (v0.5.0)

**Algorithm (per Swift, corrected):**

```rust
pub struct CostForecast {
    pub predicted_month_total: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub actual_to_date: f64,
    pub data_point_count: usize,  // always == day_of_month after fill
    pub current_day_of_month: u32,
    pub days_in_month: u32,
    pub is_reliable: bool,        // data_point_count >= 3 AND actual_to_date > 0
}
```

Pseudo-Swift port (exact behavior parity, including the iter21 last-day-of-month fix):

```rust
pub fn forecast_from_daily(
    daily: &[DailyUsageRow],
    reference: chrono::NaiveDate,
) -> Option<CostForecast> {
    let day_of_month = reference.day();
    let days_in_month = days_in_month_for(reference);

    // Aggregate cost per ISO-date string (same key shape as Swift).
    let cost_by_date = aggregate_cost_by_date(daily);

    // Build dense series 1..=day_of_month, missing days = 0.
    let mut points: Vec<(f64, f64)> = Vec::with_capacity(day_of_month as usize);
    let mut actual_to_date = 0.0;
    for day in 1..=day_of_month {
        let key = format!("{:04}-{:02}-{:02}", reference.year(), reference.month(), day);
        let cost = cost_by_date.get(&key).copied().unwrap_or(0.0);
        actual_to_date += cost;
        points.push((day as f64, cost));
    }

    // Note: Swift returns the zero forecast (not None) here, only flips
    // is_reliable=false. Match that semantics.
    let is_reliable = points.len() >= 3 && actual_to_date > 0.0;

    let avg_daily = actual_to_date / day_of_month as f64;
    let simple_projection = avg_daily * days_in_month as f64;

    let regression = linear_regression(&points);
    let remaining_days = days_in_month.saturating_sub(day_of_month);

    // iter21 fix: on the LAST day of month, remaining_days == 0; skip
    // the regression-projection extrapolation entirely. Swift returned
    // a flat-final-day prediction; we do the same.
    let predicted = if remaining_days == 0 {
        actual_to_date
    } else {
        // Project remaining via regression slope, blended with simple average.
        // (Mac's CostForecastEngine.swift:75 — read it before coding.)
        ...
    };

    let std_dev = standard_deviation_of_residuals(&points, &regression);
    let lower = (predicted - std_dev).max(0.0);  // clamp negative bound
    let upper = predicted + std_dev;

    Some(CostForecast {
        predicted_month_total: predicted,
        lower_bound: lower,
        upper_bound: upper,
        actual_to_date,
        data_point_count: points.len(),
        current_day_of_month: day_of_month,
        days_in_month,
        is_reliable,
    })
}
```

**Tests (mirror `CostForecastEngineTests.swift`):**
1. `forecast_returns_zero_forecast_unreliable_when_no_data` — Swift parity: `Some(forecast)` not `None`, `is_reliable=false`.
2. `forecast_three_uniform_days_predicts_avg_times_days_in_month` — handles zero-slope regression without div-by-zero.
3. `forecast_growing_trend_extrapolates_via_regression` — slope > 0 case.
4. `forecast_on_last_day_of_month_returns_actual_no_extrapolation` — iter21 regression pin.
5. `forecast_clamps_negative_lower_bound_to_zero` — std-dev > predicted edge case.
6. `forecast_with_two_points_marks_unreliable` — `< 3` boundary.

Lift Swift fixture inputs verbatim where possible (use shared JSON golden files per Codex). f64 tolerance ±0.01 USD on outputs.

### Item 2 — `top_projects` server-side path (v0.5.0)

**Pre-flight required:** dump real `dashboard_summary` payload. If `top_projects` is in there, just extend desktop's struct:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TopProject {
    pub project: String,
    pub cost_usd: f64,
    pub message_count: i64,
    pub last_active: Option<String>,  // ISO-8601 from server
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DashboardSummary {
    // ... existing fields ...
    #[serde(default)]
    pub top_projects: Option<Vec<TopProject>>,
    #[serde(default)]
    pub risk_signals: Option<Vec<String>>,
}
```

If `top_projects` is **not** in the server payload, defer to a v0.5.0a follow-up that adds the server-side aggregation (out of scope for this plan; would need backend RPC change which requires explicit user approval).

### Item 3 — `risk_signals: Vec<String>` (v0.5.0)

Pre-flight: confirm server returns array of strings (per Mac `Models.swift:270`). Add to `DashboardSummary` struct with `#[serde(default)]`. No new Rust logic. Frontend in v0.5.1 renders.

### Item 4 — Yield score: DEFERRED

Per Codex: "Yield requires git attribution infrastructure, not scan math." Mac's `YieldScoreCard.swift:56` gates UI on `gitTrackingEnabled`. Desktop has no equivalent. Implementing yield without git attribution would either:
- Show wrong values, OR
- Show parity-broken values vs Mac

Defer until desktop has git-attribution feature. Track but don't ship.

### Item 5 — Overview restructure (v0.5.1)

Two-column at md:+. **Per-card error states** (Gemini): if `get_cost_forecast` fails, the card itself shows a small "Failed to load forecast" placeholder; the rest of Overview renders normally. Same for top_projects / risk_signals fetch failures.

### Item 8 — Onboarding (v0.5.2, compressed)

3 steps:

- **Step 0 — Welcome:** "CLI Pulse tracks your AI CLI usage. Local scan by default."
- **Step 1 — Privacy:** "Data stays on this machine. Pair (optional) to sync across devices. Source on GitHub."
- **Step 2 — Sign in / Skip:** Email OTP form. "Skip for now" button bottom-right.

Permanent close-X top-right on every step (Mac iter13 lesson).

State: typed reducer with `Step = Welcome | Privacy | SignIn`, transitions `Next | Back | Skip | Complete`. No XState (per Codex).

## Sign-off pre-flight check (BEFORE any Rust struct change)

Before changing `supabase::DashboardSummary`:

1. **Confirm server payload shape.** Use Supabase MCP `execute_sql` to inspect the `dashboard_summary` RPC definition: `SELECT pg_get_functiondef('public.dashboard_summary'::regproc);`. Capture which fields the function actually returns.

2. **Capture a real response sample.** With the demo user JWT (per `feedback_desktop_autonomy.md` reference, demo user `2bbcf049-…`), call the RPC. JSON-pretty the response. Confirm field shapes:
   - `risk_signals`: `[]` or `["..."]` or `[{...}]`?
   - `top_projects`: present? array of objects? what fields per object?
   - `yield_score`: present? scalar or null?

3. **Lock Rust struct against the real payload.** Add fields with `#[serde(default)]` so older / variant payloads don't break deserialization.

4. **Only then start coding.** Don't infer schema from Swift; the Mac client may have local synthesis (per Codex's note that `APIClient.dashboard()` synthesizes empty `risk_signals: []` when server omits the field).

## Risks (revised)

1. **Server payload may not contain `top_projects`.** If the field doesn't exist server-side, Wave 1a is reduced to forecast + risk_signals, and top_projects becomes a "needs backend RPC change" item that requires user approval per autonomy contract. Pre-flight check answers this.

2. **`risk_signals` may be `[String]` AND empty for the demo user.** Pre-flight may show empty array, leaving the card untestable on that account. Test with synthetic data or a paired user that has alerts.

3. **Forecast f64 parity with Swift.** Floating-point ordering of operations differs subtly between Rust and Swift. Tolerance ±0.01 USD per Codex. Pin the iter21 last-day fix as a test.

4. **Per-card error-state UI is real work.** Each card needs a degraded skeleton. Don't skip this — Gemini explicitly flagged it as a missing dimension.

5. **Pre-flight RPC inspection requires Supabase MCP / JWT.** If MCP isn't reachable, fall back to running the desktop app paired to the demo account and sniffing the network trace. Don't proceed without one of these.

## Post-pre-flight schema findings (2026-05-05)

Supabase MCP `execute_sql` against project `gkjwsxotmwrgqsvfijzs` confirmed:

1. `dashboard_summary` RPC body returns ONLY: `today_usage`, `today_cost`, `active_sessions`, `online_devices`, `unresolved_alerts`, `today_sessions`. **No `risk_signals`, no `top_projects`, no `yield_score`.** Codex was correct — Mac client synthesizes those fields locally.
2. `daily_usage_metrics` table columns: `user_id`, `metric_date`, `provider`, `model`, `input_tokens`, `cached_tokens`, `output_tokens`, `cost`, `updated_at`, `device_id`. **No `project` column** — top_projects cannot be aggregated from this table.
3. `sessions` table HAS `project`, `estimated_cost`, `last_active_at`, `total_usage`, `requests`. **Top projects can be computed from sessions client-side** without touching backend.
4. `alerts` table has `severity` (text), `related_project_*`, `type`, `is_resolved`. **Risk signals can be derived from open/recent alerts client-side** — no new RPC needed.
5. Yield-score infrastructure exists (`_recompute_yield_scores_for_user_internal`, `recompute_yield_scores_for_user`, `get_track_git_activity`) but **no public read RPC** for the computed values. Yield-card UI requires either a new RPC (backend schema change → user approval) or fetching the underlying yield_scores table directly.

### v0.5.0 scope adjustment (final)

- **v0.5.0:** Cost forecast ONLY. Truly local — uses existing `get_daily_usage` Tauri command. No new RPCs, no struct extensions, no UI.
- **v0.5.1:** Frontend Overview restructure with 3 cards:
  - `CostForecastCard` — uses v0.5.0 backend.
  - `TopProjectsCard` — new Rust aggregator over the existing `sessions` table query (last-30-days, group by project, sum estimated_cost). New `get_top_projects(days)` Tauri command.
  - `RiskSignalsCard` — derives 1–3 short labels from existing `preview_alerts` payload (e.g., "$X spent on Y today" / "Provider Z unhealthy" — stringify the top-N unresolved alerts as human-readable signals). No new Tauri command.
- **v0.5.2:** Onboarding (compressed to 3 steps).
- **Yield score:** **DROPPED** from this sprint. Re-cut as separate sprint when desktop adds git-tracking infrastructure.

The v0.5.1 client-side aggregation approach (Codex flagged the original "scan-based" plan as incorrect) operates against the existing `sessions` table — no schema changes, no new RPCs, no autonomy-contract approval needed.

## Sequencing (committed, final)

1. **NOW:** Pre-flight done — see "Post-pre-flight schema findings" above.
2. **Then v0.5.0:** Cost forecast Rust module + Tauri command + 6 tests. No UI changes, no struct extensions, no other features. Single small ship.
3. **Then VM verify v0.5.0** (smoke check: forecast returns sane numbers; backend serializes new fields without errors).
4. **Then v0.5.1:** Overview UI restructure with the 3 cards. Single ship.
5. **VM verify v0.5.1** (visual check: 2-col layout, cards render, per-card error states work).
6. **Then v0.5.2** (onboarding). Optional — depends on user direction after v0.5.1.

## Review changelog (for archive)

- **v1 → v2 changes are P1+P2 corrections from Codex + Gemini 3.1 Pro**, both consulted 2026-05-05 same day. Convergent findings on 6 items (risk_signals schema, yield infrastructure dependency, top_projects backend coupling, Wave 1 split, Wave 2 cut, forecast semantic). Codex caught additional factual errors (minWidth, dialog plugin not a dep, printpdf version stale, Mac onboarding step 4 renamed). Gemini caught additional design gaps (icon-not-just-color for risk signals, SQL GROUP BY constraint, per-card error states, Tokio async).
- All catches incorporated into v2.

— end of v2 plan —
