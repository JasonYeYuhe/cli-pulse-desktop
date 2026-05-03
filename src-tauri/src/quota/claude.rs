//! v0.4.0 — Claude OAuth-based quota collection.
//! v0.4.4 — schema corrected to nested-only.
//!
//! Mirrors macOS `ClaudeOAuthStrategy.swift`, ported to Rust + portable I/O
//! so Win / Linux / Mac desktops can populate `provider_quotas` server-side
//! independently of whether a Mac scanner is online for the same account.
//!
//! Source of truth for the on-disk shape: CodexBar upstream commit `82bbcde`
//! (2026-05-02 16:17 JST), file
//! `Sources/CodexBarCore/Providers/Claude/ClaudeOAuth/ClaudeOAuthCredentialModels.swift:65-78`.
//! Real claude CLI ≥2.x writes:
//!     { "claudeAiOauth": { "accessToken": "sk-ant-oat01-...",
//!                          "refreshToken": "...",
//!                          "expiresAt": 1746789600000,
//!                          "scopes": [...],
//!                          "subscriptionType": "max",
//!                          "rateLimitTier": "max_20x" } }
//! v0.4.3 spec assumed flat top-level (`{accessToken, expiresAt, ...}`),
//! which never matched real claude CLI output — collector silently returned
//! `None` for every cycle. v0.4.4: nested-only, no flat fallback. CodexBar
//! upstream never accepted flat top-level either; matching upstream 1:1
//! avoids carrying our own divergent schema.
//!
//! Note: `subscriptionType` is preserved by Anthropic in the file but
//! CodexBar upstream does NOT consume it for plan_type; we mirror that.
//! `format_plan` (v0.4.2) reads `rateLimitTier` exclusively — see
//! `provider_name_contract` in `mod.rs` for the dual-writer invariant.
//!
//! API: `GET https://api.anthropic.com/api/oauth/usage` with
//!     Authorization: Bearer <accessToken>
//!     anthropic-beta: oauth-2025-04-20
//!
//! Best-effort: failures (missing creds, expired token, HTTP error, parse
//! error) all return `None` so the calling sync_now flow ships an empty
//! tiers map without aborting sessions/alerts. Log levels (v0.4.4):
//!   - file absent / token expired / no access_token: `debug!` (expected,
//!     "user not signed in" / "user idle past expiry")
//!   - file present but JSON parse fails OR `claudeAiOauth` key missing:
//!     `warn!` (schema drift — surface immediately so future Anthropic
//!     shape changes don't go silent like v0.4.3 did)

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;

use super::{QuotaSnapshot, TierEntry};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Skip the API call if the persisted access_token expires within this
/// window. Avoids racing token rotation when Claude Code refreshes.
const EXPIRY_SAFETY_MARGIN_SECS: i64 = 60;

/// Top-level shape of `~/.claude/.credentials.json` (claude CLI ≥2.x).
/// Single key `claudeAiOauth` wrapping the OAuth payload.
#[derive(Debug, Clone, Deserialize)]
struct ClaudeCredentialsFile {
    #[serde(rename = "claudeAiOauth", default)]
    oauth: Option<ClaudeOAuthInner>,
}

/// Inner OAuth block. Field names match CodexBar upstream
/// `ClaudeOAuthCredentialModels.swift:65-78` exactly.
#[derive(Debug, Clone, Deserialize)]
struct ClaudeOAuthInner {
    #[serde(rename = "accessToken", default)]
    access_token: Option<String>,
    /// Reserved — not currently consumed (passive refresh requires the
    /// `claude` CLI; v0.4.5+ may add active refresh).
    #[serde(rename = "refreshToken", default)]
    #[allow(dead_code)]
    refresh_token: Option<String>,
    /// Epoch milliseconds. CodexBar parses as Double, not String —
    /// real claude CLI never writes ISO-8601 here. v0.4.3 had a string|
    /// number branching deserializer based on incorrect docstring
    /// assumption; v0.4.4 drops the string path entirely.
    #[serde(rename = "expiresAt", default)]
    expires_at: Option<f64>,
    /// Reserved — kept for completeness with CodexBar struct; not read.
    #[serde(default)]
    #[allow(dead_code)]
    scopes: Vec<String>,
    /// Plan tier source-of-truth ("max_20x", "max_5x", "pro", etc.).
    /// CodexBar reads this field; v0.4.2 `format_plan` mirrors the same
    /// lowercase substring matching used in `ClaudeSourceStrategy.swift:166-178`.
    /// Note: Anthropic also writes `subscriptionType` in the file but
    /// CodexBar upstream does NOT consume it — we follow upstream.
    #[serde(rename = "rateLimitTier", default)]
    rate_limit_tier: Option<String>,
}

/// One usage window in the OAuth /usage response.
#[derive(Debug, Clone, Deserialize)]
struct UsageWindow {
    /// 0–100 percentage. JSON returns either Int (`9`) or Float (`9.0`);
    /// the custom deserializer handles both.
    #[serde(deserialize_with = "deser_int")]
    utilization: i64,
    #[serde(default)]
    resets_at: Option<String>,
}

/// `/api/oauth/usage` response. All windows are optional — older
/// or non-rolled-out accounts may omit launch-window keys entirely.
#[derive(Debug, Clone, Deserialize, Default)]
struct UsageResponse {
    #[serde(default)]
    five_hour: Option<UsageWindow>,
    #[serde(default)]
    seven_day: Option<UsageWindow>,
    #[serde(default)]
    seven_day_sonnet: Option<UsageWindow>,
    /// Opus fallback feeds the "Sonnet only" tier when `seven_day_sonnet`
    /// is absent. Mirrors `ClaudeSourceStrategy.swift:156` — without this,
    /// Mac and Win uploads disagree on tier count and the row flickers.
    #[serde(default)]
    seven_day_opus: Option<UsageWindow>,
    /// Internal codename for "Designs". Anthropic serves the key as
    /// `null` on accounts that are on the rollout but haven't used the
    /// bucket yet — Mac (`parseLaunchWindow`) emits it as a 0/None tier
    /// in that case so the row is visible. The custom deserializer here
    /// distinguishes absent (skip) from present-null (emit at 100%).
    #[serde(default, deserialize_with = "deserialize_launch_window")]
    iguana_necktie: LaunchWindow,
    /// Internal codename for "Daily Routines". Same launch-window null
    /// semantics as `iguana_necktie`.
    #[serde(default, deserialize_with = "deserialize_launch_window")]
    seven_day_omelette: LaunchWindow,
}

/// Three-state representation for launch windows where present-but-null
/// means "rolled out but unused" rather than "not rolled out". Mirrors
/// the macOS `parseLaunchWindow` semantics so dual-writer updates
/// converge on the same tier shape.
#[derive(Debug, Clone, Default)]
enum LaunchWindow {
    #[default]
    Absent,
    PresentNull,
    Present(UsageWindow),
}

fn deserialize_launch_window<'de, D>(d: D) -> Result<LaunchWindow, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    if v.is_null() {
        return Ok(LaunchWindow::PresentNull);
    }
    serde_json::from_value::<UsageWindow>(v)
        .map(LaunchWindow::Present)
        .map_err(serde::de::Error::custom)
}

/// Top-level entry. Reads the persisted Claude credentials, validates
/// freshness, hits the OAuth /usage API, and maps the response to a
/// portable `QuotaSnapshot`. Returns `None` on any failure — log levels
/// distinguish "user not signed in" (debug) from "schema drift" (warn)
/// per v0.4.4 module docstring.
pub async fn collect() -> Option<QuotaSnapshot> {
    let path = credentials_path()?;
    let oauth = match read_credentials(&path) {
        Ok(Some(file)) => match file.oauth {
            Some(o) => o,
            None => {
                log::warn!(
                    "Claude .credentials.json missing 'claudeAiOauth' key — schema mismatch \
                     (real claude CLI ≥2.x writes nested shape; see CodexBar 82bbcde)"
                );
                return None;
            }
        },
        Ok(None) => {
            log::debug!("Claude .credentials.json absent — skipping quota fetch");
            return None;
        }
        Err(e) => {
            log::warn!("Claude .credentials.json parse failed (non-fatal): {e}");
            return None;
        }
    };
    let access_token = match oauth.access_token.as_deref().filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => {
            log::warn!(
                "Claude .credentials.json claudeAiOauth.accessToken absent or empty — \
                 corrupted oauth block"
            );
            return None;
        }
    };
    if !is_token_fresh(&oauth) {
        log::debug!(
            "Claude OAuth access token expired (or expiresAt missing) — \
             run claude CLI to refresh"
        );
        return None;
    }
    match fetch_usage(&access_token).await {
        Ok(usage) => {
            let snap = map_to_snapshot(&oauth, &usage);
            log::info!(
                "Claude quota updated: plan={}, tiers={}, remaining={}",
                snap.plan_type,
                snap.tiers.len(),
                snap.remaining,
            );
            Some(snap)
        }
        Err(e) => {
            log::warn!("Claude OAuth /usage fetch failed (non-fatal): {e}");
            None
        }
    }
}

fn credentials_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude").join(".credentials.json"))
}

/// Read + parse credentials file with three-state outcome:
/// - `Ok(Some(file))` — file present and JSON parsed.
/// - `Ok(None)` — file absent (legitimate "user not signed in" skip).
/// - `Err(msg)` — file present but read or JSON parse failed (schema drift).
fn read_credentials(path: &Path) -> Result<Option<ClaudeCredentialsFile>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("JSON: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("IO: {e}")),
    }
}

/// Token is fresh if `expiresAt` is present and at least
/// `EXPIRY_SAFETY_MARGIN_SECS` seconds in the future. Missing
/// `expiresAt` is treated as not-fresh (defensive — a credentials block
/// without expiry can't be safely used).
fn is_token_fresh(oauth: &ClaudeOAuthInner) -> bool {
    let Some(exp_ms) = oauth.expires_at else {
        return false;
    };
    let margin_ms = (EXPIRY_SAFETY_MARGIN_SECS as f64) * 1000.0;
    let now_ms = Utc::now().timestamp_millis() as f64;
    exp_ms > now_ms + margin_ms
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
        // Don't include the raw body here — it can carry diagnostic
        // info like the token in echoed Authorization headers in some
        // Cloudflare error pages. Truncate hard.
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(120).collect();
        return Err(format!("HTTP {} — {}", status.as_u16(), snippet));
    }
    resp.json::<UsageResponse>()
        .await
        .map_err(|e| format!("parse: {e}"))
}

fn map_to_snapshot(oauth: &ClaudeOAuthInner, usage: &UsageResponse) -> QuotaSnapshot {
    let mut tiers = Vec::new();
    if let Some(w) = &usage.five_hour {
        tiers.push(window_to_tier("5h Window", w));
    }
    if let Some(w) = &usage.seven_day {
        tiers.push(window_to_tier("Weekly", w));
    }
    let sonnet_or_opus = usage
        .seven_day_sonnet
        .as_ref()
        .or(usage.seven_day_opus.as_ref());
    if let Some(w) = sonnet_or_opus {
        tiers.push(window_to_tier("Sonnet only", w));
    }
    push_launch_tier(&mut tiers, "Designs", &usage.iguana_necktie);
    push_launch_tier(&mut tiers, "Daily Routines", &usage.seven_day_omelette);
    let remaining = tiers.iter().map(|t| t.remaining).min().unwrap_or(100);
    let session_reset = usage.five_hour.as_ref().and_then(|w| w.resets_at.clone());
    QuotaSnapshot {
        plan_type: format_plan(oauth.rate_limit_tier.as_deref()),
        remaining,
        quota: 100,
        session_reset,
        tiers,
    }
}

fn push_launch_tier(tiers: &mut Vec<TierEntry>, name: &str, lw: &LaunchWindow) {
    match lw {
        LaunchWindow::Absent => {}
        LaunchWindow::PresentNull => tiers.push(TierEntry {
            name: name.to_string(),
            quota: 100,
            remaining: 100,
            reset_time: None,
        }),
        LaunchWindow::Present(w) => tiers.push(window_to_tier(name, w)),
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

/// Mirror `ClaudeSourceStrategy.swift:166-178` so Mac and Win/Linux
/// writers produce identical `plan_type` strings. The `provider_quotas`
/// upsert is a full-replace, so any divergence here flickers the
/// displayed plan name every time the alternate writer polls.
fn format_plan(raw: Option<&str>) -> String {
    let raw = raw.unwrap_or("").to_lowercase();
    if raw.contains("max_20x") || raw.contains("max 20x") {
        "Max 20x".into()
    } else if raw.contains("max") {
        // Mac checks `max_5x` / `max 5x` then a generic `max` fallback
        // separately (ClaudeSourceStrategy.swift:170-171); both branches
        // return "Max 5x". Collapsed here to satisfy clippy without
        // changing behavior — `max_20x` was already filtered above, so
        // any remaining "max" defaults to 5x.
        "Max 5x".into()
    } else if raw.contains("ultra") {
        "Ultra".into()
    } else if raw.contains("pro") {
        "Pro".into()
    } else if raw.contains("team") {
        "Team".into()
    } else if raw.contains("enterprise") {
        "Enterprise".into()
    } else if raw.contains("free") {
        "Free".into()
    } else {
        "Unknown".into()
    }
}

/// serde helper: coerce JSON number (Int or Float) into i64. Anthropic's
/// /usage endpoint occasionally returns `9.0` instead of `9`, and serde's
/// default i64 deserializer rejects float forms. Round on the way in.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn oauth_with_expires_ms(exp_ms: Option<f64>) -> ClaudeOAuthInner {
        ClaudeOAuthInner {
            access_token: Some("sk-ant-oat01-fake".into()),
            refresh_token: None,
            expires_at: exp_ms,
            scopes: vec![],
            rate_limit_tier: Some("max_20x".into()),
        }
    }

    fn oauth_with_tier(tier: Option<&str>) -> ClaudeOAuthInner {
        ClaudeOAuthInner {
            access_token: Some("sk-ant-oat01-fake".into()),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
            rate_limit_tier: tier.map(String::from),
        }
    }

    // ---- v0.4.4 schema fixtures (5 user-spec'd) ----

    #[test]
    fn parse_nested_shape_happy() {
        let json = r#"{
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-fake",
                "refreshToken": "rt-fake",
                "expiresAt": 1777761408739,
                "scopes": ["user:profile", "user:inference"],
                "subscriptionType": "max",
                "rateLimitTier": "max_20x"
            }
        }"#;
        let parsed: ClaudeCredentialsFile = serde_json::from_str(json).unwrap();
        let oauth = parsed.oauth.expect("oauth must parse from nested shape");
        assert_eq!(oauth.access_token.as_deref(), Some("sk-ant-oat01-fake"));
        assert_eq!(oauth.expires_at, Some(1777761408739.0));
        assert_eq!(oauth.rate_limit_tier.as_deref(), Some("max_20x"));
        // Unknown field `subscriptionType` is silently ignored by serde —
        // CodexBar 82bbcde does not consume it; we follow upstream.
    }

    #[test]
    fn parse_legacy_flat_shape_yields_none_oauth() {
        // Pre-v0.4.4 spec assumed flat top-level. CodexBar upstream
        // never accepted this shape either; nested-only is correct.
        let json = r#"{
            "accessToken": "x",
            "refreshToken": "y",
            "expiresAt": 1777761408739,
            "rateLimitTier": "max_20x"
        }"#;
        let parsed: ClaudeCredentialsFile = serde_json::from_str(json).unwrap();
        assert!(
            parsed.oauth.is_none(),
            "flat shape (no claudeAiOauth wrapper) must yield oauth=None — \
             nested-only per CodexBar 82bbcde"
        );
    }

    #[test]
    fn parse_nested_with_empty_access_token() {
        // collect() must skip with WARN when access_token is empty —
        // distinguishes "wrong shape" (warn) from "user signed out" (debug).
        let json = r#"{
            "claudeAiOauth": {
                "accessToken": "",
                "expiresAt": 1777761408739,
                "rateLimitTier": "pro"
            }
        }"#;
        let parsed: ClaudeCredentialsFile = serde_json::from_str(json).unwrap();
        let oauth = parsed.oauth.expect("oauth must parse");
        assert_eq!(oauth.access_token.as_deref(), Some(""));
        // The empty-string filter lives in collect(); here we only assert
        // the empty value is preserved through parse so the gate can fire.
    }

    #[test]
    fn token_not_fresh_past_expires_at() {
        let now_ms = Utc::now().timestamp_millis() as f64;
        let past = oauth_with_expires_ms(Some(now_ms - 60_000.0));
        assert!(!is_token_fresh(&past));
    }

    #[test]
    fn token_not_fresh_missing_expires_at() {
        let no_exp = oauth_with_expires_ms(None);
        assert!(
            !is_token_fresh(&no_exp),
            "missing expiresAt must defensively return false"
        );
    }

    // ---- is_token_fresh edge cases ----

    #[test]
    fn token_fresh_epoch_ms_future() {
        let now_ms = Utc::now().timestamp_millis() as f64;
        let future = oauth_with_expires_ms(Some(now_ms + 3_600_000.0)); // +1h
        assert!(is_token_fresh(&future));
    }

    #[test]
    fn token_not_fresh_within_grace_window() {
        // 30s in future → less than 60s safety margin → NOT fresh.
        let now_ms = Utc::now().timestamp_millis() as f64;
        let near = oauth_with_expires_ms(Some(now_ms + 30_000.0));
        assert!(!is_token_fresh(&near));
    }

    // ---- existing usage-response + map_to_snapshot tests (unchanged behavior) ----

    #[test]
    fn parse_full_usage_response() {
        let json = r#"{
            "five_hour": {"utilization": 20, "resets_at": "2026-05-02T22:00:00Z"},
            "seven_day": {"utilization": 34.0, "resets_at": "2026-05-09T00:00:00Z"},
            "seven_day_sonnet": {"utilization": 2},
            "iguana_necktie": {"utilization": 0, "resets_at": "2026-05-05T12:00:00Z"},
            "seven_day_omelette": {"utilization": 5}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let oauth = oauth_with_tier(Some("max_20x"));
        let snap = map_to_snapshot(&oauth, &usage);
        assert_eq!(snap.tiers.len(), 5);
        assert_eq!(snap.tiers[0].name, "5h Window");
        assert_eq!(snap.tiers[0].remaining, 80);
        assert_eq!(snap.tiers[1].name, "Weekly");
        assert_eq!(snap.tiers[1].remaining, 66); // 100 - 34
        assert_eq!(snap.tiers[2].remaining, 98);
        assert_eq!(snap.tiers[3].name, "Designs");
        assert_eq!(snap.tiers[4].name, "Daily Routines");
        assert_eq!(snap.remaining, 66);
        assert_eq!(snap.quota, 100);
        assert_eq!(snap.plan_type, "Max 20x");
    }

    #[test]
    fn parse_legacy_response_no_launch_windows() {
        let json = r#"{
            "five_hour": {"utilization": 12},
            "seven_day": {"utilization": 40}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let oauth = oauth_with_tier(Some("pro"));
        let snap = map_to_snapshot(&oauth, &usage);
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.plan_type, "Pro");
        assert_eq!(snap.remaining, 60);
    }

    #[test]
    fn utilization_int_or_float() {
        let int_form: UsageWindow = serde_json::from_str(r#"{"utilization": 9}"#).unwrap();
        let float_form: UsageWindow = serde_json::from_str(r#"{"utilization": 9.4}"#).unwrap();
        let float_round_up: UsageWindow = serde_json::from_str(r#"{"utilization": 9.6}"#).unwrap();
        assert_eq!(int_form.utilization, 9);
        assert_eq!(float_form.utilization, 9);
        assert_eq!(float_round_up.utilization, 10);
    }

    #[test]
    fn format_plan_buckets_match_mac() {
        assert_eq!(format_plan(Some("max_20x")), "Max 20x");
        assert_eq!(format_plan(Some("MAX_20x")), "Max 20x");
        assert_eq!(format_plan(Some("max 20x")), "Max 20x");
        assert_eq!(format_plan(Some("max_5x")), "Max 5x");
        assert_eq!(format_plan(Some("max 5x")), "Max 5x");
        assert_eq!(format_plan(Some("max")), "Max 5x");
        assert_eq!(format_plan(Some("ultra")), "Ultra");
        assert_eq!(format_plan(Some("pro")), "Pro");
        assert_eq!(format_plan(Some("team")), "Team");
        assert_eq!(format_plan(Some("enterprise")), "Enterprise");
        assert_eq!(format_plan(Some("custom_enterprise")), "Enterprise");
        assert_eq!(format_plan(Some("free")), "Free");
        assert_eq!(format_plan(None), "Unknown");
        assert_eq!(format_plan(Some("")), "Unknown");
        assert_eq!(format_plan(Some("garbage_tier")), "Unknown");
    }

    #[test]
    fn snapshot_remaining_clamps_to_zero() {
        let json = r#"{"five_hour": {"utilization": 120}}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&oauth_with_tier(None), &usage);
        assert_eq!(snap.tiers[0].remaining, 0);
    }

    #[test]
    fn sonnet_falls_back_to_opus_when_sonnet_absent() {
        let json = r#"{
            "five_hour": {"utilization": 10},
            "seven_day_opus": {"utilization": 25, "resets_at": "2026-05-09T00:00:00Z"}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&oauth_with_tier(None), &usage);
        assert_eq!(snap.tiers.len(), 2);
        let sonnet = snap.tiers.iter().find(|t| t.name == "Sonnet only").unwrap();
        assert_eq!(sonnet.remaining, 75);
        assert_eq!(sonnet.reset_time.as_deref(), Some("2026-05-09T00:00:00Z"));
    }

    #[test]
    fn sonnet_takes_priority_over_opus_when_both_present() {
        let json = r#"{
            "seven_day_sonnet": {"utilization": 5},
            "seven_day_opus": {"utilization": 90}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&oauth_with_tier(None), &usage);
        let sonnet = snap.tiers.iter().find(|t| t.name == "Sonnet only").unwrap();
        assert_eq!(sonnet.remaining, 95);
    }

    #[test]
    fn launch_window_present_null_emits_full_remaining() {
        let json = r#"{
            "five_hour": {"utilization": 0},
            "iguana_necktie": null,
            "seven_day_omelette": null
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&oauth_with_tier(None), &usage);
        let designs = snap.tiers.iter().find(|t| t.name == "Designs").unwrap();
        assert_eq!(designs.remaining, 100);
        assert!(designs.reset_time.is_none());
        let routines = snap
            .tiers
            .iter()
            .find(|t| t.name == "Daily Routines")
            .unwrap();
        assert_eq!(routines.remaining, 100);
    }

    #[test]
    fn launch_window_absent_skips_tier() {
        let json = r#"{"five_hour": {"utilization": 0}}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&oauth_with_tier(None), &usage);
        assert!(!snap.tiers.iter().any(|t| t.name == "Designs"));
        assert!(!snap.tiers.iter().any(|t| t.name == "Daily Routines"));
    }

    #[test]
    fn outer_session_reset_taken_from_five_hour() {
        let json = r#"{
            "five_hour": {"utilization": 20, "resets_at": "2026-05-02T22:00:00Z"},
            "seven_day": {"utilization": 50, "resets_at": "2026-05-09T00:00:00Z"}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&oauth_with_tier(None), &usage);
        assert_eq!(snap.session_reset.as_deref(), Some("2026-05-02T22:00:00Z"));
    }

    #[test]
    fn outer_session_reset_none_when_five_hour_missing() {
        let json = r#"{"seven_day": {"utilization": 50}}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&oauth_with_tier(None), &usage);
        assert!(snap.session_reset.is_none());
    }
}
