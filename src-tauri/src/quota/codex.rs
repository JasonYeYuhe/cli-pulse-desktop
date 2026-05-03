//! Codex (OpenAI) quota collection — port of macOS
//! `Collectors/CodexCollector.swift`.
//!
//! Auth source: `~/.codex/auth.json` (or `$CODEX_HOME/auth.json`).
//! JSON shape: `{ "tokens": { "access_token", "refresh_token",
//! "id_token", "account_id" }, "last_refresh": "ISO-8601",
//! "OPENAI_API_KEY": "sk-..." }`. The `OPENAI_API_KEY` falls back as
//! the access_token if `tokens.access_token` is absent (Mac line 89-91).
//!
//! Token refresh: if `last_refresh` > 8 days old (or absent), POST to
//! `https://auth.openai.com/oauth/token` with the public client_id
//! `app_EMoamEEZ73f0CkXaXp7hrann` (per RFC 6749 §2.2 client_id is
//! non-secret). On refresh failure, proceed with the existing token
//! (matches Mac's `try?` non-fatal pattern).
//!
//! Usage endpoint: `GET https://chatgpt.com/backend-api/wham/usage`
//! with `Authorization: Bearer <token>`, `User-Agent: cli-pulse-desktop/<v>`,
//! optional `ChatGPT-Account-Id: <id>`. 30s timeout.
//!
//! Tiers emitted: "5h Window", "Weekly", "Credits".
//! Best-effort: failures return `None`, structured `[Codex]` log line.
//! Log levels (v0.4.4):
//!   - file absent / no token: `debug!` (user not signed in)
//!   - file present but JSON parse fails: `warn!` (schema drift)
//!
//! v0.4.4 fix — `credits.balance` parser:
//!   v0.4.3 spec assumed `balance` was a JSON number, but the real
//!   `/wham/usage` response returns a JSON STRING (e.g. `"5.43"`). Every
//!   v0.4.3 sync therefore failed `resp.json::<UsageResponse>()` with
//!   `parse: error decoding response body`. v0.4.4 adds a string|number
//!   custom deserializer. Verified by `wham_inspect.py` 2026-05-03 JST
//!   against a live ChatGPT Plus account.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{QuotaSnapshot, TierEntry};

const REFRESH_URL: &str = "https://auth.openai.com/oauth/token";
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// Public OAuth client_id per RFC 6749 §2.2 (no secret). Same value
/// shipped by Mac at `CodexCollector.swift:134`. Codex 2026-05-02
/// review confirmed safe to ship.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REFRESH_STALENESS_DAYS: i64 = 8;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
const USAGE_TIMEOUT: Duration = Duration::from_secs(30);
/// $1 = 100,000 units (matches Mac line 237). KNOWN BUG: i32 overflow
/// at ~$21k balance — see openrouter.rs module docs.
const CREDITS_SCALE: f64 = 100_000.0;

#[derive(Debug, Clone, Default, Deserialize)]
struct AuthFile {
    #[serde(default)]
    tokens: Option<TokensBlock>,
    #[serde(default)]
    last_refresh: Option<String>,
    #[serde(rename = "OPENAI_API_KEY", default)]
    openai_api_key: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TokensBlock {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    // `id_token` exists in auth.json but isn't used by /wham/usage —
    // serde silently ignores it on deserialize.
    #[serde(default)]
    account_id: Option<String>,
}

#[derive(Debug, Clone)]
struct CodexAuth {
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
    needs_refresh: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct RefreshResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    // `id_token` is returned by the OAuth refresh response but unused
    // by the desktop — serde silently ignores it.
}

#[derive(Debug, Clone, Default, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    plan_type: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimit>,
    #[serde(default)]
    credits: Option<Credits>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RateLimit {
    #[serde(default)]
    primary_window: Option<RateWindow>,
    #[serde(default)]
    secondary_window: Option<RateWindow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct RateWindow {
    #[serde(default, deserialize_with = "deser_int")]
    used_percent: i64,
    #[serde(default)]
    reset_at: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Credits {
    #[serde(default)]
    has_credits: bool,
    #[serde(default)]
    unlimited: bool,
    /// Real `/wham/usage` returns `balance` as a JSON STRING (e.g. `"5.43"`),
    /// not a number — verified 2026-05-03 JST. v0.4.3's `Option<f64>`
    /// rejected the string and broke parse for every sync cycle.
    /// v0.4.4: accept either string or number via custom deserializer.
    #[serde(default, deserialize_with = "deser_balance_string_or_number")]
    balance: Option<f64>,
}

/// Top-level entry. Reads the persisted Codex auth, refreshes if
/// stale (best-effort), hits /wham/usage, returns the snapshot.
pub async fn collect() -> Option<QuotaSnapshot> {
    let path = match auth_path() {
        Some(p) => p,
        None => {
            log::debug!("[Codex] could not resolve home dir — skipping");
            return None;
        }
    };
    let mut auth = match read_auth(&path) {
        Ok(Some(a)) => a,
        Ok(None) => {
            log::debug!("[Codex] auth.json absent or no access_token — skipping");
            return None;
        }
        Err(e) => {
            log::warn!("[Codex] auth.json parse failed (non-fatal): {e}");
            return None;
        }
    };
    if auth.needs_refresh {
        match refresh_tokens(&auth).await {
            Ok(refreshed) => {
                auth = refreshed;
                let _ = write_auth(&path, &auth);
            }
            Err(e) => {
                log::warn!("[Codex] OAuth refresh failed (non-fatal, using stale token): {e}");
                // Continue with existing token. Mac line 30-35.
            }
        }
    }
    match fetch_usage(&auth).await {
        Ok(usage) => Some(map_to_snapshot(&usage)),
        Err(e) => {
            log::warn!("[Codex] /wham/usage fetch failed (non-fatal): {e}");
            None
        }
    }
}

fn auth_path() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("CODEX_HOME") {
        if !env.is_empty() {
            return Some(PathBuf::from(env).join("auth.json"));
        }
    }
    Some(dirs::home_dir()?.join(".codex").join("auth.json"))
}

/// Read + parse auth.json with three-state outcome:
/// - `Ok(Some(auth))` — file present, parsed, has usable access_token.
/// - `Ok(None)` — file absent OR file parses but no usable token (both
///   are "user not signed in" → debug skip).
/// - `Err(msg)` — file present but read or JSON parse failed (schema drift).
fn read_auth(path: &Path) -> Result<Option<CodexAuth>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("IO: {e}")),
    };
    let file: AuthFile = serde_json::from_str(&text).map_err(|e| format!("JSON: {e}"))?;
    Ok(parse_auth(&file))
}

fn parse_auth(file: &AuthFile) -> Option<CodexAuth> {
    let tokens = file.tokens.clone().unwrap_or_default();
    // access_token: prefer tokens.access_token, fall back to OPENAI_API_KEY.
    let access_token = tokens
        .access_token
        .clone()
        .or_else(|| file.openai_api_key.clone())
        .filter(|s| !s.is_empty())?;
    Some(CodexAuth {
        access_token,
        refresh_token: tokens.refresh_token.clone(),
        account_id: tokens.account_id.clone(),
        needs_refresh: needs_refresh(file.last_refresh.as_deref()),
    })
}

fn needs_refresh(last_refresh: Option<&str>) -> bool {
    let Some(s) = last_refresh else { return true };
    let Ok(parsed) = DateTime::parse_from_rfc3339(s) else {
        return true;
    };
    let last = parsed.with_timezone(&Utc);
    Utc::now().signed_duration_since(last) > chrono::Duration::days(REFRESH_STALENESS_DAYS)
}

async fn refresh_tokens(auth: &CodexAuth) -> Result<CodexAuth, String> {
    let refresh_token = auth
        .refresh_token
        .as_ref()
        .ok_or("no refresh_token in auth.json")?;
    let client = reqwest::Client::builder()
        .timeout(REFRESH_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let body = serde_json::json!({
        "client_id": CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "scope": "openid profile email",
    });
    let resp = client
        .post(REFRESH_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(120).collect();
        return Err(format!("HTTP {} — {}", status.as_u16(), snippet));
    }
    let parsed: RefreshResponse = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    let new_access = parsed
        .access_token
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| auth.access_token.clone());
    let new_refresh = parsed
        .refresh_token
        .filter(|s| !s.is_empty())
        .or_else(|| auth.refresh_token.clone());
    Ok(CodexAuth {
        access_token: new_access,
        refresh_token: new_refresh,
        account_id: auth.account_id.clone(),
        needs_refresh: false,
    })
}

fn write_auth(path: &Path, auth: &CodexAuth) -> Result<(), String> {
    // Round-trip: read existing JSON to preserve unknown fields, then
    // patch the tokens block + last_refresh.
    let existing = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let mut json: serde_json::Value =
        serde_json::from_str(&existing).map_err(|e| format!("parse: {e}"))?;
    let obj = json.as_object_mut().ok_or("auth.json not an object")?;
    let tokens = obj
        .entry("tokens")
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    if let Some(t) = tokens.as_object_mut() {
        t.insert(
            "access_token".into(),
            serde_json::Value::String(auth.access_token.clone()),
        );
        if let Some(rt) = &auth.refresh_token {
            t.insert(
                "refresh_token".into(),
                serde_json::Value::String(rt.clone()),
            );
        }
    }
    obj.insert(
        "last_refresh".into(),
        serde_json::Value::String(Utc::now().to_rfc3339()),
    );
    let serialized = serde_json::to_string_pretty(&json).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, serialized).map_err(|e| format!("write: {e}"))
}

async fn fetch_usage(auth: &CodexAuth) -> Result<UsageResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(USAGE_TIMEOUT)
        .user_agent(concat!("cli-pulse-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let mut req = client
        .get(USAGE_URL)
        .bearer_auth(&auth.access_token)
        .header("Accept", "application/json");
    if let Some(aid) = &auth.account_id {
        if !aid.is_empty() {
            req = req.header("ChatGPT-Account-Id", aid);
        }
    }
    let resp = req.send().await.map_err(|e| format!("request: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(120).collect();
        return Err(format!("HTTP {} — {}", status.as_u16(), snippet));
    }
    resp.json::<UsageResponse>()
        .await
        .map_err(|e| format!("parse: {e}"))
}

fn map_to_snapshot(usage: &UsageResponse) -> QuotaSnapshot {
    let mut tiers = Vec::new();
    let mut session_reset = None;

    if let Some(rl) = &usage.rate_limit {
        if let Some(w) = &rl.primary_window {
            session_reset = parse_reset_to_iso(&w.reset_at);
            tiers.push(TierEntry {
                name: "5h Window".to_string(),
                quota: 100,
                remaining: (100 - w.used_percent).clamp(0, 100),
                reset_time: session_reset.clone(),
            });
        }
        if let Some(w) = &rl.secondary_window {
            tiers.push(TierEntry {
                name: "Weekly".to_string(),
                quota: 100,
                remaining: (100 - w.used_percent).clamp(0, 100),
                reset_time: parse_reset_to_iso(&w.reset_at),
            });
        }
    }

    if let Some(c) = &usage.credits {
        if c.has_credits && !c.unlimited {
            if let Some(balance) = c.balance {
                let units = scale_to_units(balance);
                tiers.push(TierEntry {
                    name: "Credits".to_string(),
                    quota: units,
                    remaining: units,
                    reset_time: None,
                });
            }
        }
    }

    let plan_type = usage
        .plan_type
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown".to_string());

    // Outer remaining: prefer 5h Window (matches Mac primary semantics);
    // fall back to min across emitted tiers; else 100.
    let outer_remaining = tiers.first().map(|t| t.remaining).unwrap_or(100);

    QuotaSnapshot {
        plan_type,
        remaining: outer_remaining,
        quota: 100,
        session_reset,
        tiers,
    }
}

/// `reset_at` is either an epoch double (seconds) or an ISO-8601
/// string. Normalize to ISO-8601 for storage consistency.
fn parse_reset_to_iso(v: &Option<serde_json::Value>) -> Option<String> {
    match v.as_ref()? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => {
            let seconds = n.as_f64()?;
            DateTime::<Utc>::from_timestamp(seconds.round() as i64, 0).map(|dt| dt.to_rfc3339())
        }
        _ => None,
    }
}

fn scale_to_units(dollars: f64) -> i64 {
    let units = (dollars * CREDITS_SCALE).round();
    if units >= i64::MAX as f64 {
        i64::MAX
    } else if units <= 0.0 {
        0
    } else {
        units as i64
    }
}

/// serde helper: coerce JSON number (Int or Float) into i64. Anthropic-
/// style copy from Claude's quota module — Codex `used_percent` returns
/// either Int or Float per Mac line 252-258.
fn deser_int<'de, D>(d: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Null => Ok(0),
        serde_json::Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f.round() as i64))
            .ok_or_else(|| Error::custom("used_percent not a number")),
        _ => Err(Error::custom("used_percent not a number")),
    }
}

/// serde helper: accept `balance` as either JSON number or JSON string.
/// v0.4.4 fix — real `/wham/usage` returns balance as `"5.43"` (string),
/// not `5.43` (number). The v0.4.3 spec assumed number; every cycle's
/// `resp.json::<UsageResponse>()` therefore failed with "parse: error
/// decoding response body". Verified via wham_inspect.py 2026-05-03 JST.
fn deser_balance_string_or_number<'de, D>(d: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::Number(n) => Ok(n.as_f64()),
        serde_json::Value::String(s) => {
            if s.is_empty() {
                Ok(None)
            } else {
                s.parse::<f64>().map(Some).map_err(Error::custom)
            }
        }
        _ => Err(Error::custom("balance must be string, number, or null")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_auth_with_tokens_block() {
        let json = r#"{
            "tokens": {"access_token": "xyz", "refresh_token": "rt", "account_id": "acc"},
            "last_refresh": "2026-04-25T10:00:00Z"
        }"#;
        let file: AuthFile = serde_json::from_str(json).unwrap();
        let auth = parse_auth(&file).unwrap();
        assert_eq!(auth.access_token, "xyz");
        assert_eq!(auth.refresh_token.as_deref(), Some("rt"));
        assert_eq!(auth.account_id.as_deref(), Some("acc"));
    }

    #[test]
    fn parse_auth_falls_back_to_openai_api_key() {
        let json = r#"{"OPENAI_API_KEY": "sk-proj-fallback"}"#;
        let file: AuthFile = serde_json::from_str(json).unwrap();
        let auth = parse_auth(&file).unwrap();
        assert_eq!(auth.access_token, "sk-proj-fallback");
    }

    #[test]
    fn parse_auth_returns_none_when_no_token() {
        let file: AuthFile = serde_json::from_str("{}").unwrap();
        assert!(parse_auth(&file).is_none());
    }

    #[test]
    fn needs_refresh_8_day_threshold() {
        let recent = (Utc::now() - chrono::Duration::days(3)).to_rfc3339();
        assert!(!needs_refresh(Some(&recent)));
        let old = (Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        assert!(needs_refresh(Some(&old)));
        assert!(needs_refresh(None));
        assert!(needs_refresh(Some("not-a-date")));
    }

    #[test]
    fn parse_usage_full() {
        let json = r#"{
            "plan_type": "Plus",
            "rate_limit": {
                "primary_window": {"used_percent": 30, "reset_at": "2026-05-02T22:00:00Z"},
                "secondary_window": {"used_percent": 50, "reset_at": "2026-05-09T00:00:00Z"}
            },
            "credits": {"has_credits": true, "unlimited": false, "balance": 5.43}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 3);
        assert_eq!(snap.tiers[0].name, "5h Window");
        assert_eq!(snap.tiers[0].remaining, 70);
        assert_eq!(snap.tiers[1].name, "Weekly");
        assert_eq!(snap.tiers[1].remaining, 50);
        assert_eq!(snap.tiers[2].name, "Credits");
        assert_eq!(snap.tiers[2].quota, 543_000); // 5.43 * 100k
        assert_eq!(snap.plan_type, "Plus");
        assert_eq!(snap.session_reset.as_deref(), Some("2026-05-02T22:00:00Z"));
    }

    #[test]
    fn parse_usage_balance_as_string() {
        // v0.4.4 — reproduces the actual `/wham/usage` response shape
        // observed via wham_inspect.py 2026-05-03 JST. Real Anthropic
        // ships `balance` as a JSON string, not a number.
        let json = r#"{
            "plan_type": "Plus",
            "rate_limit": {
                "primary_window": {"used_percent": 30, "reset_at": 1746789600},
                "secondary_window": {"used_percent": 50, "reset_at": 1747304400}
            },
            "credits": {
                "has_credits": true,
                "unlimited": false,
                "balance": "5.43",
                "approx_cloud_messages": [10, 100],
                "approx_local_messages": [5, 50],
                "overage_limit_reached": false
            },
            "account_id": "acc-fake",
            "additional_rate_limits": null,
            "code_review_rate_limit": null,
            "email": "user@example.com",
            "promo": null,
            "rate_limit_reached_type": null,
            "referral_beacon": null,
            "spend_control": {"individual_limit": null, "reached": false},
            "user_id": "user-fake"
        }"#;
        let usage: UsageResponse = serde_json::from_str(json)
            .expect("v0.4.4 must accept string balance + ignore unknown fields");
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 3);
        assert_eq!(snap.tiers[2].name, "Credits");
        assert_eq!(snap.tiers[2].quota, 543_000); // "5.43" parsed → 543k units
        assert_eq!(snap.plan_type, "Plus");
    }

    #[test]
    fn balance_deserializer_accepts_string_number_or_null() {
        // String form (v0.4.4 real-world).
        let s: Credits =
            serde_json::from_str(r#"{"has_credits": true, "balance": "9.99"}"#).unwrap();
        assert_eq!(s.balance, Some(9.99));

        // Number form (v0.4.3 spec assumption + back-compat).
        let n: Credits = serde_json::from_str(r#"{"has_credits": true, "balance": 9.99}"#).unwrap();
        assert_eq!(n.balance, Some(9.99));

        // Null form.
        let null_form: Credits =
            serde_json::from_str(r#"{"has_credits": true, "balance": null}"#).unwrap();
        assert_eq!(null_form.balance, None);

        // Empty string → None.
        let empty: Credits =
            serde_json::from_str(r#"{"has_credits": true, "balance": ""}"#).unwrap();
        assert_eq!(empty.balance, None);

        // Absent field → None (default).
        let absent: Credits = serde_json::from_str(r#"{"has_credits": true}"#).unwrap();
        assert_eq!(absent.balance, None);
    }

    #[test]
    fn parse_usage_no_credits_block() {
        let json = r#"{
            "plan_type": "Free",
            "rate_limit": {"primary_window": {"used_percent": 10}}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.plan_type, "Free");
    }

    #[test]
    fn parse_usage_reset_at_epoch_double() {
        let json = r#"{
            "rate_limit": {"primary_window": {"used_percent": 0, "reset_at": 1746789600}}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert!(snap.session_reset.is_some());
        assert!(snap.session_reset.unwrap().starts_with("20"));
    }

    #[test]
    fn parse_usage_used_percent_float() {
        let json = r#"{
            "rate_limit": {"primary_window": {"used_percent": 9.6, "reset_at": "2026-05-02T22:00:00Z"}}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert_eq!(snap.tiers[0].remaining, 90); // 100 - round(9.6) = 100 - 10 = 90
    }

    #[test]
    fn parse_usage_unlimited_credits_skips_tier() {
        let json = r#"{
            "credits": {"has_credits": true, "unlimited": true, "balance": "999.99"}
        }"#;
        let usage: UsageResponse = serde_json::from_str(json).unwrap();
        let snap = map_to_snapshot(&usage);
        assert!(!snap.tiers.iter().any(|t| t.name == "Credits"));
    }
}
