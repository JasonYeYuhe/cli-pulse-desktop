# Dev Plan — v0.4.20 sync-health visibility

**Date:** 2026-05-05
**Author:** Claude (Opus 4.7)
**Reviewer (requested):** Gemini 3.1 Pro
**Scope:** Three small items focused on making sync health visible. All local-only (no backend schema changes, no new external endpoints, no new deps). Total estimated effort: half a dev day.

## Context

The v0.4.x sprint (v0.4.13 → v0.4.19, all VM-verified) closed the reliability backlog: Claude/Gemini OAuth refresh, OS keychain, stale indicator, OpenRouter bigint, force-refresh button, breadcrumb cleanup, proactive pre-expiry refresh. Daily experience is now solid.

Gaps remaining are **failure-mode visibility** and **deferred architectural polish**. v0.4.20 covers three contained items.

## Surprise finding

**Codex active OAuth refresh has shipped since v0.4.3** (`quota/codex.rs:226-269`). The earlier deferral note in the v0.4.14-v0.4.16 plan that claimed otherwise was wrong. v0.4.19 corrected this in the CHANGELOG; mentioning here so the new local Claude session that picks this plan up isn't tempted to "implement" it.

---

## Item 1 — MPSC tick-reset on manual refresh (Gemini-flagged)

### Background

`spawn_background_sync` in `lib.rs:994-1056` runs an infinite loop:
```
loop {
  background_tick().await;
  sleep(SYNC_INTERVAL).await;  // SYNC_INTERVAL = 120s
}
```

When the user clicks "Refresh now" (v0.4.19 — `forceRefresh` in `App.tsx`), the manual `sync_now` invoke runs SEPARATELY from the background loop. If the user clicks at second 118 of a 120s tick, the background tick fires 2s later — a redundant sync.

`sync_now` is idempotent at the helper_sync level (upsert by `(user_id, provider)`), so this is a correctness improvement, not a critical bug — but it's "free" once we restructure the loop.

### Implementation

**Per Gemini 3.1 Pro v0.4.20 review (P1):** my initial proposal used `tokio::sync::Notify` and Gemini correctly caught that it has the exact bug we're trying to fix. If the user clicks "Refresh now" while the background loop is already executing `background_tick().await` (NOT during the sleep), the `Notify` buffers a permit. When `background_tick` finishes, the loop hits `select!`, instantly consumes the buffered permit, and skips the 120s sleep — firing a redundant background tick right after the manual one. Net result: redundant sync, same as today.

Use `tokio::sync::mpsc::channel(1)` instead, draining buffered messages BEFORE entering the `select!` sleep so only manual refreshes that fire DURING the sleep window cause a reset:

```rust
// in lib.rs near the other sync globals:
use tokio::sync::mpsc;

// Channel sender exposed to the Tauri sync_now command. Capacity 1 +
// drain-before-select discards any signals that fired during the
// active background_tick — only refreshes that hit during the sleep
// window cause an interval reset. Per Gemini 3.1 Pro v0.4.20 review.
static MANUAL_REFRESH_TX: OnceLock<mpsc::Sender<()>> = OnceLock::new();

pub fn poke_manual_refresh() {
    if let Some(tx) = MANUAL_REFRESH_TX.get() {
        let _ = tx.try_send(());
    }
}

// in spawn_background_sync:
let (tx, mut rx) = mpsc::channel::<()>(1);
let _ = MANUAL_REFRESH_TX.set(tx);

loop {
    background_tick().await;
    // Drain any signals that landed during the active tick — those
    // already got their result via the manual sync_now path; we only
    // want to react to clicks that happen while we're idle.
    while rx.try_recv().is_ok() {}
    tokio::select! {
        _ = tokio::time::sleep(SYNC_INTERVAL) => {}
        _ = rx.recv() => {
            log::debug!("background tick reset by manual refresh during idle window");
        }
    }
}
```

Then `sync_now` Tauri command's existing implementation gets ONE extra line at the end:
```rust
poke_manual_refresh();
```

### Tests

- Unit test: spawn the loop in a tokio test with `SYNC_INTERVAL` patched to 5 seconds, fire `poke_manual_refresh()` after 1s, assert that the loop's next sleep returns within 100ms (NOT 5s).

### Risk

Low. The Notify pattern is idiomatic tokio. Worst case: the test catches a regression.

---

## Item 2 — Per-provider client-side error tracking + UI badge

### Background

When `collect()` returns `None` for a specific provider (e.g. Gemini's OAuth refresh fails), today the user sees:
- Server-side: `provider_quotas` row not updated → `updated_at` ages → eventually the v0.4.15 "stale" badge fires (>6 min).
- Client-side: nothing visible. The user sees the same card with cached data.

Gap: there's a 6-minute window where collection has been failing but the UI looks fine. Worse: if the provider was NEVER successfully collected (just signed in, refresh broken), the row may not exist server-side at all — no "stale" because no data.

### Implementation

Add a client-side `last_error: Option<String>` per provider tracked in `quota::collect_all`. When a provider's `collect()` returns `None`, populate the error with a short reason from the warn-log message pattern (e.g., "OAuth refresh failed", "HTTP 401", "creds file absent"). On success, clear it.

Two layers:

**Backend (`src-tauri/src/quota/mod.rs`):**

```rust
pub struct CollectorOutcome {
    pub provider: &'static str,
    pub snapshot: Option<QuotaSnapshot>,
    pub error: Option<String>,  // Some when snapshot is None and we know why
}

pub async fn collect_all_with_status() -> Vec<CollectorOutcome> { ... }
```

Each `<provider>::collect()` would optionally return `Result<Option<QuotaSnapshot>, String>` so the orchestrator can capture the error. Today they all return `Option<QuotaSnapshot>` and log the reason via `warn!`. We'd need to either:

a) Refactor each `collect()` to return `Result` (touches all 6 collector modules), OR
b) Use a thread-local string set inside each collector via `tracing` or a custom helper, picked up by the orchestrator. (More magic, less clear.)

**Decision: option (a) is cleaner. Half-day of mechanical refactor; each collector already has a single `return None` / `return Some(...)` exit path so threading through `Result` is straightforward.**

**Per Gemini 3.1 Pro v0.4.20 review (suggestion 2):** use a custom error type, not raw `String`:

```rust
// in src-tauri/src/quota/mod.rs
#[derive(Debug, Clone, Serialize)]
pub enum CollectorError {
    /// Credentials file absent / no access token / expected "user not signed in" state.
    NotConfigured(String),
    /// HTTP failure (4xx/5xx, network timeout, rate-limited).
    Http(String),
    /// JSON parse failure (schema drift) or atomic write-back failure.
    SchemaOrIo(String),
    /// OAuth refresh failed (active refresh path returned Err).
    RefreshFailed(String),
}
```

Wraps a `String` for now, but gives a clean path to typed pattern-matching in the orchestrator (e.g. distinguish "auth expired" from "rate-limited" for retry policy) without revisiting the collector signatures.

**Frontend (`src/App.tsx`):**

Expose `collect_all_with_status` via a new Tauri command `get_last_collector_status`. Call it on mount + after every `forceRefresh`. State per-provider: `{ ok: bool, error: string | null }`.

Render a red error badge next to the provider name on the card when `error != null`, with the error message in the title attribute. Color/style mirrors the v0.4.15 amber "stale" badge but in red.

i18n keys (3 langs):
- `providers.error_badge` — "error" / "异常" / "エラー"
- `providers.error_tooltip` — "Last sync failed: {{reason}}. Try Refresh now or check Settings → Integrations."

### Tests

- Backend: each collector's error path returns the expected `Err(String)` with the warn-message text.
- Frontend vitest: mock `get_last_collector_status` returning `{ Claude: { ok: false, error: "..." } }` and assert the badge renders.

### Risk

Medium. The Result-return refactor across 6 collectors is mechanical but touches every quota file. Order of changes matters — do one collector at a time, run that collector's tests, then move on. If the refactor goes wrong, we get compile errors, not runtime breakage (good).

**Defer trigger:** if the refactor balloons past 200 LOC across the 6 files, drop Item 2 from this release and ship just Items 1+3. Re-cut Item 2 as a standalone v0.4.21.

---

## Item 3 — Settings → Integrations "Storage" line

### Background

v0.4.16 added `provider_creds_backend` to `DiagnosticSnapshot` ("os_keychain" or "file"). v0.4.17 wired it into the Copy Diagnostic clipboard string. Gemini's v0.4.16 review specifically said: "make this state visible in the UI rather than relying solely on a one-time INFO log." We did half the job (Copy Diagnostic) but the SETTINGS UI itself doesn't show backend choice — a Linux user without `libsecret` who never clicks "Copy Diagnostic" silently gets file storage.

### Implementation

In `App.tsx::IntegrationsSection`, add a single line at the top.

**Per Gemini 3.1 Pro review (suggestion 3):** for the file-fallback (degraded) state, attach the tooltip to a small ⚠ icon next to the amber text. Plain-text tooltips are easy for users to miss; an icon signals "there's actionable info here" much more discoverably.

```tsx
<div className="text-xs text-neutral-500 mb-3">
  {t("settings.integrations.storage_label")}:{" "}
  {view.storage_backend === "os_keychain" ? (
    <span className="text-emerald-300">{t("settings.integrations.storage_os_keychain")}</span>
  ) : (
    <>
      <span className="text-amber-300">{t("settings.integrations.storage_file")}</span>
      {" "}
      <span
        className="cursor-help text-amber-400"
        title={t("settings.integrations.storage_file_tooltip") || ""}
        aria-label="info"
      >
        ⚠
      </span>
    </>
  )}
</div>
```

`ProviderCredsView` (Tauri response) currently doesn't include the backend; add it (mirror to `provider_creds.rs::current_backend()`).

i18n keys (3 langs):
- `settings.integrations.storage_label` — "Storage" / "存储位置" / "保存先"
- `settings.integrations.storage_os_keychain` — "OS keychain" / "系统凭据管理器" / "OS キーチェーン"
- `settings.integrations.storage_file` — "file (keyring unavailable)" / "文件 (系统凭据管理器不可用)" / "ファイル (キーリング使用不可)"
- `settings.integrations.storage_file_tooltip` — "Install gnome-keyring or kwallet on Linux to enable OS keychain storage."

### Tests

Frontend vitest: render IntegrationsSection with mocked `os_keychain` view, assert emerald label shows. Same with `file`, assert amber label + tooltip.

### Risk

Low. Pure rendering change.

---

## Sequencing

Implementation order:

1. **Item 3 first** — smallest, validates the i18n key pattern.
2. **Item 1 second** — contained, well-known tokio pattern.
3. **Item 2 last** — touches 6 collector files. If anything threatens to balloon, drop this item and re-cut as v0.4.21.

Single VM verification at the end:
- Item 1: click "Refresh now", check log shows the next background tick INTERRUPTED at the manual click time, NOT 120s later.
- Item 2: cause a Gemini auth failure (manually corrupt `~/.gemini/oauth_creds.json` to bad JSON), verify red error badge appears on Gemini card with parse-error reason in tooltip.
- Item 3: open Settings → Integrations, see "Storage: OS keychain" emerald line at the top.

---

## Out of scope (explicitly deferred)

- **Onboarding flow for new users** — bigger UX effort. Deserves its own plan.
- **Cost forecasting** ("at current rate you'll spend $X by month-end") — Mac sibling app has this, desktop doesn't. Worth porting later.
- **`_json_ref_placeholder` / `wipe_all` cleanup** — same as v0.4.19's deferred list.
- **Anomaly detection / model-recommendation features** — exploratory product features, not reliability/polish.
- **Codex `last_refresh` window tuning** — no incident yet.

---

## Review questions for Gemini 3.1 Pro

1. **Item 1 race:** if `notify_one()` fires while the loop is mid-tick (not waiting on `.notified()`), the notify is buffered. The NEXT iteration's `select!` consumes the buffered notify and resets the cycle to ~0s instead of running the planned 120s sleep. Is that desired? Should I instead `Notify::new_with_no_initial_permit()` or similar to explicitly drop unmatched notifies?

2. **Item 2 refactor scope:** option (a) refactors 6 collector signatures from `Option<QuotaSnapshot>` to `Result<Option<QuotaSnapshot>, String>`. Is there a cleaner pattern — e.g., a `tracing::Span`-based error capture that lets the existing `warn!` calls pass-through and the orchestrator harvests the latest warn-message keyed on a thread-local provider tag?

3. **Item 3 backend addition:** I'm extending `ProviderCredsView` to include `storage_backend`. Is that the right shape, or should I fetch backend separately via a dedicated `get_storage_backend` Tauri command to keep `ProviderCredsView` focused on creds-state-only?

4. **Bundling:** Items 1+3 are tiny. Item 2 is medium-sized and touches more files. Bundle all three OR split Item 2 to v0.4.21?
