//! v0.4.0 — Claude OAuth-based quota collection.
//!
//! Mirrors macOS `ClaudeOAuthStrategy.swift`, ported to Rust + portable I/O
//! so Win / Linux / Mac desktops can populate `provider_quotas` server-side
//! independently of whether a Mac scanner is online for the same account.
//!
//! Source of truth: `~/.claude/.credentials.json` — Claude Code writes this
//! file on every successful OAuth sign-in regardless of OS. JSON shape:
//!     { "accessToken": "sk-ant-oat01-...",
//!       "refreshToken": "...",
//!       "expiresAt": "2026-05-09T08:20:00Z" | 1746789600000,
//!       "rateLimitTier": "max_20x" }  // optional
//!
//! API: `GET https://api.anthropic.com/api/oauth/usage` with
//!     Authorization: Bearer <accessToken>
//!     anthropic-beta: oauth-2025-04-20
//!
//! Best-effort: failures (missing creds, expired token, HTTP error, parse
//! error) all return `None` so the calling sync_now flow ships an empty
//! tiers map without aborting sessions/alerts.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Skip the API call if the persisted access_token expires within this
/// window. Avoids racing token rotation when Claude Code refreshes.
const EXPIRY_SAFETY_MARGIN_SECS: i64 = 60;

/// Shape of `~/.claude/.credentials.json`.
#[derive(Debug, Clone, Deserialize)]
struct ClaudeCredentials {
    #[serde(rename = "accessToken")]
    access_token: String,
    /// ISO-8601 string OR epoch milliseconds. Both shapes appear in the
    /// wild depending on Claude Code version. Stored as a raw JSON value
    /// so `is_token_fresh` can branch.
    #[serde(rename = "expiresAt", default)]
    expires_at: serde_json::Value,
    /// Optional plan tier ("max_20x", "max_5x", "pro", etc.). Older
    /// Claude Code installs may omit this.
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

/// Snapshot returned to the helper_sync caller. `None` from the public
/// `collect_claude` means "skip uploading quota this cycle" — the caller
/// should ship an empty `{}` for `p_provider_tiers` / `p_provider_remaining`.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaSnapshot {
    pub plan_type: String,
    /// Min remaining across all tiers. The dashboard "headline" remaining
    /// number reflects the user's most-constrained dimension (matches
    /// Mac convention).
    pub remaining: i64,
    pub quota: i64,
    /// Outer `reset_time` — the 5h Window reset, mirroring Mac's
    /// `ClaudeSourceStrategy.swift:217` (`reset_time: snapshot.sessionReset`).
    /// `helper_sync` writes this to `provider_quotas.reset_time` and a
    /// missing value flips the column NULL on every Win sync, flickering
    /// against Mac's writes.
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

/// Top-level entry. Reads the persisted Claude credentials, validates
/// freshness, hits the OAuth /usage API, and maps the response to a
/// portable `QuotaSnapshot`. Returns `None` on any failure — callers log
/// at info level and continue without aborting.
pub async fn collect_claude() -> Option<QuotaSnapshot> {
    let creds = match read_credentials() {
        Some(c) => c,
        None => {
            log::debug!("Claude .credentials.json absent or unparseable — skipping quota fetch");
            return None;
        }
    };
    if !is_token_fresh(&creds) {
        log::debug!("Claude OAuth token expired — skipping quota fetch");
        return None;
    }
    match fetch_usage(&creds.access_token).await {
        Ok(usage) => {
            let snap = map_to_snapshot(&creds, &usage);
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

fn read_credentials() -> Option<ClaudeCredentials> {
    let path = credentials_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

fn is_token_fresh(creds: &ClaudeCredentials) -> bool {
    parse_expiry(&creds.expires_at)
        .map(|expiry| expiry > Utc::now() + chrono::Duration::seconds(EXPIRY_SAFETY_MARGIN_SECS))
        .unwrap_or(false)
}

/// Parse `expires_at` from either an ISO-8601 string or an epoch-ms
/// number. Returns `None` for unrecognized shapes.
fn parse_expiry(v: &serde_json::Value) -> Option<DateTime<Utc>> {
    match v {
        serde_json::Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        serde_json::Value::Number(n) => n.as_i64().and_then(DateTime::<Utc>::from_timestamp_millis),
        _ => None,
    }
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

fn map_to_snapshot(creds: &ClaudeCredentials, usage: &UsageResponse) -> QuotaSnapshot {
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
        plan_type: format_plan(creds.rate_limit_tier.as_deref()),
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
    use chrono::Duration as ChronoDuration;

    fn creds_with_expiry(v: serde_json::Value) -> ClaudeCredentials {
        ClaudeCredentials {
            access_token: "sk-ant-oat01-fake".into(),
            expires_at: v,
            rate_limit_tier: Some("max_20x".into()),
        }
    }

    #[test]
    fn token_fresh_iso_string_future() {
        let future = (Utc::now() + ChronoDuration::hours(1)).to_rfc3339();
        assert!(is_token_fresh(&creds_with_expiry(
            serde_json::Value::String(future)
        )));
    }

    #[test]
    fn token_not_fresh_iso_string_past() {
        let past = (Utc::now() - ChronoDuration::hours(1)).to_rfc3339();
        assert!(!is_token_fresh(&creds_with_expiry(
            serde_json::Value::String(past)
        )));
    }

    #[test]
    fn token_not_fresh_within_grace_window() {
        // 30s in future → less than the 60s safety margin → NOT fresh.
        let near = (Utc::now() + ChronoDuration::seconds(30)).to_rfc3339();
        assert!(!is_token_fresh(&creds_with_expiry(
            serde_json::Value::String(near)
        )));
    }

    #[test]
    fn token_fresh_epoch_ms_future() {
        let future_ms = (Utc::now() + ChronoDuration::hours(1)).timestamp_millis();
        assert!(is_token_fresh(&creds_with_expiry(
            serde_json::Value::Number(future_ms.into())
        )));
    }

    #[test]
    fn token_not_fresh_epoch_ms_past() {
        let past_ms = (Utc::now() - ChronoDuration::hours(1)).timestamp_millis();
        assert!(!is_token_fresh(&creds_with_expiry(
            serde_json::Value::Number(past_ms.into())
        )));
    }

    #[test]
    fn token_not_fresh_unparseable() {
        assert!(!is_token_fresh(&creds_with_expiry(serde_json::Value::Null)));
        assert!(!is_token_fresh(&creds_with_expiry(
            serde_json::Value::String("not-a-date".into())
        )));
    }

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
        let creds = creds_with_expiry(serde_json::Value::Null);
        let snap = map_to_snapshot(&creds, &usage);
        assert_eq!(snap.tiers.len(), 5);
        assert_eq!(snap.tiers[0].name, "5h Window");
        assert_eq!(snap.tiers[0].remaining, 80);
        assert_eq!(snap.tiers[1].name, "Weekly");
        assert_eq!(snap.tiers[1].remaining, 66); // 100 - 34
        assert_eq!(snap.tiers[2].remaining, 98);
        assert_eq!(snap.tiers[3].name, "Designs");
        assert_eq!(snap.tiers[4].name, "Daily Routines");
        // remaining = min across tiers
        assert_eq!(snap.remaining, 66);
        assert_eq!(snap.quota, 100);
        assert_eq!(snap.plan_type, "Max 20x");
    }

    #[test]
    fn parse_legacy_response_no_launch_windows() {
        // Older accounts: only five_hour + seven_day.
        let json = r#"{
            "five_hour": {"utilization": 12},
            "seven_day": {"utilization": 40}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let creds = ClaudeCredentials {
            access_token: "sk-ant-oat01-fake".into(),
            expires_at: serde_json::Value::Null,
            rate_limit_tier: Some("pro".into()),
        };
        let snap = map_to_snapshot(&creds, &usage);
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.plan_type, "Pro");
        assert_eq!(snap.remaining, 60); // 100 - 40
    }

    #[test]
    fn utilization_int_or_float() {
        // Both shapes coerce cleanly to i64.
        let int_form: UsageWindow = serde_json::from_str(r#"{"utilization": 9}"#).unwrap();
        let float_form: UsageWindow = serde_json::from_str(r#"{"utilization": 9.4}"#).unwrap();
        let float_round_up: UsageWindow = serde_json::from_str(r#"{"utilization": 9.6}"#).unwrap();
        assert_eq!(int_form.utilization, 9);
        assert_eq!(float_form.utilization, 9);
        assert_eq!(float_round_up.utilization, 10);
    }

    #[test]
    fn format_plan_buckets_match_mac() {
        // Mirrors ClaudeSourceStrategy.swift:166-178 so dual-writer
        // produces identical plan_type strings.
        assert_eq!(format_plan(Some("max_20x")), "Max 20x");
        assert_eq!(format_plan(Some("MAX_20x")), "Max 20x");
        assert_eq!(format_plan(Some("max 20x")), "Max 20x");
        assert_eq!(format_plan(Some("max_5x")), "Max 5x");
        assert_eq!(format_plan(Some("max 5x")), "Max 5x");
        // Generic max → 5x default.
        assert_eq!(format_plan(Some("max")), "Max 5x");
        assert_eq!(format_plan(Some("ultra")), "Ultra");
        assert_eq!(format_plan(Some("pro")), "Pro");
        assert_eq!(format_plan(Some("team")), "Team");
        assert_eq!(format_plan(Some("enterprise")), "Enterprise");
        // Substring fallback: "custom_enterprise" → "Enterprise".
        assert_eq!(format_plan(Some("custom_enterprise")), "Enterprise");
        assert_eq!(format_plan(Some("free")), "Free");
        // Empty / None / unrecognized all collapse to "Unknown".
        assert_eq!(format_plan(None), "Unknown");
        assert_eq!(format_plan(Some("")), "Unknown");
        assert_eq!(format_plan(Some("garbage_tier")), "Unknown");
    }

    #[test]
    fn snapshot_remaining_clamps_to_zero() {
        // utilization > 100 (shouldn't happen but defensive)
        let json = r#"{"five_hour": {"utilization": 120}}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&creds_with_expiry(serde_json::Value::Null), &usage);
        assert_eq!(snap.tiers[0].remaining, 0);
    }

    #[test]
    fn sonnet_falls_back_to_opus_when_sonnet_absent() {
        // Mirrors ClaudeSourceStrategy.swift:156: emit "Sonnet only" using
        // sonnet OR opus. Without this, Mac and Win uploads disagree on
        // tier count for accounts where only opus is populated.
        let json = r#"{
            "five_hour": {"utilization": 10},
            "seven_day_opus": {"utilization": 25, "resets_at": "2026-05-09T00:00:00Z"}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&creds_with_expiry(serde_json::Value::Null), &usage);
        // 5h Window + Sonnet only (from opus) = 2 tiers.
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
        let snap = map_to_snapshot(&creds_with_expiry(serde_json::Value::Null), &usage);
        let sonnet = snap.tiers.iter().find(|t| t.name == "Sonnet only").unwrap();
        // sonnet wins: remaining = 100 - 5 = 95 (not 100 - 90 = 10).
        assert_eq!(sonnet.remaining, 95);
    }

    #[test]
    fn launch_window_present_null_emits_full_remaining() {
        // Mirrors ClaudeOAuthStrategy.swift:152-160 parseLaunchWindow:
        // present-but-null means "rolled out but unused" — emit a tier
        // at 100% remaining so the row appears in the UI.
        let json = r#"{
            "five_hour": {"utilization": 0},
            "iguana_necktie": null,
            "seven_day_omelette": null
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&creds_with_expiry(serde_json::Value::Null), &usage);
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
        // Distinct from null: key absent means "not on the rollout" —
        // skip emission entirely.
        let json = r#"{"five_hour": {"utilization": 0}}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&creds_with_expiry(serde_json::Value::Null), &usage);
        assert!(!snap.tiers.iter().any(|t| t.name == "Designs"));
        assert!(!snap.tiers.iter().any(|t| t.name == "Daily Routines"));
    }

    #[test]
    fn outer_session_reset_taken_from_five_hour() {
        // Mirrors ClaudeSourceStrategy.swift:217 — outer reset_time is the
        // 5h Window reset. Without this field on the upload, helper_sync
        // writes NULL to provider_quotas.reset_time, flickering against
        // Mac's writes.
        let json = r#"{
            "five_hour": {"utilization": 20, "resets_at": "2026-05-02T22:00:00Z"},
            "seven_day": {"utilization": 50, "resets_at": "2026-05-09T00:00:00Z"}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&creds_with_expiry(serde_json::Value::Null), &usage);
        assert_eq!(snap.session_reset.as_deref(), Some("2026-05-02T22:00:00Z"));
    }

    #[test]
    fn outer_session_reset_none_when_five_hour_missing() {
        let json = r#"{"seven_day": {"utilization": 50}}"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&creds_with_expiry(serde_json::Value::Null), &usage);
        assert!(snap.session_reset.is_none());
    }
}
