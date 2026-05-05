# Dev Plan — v0.5.3 + v0.5.4 (post-parity polish + onboarding)

**Date:** 2026-05-05
**Builds on:** v0.5.0+v0.5.1+v0.5.2 (Mac-Overview parity wave, all VM-verified clean)
**Reviewers (requested):** Codex + Gemini 3.1 Pro
**Trigger:** v0.5.0+0.5.1+0.5.2 VM verify report came back PASS on every functional block but flagged 3 polish items: (1) auto-updater banner click doesn't trigger install (carryover from v0.4.x verifies), (2) Risk Signals card vs "未解决告警 7" tile data-source mismatch, (3) Sentry release-filter semantics doc. Plus the v2 plan's deferred onboarding wizard is still on the table.

User direction: "全部做" — bundle the three polish items as v0.5.3, then ship onboarding as v0.5.4.

## Context

Today's ship cadence: 7 versions (v0.4.21 → v0.5.2), zero rollbacks, two VM verify cycles. Reliability + parity are both in good shape. v0.5.3 closes loose ends from the verify reports; v0.5.4 closes the last v2-plan item (onboarding) so the v0.5.x sprint formally ends.

After v0.5.4, the v2-plan deferred items (yield score, PDF export, activity timeline, demo mode, subscription, remote control) all sit at "real-user-signal-driven" not "schedule-driven."

## v0.5.3 — three contained items, single ship

### Item 1 — Memory updates (atomic, no code)

**File:** `/Users/jason/.claude/projects/-Users-jason-Documents-cli-pulse/memory/reference_sentry.md`

Add a new section "Issue-vs-event release filter semantics":

> `sentry-cli issues list --query "release:cli-pulse-desktop@X.Y.Z"` filters by **first-seen release**, not by "any event in this release." An existing fingerprinted issue (e.g. `DESKTOP-1` first seen in v0.4.23) won't appear under v0.5.0+ filters even if it has events in those releases. To verify a specific event-release attribution, query the issue's events endpoint directly: `sentry-cli api -m GET "/issues/<id>/events/?statsPeriod=1h"` returns events with their actual `release` field. Surfaced by the v0.5.0+0.5.1+0.5.2 VM verify report 2026-05-05.

**File:** `/Users/jason/.claude/projects/-Users-jason-Documents-cli-pulse/memory/reference_desktop_repo.md`

In the test-infrastructure section, add a known-issue entry:

> **Known issue (open):** Auto-updater banner in the top-right header navigates to Settings tab on click but does not trigger the actual download+install flow. Banner text "⬆ 有新版本 vX.Y.Z · 更新" sets the user's expectation that clicking will install; current wiring (`onClick={() => setTab("settings")}`) only switches tabs. v0.5.3 fixes this by directly invoking the existing `doCheckUpdate` flow from the banner click. Tracked across v0.4.21+22+23 VM verify (2026-05-05) and v0.5.0+0.5.1+0.5.2 VM verify (2026-05-05) — both reports flagged "click didn't dispatch."

No code changes for this item. ~15 lines of memory addition total.

### Item 2 — Auto-updater banner: click directly triggers install

**File:** `src/App.tsx`

**Current (lines 242-251):**
```tsx
<button
  onClick={() => setTab("settings")}
  className="..."
  title={t("updater.banner_available", { version: updateAvailable })}
>
  ⬆ {t("updater.banner_available", { version: updateAvailable })} ·{" "}
  <span className="font-semibold">{t("updater.banner_action")}</span>
</button>
```

This wires the click to ONLY tab-switch. The actual download+install lives in `doCheckUpdate` inside the `UpdatesSection` component (line ~1628). The user's mental model from the banner is "click → install"; the current behavior is "click → navigate to Settings → find Updates section → click another button → install." That's 3 clicks, not 1.

**Fix design:**

Lift `doCheckUpdate` (or a slim variant) up to App-level so the banner's click handler can invoke it directly. Two paths:

**(a) Lift the whole updater state machine to App-level.** Pass the state down to `UpdatesSection` so it renders the same UI it does today, but the state lives at App-level. Banner click triggers App-level `doCheckUpdate`. Banner can then also reflect download progress instead of just sitting there (e.g., "⬇ 下载中… 47 %").

**(b) Inline a minimal install-trigger from the banner.** Banner click calls `checkUpdate().downloadAndInstall(...)` directly. UpdatesSection stays unchanged with its own internal state. Two parallel state machines feels awkward and could race if user clicks both.

**Recommendation: (a).** Single source of truth for updater state. The banner becomes a "primary install affordance" and the Settings → Updates section becomes a secondary detailed view (with "check now" for users who navigate there proactively).

**Implementation outline:**

```tsx
// At App-level (around line 130 with the other state):
const [updater, setUpdater] = useState<UpdaterState>({ state: "idle" });

// On mount (extending the existing checkUpdate effect at line ~200):
useEffect(() => {
  let cancelled = false;
  (async () => {
    try {
      const upd = await checkUpdate();
      if (cancelled) return;
      if (upd) {
        setUpdateAvailable(upd.version);
        setUpdater({ state: "available", version: upd.version, body: upd.body });
      }
    } catch (e: any) {
      // existing silent-fail behavior
    }
  })();
  return () => { cancelled = true; };
}, []);

// New App-level handler:
async function doInstallUpdate() {
  if (updater.state !== "available") return;
  setUpdater({ state: "checking" });
  try {
    const upd = await checkUpdate();
    if (!upd) {
      setUpdater({ state: "up-to-date" });
      setUpdateAvailable(null);
      return;
    }
    let total = 0, downloaded = 0;
    await upd.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? 0;
        setUpdater({ state: "downloading", pct: 0 });
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        const pct = total > 0 ? Math.round((downloaded / total) * 100) : 0;
        setUpdater({ state: "downloading", pct });
      } else if (event.event === "Finished") {
        setUpdater({ state: "ready" });
      }
    });
  } catch (e: any) {
    setUpdater({ state: "error", text: String(e) });
  }
}

// Banner click (replacing setTab("settings")):
onClick={() => {
  if (updater.state === "ready") {
    relaunch();
  } else if (updater.state === "available") {
    doInstallUpdate();
  } else if (updater.state === "downloading") {
    // no-op, click while downloading is harmless
  } else {
    // fallback for "checking"/"error" — navigate to Settings for detail
    setTab("settings");
  }
}}
```

Banner text changes per state:
- `available`: "⬆ 有新版本 v0.5.3 · 更新" (kicks off download on click)
- `downloading`: "⬇ 下载中 47 %" (no-op click, progress visible)
- `ready`: "✓ 重启以应用 v0.5.3" (click → relaunch)
- `error`: "⚠ 更新失败 — 重试" (click → retry → setTab settings for detail)

**Tests:** Frontend test with mocked `@tauri-apps/plugin-updater`:
- Click on `available` state triggers `downloadAndInstall`
- Click on `ready` state triggers `relaunch`
- Click on `downloading` state is a no-op (no double-fire)

**Risk:** Tauri's `plugin-updater` mock surface is non-trivial. If the test gets ugly, document the click flow in code comments and rely on VM verify for end-to-end coverage. Fall back to a unit test on the click-to-action mapping function only.

### Item 3 — Risk Signals card data-source clarity

**Problem (from VM report):** Overview's top tile "未解决告警 7" comes from `dashboard_summary.unresolved_alerts` (server-side count of alerts in the `alerts` table where `is_resolved = false`). Same Overview's RiskSignalsCard uses `preview_alerts` data, which is **client-computed** alerts (local scan + thresholds, runs on the desktop side). These two paths produce **different alert sets** — there's no contract that they'll agree.

VM verify saw "tile = 7, card = no risk signals" because the client preview said no alert applies, but server has 7 unresolved budget alerts from past sync activity.

**Two options, picking the simpler:**

**(a) Switch RiskSignalsCard to server-side alert source.** New `supabase::get_unresolved_alerts(user_id, jwt)` PostgREST GET (mirroring the v0.5.2 sessions pattern). New `get_server_alerts` Tauri command. Risk card renders from server data. Tile and card now agree by construction.

**(b) Keep the client-computed source but rename + clarify.** Risk card renames from "Risk signals" to "Today's alerts" / "今日提示" or similar. Add subtitle "Computed locally from today's usage." User understands the divergence.

**Recommendation: (a).** Mac sibling's `RiskSignalsList` reads from `dash.risk_signals` (which the desktop server doesn't return — see v2 plan pre-flight). The right long-term shape is server-side alerts, and we already have the alerts table sitting there with all the data. Adding a PostgREST GET helper is what we did in v0.5.2 for sessions; this is the same pattern.

**Implementation outline:**

```rust
// supabase.rs — new helper, mirrors get_sessions_since
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerAlert {
    pub id: String,
    pub severity: String,           // "Info" / "Warning" / "Critical"
    pub title: String,
    pub message: Option<String>,
    pub related_project_name: Option<String>,
    pub related_provider: Option<String>,
    pub created_at: String,
    pub is_resolved: bool,
}

pub async fn get_unresolved_alerts(
    user_id: &str,
    user_jwt: &str,
) -> SupabaseResult<Vec<ServerAlert>> {
    // GET /rest/v1/alerts?user_id=eq.<id>&is_resolved=eq.false
    //   &select=id,severity,title,message,related_project_name,
    //           related_provider,created_at,is_resolved
    //   &order=created_at.desc&limit=50
    ...
}

// lib.rs — new Tauri command
#[tauri::command]
async fn get_server_alerts() -> Result<Vec<supabase::ServerAlert>, String> {
    let Some(cfg) = config::load().map_err(|e| e.to_string())? else {
        return Ok(vec![]);
    };
    let user_id = cfg.user_id.clone();
    with_user_jwt(move |jwt| {
        let user_id = user_id.clone();
        async move { supabase::get_unresolved_alerts(&user_id, &jwt).await }
    }).await
}
```

Frontend RiskSignalsCard switches from `alerts` prop (preview_alerts data) to its own `get_server_alerts` fetch. Same severity-coded SVG icons, same "Looking good" empty state, same top-3-after-sort. The CARD now matches the TILE because both source from the same server-side `alerts` table.

**Tests:** Backend tests for the PostgREST GET path are tricky without a mock server. Smoke-tested by VM verify (tile count and card top-3 must agree).

**Risk:** Two HTTP calls per Overview load now (one for forecast, one for server-alerts) on top of the existing dashboard_summary. Minor, mitigated by per-card 60 s polling staggered across mounts.

## v0.5.4 — Onboarding wizard (3-step compressed)

Per v2 plan: compress Mac's 5-step wizard to 3 steps. Mac's iter13 fix-comment shows the 5-step trap was internally regretted.

**Steps:**

- **Step 0 — Welcome:** Big logo + headline "CLI Pulse — Track your AI CLI usage." Subtext "Local scan by default. Pair to sync across devices." `Next` button.
- **Step 1 — Privacy:** "Your data stays on this machine unless you pair. Source code on GitHub. No telemetry beyond Sentry crash reports (you control the DSN)." `Next` button.
- **Step 2 — Sign in / Skip:** Email OTP input. `Send code` button. Below: secondary `Skip for now` link. After successful OTP: completes wizard. After Skip: same — wizard closes, user lands on Overview tab.

**Persistent close-X** in the top-right of every step, sets the sentinel and exits (Mac iter13 lesson). Sets `app_config_dir()/onboarding_completed.flag` (zero-byte sentinel).

**First-launch detection:**
```rust
// On app boot, ahead of rendering:
fn should_show_onboarding() -> bool {
    if config::load().map(|c| c.is_some()).unwrap_or(false) {
        return false;  // already paired = onboarding complete
    }
    let path = onboarding_sentinel_path();
    !path.exists()
}
```

**Reset path:** Settings → "Re-run onboarding" button. Deletes the sentinel.

**State machine:** Typed reducer (no XState):
```tsx
type Step = "welcome" | "privacy" | "signin";
type Action = { type: "next" } | { type: "back" } | { type: "skip" } | { type: "complete" };
```

**i18n keys:** ~12 new keys (3 steps × ~4 strings + skip/back/close labels).

**Tests:** Frontend vitest:
- Each step renders the expected headline + body
- `Next` advances correctly
- `Back` reverses correctly
- Skip writes sentinel + closes wizard
- Sentinel-present-on-launch suppresses wizard
- Already-paired suppresses wizard (independent of sentinel)

## Out of scope (this sprint)

- **Sentry release-event semantics doc** beyond the memory note (no code change needed; reference_sentry.md update covers it).
- **Auto-updater retry-with-backoff on transient install failure** — current state machine handles "error" via banner-error visual, but doesn't auto-retry. Defer until a real production failure surfaces.
- **Onboarding step 4 ("Pair Device")** — Mac's wizard had a pair step that was renamed "All set" per `OnboardingWizardView.swift:450`. Skipping entirely on desktop's 3-step compression. Pairing is a Settings action, not an onboarding gate.
- **Yield score** — still gated on git-attribution infra desktop doesn't have.
- **PDF export, activity timeline, demo mode** — same as v2 plan: defer until real-user signal asks for them.

## Sequencing

1. **v0.5.3** — single ship bundling Items 1+2+3.
2. **VM verify v0.5.3** focused checks: (i) banner click triggers download, (ii) tile count == card top-3 count, (iii) memory note correctness.
3. **v0.5.4** — onboarding wizard. Single ship.
4. **VM verify v0.5.4** — fresh-install path, sentinel writes, skip works, re-run onboarding from Settings.
5. **Stop sprint.** v0.5.x parity wave formally closes.

## Tests target

- v0.5.3: 234 → ~242 (180 backend, +1 server-alerts mock placeholder; 54 frontend, +6: 4 banner-click-state-machine cases + 2 risk card data-source switch).
- v0.5.4: 242 → ~252 (180 backend, +0; 54 frontend, +10: 6 onboarding step rendering + sentinel + skip + already-paired + reset).

## Risks

1. **Lifting updater state to App-level changes a working code path** (Settings → Updates flow). If the lift introduces a regression, the user has TWO broken update paths instead of ONE awkward one. Mitigation: test the Settings flow explicitly post-lift; keep the same `UpdatesSection` UI by passing state down as props.

2. **Server-alerts switch may show a different count than tile in some edge cases** — e.g., if `evaluate_budget_alerts` runs server-side between the dashboard_summary call and the get_server_alerts call, the count could differ by one or two. Acceptable race; both tile and card refresh on next 60 s poll cycle.

3. **Onboarding wizard suppression check** — if the sentinel write fails (filesystem permission, ENOSPC), the wizard re-shows on every launch. Mitigation: log on write failure; check at launch fails-open (don't show wizard on read error to avoid trapping users behind a broken sentinel).

4. **Auto-updater banner click — Tauri plugin behavior on Windows.** Per v0.4.21+22+23 VM verify, the bug was hypothesized as "WebView2 focus quirk." If the actual root cause IS focus-related (not the wiring), my fix doesn't help. The wiring fix is still strictly an improvement (1-click install vs 3-click navigate-then-install), so even if focus is the underlying issue, v0.5.3 is a forward step.

## Review questions for Codex

1. **Tauri updater state-machine lift to App-level:** any plugin-updater quirks I'm missing? The `checkUpdate()` call is idempotent (returns the same `Update` instance for the same version), but the `downloadAndInstall` callback is single-fire — should the App-level state guard against double-clicks during the `available → downloading` transition?

2. **PostgREST GET against `alerts` table — RLS implications.** RLS on `alerts` should restrict to the authenticated user's rows. Is there a server-side gotcha I'm not seeing (e.g., a `service_role`-only check that would 403 the desktop's user JWT)?

3. **Onboarding sentinel — file system semantics.** `app_config_dir()` on Win+Linux is well-known, but should the sentinel be a file (zero-byte presence check) or a key in a JSON config? Any persistence-failure-mode I should plan for?

## Review questions for Gemini 3.1 Pro

1. **Banner state machine UX:** is showing 4 different banner texts (available / downloading / ready / error) too noisy in a global header? Mac's menu-bar app has a similar problem space — does compressing the banner to "ready" being the only state visible (with downloading hidden behind a tooltip) read better?

2. **Risk Signals card source-of-truth shift.** Switching from client-computed `preview_alerts` to server `get_server_alerts` means the card now shows alerts the user might never have triggered locally. Is that a feature (cross-device visibility) or a confusion (why does my desktop show a budget alert that fired on my phone)?

3. **Onboarding 3-step flow.** Step 2 has both the OTP form AND a Skip link in the same view. Is that visually crowded? Better as Step 2 = "Sign in" with required action and Step 3 = "Or skip" as a separate decision page?

4. **Anything I missed at P1/P2 severity** in either v0.5.3 or v0.5.4. Be aggressive — we've shipped 7 versions today and a fresh pair of eyes is most valuable on plan documents that are easy to skim past mistakes.

## Files this plan would touch

**v0.5.3:**
- `~/.claude/projects/.../reference_sentry.md` (memory)
- `~/.claude/projects/.../reference_desktop_repo.md` (memory)
- `src/App.tsx` (banner click handler + lifted updater state)
- `src-tauri/src/supabase.rs` (new `get_unresolved_alerts`)
- `src-tauri/src/lib.rs` (new `get_server_alerts` Tauri command)
- `src/locales/{en,zh-CN,ja}.json` (i18n for new banner state texts)
- 3 manifests + 2 lock files

**v0.5.4:**
- `src/components/OnboardingWizard.tsx` (NEW, ~200 lines)
- `src/App.tsx` (first-launch wizard mount)
- `src-tauri/src/lib.rs` (new commands: `is_onboarding_complete`, `mark_onboarding_complete`, `reset_onboarding`)
- `src/locales/{en,zh-CN,ja}.json` (~12 new keys × 3)
- `src/i18n.test.ts` (critical-labels)
- 3 manifests + 2 lock files

— end of plan —
