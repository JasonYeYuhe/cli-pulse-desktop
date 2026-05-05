# Dev Plan — desktop ↔ Mac feature parity sprint

**Date:** 2026-05-05
**Author:** Claude (Opus 4.7)
**Repo:** `/Users/jason/Documents/cli-pulse-desktop` (Tauri 2 + Rust + React, Win + Linux)
**Sibling:** `/Users/jason/Documents/cli-pulse` (CLI Pulse Bar — macOS Swift, plus iOS / watchOS / Android)
**Reviewers (requested):** Codex + Gemini 3.1 Pro
**Trigger:** v0.4.21–v0.4.23 just shipped (sync hardening + diagnostics). Backlog of reliability items is closed; user explicitly asked to plan a feature-parity sprint with the Mac sibling.

## Context

Desktop has shipped 23 patch versions since v0.2.x and is feature-stable on the *reliability axis* (4-platform CI, signed auto-update, OS keychain, OAuth refresh, per-provider error+stale+synced-ago badges, mpsc tick-reset, fast shutdown, 5xx retry, Sentry instrumentation). What it lacks is the *insight* axis the Mac app delivers: cost forecasting, productivity scoring, project leaderboards, risk-signal callouts, onboarding, PDF export.

The Mac app is on v1.12.0; desktop is on v0.4.23. This plan closes the Overview / Settings UX gap with three release waves over the next ~2 weeks. Wave 3 (subscription / remote control) is sized as a separate sprint because of platform-IAP and backend coordination dependencies.

Constraints already honored throughout:
- Local-only changes wherever possible (the v0.4.x ships taught us the cost of touching backend schema mid-sprint).
- Each wave VM-verifiable on `clipulse-win-test` per `feedback_desktop_autonomy.md`.
- Pre-push hook gates (fmt + clippy + test + frontend tests) hold for every commit.
- i18n parity (en / zh-CN / ja) for every user-visible string.

## Inventory: Mac CLI Pulse Bar feature → Desktop status

Drawn from `/Users/jason/Documents/cli-pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/` (60 Swift modules) and `CLI Pulse Bar/` (top-level views).

| # | Mac module / feature | Source path | Desktop status | Notes |
|---|---|---|---|---|
| 1 | `CostForecastEngine` (linear regression on daily cost → month-end prediction with bounds) | `CLIPulseCore/Sources/CLIPulseCore/CostForecastEngine.swift` | **MISSING** | Already explicitly deferred in v0.4.20 dev plan as "Worth porting later." |
| 2 | `TopProjectsList` (per-project cost / token leaderboard) | `CLIPulseCore/Sources/CLIPulseCore/TopProjectsList.swift` | **MISSING** | Data already in scan (project field). Pure rendering port. |
| 3 | `RiskSignalsList` (anomaly callouts: spike days, model regression, etc.) | `CLIPulseCore/Sources/CLIPulseCore/RiskSignalsList.swift` | **MISSING** | Server provides `risk_signals` already (Mac OverviewTab.swift:637 reads `dash.risk_signals`). |
| 4 | `YieldScoreCard` (productivity metric — output per dollar) | `CLI Pulse Bar/YieldScoreCard.swift` | **MISSING** | Computed from existing scan data + sessions. |
| 5 | `OnboardingWizardView` (5-step: Welcome → Features → Privacy → Sign In → Pair) | `CLI Pulse Bar/OnboardingWizardView.swift` | **MISSING** | Mac forces this on first launch via `MenuBarView.swift:73`. Desktop has no first-run UX. |
| 6 | `WelcomeModeChoice` (local-only vs paired choice) | `CLI Pulse Bar/WelcomeModeChoice.swift` | **MISSING** | Companion to onboarding. |
| 7 | `DemoDataProvider` (canned demo data for first-launch / App Store reviewers) | `CLIPulseCore/Sources/CLIPulseCore/DemoDataProvider.swift` | **MISSING** | Lower priority for desktop (no App Store review path). |
| 8 | `PDFReportGenerator` (signed PDF report with branding) | `CLIPulseCore/Sources/CLIPulseCore/PDFReportGenerator.swift` | **MISSING** | Desktop has CSV / JSON only. Mac uses NSSavePanel. |
| 9 | `ActivityTimelineChart` (visual session timeline) | `CLIPulseCore/Sources/CLIPulseCore/ActivityTimelineChart.swift` | **MISSING** | Sessions tab is text-list-only on desktop. |
| 10 | `OverviewFormatters` (currency, locale, etc.) | `CLIPulseCore/Sources/CLIPulseCore/OverviewFormatters.swift` | **PARTIAL** | Desktop has `formatUSD`, `formatInt`, `formatRelativeMinutes/Short`. Add forecast-bound formatter. |
| 11 | `RemoteApprovalsSheet` + `RemoteSessionControlClient` (approve / control remote agent sessions from menu bar) | `CLI Pulse Bar/RemoteApprovalsSheet.swift`, `CLIPulseCore/.../RemoteSessionControlClient.swift` | **MISSING** | Larger feature. Backend RPCs already exist (per `PROJECT_DEV_PLAN_2026-04-29_remote_approvals_push.md` in cli-pulse repo). |
| 12 | `LocalSessionControlClient` (control sessions on the same machine) | `CLIPulseCore/.../LocalSessionControlClient.swift` | **MISSING** | Companion to remote control. |
| 13 | `SubscriptionManager` / `SubscriptionView` / `SubscriptionPricing` / `SubscriptionSection` | `CLIPulseCore/.../Subscription*.swift`, `CLI Pulse Bar/SubscriptionSection.swift` | **NOT APPLICABLE** | Mac uses StoreKit. Win/Linux has no IAP infrastructure on the table; ride-along the existing Stripe path on web if needed (out of scope here). |
| 14 | `TeamView` (team plan members + roles) | `CLI Pulse Bar/TeamView.swift` | **MISSING** | Depends on subscription UI; lower priority. |
| 15 | `MenuBarView` (compact menu-bar mini-UI) | `CLI Pulse Bar/MenuBarView.swift` | **PARTIAL** | Desktop has tray-icon (since v0.2.x) but only context-menu actions, no tray-popover with metrics. |
| 16 | `OverviewTab` (full Mac Overview composition) | `CLI Pulse Bar/OverviewTab.swift` | **PARTIAL** | Desktop has metric tiles + trend chart. Missing forecast / yield-score / top-projects / risk-signals composition. |
| 17 | `HowItWorksCard` / `LocalModeGuideCard` (in-app explainers) | `CLI Pulse Bar/HowItWorksCard.swift`, `LocalModeGuideCard.swift` | **MISSING** | Small UX polish. |
| 18 | `DangerZoneSection` (delete account, unpair-everywhere) | `CLI Pulse Bar/DangerZoneSection.swift` | **PARTIAL** | Desktop has unpair-this-device. No delete-account flow. |
| 19 | `RiskSignal` / `RiskSignalsList` rendering | `CLIPulseCore/.../RiskSignal*.swift` | **MISSING** | See #3. |
| 20 | `BookmarkManager` / `SandboxFileAccess` | `CLIPulseCore/.../BookmarkManager.swift` | **NOT APPLICABLE** | macOS App-Sandbox-specific. Desktop already accesses `~/.claude` / `~/.codex` / `~/.gemini` directly (no sandbox). |
| 21 | `ExportService` | `CLIPulseCore/.../ExportService.swift` | **PARTIAL** | Desktop exports CSV / JSON. Mac adds PDF. |
| 22 | iOS / watchOS / Android-specific code | various | **N/A** | Different platform. |

Status totals: **MISSING** = 12 items, **PARTIAL** = 5, **NOT APPLICABLE** = 4, with one overlap (PDF straddles #8 + #21).

## Proposed roadmap

Three waves, paced to match the v0.4.x sprint cadence (~1 ship per 30–60 min, ~5 ships per wave, VM verify between waves).

### Wave 1 — v0.5.0 — Overview parity (target: this week)

Bundles items #1, #2, #3, #4, #16. Single `v0.5.0` ship — the Overview tab gets its big Mac-style upgrade in one go, because partial Overviews look worse than the existing simple version.

| Ship | Item | Scope |
|---|---|---|
| v0.5.0 | Cost forecast card | New backend `quota::cost_forecast` module. New Tauri command `get_cost_forecast`. New `CostForecastCard` React component. |
| v0.5.0 | Top projects card | New `top_projects` aggregator (groups scan entries by project). New Tauri command `get_top_projects`. `TopProjectsCard` component. |
| v0.5.0 | Risk signals card | Read `risk_signals` from existing `dashboard_summary` server payload (already there, just unused). `RiskSignalsCard` component. |
| v0.5.0 | Yield score card | Server-side metric already in `dashboard_summary`. New `YieldScoreCard` component. (If absent server-side: client-side compute from cost / tokens.) |
| v0.5.0 | Overview tab restructure | New 2-column layout: left = original tiles + trend, right = forecast + yield + top-projects + risk-signals. Keep responsive collapse to single column on narrow windows. |

### Wave 2 — v0.5.1–v0.5.4 — Polish + onboarding

| Ship | Item | Scope |
|---|---|---|
| v0.5.1 | Activity timeline chart on Sessions tab | Visual replacement / augment for the text-only session list. Reuse existing `lucide` icons + a simple SVG timeline. |
| v0.5.2 | PDF report export | New `pdf-report` Rust crate dep (`printpdf` or similar). New Tauri command `export_pdf_report` triggering OS save dialog. |
| v0.5.3 | Onboarding wizard (5-step modal) | First-launch detection via Tauri `app_config_dir`/`onboarding_completed` flag. 5 steps: Welcome → Features → Privacy → Sign in (OTP / pair) → Done. Skip-anywhere button. |
| v0.5.4 | Demo mode + How-it-works / Local-mode-guide cards | Sample data when not paired and demo flag set. Two info cards in Settings. |

### Wave 3 — v0.6.0+ — Larger features (separate sprint, NOT in this plan's commit window)

| Item | Scope estimate | Blocker |
|---|---|---|
| Remote session approval/control | 1–2 weeks | Backend RPCs already exist per cli-pulse `PROJECT_DEV_PLAN_2026-04-29_remote_approvals_push.md`. UI is the work. WebView2 / WebKitGTK security review needed. |
| Subscription / Team UI | 2–4 weeks | Stripe-on-web flow + license-key validation in desktop. Cross-platform IAP design needed. Coordinate with iOS/macOS StoreKit unification. |
| Tray popover with metrics | 3–5 days | Replace tray context-menu with a Win10/11-style popover. Tauri `tray-icon` v2 supports it. |
| Delete-account flow (DangerZoneSection parity) | 2–3 days | Server RPC exists. UI + confirmation dialog + post-delete state. |

Wave 3 is **not committed** in this plan — listed for visibility so reviewers can comment on prioritization.

## Per-item design detail

### Item 1 — Cost forecast card (v0.5.0)

**Backend (`src-tauri/src/quota/cost_forecast.rs`, new file):**

Port `CostForecastEngine.swift` to Rust. Same interface:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct CostForecast {
    pub predicted_month_total: f64,
    pub lower_bound: f64,
    pub upper_bound: f64,
    pub actual_to_date: f64,
    pub data_point_count: usize,
    pub current_day_of_month: u32,
    pub days_in_month: u32,
    pub is_reliable: bool,  // true if data_point_count >= 3 and actual_to_date > 0
}

pub fn forecast_from_daily(
    daily: &[DailyUsageRow],
    reference_date: chrono::NaiveDate,
) -> Option<CostForecast> { ... }

fn linear_regression(points: &[(f64, f64)]) -> (f64, f64) { ... }
```

Algorithm exactly mirrors Mac (linear regression on per-day cost with std-dev bounds + simple-average fallback when < 3 points).

**Critical port-correctness item, per Mac iter21 hotfix `cf52c32`:** the LAST DAY of the month case where `day_of_month == days_in_month` produces a zero-length forecast range. Mac trapped on the closed-range invalid bound; Rust rust would silently return wrong data. Pin with a test: `forecast_on_last_day_of_month_does_not_panic`.

**Tauri command:**

```rust
#[tauri::command]
async fn get_cost_forecast() -> Result<Option<CostForecast>, String> {
    let daily = supabase::fetch_daily_usage(30).await?;  // existing path
    Ok(quota::cost_forecast::forecast_from_daily(&daily, chrono::Local::now().date_naive()))
}
```

**Frontend (`src/App.tsx::CostForecastCard`):**

```tsx
function CostForecastCard() {
  const [forecast, setForecast] = useState<CostForecast | null>(null);
  // load on mount + on Refresh-quota-now click
  // render: predicted total + bound range + "based on N days" + reliability badge
}
```

Layout: large predicted total (USD), small range below (`$X – $Y`), "based on N days" hint, amber `not enough data` badge when `!is_reliable`.

i18n keys (3 langs): `overview.forecast_title`, `overview.forecast_predicted`, `overview.forecast_bounds`, `overview.forecast_based_on`, `overview.forecast_unreliable`.

**Tests (Rust):**
- `forecast_with_three_uniform_days_predicts_average_x_days_in_month`
- `forecast_with_zero_data_returns_none`
- `forecast_on_last_day_of_month_does_not_panic` (the iter21 regression)
- `forecast_with_growing_trend_extrapolates_via_regression`
- `forecast_clamps_negative_lower_bound_to_zero`

### Item 2 — Top projects card (v0.5.0)

**Backend:**

Aggregate scan entries by `project` field. Sort by total cost descending. Take top 5.

```rust
#[derive(Debug, Clone, Serialize)]
pub struct TopProject {
    pub project: String,    // "<unknown>" if entry has no project
    pub cost_usd: f64,
    pub message_count: i64,
    pub model_count: usize,
    pub last_active: Option<chrono::DateTime<chrono::Utc>>,
}

#[tauri::command]
async fn get_top_projects(days: u32) -> Result<Vec<TopProject>, String> { ... }
```

Default `days = 30`. Cache result for ~30 s on the client (matches Provider summary cadence).

**Frontend:**

Card with rows: project name (truncated to ~30 chars), cost, message count, "5 days ago" relative time. Click → could deep-link to Sessions tab filtered by project (deferred — not in v0.5.0).

**Tests:**
- `top_projects_aggregates_cost_per_project`
- `top_projects_handles_unknown_project_field`
- `top_projects_caps_to_top_5`
- `top_projects_sorted_by_cost_desc`

### Item 3 — Risk signals card (v0.5.0)

**Backend:**

The `dashboard_summary` RPC ALREADY returns `risk_signals` (per Mac OverviewTab.swift:637 reading `dash.risk_signals`). Desktop's existing `get_dashboard_summary` Tauri command surfaces the row but `App.tsx` ignores the field. **No new Rust code needed** — just expose the field in the TS type and render.

**Frontend:**

```tsx
type RiskSignal = {
  kind: "spike" | "trend" | "anomaly" | string;
  severity: "info" | "warn" | "critical";
  title: string;
  detail?: string;
};

function RiskSignalsCard({ signals }: { signals: RiskSignal[] }) {
  if (!signals?.length) return <SafeBadge />;  // "No risk signals — looking good"
  return (
    <ul>
      {signals.map((s, i) => (
        <li key={i} className={severityColor(s.severity)}>
          <strong>{s.title}</strong>
          {s.detail && <span> · {s.detail}</span>}
        </li>
      ))}
    </ul>
  );
}
```

i18n: `overview.risk_no_signals`, `overview.risk_severity_info`, etc.

**Tests:** vitest, mock 3 severity levels, assert color classes applied.

### Item 4 — Yield score card (v0.5.0)

Yield = output_tokens / cost_usd over last 30 days, vs. baseline (last 30–60 days). Higher = more output per dollar.

**Backend:**

If `dashboard_summary` already exposes yield_score (need to verify against Mac's `YieldScore.swift`):

```swift
// Mac:
public struct YieldScore { ... }
```

— if YES, just expose. If NO, compute client-side from `daily_usage` + scan.

Either way, no new server work.

**Frontend:**

Big number (e.g. "1.4×") + arrow indicator vs baseline + tooltip explaining the metric.

**Tests:** Pure-function port from `YieldScore.swift`. Pin baseline-calculation parity.

### Item 5 — Overview tab restructure (v0.5.0)

Current Overview structure:

```
[Account-today tiles (6)]
[This-device tiles (4)]
[Cost trend chart]
[Today's breakdown]
```

New structure:

```
[Account-today tiles (6)]                [Forecast card]
[This-device tiles (4)]                  [Yield score card]
[Cost trend chart]                       [Top projects card (5 rows)]
                                         [Risk signals card]
[Today's breakdown — full width]
```

Two-column at md:+ breakpoint. Single-column collapse < md.

i18n: just section headers, individual card content covered above.

### Item 6 — Activity timeline chart (v0.5.1)

Sessions tab today is a text list. Mac has a horizontal timeline showing session start/end + activity density.

**Implementation:** SVG-based, no new dep. Each row = one session. X-axis = last 24 h. Color by provider. Hover → tooltip with details. Reuse session data already returned from `list_sessions`.

**Tests:** vitest snapshot of the SVG with mocked sessions.

### Item 7 — PDF report export (v0.5.2)

Mac has `PDFReportGenerator` + NSSavePanel. Desktop needs a similar export.

**Backend (Rust):** Add `printpdf = "0.7"` (well-maintained, Tauri-compatible). New `export_pdf_report` Tauri command that:
1. Opens OS save dialog via Tauri `dialog` plugin (already a dep)
2. Generates PDF with: header (logo + version + date), 30-day cost summary table, top 5 projects, top 5 models, forecast card, footer
3. Writes to chosen path
4. Returns success / error

**Test:** snapshot byte-length sanity (PDF generation is deterministic at the byte level if we fix the date input).

### Item 8 — Onboarding wizard (v0.5.3)

Mac's `OnboardingWizardView` is 5 steps. Desktop port should match closely:

- Step 0: Welcome — "CLI Pulse tracks your AI CLI usage."
- Step 1: Features — "Local scan, no upload by default. Pair to sync across devices."
- Step 2: Privacy — "Data stays on your machine unless you pair. Source code on GitHub."
- Step 3: Sign in — email OTP form (existing flow)
- Step 4: Pair device (optional) — "Pair with your Mac / phone? (you can skip)"

**First-launch detection:** Tauri `app_config_dir()/onboarding_completed.flag` (zero-byte sentinel). On launch: if absent AND `is_paired() == false`, show wizard.

**Skip-anywhere button:** top-right X, sets the sentinel. (Mac learned this the hard way at iter13.)

**Reset path:** Settings → "Re-run onboarding" button (deletes the sentinel). Mac doesn't expose this; desktop should, because dev iteration benefits.

**Tests:** vitest — render each step, assert skip button present, assert sentinel write on completion.

### Item 9 — Demo mode + info cards (v0.5.4)

Lower priority. Skip detail until Wave 1+2 complete and we re-evaluate.

## Sequencing rationale

- **Wave 1 single bundle (v0.5.0):** Overview is the user's first impression. Half-rebuilding it leaves a worse UX than the current simple-tiles state. All four cards land in one ship.
- **Wave 2 split (v0.5.1–v0.5.4):** Each item is independent and small. Splits reduce blast radius if any one item regresses.
- **Wave 3 deferred:** Subscription / remote control are weeks of work. Don't commit them in this plan.
- **No interleaving with reliability fixes:** This is a feature sprint. If a real Sentry event surfaces during Wave 1, halt and ship a fix; otherwise stay on plan.

## Tests / metrics target

Current: 167 backend + 50 frontend = 217 tests.

Per-item additions:
- Item 1 (forecast): +5 backend tests
- Item 2 (top projects): +4 backend tests
- Item 3 (risk signals): +2 frontend tests
- Item 4 (yield score): +3 backend (or frontend if client-side)
- Item 5 (overview restructure): +0 tests (visual layout)
- Item 6 (timeline chart): +1 frontend snapshot
- Item 7 (PDF export): +1 backend (PDF byte-length sanity), +1 frontend (button-disabled-while-exporting)
- Item 8 (onboarding): +5 frontend (each step renders, skip works, sentinel writes)

Wave 1 target: ~232 tests (+12–14). Wave 2 target: ~245 tests (+13).

## Risks

1. **Forecast algorithm parity with Mac.** Mac's `CostForecastEngine` has subtle edge-case fixes (last-day-of-month, zero-data, negative-bound clamp). Direct port + identical-input-output tests are the only safe path. If Mac is wrong, desktop should be wrong the same way until Mac fixes — divergence between platforms looks broken to users.

2. **`risk_signals` server format may have drifted.** Mac code is on v1.12.0. Desktop's `dashboard_summary` consumer was last touched v0.3.4. Need to verify the JSON shape hasn't changed; if it has, we silently render nothing for the affected severity. **Action item: read `dash.risk_signals` actual response on a paired test account before coding the card.**

3. **PDF export Linux ARM64.** `printpdf` is pure-Rust → should be fine, but Tauri Linux ARM64 builds have historically been the first to break on new deps. Pre-verify by adding the dep + a no-op call in CI before committing the feature work.

4. **Onboarding sentinel collision.** First-launch sentinel must NOT trip when users uninstall + reinstall (no `~/.config/cli-pulse-desktop/` cleanup). Solution: sentinel keyed on app version, OR check `is_paired()` first (paired users skip onboarding regardless of sentinel state — covers reinstalls).

5. **Two-column Overview at narrow widths.** Tauri main-window minWidth is 880 px (per `tauri.conf.json`). Need to confirm the right column has visual breathing room at ~600 px. If not, drop to single-column layout for cards.

6. **Yield-score baseline.** If we compute client-side, the formula must match Mac's exactly — otherwise paired users see "1.4×" on Mac and "1.7×" on desktop for the same account, which looks like a bug. Recommend SERVER-SIDE compute (one source of truth) — confirm what `dashboard_summary` returns first.

## Out of scope (with reasons)

- **Subscription UI / TeamView (Mac items #13, #14):** Win/Linux IAP infrastructure is a separate sprint with cross-platform implications. Defer to v0.6.0 sprint.
- **Remote session control (#11, #12):** Same — separate sprint, larger scope, security review needed.
- **`SandboxFileAccess` / `BookmarkManager` (#20):** macOS-specific. Desktop already has direct file access.
- **Mac-specific UI (sidebar, window glass, NSPopover-style menu bar):** Different window paradigm. Desktop tray-popover replacement is Wave 3.
- **Backend schema changes:** Per autonomy contract, requires explicit user approval. None proposed in Waves 1–2.
- **Test infrastructure changes** (e.g. cross-platform snapshot testing, Tauri integration tests, real-DOM tests): Out of scope here. Mention if reviewers think it's blocking.

## Review questions for Codex

1. **Forecast port correctness.** The Swift `CostForecastEngine` has line-by-line tests; the Rust port should produce byte-identical output for byte-identical input. What's the safest cross-compilation strategy — port the Swift tests verbatim, or write Rust-native tests against fresh fixtures? If verbatim, is there a tooling pattern for keeping the two test suites in sync?

2. **`risk_signals` schema check.** What's the cheapest way to verify the server's current JSON shape before coding the card? `psql` against the read replica? `gh` action that snapshots the response? curl with the demo user's JWT?

3. **`printpdf` vs alternatives.** Have you used `printpdf`, `pdf-writer`, or `weasyprint` (Python sidecar) for Tauri PDF export? Mac's report has logo + tables + colored cards — what library handles this cleanly with Win+Linux ARM64 binary support?

4. **Onboarding-state machine.** A 5-step wizard with skip-anywhere has 6 entry points × 2 exit paths × N "back-button" cases. State-machine library (XState) overkill, or worth it for the testability gains?

5. **Wave-1 bundling vs splitting.** Should v0.5.0 really land all four Overview cards at once, or do we split Item 5 (the layout restructure) from Items 1–4 (the cards themselves)? The benefit of splitting is per-card VM verifiability; the cost is the Overview tab "looks weird" mid-sprint.

6. **Anything you'd cut from the plan?** Honest take — does Wave 2 belong on the roadmap, or is the user well-served stopping after Wave 1?

## Review questions for Gemini 3.1 Pro

1. **CostForecastEngine math.** Mac uses linear regression with std-dev bounds, falling back to simple-average projection when `n < 3`. The fallback boundary — does linear-regression-with-2-points actually produce a useful forecast (it can, with high uncertainty), or is the `n < 3` cliff arbitrary? Should the desktop port keep the cliff for parity, or do better?

2. **Last-day-of-month bug.** Mac's iter21 hotfix `cf52c32` fixed a closed-range crash on the last day of the month. The fix is "skip the regression-projection path entirely on the last day." Is that the right fix, or should we predict a flat-final-day with no extrapolation? (Edge case where it matters: cost spike on the 31st making the prediction look way off.)

3. **`risk_signals` rendering.** Three severity levels (info / warn / critical). Mac uses three colors. Should desktop differentiate by ICON instead of just color (accessibility — color-blind users)? If yes, what icon vocabulary makes sense?

4. **Onboarding wizard scope.** Mac's wizard has 5 steps. Should the desktop port keep all 5, or compress to 3 (Welcome / Privacy / Sign-in)? The 5-step has been criticized internally on Mac (per OnboardingWizardView.swift:13's iter13 fix-comment about the user being trapped).

5. **PDF export — what should it CONTAIN?** Mac's PDF has the OverviewTab snapshot. Desktop's PDF: same? Or richer (last 30 days table, project breakdown, sessions list)? Larger PDFs are slower to generate and the user mostly wants a TL;DR.

6. **Demo mode justification.** Mac's `DemoDataProvider` exists primarily for App Store reviewers (per AppStoreConnect submission flow). Desktop has no review gate. Is demo mode worth building, or strictly App-Store-driven?

7. **Feature creep risk.** Wave 1+2 = 9 items. The autonomy contract warns "stop pushing when… adding a feature would touch the backend schema, a paid account, or an external publisher." None of these items hit those, but cumulative-feature-load is itself a risk vector. Where's the cliff?

## Files this plan would touch

Wave 1 (v0.5.0):
- `src-tauri/src/quota/cost_forecast.rs` (NEW)
- `src-tauri/src/quota/top_projects.rs` (NEW)
- `src-tauri/src/quota/mod.rs` (export)
- `src-tauri/src/lib.rs` (3 new Tauri commands + invoke_handler entries)
- `src-tauri/src/supabase.rs` (extend `DashboardSummaryRow` to include risk_signals + yield_score if not already)
- `src/App.tsx` (Overview restructure, 4 new cards)
- `src/locales/{en,zh-CN,ja}.json` (i18n keys)
- `src/i18n.test.ts` (critical-labels list)
- `CHANGELOG.md` (single v0.5.0 entry)
- 3 manifests + 2 lock files

Wave 2 (v0.5.1–v0.5.4):
- `src/components/ActivityTimelineChart.tsx` (NEW)
- `src-tauri/Cargo.toml` (add `printpdf`)
- `src-tauri/src/pdf_report.rs` (NEW)
- `src/components/OnboardingWizard.tsx` (NEW)
- `src/locales/*` (more keys)

## Sign-off check

Before any coding starts:
1. Verify `risk_signals` and `yield_score` actually exist in the current `dashboard_summary` payload.
2. Verify `printpdf` cross-compiles for Linux ARM64 in CI (5-min experiment).
3. Confirm the 880 px main-window minimum is fine for the new two-column Overview at smallest size.
4. Capture all three reviewer answers (Codex + Gemini 3.1 Pro) before locking the v0.5.0 design.

— end of plan —
