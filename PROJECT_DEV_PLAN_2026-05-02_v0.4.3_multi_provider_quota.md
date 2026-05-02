# v0.4.3 — Multi-provider quota collection (Codex / Cursor / Gemini / Copilot / OpenRouter)

**Status:** spec — pending Codex + Gemini reviews.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-02).
**Tracks:** v0.4.2 promotion + user observation 2026-05-02 ("不只是 Claude — 别的 provider 的 quota 也要能被读取").
**Parent:** v0.4.2 (Pre-release).

## 1. Problem

v0.4.0 / 0.4.1 / 0.4.2 ported only Claude OAuth quota to the Win / Linux
Tauri desktop. The Mac Swift menu-bar app ships **21 collectors** in
`CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/Collectors/` covering
Claude, Codex, Cursor, Gemini, Copilot, OpenRouter, plus 15 niche / China
providers. Users running Win / Linux only get Claude tier bars; every
other provider's `provider_quotas` row stays empty unless a Mac is
also paired and online for the same account.

The user-visible defect: a Win/Linux user with a Cursor + Copilot stack
sees "Quota data unavailable" on those cards even when their tokens are
present locally.

## 2. Goal

After v0.4.3, the Win / Linux desktop scrapes 5 additional providers
itself and uploads each one via `helper_sync`'s existing
`p_provider_tiers` map (which already supports multi-provider —
`for v_provider in jsonb_object_keys(p_provider_tiers) loop ... end`).
A signed-in desktop sees real quota tier bars for any of:
**Claude, Codex, Cursor, Gemini, Copilot, OpenRouter** — within ~2 min
of starting the app, regardless of Mac state.

### Scope (6 providers total)

| Provider | Auth | Refresh? | Mac collector |
|---|---|---|---|
| Claude | `~/.claude/.credentials.json` | passive (waits for `claude` CLI) | already shipped (v0.4.0–0.4.2) |
| Codex | `~/.codex/auth.json` | active (8-day staleness, OpenAI OAuth) | `CodexCollector.swift` |
| Cursor | env `CURSOR_COOKIE` (no UI yet) | none (cookie based) | `CursorCollector.swift` |
| Gemini | `~/.gemini/oauth_creds.json` (file-only) | passive (waits for `gemini` CLI) | `GeminiCollector.swift` |
| Copilot | env `COPILOT_API_TOKEN` | none | `CopilotCollector.swift` |
| OpenRouter | env `OPENROUTER_API_KEY` | none | `OpenRouterCollector.swift` |

### Non-goals

- **Active Gemini OAuth refresh** with PKCE + browser redirect listener.
  Mac's `GeminiOAuthManager.swift` does this via macOS-native
  `ASWebAuthenticationSession`; porting to Tauri requires a webview
  redirect listener + `keyring` crate. Deferred to v0.4.5+.
- **Per-provider on/off toggle UI**. Mac respects `config.isEnabled`
  (`HelperDaemon.swift:228`); desktop has no Settings UI for this yet.
  v0.4.3 ships "all six on by default"; a v0.4.4 UI sprint can add the
  toggle.
- **Cookie/token entry UI** for Cursor / Copilot / OpenRouter. v0.4.3
  reads only from env vars. Users without env vars get the existing
  empty-state. v0.4.4 adds a Settings → Provider Credentials panel.
- **China providers** (Kimi / KimiK2 / GLM / Zai / MiniMax / Alibaba /
  VolcanoEngine). Each has a different API; v0.4.4+ separate sprint.
- **Local-only providers** (Ollama). No quota concept.
- **Aggregator-of-aggregators** (Augment, JetBrainsAI, Warp, Kilo). Niche;
  v0.4.4+.

### Plus one **opportunistic** fix

- Any new token formats that hit error paths must be added to the Sentry
  scrubber regex set (currently covers JWTs + `sk-ant-*`). v0.4.3 adds:
  OpenAI tokens (`sk-proj-*`, `sk-*`), GitHub PATs (`ghp_*`, `ghs_*`,
  `gho_*`, `ghu_*`), OpenRouter (`sk-or-*`), Google OAuth bearer
  (`ya29.*`). Cursor session cookies are opaque and harder to regex
  reliably — we'll pattern on `Cookie:` header redaction instead.

## 3. Decisions

**Module structure: `src-tauri/src/quota/<provider>.rs`.** Move existing
v0.4.2 `quota.rs` to `quota/claude.rs` and add 5 sibling modules. The
new `quota/mod.rs` becomes the orchestrator that runs all collectors
**concurrently** via `tokio::join!` and aggregates results into the
multi-provider payload `lib.rs::sync_now` ships.

```
src-tauri/src/quota/
├── mod.rs           # orchestrator, public API
├── claude.rs        # moved from quota.rs
├── codex.rs
├── cursor.rs
├── gemini.rs
├── copilot.rs
└── openrouter.rs
```

This is a breaking import path internally (`quota::collect_claude` →
`quota::claude::collect`) but contained — only `lib.rs::sync_now` calls it.

**Concurrent collection with panic isolation.** Mac runs collectors
sequentially (`HelperDaemon.swift` for-loop). At 6 providers × ~1s HTTP
each, that's 6s per sync cycle sequential. We run them concurrently —
but **NOT** via `tokio::join!` (per Codex 2026-05-02 review: `join!`
runs branches on the same task, so a panic in any arm unwinds the
parent task and kills `sync_now`). Instead use `tokio::spawn` per arm +
collect via `JoinHandle::await`, branching on `JoinError::is_panic()`:

```rust
let tasks = vec![
    ("Claude", tokio::spawn(claude::collect())),
    ("Codex", tokio::spawn(codex::collect())),
    // ... 4 more
];
for (name, t) in tasks {
    match t.await {
        Ok(Some(snap)) => out.push((name, snap)),
        Ok(None) => { /* best-effort failure already logged */ }
        Err(e) if e.is_panic() => log::error!("Provider {name} panicked: {e}"),
        Err(e) => log::warn!("Provider {name} task cancelled: {e}"),
    }
}
```

Each `collect()` returns `'static + Send` — no captured references —
so spawn is sound. Total wall time bounded by the slowest single fetch
(~1.5s typical, 30s timeout for Codex). A panic in one arm logs at
ERROR level and the rest still upload.

**Error isolation.** Each provider's `collect()` is best-effort: missing
creds, expired tokens, HTTP errors, parse errors all return `None` with
a structured per-provider WARN/DEBUG log (see §4.8). The orchestrator
filters Nones out of the upload map. Non-empty results upload via
`helper_sync`; absent ones leave the row untouched (helper_sync's
`jsonb_object_keys` loop is a no-op for absent keys, confirmed in
v0.4.2 audit).

**Token-expired UX deferred.** Gemini 3.1 Pro 2026-05-02 review flagged
that file-source Gemini tokens silently skipping looks like
unexplained data loss to the user. Codex 2026-05-02 review confirmed
that helper_sync's empty-key no-op leaves the previous row in place
(stale, but not cleared), which matches Mac's current behavior. v0.4.3
ships with the silent-skip semantic + per-provider WARN logs; **v0.4.4
adds an explicit `CollectorStatus { Ok(snap), Expired(reason),
Missing(reason), Error(reason) }` enum + UI warning state** so the
user sees "Run `gemini` CLI to refresh" instead of a stale card.

**Scaling / unit conventions.** Mirrors Mac for cross-writer
correctness — same dual-writer concern as v0.4.2 INV-1/3/4/5.

| Provider | quota / remaining unit | Rationale |
|---|---|---|
| Claude | 0–100 (percentage) | matches Mac / iOS convention |
| Codex | 0–100 (percentage) for windows; balance scaled `× 100_000` | matches `CodexCollector.swift:237` |
| Cursor | cents (`planLimitCents`) | matches `CursorCollector.swift:90-96` |
| Gemini | 0–100 (percentage from `remainingFraction × 100`) | matches `GeminiCollector.swift:264` |
| Copilot | absolute (`Int(entitlement)`) | matches `CopilotCollector.swift:92-93` |
| OpenRouter | dollars × 100_000 | matches `OpenRouterCollector.swift:119-122` |

Any deviation from Mac means alternate-writer flicker per the v0.4.2
finding. Each sub-module gets a `// Mirrors <Mac file>:<line> for
dual-writer parity` comment cross-referencing the source line.

**Refresh strategy decision tree.**
- Claude: passive (existing). v0.4.5+ may add active.
- Codex: **active**. 8-day staleness check + POST to
  `auth.openai.com/oauth/token` with public `client_id`. On failure,
  proceed with the existing access token (Mac line 30-35: non-fatal).
  If Mac's flow fails, our cycle still emits the snapshot since
  Anthropic-style "best effort" works here too.
- Cursor: no refresh (cookies don't refresh).
- Gemini: **passive only** for v0.4.3 (file-source). If `expiry_date`
  in `~/.gemini/oauth_creds.json` is past, silent skip + log debug.
  Document UX: user must run `gemini` CLI periodically to keep file
  fresh. Active refresh = v0.4.5+ alongside the Tauri OAuth UI sprint.
- Copilot, OpenRouter: static tokens, no refresh.

**Sentry token scrubbing.** Today's regex covers JWTs +
`sk-ant-(oat|api|sid)*`. Extend to:
- OpenAI: `sk-proj-[A-Za-z0-9_-]{30,}`, `sk-[A-Za-z0-9]{32,}`
- GitHub: `gh[pousr]_[A-Za-z0-9]{36,}` (PAT, OAuth, server-token, refresh, user)
- OpenRouter: `sk-or-(?:v\d-)?[A-Za-z0-9]{40,}`
- Google OAuth: `ya29\.[A-Za-z0-9_-]{40,}`

Cursor cookies aren't a single regex-friendly format; we redact any
substring after `Cookie:` header label up to the next newline /
whitespace boundary, matching the v0.4.0 Bearer-header pattern.

**Per-provider toggles deferred.** v0.4.3 collects all 6 unconditionally
when creds are present. Each per-provider config (env var or file)
acts as the implicit toggle: no creds → no collection. Future Settings
UI = v0.4.4.

## 4. Implementation

### 4.0 Module structure

#### `src-tauri/src/quota/mod.rs` (~120 LOC)

```rust
//! Multi-provider quota collection. Each `<provider>::collect()` is
//! best-effort — `None` means "no upload this cycle, leave server state
//! untouched". Concurrent execution via `tokio::join!` bounds wall time
//! by the slowest single fetch.

pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod openrouter;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct QuotaSnapshot {
    pub plan_type: String,
    pub remaining: i64,
    pub quota: i64,
    pub session_reset: Option<String>,
    pub tiers: Vec<TierEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierEntry {
    pub name: String,
    pub quota: i64,
    pub remaining: i64,
    pub reset_time: Option<String>,
}

/// Run all 6 collectors concurrently with panic isolation. Returns a
/// vec of (provider_name, snapshot) — providers that returned None or
/// panicked are filtered out, with per-provider error logging.
///
/// Critical: uses `tokio::spawn` (not `tokio::join!`) so a panic in
/// one arm doesn't unwind the parent task and kill sync_now. Per
/// Codex 2026-05-02 review of v0.4.3 spec.
pub async fn collect_all() -> Vec<(&'static str, QuotaSnapshot)> {
    let tasks: Vec<(&'static str, tokio::task::JoinHandle<Option<QuotaSnapshot>>)> = vec![
        ("Claude", tokio::spawn(claude::collect())),
        ("Codex", tokio::spawn(codex::collect())),
        ("Cursor", tokio::spawn(cursor::collect())),
        ("Gemini", tokio::spawn(gemini::collect())),
        ("Copilot", tokio::spawn(copilot::collect())),
        ("OpenRouter", tokio::spawn(openrouter::collect())),
    ];
    let mut out = Vec::new();
    for (name, task) in tasks {
        match task.await {
            Ok(Some(snap)) => out.push((name, snap)),
            Ok(None) => { /* per-provider WARN already logged in collect() */ }
            Err(e) if e.is_panic() => log::error!("Provider {name} panicked: {e}"),
            Err(e) => log::warn!("Provider {name} task cancelled: {e}"),
        }
    }
    log::info!(
        "quota::collect_all → {} provider(s) populated: {}",
        out.len(),
        out.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
    );
    out
}

/// Provider name constants — must match Mac's `ProviderKind` raw
/// values at `Models.swift:10-37` exactly. A drift here causes
/// dual-writer inserts to land on different `(user_id, provider)` PKs
/// instead of converging on one row. Asserted by `provider_name_contract`
/// test in `mod tests` below using a checked-in snapshot of the Swift
/// enum (rebuilt manually when Mac adds providers — there's no compile-
/// time link between Swift and Rust).
const PROVIDER_CLAUDE: &str = "Claude";
const PROVIDER_CODEX: &str = "Codex";
const PROVIDER_CURSOR: &str = "Cursor";
const PROVIDER_GEMINI: &str = "Gemini";
const PROVIDER_COPILOT: &str = "Copilot";
const PROVIDER_OPENROUTER: &str = "OpenRouter";
```

Provider names match `Models.swift:10-37` `ProviderKind` raw values
verbatim — critical for `provider_quotas.(user_id, provider)` PK
agreement with Mac writes.

### 4.1 Codex collector — `quota/codex.rs` (~250 LOC)

**Auth source:** `~/.codex/auth.json`, JSON shape:
```json
{
  "tokens": {
    "access_token": "...",
    "refresh_token": "...",
    "id_token": "...",
    "account_id": "..."
  },
  "last_refresh": "2026-04-25T10:30:00Z",
  "OPENAI_API_KEY": "sk-proj-..."
}
```
The fallback `OPENAI_API_KEY` field is honored if `tokens.access_token`
is absent (`CodexCollector.swift:89-91`).

`CODEX_HOME` env var overrides `~/.codex` — match Mac's
`CodexCollector.swift:60-65`.

**Refresh:** if `last_refresh` is more than 8 days old (or absent),
POST to `https://auth.openai.com/oauth/token`:

```jsonc
// Headers: Content-Type: application/json
// Body:
{
  "client_id": "app_EMoamEEZ73f0CkXaXp7hrann",  // public, OK to ship
  "grant_type": "refresh_token",
  "refresh_token": "<from auth.json>",
  "scope": "openid profile email"
}
```

15s timeout. Response: `{access_token, refresh_token, id_token}`.
Write back to `auth.json` with updated `last_refresh = now()`. On
failure: proceed with the existing access token (non-fatal — Mac
line 30-35: `try? await refreshTokens` swallows failure).

**Usage endpoint:** `GET https://chatgpt.com/backend-api/wham/usage`,
30s timeout, headers:
```
Authorization: Bearer <access_token>
Accept: application/json
User-Agent: cli-pulse-desktop/<version>
ChatGPT-Account-Id: <account_id>     # if present in auth.json
```

**Response shape** (parsed in `parseUsage`, `CodexCollector.swift:220-246`):
```jsonc
{
  "plan_type": "Plus" | "Pro" | "Free" | ...,
  "rate_limit": {
    "primary_window": {"used_percent": 23, "reset_at": 1746800000.0, "limit_window_seconds": 18000},
    "secondary_window": {"used_percent": 50, "reset_at": "2026-05-09T00:00:00Z", ...}
  },
  "credits": {"has_credits": true, "unlimited": false, "balance": 5.43}
}
```

`reset_at` is **either epoch double OR ISO-8601 string** (Mac line
260-264 handles both). Use the same logic Claude does for `expires_at`.
`used_percent` is **either Int OR Float** (line 252-258) — coerce.

**Tier emission:**
- "5h Window" from `primary_window`: quota=100, remaining=100−used_percent, reset_time=ISO of reset_at
- "Weekly" from `secondary_window`: same shape
- "Credits" if `credits.has_credits`: quota=`Int(balance × 100_000)`,
  remaining=same, reset_time=None (credits are persistent)

**plan_type:** pass through verbatim from `plan_type` field. Mac
doesn't normalize (`CodexCollector.swift:225`).

**Outer session_reset:** `primary_window.reset_at` (matches the
"5h Window" tier's reset, mirrors Claude's `session_reset`).

**Tests** (8 unit tests):
- `parse_usage_full` (all 3 windows + credits)
- `parse_usage_no_credits` (free-tier user)
- `parse_usage_reset_at_epoch_double` / `..._iso_string`
- `parse_usage_used_percent_int` / `..._float`
- `read_auth_file_ok` / `read_auth_file_with_openai_api_key_fallback`
- `needs_refresh_8_days_threshold`

### 4.2 Cursor collector — `quota/cursor.rs` (~120 LOC)

**Auth source:** env `CURSOR_COOKIE` only (no file-based persistence
yet — UI is v0.4.4 work). If env empty → return None (no collection).

**Endpoint:** `GET https://cursor.com/api/usage-summary`, 15s timeout,
headers:
```
Cookie: <CURSOR_COOKIE value>
Accept: application/json
```

**Response shape** (`CursorCollector.swift:62-85`):
```jsonc
{
  "membershipType": "pro" | "free" | ...,
  "billingCycleEnd": "2026-05-31T00:00:00Z",
  "individualUsage": {
    "plan": {"used": 1234, "limit": 5000, "remaining": 3766, "totalPercentUsed": 24.7},
    "onDemand": {"used": 0, "limit": null}
  }
}
```

All numeric fields are cents (`CursorCollector.swift:74-78`). Keep as
cents in the upload — desktop UI knows how to format.

**Tier emission:**
- "Plan" if `plan.limit > 0`: quota=`limit`, remaining=`remaining` (or
  `limit - used` fallback), reset_time=`billingCycleEnd`
- "On-Demand" if `onDemand.limit > 0`: quota=`onDemand.limit`,
  remaining=`max(0, limit - used)`, reset_time=`billingCycleEnd`

**plan_type:** `membershipType` capitalized, default "Unknown" (matches
Mac line 111).

**Outer session_reset:** `billingCycleEnd`.

**Outer remaining/quota:** `plan.remaining` / `plan.limit` if available,
else None.

**Tests** (5 unit tests):
- `parse_response_full`
- `parse_response_no_on_demand`
- `parse_response_membership_unknown`
- `parse_response_remaining_fallback_to_limit_minus_used`
- `collect_skips_when_no_cookie_env`

### 4.3 Gemini collector — `quota/gemini.rs` (~280 LOC, file-only)

**Auth source:** `~/.gemini/oauth_creds.json`, JSON shape:
```json
{
  "access_token": "ya29....",
  "refresh_token": "1//0e...",
  "id_token": "eyJ...",
  "expiry_date": 1746800000000
}
```

`expiry_date` is epoch **milliseconds** (Mac line 102-104).

**Refresh:** **NOT in v0.4.3.** If `expiry_date < now()`, silent skip +
`log::debug!("Gemini OAuth token expired — skipping; run `gemini` CLI to
refresh")`. Returns None.

**Endpoints** (both POST, 10s timeout, JSON body):
1. **Tier discovery:** `https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist`
   ```jsonc
   // Body:
   {"metadata": {"ideType": "GEMINI_CLI", "pluginType": "GEMINI"}}
   ```
   Response: `{"currentTier": {"id": "free-tier" | "standard-tier" | "legacy-tier"}, "cloudaicompanionProject": "..." | {"projectId": "...", "id": "..."}}`
   (Mac line 173-181 handles both string and object shapes.)

2. **Quota:** `https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota`
   ```jsonc
   // Body (project optional):
   {"project": "<projectId or omitted>"}
   ```
   Response: `{"buckets": [{"modelId": "gemini-1.5-pro-001", "remainingFraction": 0.83, "resetTime": "2026-05-09T00:00:00Z"}, ...], "resetTime": "..."}`

**Tier mapping** (mirrors `GeminiCollector.swift:243-278`):
- Group buckets by **model family** via `classifyModel()`:
  ```rust
  fn classify_model(id: &str) -> &str {
      let l = id.to_lowercase();
      if l.contains("flash-lite") || l.contains("flash_lite") { "Flash Lite" }
      else if l.contains("flash") { "Flash" }
      else if l.contains("pro") { "Pro" }
      else { id }  // raw id when unknown
  }
  ```
- Within a family, keep the LOWEST `remainingFraction` (most-constrained
  bucket).
- Emit tiers in preferred order: `["Pro", "Flash", "Flash Lite"]` first,
  then any unknown families alphabetically.
- Each tier: name=family, quota=100, remaining=`Int(fraction × 100).clamp(0, 100)`,
  reset_time=bucket's resetTime.

**plan_type** (line 281-287):
- `standard-tier` → "Paid"
- `free-tier` → "Free"
- `legacy-tier` → "Legacy"
- else → "Unknown"

**Outer session_reset:** primary family's reset_time (matches Mac line
296: "Pro first, then Flash, then Flash Lite as fallback when Pro is
unavailable").

**Outer remaining:** primary family's percentLeft, else min across all
families, else 100 (line 293-295).

**Tests** (7 unit tests):
- `parse_quota_with_buckets`
- `parse_quota_global_reset_time_fallback`
- `classify_model_flash_lite_pro_unknown`
- `family_lowest_fraction_wins`
- `tier_order_pro_flash_flash_lite_then_unknown`
- `plan_type_tier_id_buckets`
- `read_creds_file_ok`

### 4.4 Copilot collector — `quota/copilot.rs` (~140 LOC)

**Auth source:** env `COPILOT_API_TOKEN` only. If absent → None.

**Endpoint:** `GET https://api.github.com/copilot_internal/user`, 15s,
headers:
```
Authorization: token <COPILOT_API_TOKEN>     # NOT "Bearer"
Accept: application/json
Editor-Version: vscode/1.96.2
Editor-Plugin-Version: copilot-chat/0.26.7
User-Agent: GitHubCopilotChat/0.26.7
```

The `Editor-*` and `User-Agent` headers are GitHub Copilot internal API
quirks (Mac line 38-40). They're required — without them the endpoint
401s. Keep verbatim.

**Response shape** (Mac line 62-85, snake_case + camelCase fallbacks):
```jsonc
{
  "copilotPlan": "business" | "individual" | "unknown",
  "quotaResetDate": "2026-05-09T00:00:00Z",
  "quotaSnapshots": {
    "premiumInteractions": {"entitlement": 300.0, "remaining": 245.0, "percentRemaining": 81.6},
    "chat": {"entitlement": 1000.0, "remaining": 850.0, "percentRemaining": 85.0}
  }
}
```

Some accounts ship snake_case (`copilot_plan`, `quota_reset_date`,
`quota_snapshots`, `percent_remaining`); accept both for each key.

**Tier emission:**
- "Premium" if `premiumInteractions.entitlement > 0`: quota=`Int(entitlement)`,
  remaining=`Int(remaining ?? percentRemaining/100 × entitlement ?? entitlement)`,
  reset_time=`quotaResetDate`
- "Chat" same shape from `chat`

**plan_type:** `copilotPlan.capitalize()`, default "Unknown".

**Outer session_reset:** `quotaResetDate`.
**Outer remaining/quota:** premium tier's values.

**Tests** (5 unit tests):
- `parse_response_camel_case` / `..._snake_case`
- `parse_response_chat_only_no_premium`
- `tier_remaining_falls_back_to_pct_then_entitlement`
- `plan_type_unknown_default`

### 4.5 OpenRouter collector — `quota/openrouter.rs` (~180 LOC)

**Auth source:** env `OPENROUTER_API_KEY`. Optional env
`OPENROUTER_API_URL` to override base URL (default
`https://openrouter.ai/api/v1`).

**Endpoints:**
1. **Credits (required):** `GET <base>/credits`, 15s timeout, header
   `Authorization: Bearer <key>`. Response: `{"data": {"total_credits": 5.43, "total_usage": 1.20}}`.
   `balance = max(0, total_credits - total_usage)` (Mac line 77).
2. **Key info (optional):** `GET <base>/key`, **3s** timeout, same auth.
   Response: `{"data": {"limit": 10.0, "usage": 4.5, "rate_limit": {"requests": 100, "interval": "10s"}}}`.
   Wrap in `try` — failure is non-fatal.

**Tier emission** (Mac line 124-142):
- "Credits" (always emit): quota=`Int(total_credits × 100_000)`,
  remaining=`Int(balance × 100_000)`, reset_time=None (credits don't
  reset on schedule).
- "Key Limit" (if `keyInfo.limit > 0`): quota=`Int(limit × 100_000)`,
  remaining=`Int(max(0, limit - usage) × 100_000)`, reset_time=None.

**plan_type:** "Credits" hardcoded (line 154).

**Outer session_reset:** None.
**Outer quota / remaining:** Credits tier values.

**Tests** (5 unit tests):
- `parse_credits_response`
- `parse_key_response_full`
- `parse_key_response_missing_rate_limit`
- `balance_clamps_negative_to_zero`
- `key_info_failure_doesnt_abort_credits`

### 4.6 Per-provider structured logging — `quota::*::collect()`

Per Gemini 3.1 Pro 2026-05-02 review: the orchestrator's
`quota::collect_all → 0 provider(s) populated` line is insufficient
for diagnosing real-user issues. Each `collect()` MUST log its
specific failure reason at WARN (or DEBUG for "no creds yet" routine
absence) so support can read `cli-pulse.log` and tell apart:

| Symptom | Expected log |
|---|---|
| `~/.codex/auth.json` absent | `DEBUG [Codex] auth.json absent — skipping` |
| Codex token refresh failed | `WARN [Codex] OAuth refresh failed (non-fatal): {e}` |
| `/wham/usage` 401 | `WARN [Codex] /wham/usage returned 401 — token revoked?` |
| Cursor env var unset | `DEBUG [Cursor] CURSOR_COOKIE env var not set — skipping` |
| Cursor 401 | `WARN [Cursor] /usage-summary returned 401 — cookie likely expired` |
| Gemini file expired | `DEBUG [Gemini] oauth_creds.json expiry_date past now() — run \`gemini\` CLI to refresh` |
| Copilot env var unset | `DEBUG [Copilot] COPILOT_API_TOKEN env var not set — skipping` |
| OpenRouter env var unset | `DEBUG [OpenRouter] OPENROUTER_API_KEY env var not set — skipping` |

The convention: `[Provider]` prefix at the start of the message so
`grep '\[Codex\]' cli-pulse.log` works as a triage tool. Each
provider's `collect()` is responsible for its own logs.

### 4.7b `lib.rs::sync_now` wiring

Replace the v0.4.2 single-Claude block with:

```rust
let snaps = quota::collect_all().await;

let mut tier_map = serde_json::Map::new();
let mut remaining_map = serde_json::Map::new();
for (name, snap) in &snaps {
    tier_map.insert(name.to_string(), json!({
        "quota": snap.quota,
        "remaining": snap.remaining,
        "plan_type": snap.plan_type,
        "reset_time": snap.session_reset,
        "tiers": snap.tiers,
    }));
    remaining_map.insert(name.to_string(), json!(snap.remaining));
}
let p_provider_remaining = serde_json::Value::Object(remaining_map);
let p_provider_tiers = serde_json::Value::Object(tier_map);
```

`helper_sync` already handles multi-key maps (verified in v0.4.2 audit
of the function source: `for v_provider in jsonb_object_keys(...) loop`).
Empty `{}` is still a no-op so the code path keeps working when all 6
return None.

### 4.7 Sentry scrubber additions — `sentry_init.rs`

Add **6** new regex Lazy values (Codex review caught the count typo,
also flagged missing coverage for `github_pat_*` 47-char format and
generic `Authorization: Bearer opaque-token` redaction):

```rust
// OpenAI tokens — covers both legacy `sk-...` and new `sk-proj-...`,
// `sk-svcacct-...` formats. `\b` boundary is reliable here because
// OpenAI tokens end in alphanumeric.
static OPENAI_TOKEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bsk-(?:proj|svcacct)?-?[A-Za-z0-9_\-]{30,}\b").unwrap()
});

// GitHub tokens — TWO formats: legacy `gh[pousr]_...` and new
// `github_pat_...` (47-char body after the prefix). Codex review:
// the gh[pousr]_ regex alone misses the new 2024+ PAT format.
static GITHUB_TOKEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b").unwrap()
});
static GITHUB_PAT_NEW: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{40,90}\b").unwrap()
});

static OPENROUTER_TOKEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bsk-or-(?:v\d+-)?[A-Za-z0-9]{40,}\b").unwrap()
});

static GOOGLE_OAUTH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bya29\.[A-Za-z0-9_\-]{40,}\b").unwrap()
});

// Cookie + generic Authorization Bearer scrubbing. Codex review: a
// `Cookie:`-only regex misses `Authorization: Bearer <opaque>` for
// providers whose tokens don't match a known regex (e.g., a future
// Cursor session token rendered as Bearer instead of Cookie).
static COOKIE_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?im)^(Cookie):\s*[^\r\n]+").unwrap()
});
static AUTH_BEARER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?im)(Authorization):\s*Bearer\s+[^\r\n]+").unwrap()
});
```

Replace cookies with `Cookie: <redacted>`, Bearer with
`Authorization: Bearer <redacted>`. Replace token matches with
`<openai-token-redacted>` / `<github-token-redacted>` / etc.

The `(?m)` multiline flag is critical so `^` matches per-line
boundaries when these headers appear inside multi-header strings.

Tests (8 new — was 5, expanded for new GitHub PAT + Bearer scope):
- `redacts_openai_proj_token`
- `redacts_openai_svcacct_token`
- `redacts_github_pat_legacy_ghp`
- `redacts_github_pat_new_format` ← new
- `redacts_openrouter_v1_key`
- `redacts_google_ya29_bearer`
- `redacts_cookie_header_value`
- `redacts_generic_authorization_bearer` ← new

## 5. Tests

### 5.1 Rust unit tests

**Per-provider:** see §4.1–4.5 (8 / 5 / 7 / 5 / 5 = 30 tests).

**Sentry:** 5 new regex tests in `sentry_init.rs`.

**Orchestrator (`quota::collect_all`):** integration-style tests using
test doubles (mock HTTP) — 3 tests covering all-providers-OK,
some-providers-fail, all-providers-fail. Likely ~80 LOC of mock setup;
cost-benefit acceptable for the central data path.

Total: **~38 new unit tests** (was 17 in v0.4.2).

### 5.2 E2E (VM Claude on Win)

E2E becomes more interesting because each provider has different
preconditions:

- **Claude / Codex / Gemini**: rely on respective CLI having signed
  in (`.credentials.json`, `auth.json`, `oauth_creds.json` populated).
  VM's host-managed Claude env doesn't have any of these — same
  empty-state validation as v0.4.2.
- **Cursor / Copilot / OpenRouter**: env-var driven. VM Claude can
  optionally `set CURSOR_COOKIE=fake_cookie` etc. and verify the empty
  state vs HTTP error path. Real validation deferred to passive
  Pre-release watch.

VM E2E focuses on:
1. Upgrade from v0.4.2 → v0.4.3 doesn't break sessions / pairing /
   alerts.
2. All 6 provider cards still render the empty-state copy when no creds
   on this machine.
3. New log line `quota::collect_all → 0 provider(s) populated` appears
   per cycle (since VM is host-managed).
4. No new panics / Sentry warns from the multi-collector orchestrator.

### 5.3 Real-world validation

Same passive Pre-release watch pattern as v0.4.0/0.4.1/0.4.2. After the
release ships:
- Real users with `~/.codex/auth.json` see Codex tier bars within ~2 min
- Same for `~/.gemini/oauth_creds.json`
- Users who set `CURSOR_COOKIE` etc. env vars see those bars
- 48h watch on Sentry / GitHub Issues; no investor reports → promote.

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| ChatGPT / OpenAI rate-limit `/wham/usage` | We call once per 2-min sync per device. Below public rate limit. 30s timeout. |
| `auth.openai.com/oauth/token` rejects `client_id` (it's tied to Mac OAuth registration) | The client_id is public per OAuth spec — already shipped in `CodexCollector.swift:134`. PKCE-style refresh works without client secret. If OpenAI rejects from a non-Mac UA, we fall back to using the existing access_token without refresh (matches Mac's `try?` non-fatal behavior). |
| Cursor cookies expire silently → 401 on next sync | Treat 401 as transient skip (no retry, no row-wipe). `helper_sync({})` is no-op (verified v0.4.2). User needs to re-export cookie. UI in v0.4.4 will surface this. |
| Gemini file `expiry_date` always behind because user runs `gemini` CLI rarely | Document: "v0.4.3 reads `~/.gemini/oauth_creds.json` only. Run `gemini` CLI to keep it fresh. Active refresh in v0.4.5+." |
| OpenAI / Google / GitHub return 4xx with the token in error body | Sentry scrubber (§4.7) catches before upload. Non-success bodies are also truncated to 120 chars per `quota.rs::fetch_usage` pattern. |
| Concurrent collection causes spurious cross-provider failures (e.g., one network blip) | Each `tokio::join!` arm has its own timeout (3s–30s). One slow / failing arm doesn't propagate — `Option<QuotaSnapshot>` per arm. Verified by orchestrator tests. |
| Mac and Win/Linux send different `provider` strings (Codex vs codex vs OpenAI Codex) | Use `ProviderKind` raw values from `Models.swift:10-37` verbatim. v0.4.3 hardcodes the same strings. Cross-checked in §4.0. |
| OpenRouter's $1 = 100,000 unit scaling causes integer overflow at high balances | Max representable `i64` at 100k scale is $9.2 quintillion. Safe. |
| Schema's `provider_quotas.quota` / `remaining` are `integer` (i32) | **Inherited Mac bug** — Mac (`OpenRouterCollector.swift:119-122`) also scales $1=100k without clamp; helper_sync (`helper_rpc.sql:245-246`) casts to integer. At balance > ~$21k the server `INSERT` errors out. Per Codex 2026-05-02 review: clamp/rescale would break Mac parity, only correct fix is bigint migration. **v0.4.3 inherits the bug rather than fix it** — bigint migration is a backend schema change requiring user approval (per `feedback_cli_pulse_autonomy.md` rules) and is split into a separate v0.4.4+ task. Realistic exposure: < 0.001% of users have OpenRouter balance > $21k. |
| User sets a fake `CURSOR_COOKIE` to disable Cursor (using empty-string trick) | Empty string fails the `!isEmpty` guard, returns None, no upload. ✓ |
| Multiple writers race on `provider_quotas.(user_id, "Codex")` row from Mac + Win simultaneously | Same as Claude: helper_sync uses `ON CONFLICT DO UPDATE`, last-writer-wins. Both clients hit same upstream API → values converge. Subject to v0.4.2 INV-1/3/4/5 alignment for non-Claude providers (handled in §4.1–4.5 by mirroring Mac code line-for-line). |

## 7. Milestones (~3 days)

| Day | Work |
|---|---|
| 0.5 | Module structure + move `quota.rs` → `quota/claude.rs`. Orchestrator skeleton. cargo fmt + clippy + tests still pass. |
| 1 | Codex + Cursor + Copilot + OpenRouter modules with unit tests. (4 trivial-to-moderate ports.) ~24 unit tests. |
| 1.5 | Gemini module (file-only) + tests. ~7 unit tests. Concurrent orchestrator wiring + 3 integration tests. |
| 2 | Sentry scrubber + 5 tests. lib.rs::sync_now refactor. End-to-end cargo test green. CHANGELOG + version bump. |
| 2.5 | Codex GPT-5.4 review of full diff. Iterate on FIX-FIRSTs. |
| 3 | Push v0.4.3, watch CI, hand to VM Claude for E2E. 48h Pre-release watch. Promote to Latest. |

## 8. Backward compatibility

- v0.4.x desktops on auto-update: unaffected. Server schema unchanged.
- iOS / Android: unaffected. They consume `provider_summary`, which now
  sees more provider rows from desktops (bonus).
- Mac scanner: unaffected. Continues uploading via its own collectors.
  Last-writer-wins on each `(user_id, provider)` row; both writers use
  same upstream APIs so values converge.
- Per-provider `provider_quotas` row schema unchanged: same columns
  Mac has been writing for months. We just add 5 new providers'
  desktop-side writers.

## 9. Out of scope (deferred)

- **Active Gemini OAuth + UI** (v0.4.5).
- **Per-provider on/off toggle UI** (v0.4.4).
- **Cookie/token entry UI** for Cursor/Copilot/OpenRouter (v0.4.4).
- **China provider collectors** (Kimi/GLM/Zai/MiniMax/Alibaba/Volcano)
  (v0.4.4 separate sprint).
- **Niche aggregators** (Augment/JetBrainsAI/Warp/Kilo) (v0.4.5+).
- **Local providers** (Ollama) — no quota concept.

## 10. Decisions closed by Codex + Gemini reviews (2026-05-02)

1. **Codex `client_id` ship-it sanity.** ✅ SHIP. Per Codex review:
   public OAuth client_id is non-secret per RFC 6749 §2.2; refresh
   request sends only `Content-Type` header. No evidence OpenAI
   enforces UA. Win/Linux smoke during Pre-release window will
   validate. Document if a 401 ever appears.
2. **i32 overflow on OpenRouter scale.** ⏸ DEFERRED to v0.4.4+. Codex
   recommended bigint migration (clamp/rescale would break Mac parity);
   this requires backend schema change which by `feedback_cli_pulse_autonomy.md`
   needs explicit user approval. v0.4.3 inherits the existing Mac bug —
   < 0.001% of users have $21k+ OpenRouter balance. Tracked as v0.4.4
   schema-migration sprint candidate.
3. **Cookie redaction granularity.** ✅ Multi-line + case-insensitive
   `(?im)` flags applied (§4.7). Plus added generic `Authorization:
   Bearer ...` regex per Codex review.
4. **Concurrent vs sequential.** ✅ Concurrent via `tokio::spawn`
   per-arm + `JoinHandle::await` with `is_panic()` (NOT `tokio::join!`
   per Codex review — would have leaked panics to parent task).
   Updated in §3 + §4.0 orchestrator code.
5. **Provider name drift.** ✅ Added Rust `const` declarations in
   `quota/mod.rs` + a `provider_name_contract` unit test that
   asserts a checked-in snapshot of `Models.swift:10-37` matches.
   Manual snapshot re-build when Mac adds providers (no compile-time
   cross-language link). Spec §4.0.
6. **Token-expired UX (Gemini #3).** ⏸ DEFERRED. v0.4.3 silent-skips
   per Mac convention (helper_sync no-op leaves stale row, not
   "data lost"). v0.4.4 will introduce `enum CollectorStatus` with
   explicit `Expired/Missing/Error(reason)` variants + UI warning
   state.
7. **Empty-state copy per provider (Gemini #2).** ⏸ Frontend work,
   v0.4.4. Backend supplies structured WARN logs (§4.6) so support
   can debug; UI work is separate sprint.
8. **Env-only credential entry for Cursor/Copilot/OpenRouter
   (Gemini #1).** Accepting as known UX gap for v0.4.3 — backend
   delivers value, Settings UI for credentials is v0.4.4. Spec
   acknowledges in §2 non-goals.

## 11. Review history

- **Codex GPT-5.4 (2026-05-02)** — read full spec + 5 Mac source files
  + helper_sync RPC + schema. Verdict: 4 FIX-FIRST + 1 trivial doc fix
  + 3 ship-it. Resolutions in §10:
  - **FIX-FIRST**: i32 overflow → deferred (inherited Mac bug, needs
    schema change approval). Documented in §6 Risks instead.
  - **FIX-FIRST**: Sentry regex missed `github_pat_*` new format and
    generic `Authorization: Bearer` redaction. Both added in §4.7.
  - **FIX-FIRST**: provider name drift → contract test + Rust const
    declarations added in §4.0 + new unit test scaffold.
  - **FIX-FIRST**: `tokio::join!` panic propagation → switched to
    `tokio::spawn` per-arm + `JoinHandle::await` with `is_panic()`
    in §3 + §4.0 orchestrator code.
  - **Doc fix**: spec said "4 new regexes" but listed 5/now-6 — fixed.
  - **SHIP**: 30s Codex timeout (matches Mac, fits 2-min cycle).
  - **SHIP**: Cursor env-only (with v0.4.4 UI follow-up note).
  - **SHIP**: Gemini file-expired silent-skip (helper_sync no-op leaves
    stale row, not data lost — matches Mac).
- **Gemini 3.1 Pro (2026-05-02)** — UX / product / i18n review.
  Verdict: 4 FAIL + 3 ship-with-nit + 1 need-evidence. Resolutions:
  - **#1 env-var-only UX**: accepted as known gap; v0.4.4 Settings UI
    sprint queued. Spec §2 non-goals updated.
  - **#2 per-provider empty-state copy**: deferred to v0.4.4 (frontend
    work). Spec §10.7.
  - **#3 token-expired UX**: deferred to v0.4.4 with explicit
    `CollectorStatus` enum + UI warning state. Spec §10.6.
  - **#4 i18n**: backend keeps canonical English; frontend translates
    "Unknown"/"Free"/"Paid" via i18next. No spec change — already
    aligned.
  - **#5 sort order**: v0.4.4 frontend sprint will check Mac UI and
    replicate. v0.4.3 ships with whatever existing desktop sort is.
  - **#6 v0.4.4 UI sprint**: queued. Optional v0.4.3 docs URL not
    added (no public docs site yet).
  - **#7 telemetry per-provider WARN**: ✅ added in §4.6.
  - **#8 naming consistency** (Copilot vs GitHub Copilot): use
    "Copilot" everywhere internal, "GitHub Copilot" only in
    user-facing display strings. Spec text updated.

## 12. References

- Mac collectors: `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/Collectors/`
- ProviderKind enum: `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/Models.swift:10-37`
- Codex source: `CodexCollector.swift` (full file)
- Cursor source: `CursorCollector.swift` (120 LOC)
- Gemini source: `GeminiCollector.swift` (335 LOC) + `GeminiOAuthManager.swift` (skipping for v0.4.3)
- Copilot source: `CopilotCollector.swift` (124 LOC)
- OpenRouter source: `OpenRouterCollector.swift` (190 LOC)
- HelperDaemon iteration: `HelperDaemon.swift:200-275`
- Existing v0.4.2 quota.rs: `cli-pulse-desktop/src-tauri/src/quota.rs` (will move to `quota/claude.rs`)
- helper_sync function source: Supabase project `gkjwsxotmwrgqsvfijzs`,
  `public.helper_sync` — multi-provider loop verified working (§3).
- v0.4.0 dev plan: `PROJECT_DEV_PLAN_2026-05-02_v0.4.0_local_quota.md`
- v0.4.2 dual-writer audit + Codex review: this session
  (commits `52344c7 → 8ccaa68`).
