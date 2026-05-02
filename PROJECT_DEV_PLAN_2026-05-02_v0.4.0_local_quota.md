# v0.4.0 — Local Claude quota collection on Win / Linux

**Status:** spec — pending Codex review.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-02).
**Tracks:** v0.3.5 VM E2E + user observation 2026-05-02 ("Providers
quota still unavailable — can't Windows scan locally?")
**Parent:** v0.3.5 (Latest).

## 1. Problem

v0.3.4 wired `provider_summary` reads on the desktop, so the
Providers tab now displays plan/quota/tier bars when there's data
server-side. But the **only** uploader of `provider_quotas` rows is
the macOS menu-bar app's `ClaudeCollector` — which is gated by
`#if os(macOS)` and uses macOS-specific Keychain APIs. The desktop's
own `helper_sync` ships `p_provider_remaining: {}` and
`p_provider_tiers: {}` every cycle.

Concretely, the user observed:
- Their account has a Claude Max plan with the standard 5-tier
  layout (5h Window / Weekly / Sonnet only / Designs / Daily Routines).
- Mac scanner `last_seen_at = 2026-04-29` — 4 days dormant.
- `provider_summary` returns rows with empty `tiers` arrays for the
  user.
- Desktop renders the honest empty-state copy "Quota data
  unavailable — sign in on your phone or pair a Mac."

That's correct empty-state behavior, but it's a strict regression
when Mac is offline. Pure-Windows users (the v0.3.0 OTP-onboarding
target) never see tier bars at all.

## 2. Goal

After v0.4.0, the desktop scrapes Claude's OAuth usage API on its
own and uploads the result via the existing `helper_sync` pipeline.
A signed-in Win / Linux / Mac desktop sees real Claude tier bars
within ~2 minutes of starting Claude Code, regardless of whether
their Mac is running.

Non-goals:
- **Codex / Cursor / other providers**: deferred to v0.4.1+. Each
  has a different OAuth flow / data source. Claude alone covers the
  majority of the user complaint.
- **Active OAuth refresh**: deferred to v0.4.1. v0.4.0 reads the
  existing access_token from `.credentials.json`. If it's expired,
  this sync cycle skips quota silently — when the user next runs
  Claude Code, the CLI refreshes the token, and the next desktop
  sync picks it up.

Plus one **opportunistic** fix:
- Codex gpt-5.5 pricing entries in `pricing.rs`. v0.3.5 VM E2E
  showed Codex card $0.00 / 16K tokens because pricing.rs only goes
  up to gpt-5.4. Add gpt-5.5 (+ -mini, -nano, -pro, -codex variants)
  so cost math works.

## 3. Decisions

**Mechanism: Anthropic OAuth usage API.** Mac uses three strategies
in priority order (OAuth API, web session, CLI PTY). Strategy 1
(OAuth) is the cleanest and most portable:
- Single HTTP GET, no PTY.
- Token from `~/.claude/.credentials.json` — same file path on
  Mac/Win/Linux per Claude Code's standard.
- No browser cookie scraping (which IS macOS-specific).

We port only Strategy 1. The web session and PTY fallbacks add ~700
LOC of Mac-specific code with marginal value for desktop users
who've already signed into Claude Code (which writes
`.credentials.json`).

**Token freshness:** check `expiresAt` field before the API call. If
expired (or within 60s of expiry), skip this sync cycle entirely —
emit a Debug log and ship `{}` for tier data. When the user next
runs Claude Code, the CLI will refresh `.credentials.json` and the
next desktop sync (within 2 min) picks it up.

**On 401/403 from the API:** treat the same as expired-token —
silent skip + Debug log. Don't poison the helper_sync call (which
also carries sessions and alerts).

**Plan type detection:** Anthropic's OAuth `/usage` endpoint does
not return the plan name directly. The plan is in the
`rateLimitTier` field of `.credentials.json` (e.g. "max_20x"). Map:
- `max_20x` → "Max 20x"
- `max_5x` → "Max 5x"
- `pro` → "Pro"
- (anything else) → display verbatim

**Tier name mapping** (matches Mac convention exactly):
- `five_hour` → "5h Window"
- `seven_day` → "Weekly"
- `seven_day_sonnet` → "Sonnet only"
- `iguana_necktie` → "Designs"
- `seven_day_omelette` → "Daily Routines"

(`seven_day_opus` and `seven_day_oauth_apps` exist in the API
response but are not surfaced as user-visible tiers in the Mac UI.
We follow that convention.)

**Quota / remaining math.** API returns `utilization` as a
percentage (0-100). Derive:
- `quota = 100`
- `remaining = 100 - utilization`

Matches the convention iOS already shows (the spec earlier verified
a sample `{"name":"5h Window","quota":100,"remaining":80,...}`).

**Best-effort upload.** Quota collection runs INSIDE `sync_now` /
`background_tick`, just before `helper_sync`. Failures are logged
but never abort the sync — the existing sessions/alerts/metrics
upload must continue regardless. `helper_sync` ships `{}` for
tiers/remaining when collection fails.

**Sentry scrubbing.** The new HTTP call uses a Bearer token. The
v0.3.0 Sentry scrubber regex covers `eyJ`-prefixed JWTs but
Anthropic OAuth tokens may not match that prefix. Verify and extend
the scrubber if needed (see §5.4).

## 4. Implementation

### 4.1 New module: `src-tauri/src/quota.rs` (~200 LOC)

```rust
//! Claude OAuth-based quota collection. Mirrors macOS
//! ClaudeOAuthStrategy.swift, ported to Rust + portable I/O.
//!
//! Source: ~/.claude/.credentials.json (cross-platform — Claude
//! Code writes the same file on Mac/Win/Linux).
//! Endpoint: GET https://api.anthropic.com/api/oauth/usage
//! Beta header: anthropic-beta: oauth-2025-04-20
//!
//! Best-effort: failures log and return None so the sync_now flow
//! ships an empty tiers map without aborting sessions/alerts.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Shape of ~/.claude/.credentials.json.
#[derive(Debug, Clone, Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// ISO-8601 expiry timestamp. Pre-check before hitting the API.
    /// Some Claude Code versions write epoch ms instead — handle both.
    #[serde(rename = "expiresAt")]
    expires_at: serde_json::Value,
    /// Optional plan tier ("max_20x", "pro", etc.). May be absent on
    /// older Claude Code installs.
    #[serde(rename = "rateLimitTier", default)]
    rate_limit_tier: Option<String>,
}

/// Anthropic /api/oauth/usage response window (one tier).
#[derive(Debug, Clone, Deserialize)]
struct UsageWindow {
    /// 0-100 percentage. JSON number; coerce both i64 and f64.
    #[serde(deserialize_with = "deser_int")]
    utilization: i64,
    #[serde(default)]
    resets_at: Option<String>,
}

/// /api/oauth/usage response. All windows are optional — older or
/// non-rolled-out accounts may omit launch-window keys entirely.
#[derive(Debug, Clone, Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
    #[serde(default)]
    seven_day_sonnet: Option<UsageWindow>,
    #[serde(default)]
    iguana_necktie: Option<UsageWindow>,
    #[serde(default)]
    seven_day_omelette: Option<UsageWindow>,
}

/// Snapshot returned to the helper_sync caller. None means "skip
/// uploading quota this cycle" — caller ships {} for tiers.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaSnapshot {
    pub plan_type: String,
    /// Computed from min remaining across tiers (so the headline
    /// "remaining" matches the user's most-constrained dimension).
    pub remaining: i64,
    pub quota: i64,
    pub tiers: Vec<TierEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierEntry {
    pub name: String,
    pub quota: i64,
    pub remaining: i64,
    pub reset_time: Option<String>,
}

/// Collect Claude quota by reading credentials + hitting the API.
/// Returns None on any failure (missing creds, expired token, HTTP
/// error, parse error). Caller logs and continues — quota is
/// best-effort.
pub async fn collect_claude() -> Option<QuotaSnapshot> {
    let creds = read_credentials()?;
    if !is_token_fresh(&creds) {
        log::debug!("Claude OAuth token expired — skipping quota fetch");
        return None;
    }
    match fetch_usage(&creds.access_token).await {
        Ok(usage) => Some(map_to_snapshot(&creds, &usage)),
        Err(e) => {
            log::warn!("Claude OAuth /usage fetch failed (non-fatal): {e}");
            None
        }
    }
}

fn credentials_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude").join(".credentials.json"))
}

fn read_credentials() -> Option<ClaudeCredentials> {
    let path = credentials_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

fn is_token_fresh(creds: &ClaudeCredentials) -> bool {
    use chrono::{DateTime, Utc};
    // expires_at is ISO-8601 string OR epoch milliseconds. Handle both.
    let now = chrono::Utc::now();
    let expiry: DateTime<Utc> = match &creds.expires_at {
        serde_json::Value::String(s) => match DateTime::parse_from_rfc3339(s) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => return false,
        },
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(ms) => match chrono::DateTime::<Utc>::from_timestamp_millis(ms) {
                Some(dt) => dt,
                None => return false,
            },
            None => return false,
        },
        _ => return false,
    };
    // 60s safety margin so we don't fire a request that races expiry.
    expiry > now + chrono::Duration::seconds(60)
}

async fn fetch_usage(access_token: &str) -> Result<UsageResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("cli-pulse-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get(USAGE_URL)
        .bearer_auth(access_token)
        .header("anthropic-beta", ANTHROPIC_BETA)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {} — {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ));
    }
    resp.json::<UsageResponse>()
        .await
        .map_err(|e| format!("parse: {e}"))
}

fn map_to_snapshot(creds: &ClaudeCredentials, usage: &UsageResponse) -> QuotaSnapshot {
    let mut tiers = Vec::new();
    if let Some(w) = &usage.five_hour {
        tiers.push(window_to_tier("5h Window", w));
    }
    if let Some(w) = &usage.seven_day {
        tiers.push(window_to_tier("Weekly", w));
    }
    if let Some(w) = &usage.seven_day_sonnet {
        tiers.push(window_to_tier("Sonnet only", w));
    }
    if let Some(w) = &usage.iguana_necktie {
        tiers.push(window_to_tier("Designs", w));
    }
    if let Some(w) = &usage.seven_day_omelette {
        tiers.push(window_to_tier("Daily Routines", w));
    }
    let remaining = tiers.iter().map(|t| t.remaining).min().unwrap_or(100);
    QuotaSnapshot {
        plan_type: format_plan(creds.rate_limit_tier.as_deref()),
        remaining,
        quota: 100,
        tiers,
    }
}

fn window_to_tier(name: &str, w: &UsageWindow) -> TierEntry {
    TierEntry {
        name: name.to_string(),
        quota: 100,
        remaining: (100 - w.utilization).clamp(0, 100),
        reset_time: w.resets_at.clone(),
    }
}

fn format_plan(raw: Option<&str>) -> String {
    match raw {
        Some("max_20x") => "Max 20x".into(),
        Some("max_5x") => "Max 5x".into(),
        Some("pro") => "Pro".into(),
        Some(other) if !other.is_empty() => other.to_string(),
        _ => "Claude".into(),
    }
}

/// serde helper: coerce JSON number (Int OR Float) into i64.
fn deser_int<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.round() as i64))
            .ok_or_else(|| Error::custom("utilization not a number")),
        _ => Err(Error::custom("utilization not a number")),
    }
}
```

### 4.2 Wire into `lib.rs::sync_now`

```rust
// 4.5 (NEW v0.4.0) — best-effort Claude quota scrape, BEFORE helper_sync.
let claude_quota = quota::collect_claude().await;

// Build the provider_remaining + provider_tiers payloads.
let (p_provider_remaining, p_provider_tiers) = match &claude_quota {
    Some(snap) => {
        let remaining = json!({ "Claude": snap.remaining });
        let tiers = json!({ "Claude": {
            "quota": snap.quota,
            "remaining": snap.remaining,
            "plan_type": snap.plan_type,
            "tiers": snap.tiers,
        }});
        (remaining, tiers)
    }
    None => (json!({}), json!({})),
};

// 5. helper_sync — now includes quota when available.
let hs = supabase::helper_sync(&supabase::HelperSyncRequest {
    p_device_id: &cfg.device_id,
    p_helper_secret: &cfg.helper_secret,
    p_sessions: sessions::sessions_payload(&snapshot),
    p_alerts: alerts_payload,
    p_provider_remaining,
    p_provider_tiers,
})
.await
.map_err(friendly)?;
```

The existing `pub async fn helper_sync` signature in supabase.rs
already accepts `p_provider_remaining` and `p_provider_tiers` —
they were always there, just sent as `{}` empty. No supabase.rs
changes needed.

### 4.3 New deps in `src-tauri/Cargo.toml`

```toml
chrono = { version = "0.4", features = ["serde"] }  # already present
dirs = "5"                                          # already present
reqwest                                             # already present
serde_json                                          # already present
```

Zero new deps. Everything reuses what `auth.rs` / `supabase.rs`
already pull in.

### 4.4 Codex pricing fallback (opportunistic, separate edit)

`src-tauri/src/pricing.rs` currently has gpt-5 through gpt-5.4. Add
gpt-5.5 + variants. Rates are **approximate** because OpenAI hasn't
published official Codex billing for 5.5; mirror gpt-5.4 rates and
log a warning when prices update.

```rust
m.insert(
    "gpt-5.5",
    CodexModel { input: 2.5e-6, output: 1.5e-5, cache_read: Some(2.5e-7) },
);
m.insert(
    "gpt-5.5-codex",
    CodexModel { input: 2.5e-6, output: 1.5e-5, cache_read: Some(2.5e-7) },
);
m.insert(
    "gpt-5.5-mini",
    CodexModel { input: 7.5e-7, output: 4.5e-6, cache_read: Some(7.5e-8) },
);
m.insert(
    "gpt-5.5-nano",
    CodexModel { input: 2e-7, output: 1.25e-6, cache_read: Some(2e-8) },
);
m.insert(
    "gpt-5.5-pro",
    CodexModel { input: 3e-5, output: 1.8e-4, cache_read: None },
);
```

Add a comment noting "rates approximate, mirroring 5.4; replace with
official numbers when published."

### 4.5 Sentry scrubber audit

Anthropic OAuth tokens (`accessToken` from `.credentials.json`) are
**not** standard JWTs — they're opaque tokens with format like
`sk-ant-oat01-...`. The v0.3.0 Sentry scrubber regex
`\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\b` will NOT
match them.

Add a regex for `sk-ant-oat[a-z0-9-]{20,}` to the existing scrubber
set. Plus extend `QUERY_PARAM` to include `bearer` (case-insensitive)
since the new HTTP path uses `Authorization: Bearer <token>` and
errors might log the raw header.

```rust
// Codex review: the version-digit count in `sk-ant-oatNN-` /
// `sk-ant-apiNN-` is currently 2 digits in shipping tokens, but
// Anthropic could bump to 3 ("oat100-...") without notice. Also
// reserve room for the rumored `sk-ant-sid-...` session-ID format
// in case it appears in error messages.
static ANTHROPIC_TOKEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\bsk-ant-(?:oat|api|sid)\d{0,3}-[A-Za-z0-9_\-]{16,}\b").unwrap()
});
```

Add 4 tests:
- `redacts_anthropic_oat_token_in_message` (`sk-ant-oat01-...`)
- `redacts_anthropic_api_token` (`sk-ant-api03-...`)
- `redacts_anthropic_oat_in_authorization_header` (`Authorization: Bearer sk-ant-oat01-...`)
- `survives_unversioned_anthropic_token` (`sk-ant-oat-...` — no version digits)

Plus extend the existing `redact_secrets_in_strings` event-walking
to apply this regex on top of JWT / helper_secret / query-param.

The scrubber already round-trips the full event JSON via
`scrub_strings_recursive` (sentry_init.rs:139+), so coverage extends
to: message body, breadcrumb URLs, request/response payload strings,
error context, panic stacktrace strings (which Sentry serializes as
strings inside the event). Codex review §7 concern is closed once
the regex itself matches real tokens.

## 5. Tests

Rust unit tests in `quota.rs`:
- `is_token_fresh_string_ok` — ISO-8601 future timestamp.
- `is_token_fresh_string_expired` — ISO-8601 past timestamp.
- `is_token_fresh_within_60s_grace` — 30s in future returns false.
- `is_token_fresh_epoch_ms_ok` — JSON number future epoch ms.
- `is_token_fresh_epoch_ms_expired` — JSON number past.
- `parse_usage_full` — full response with all 5 windows.
- `parse_usage_legacy_no_launch_windows` — old account without
  iguana_necktie / seven_day_omelette.
- `parse_usage_utilization_int_or_float` — coerces both `9` and `9.0`.
- `format_plan_max_20x` / `format_plan_pro` / `format_plan_unknown`.
- `map_to_snapshot_remaining_is_min_across_tiers`.

Sentry unit tests:
- `scrubs_anthropic_oat_token_in_message`.
- `scrubs_anthropic_oat_in_authorization_header_value`.

E2E (VM Claude on Win):
- Pair the desktop, sign in to Claude Code on the VM (or assume the
  user has done so).
- Run a sync. Confirm Providers tab shows Claude tier bars.
- Confirm `provider_summary` server-side shows non-empty tiers for
  this user (`SELECT * FROM provider_quotas WHERE user_id = ...`).
- Force the credentials file empty / token expired and confirm sync
  still completes (sessions/alerts unaffected).

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| `~/.claude/.credentials.json` doesn't exist (user hasn't signed into Claude Code yet) | Silent skip, log Debug. helper_sync ships empty quota. UI shows existing empty-state copy. |
| Anthropic API rate-limits us | We call once per 2-min sync per device. At fleet scale that's well under the public Claude API rate limits. If we hit a 429, treat as transient (skip + retry next cycle). |
| Token rotation during the API call (race) | API returns 401, we treat as expired-token (skip), next cycle reads the rotated `.credentials.json`. |
| API response schema drift (new launch windows) | All windows are `Option<UsageWindow>`, deserialize uses `#[serde(default)]`. Unknown keys are ignored. New tier names (e.g. another launch window) won't break parsing. |
| The desktop and Mac upload conflicting quota for the same user | helper_sync's existing upsert keys on `(user_id, provider)`. Last write wins. Both clients use the same OAuth API, so values converge. |
| Claude Code stores token in OS keychain instead of .credentials.json on some installs | Not a regression — those installs already worked with Mac scanner using web/PTY fallbacks. v0.4.0 supports the most common path; v0.4.1 can add keyring fallback if it proves needed. |
| OAuth token leakage to Sentry breadcrumbs | New regex in `sentry_init.rs` (§4.5) covers `sk-ant-(oat|api)\d{2}-...`. New tests verify. |
| Codex gpt-5.5 prices guessed wrong | Documented as approximate. When OpenAI publishes real prices, we update one HashMap entry. Worst case: the cost shown is off by 20-50%, which is still infinitely better than $0.00. |
| Background_tick fires before user finishes Claude Code sign-in | First sync sees empty creds → skip. Second sync (2 min later) sees populated creds → quota appears. Good UX. |

## 7. Milestones (1.5 days)

| Day | Work |
|---|---|
| 0.5 | `quota.rs` module + 10 unit tests. Sentry scrubber regex + 2 tests. Codex pricing entries. cargo check + cargo test all green. |
| 1 | Wire `quota::collect_claude()` into `sync_now`. Manual smoke test on Mac dev — confirm that a real `.credentials.json` produces a populated tier upload. Verify server-side `provider_quotas` row updates. |
| 1.5 | Bump version, CHANGELOG, ship. Hand to VM Claude for paired-account E2E. Promote to Latest after E2E PASS. |

## 8. Backward compatibility

- v0.3.x desktops on auto-update: unaffected. Server schema
  unchanged. `helper_sync` signature unchanged.
- iOS / Android: unaffected. They consume `provider_summary`, which
  now sees more frequent updates from desktops as a bonus.
- Mac scanner: unaffected. Continues uploading quotas via its own
  collector chain. Last write wins; both writers converge to the
  same Anthropic API source of truth.

## 9. Out of scope (deferred)

- **Codex / Cursor / OpenAI API quota collection.** Each has a
  separate OAuth flow (or for OpenAI, requires a user-managed API
  key). v0.4.1+.
- **OAuth token refresh.** When `.credentials.json` is expired, the
  next `claude` CLI invocation refreshes it automatically. The
  desktop just waits. v0.4.1 can add proactive refresh for users
  who only run the desktop.
- **Legacy CLI PTY / web session fallbacks.** Mac uses these as
  Strategy 2 / 3 of the chain. Win/Linux desktops would need a Rust
  PTY crate. Most users won't need this — `.credentials.json`
  exists whenever they've signed into Claude Code at all.
- **Per-device quota visibility.** Quotas are per-account, not
  per-device. The new `get_daily_usage_by_device` RPC handles
  per-device tokens; quotas stay aggregated.

## 10. Decisions to close before sprint start

1. **Token freshness margin.** Going with 60s safety margin before
   expiry. Matches the implicit Mac convention.
2. **Skip-on-expiry vs proactive refresh.** v0.4.0 = skip. v0.4.1 =
   refresh. Document so users understand why quota lags by up to
   2 min after they re-run Claude Code.
3. **What happens when the API returns an unrecognized window key?**
   Ignore it. Future Anthropic launches won't break parsing. If a
   new window becomes important, we add it to `map_to_snapshot`.
4. **Codex gpt-5.5 — guess prices vs leave $0?** Going with
   approximate (mirror gpt-5.4) per §4.4. Worse-than-real-cost is
   still better than $0 for cost-aware UX.

## 11. Review history

- **Codex GPT-5.4 review (2026-05-02)** — read the full spec + Mac
  reference impl + helper_sync server body + existing supabase.rs.
  Cargo check was sandbox-blocked but evidence-only review. Two
  FIX-FIRSTs surfaced:
  1. **§4.5 Sentry scrubber regex** for `sk-ant-...` tokens. The
     spec wrote `\d{2}` for the version digits, but Anthropic could
     bump to 3-digit versions (`oat100-...`) without notice, and
     unversioned forms (`sk-ant-oat-...`) might appear in error
     messages. Resolved: regex now uses `\d{0,3}` and adds the
     `sid` prefix variant. Also relaxed the token-body length floor
     from `{20,}` to `{16,}` to catch shorter-than-expected tokens.
     Test set expanded to 4 cases covering oat / api / Bearer
     header / unversioned.
  2. **§7 Token leak risk in panic** — depends on §4.5. The v0.3.0
     scrubber already round-trips the full event JSON
     (sentry_init.rs:139+), so coverage extends to stacktrace
     strings, breadcrumb URLs, and request/response payloads — not
     just message body. Once the regex matches real tokens
     (resolved per #1), this concern closes.

  Other findings: SHIP for §4.1 freshness check, §4.1 401/403
  handling, §4.2 helper_sync wiring (matches server body), §4.4
  Codex price approximation, §6 concurrency (`ON CONFLICT DO
  UPDATE` upsert in helper_sync is race-safe). NIT only on cargo
  check being sandbox-blocked — verify locally before commit.

## 12. References

- Mac OAuth strategy: `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/Collectors/Claude/ClaudeOAuthStrategy.swift`
- Mac credentials file path: `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/CredentialBridge.swift:49`
- helper_sync provider_tiers shape: `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/HelperAPIClient.swift:80-101`
- Anthropic OAuth API: `GET https://api.anthropic.com/api/oauth/usage`,
  beta header `anthropic-beta: oauth-2025-04-20`.
- v0.3.5 audit: VM Claude Win VM, 2026-05-02.
- Existing Sentry scrubber: `src-tauri/src/sentry_init.rs:154+`.
- Existing pricing.rs: `src-tauri/src/pricing.rs:140+`.
