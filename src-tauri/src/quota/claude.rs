//! v0.4.0 — Claude OAuth-based quota collection.
//! v0.4.4 — schema corrected to nested-only.
//! v0.4.14 — active OAuth refresh on expiry. Mirrors v0.4.7-v0.4.12
//!   Gemini refresh work. When `expiresAt` is past (or within the 60s
//!   safety margin), POST to Anthropic's `/v1/oauth/token` endpoint
//!   with the public PKCE client_id, atomically write the rotated
//!   refresh_token + new access_token + new expires_at back to disk
//!   (mode 0600 set BEFORE rename), then continue with the usage fetch.
//!   All branches log at INFO so a v0.4.9-style "silent half-fix"
//!   can't hide which path was taken. Unknown keys at both nesting
//!   levels round-trip via `flatten extra` so claude CLI's own
//!   subscriptionType + scopes etc. survive our write-back.
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
use serde::{Deserialize, Serialize};

use super::{CollectorError, QuotaSnapshot, TierEntry, PRE_EXPIRY_BUFFER_MS};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const ANTHROPIC_BETA: &str = "oauth-2025-04-20";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Top-level shape of `~/.claude/.credentials.json` (claude CLI ≥2.x).
/// Single key `claudeAiOauth` wrapping the OAuth payload.
///
/// `flatten extra` preserves unknown top-level keys across our
/// write-back path. Anthropic doesn't currently ship any sibling keys
/// to `claudeAiOauth`, but if they ever add (e.g.) telemetry blocks,
/// this guarantees we don't silently drop them when v0.4.14 writes
/// the refreshed tokens back to disk.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClaudeCredentialsFile {
    #[serde(
        rename = "claudeAiOauth",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    oauth: Option<ClaudeOAuthInner>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Inner OAuth block. Field names match CodexBar upstream
/// `ClaudeOAuthCredentialModels.swift:65-78` exactly.
///
/// `flatten extra` preserves keys we don't deserialize (notably
/// `subscriptionType`, which claude CLI itself reads), so atomic
/// write-back from the v0.4.14 refresh path doesn't drop them.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClaudeOAuthInner {
    #[serde(
        rename = "accessToken",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    access_token: Option<String>,
    /// v0.4.14 — consumed by `claude_refresh::refresh()` on expiry.
    /// Anthropic rotates this on every refresh, so we persist the new
    /// value the response carries.
    #[serde(
        rename = "refreshToken",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    refresh_token: Option<String>,
    /// Epoch milliseconds. CodexBar parses as Double, not String —
    /// real claude CLI never writes ISO-8601 here. v0.4.3 had a string|
    /// number branching deserializer based on incorrect docstring
    /// assumption; v0.4.4 drops the string path entirely.
    #[serde(rename = "expiresAt", default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<f64>,
    /// Reserved — kept for completeness with CodexBar struct; not read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[allow(dead_code)]
    scopes: Vec<String>,
    /// Plan tier source-of-truth ("max_20x", "max_5x", "pro", etc.).
    /// CodexBar reads this field; v0.4.2 `format_plan` mirrors the same
    /// lowercase substring matching used in `ClaudeSourceStrategy.swift:166-178`.
    /// Note: Anthropic also writes `subscriptionType` in the file but
    /// CodexBar upstream does NOT consume it — we follow upstream.
    #[serde(
        rename = "rateLimitTier",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    rate_limit_tier: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
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
/// freshness, refreshes via Anthropic's OAuth endpoint when expired,
/// hits the OAuth /usage API, and maps the response to a portable
/// `QuotaSnapshot`.
///
/// Return shape (v0.4.20):
/// - `Ok(Some(snap))` — success.
/// - `Ok(None)` — user not signed in (expected idle state). Provider
///   card stays at last-known cached state without an error badge.
/// - `Err(...)` — schema drift / OAuth refresh failure / HTTP failure.
///   The orchestrator caches this so the Providers tab can render a
///   red badge with the tooltip set to `error.message()`.
///
/// v0.4.14 — every branch logs at INFO. Previously several branches
/// were DEBUG (filtered at INFO global level since v0.3.4) which made
/// "expired token + no refresh attempt" indistinguishable from "file
/// missing" in user logs. Mirrors the v0.4.10 Gemini fix.
pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let path = match credentials_path() {
        Some(p) => p,
        None => {
            log::info!("[Claude] collect: could not resolve home dir — skipping");
            return Ok(None);
        }
    };
    log::info!("[Claude] collect: reading creds from {}", path.display());
    let mut file = match read_credentials(&path) {
        Ok(Some(f)) => f,
        Ok(None) => {
            log::info!(
                "[Claude] collect: .credentials.json absent at {} — run `claude` CLI to authenticate",
                path.display()
            );
            return Ok(None);
        }
        Err(e) => {
            log::warn!("[Claude] .credentials.json parse failed (non-fatal): {e}");
            return Err(CollectorError::SchemaOrIo(format!(
                ".credentials.json parse failed: {e}"
            )));
        }
    };
    let mut oauth = match file.oauth.take() {
        Some(o) => o,
        None => {
            log::warn!(
                "[Claude] .credentials.json missing 'claudeAiOauth' key — schema mismatch \
                 (real claude CLI ≥2.x writes nested shape; see CodexBar 82bbcde)"
            );
            return Err(CollectorError::SchemaOrIo(
                ".credentials.json missing 'claudeAiOauth' key — schema mismatch".into(),
            ));
        }
    };

    let now_ms = Utc::now().timestamp_millis() as f64;
    let fresh = is_token_fresh(&oauth);
    log::info!(
        "[Claude] collect: expires_at_ms={:?} now_ms={} fresh={} has_refresh_token={} has_access_token={}",
        oauth.expires_at,
        now_ms,
        fresh,
        oauth
            .refresh_token
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        oauth
            .access_token
            .as_deref()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
    );

    // v0.4.14 — active refresh on expiry.
    if !fresh {
        let refresh_token = match oauth.refresh_token.clone() {
            Some(t) if !t.is_empty() => t,
            _ => {
                log::info!(
                    "[Claude] expired access_token + no refresh_token in .credentials.json — \
                     run `claude` CLI to re-authenticate"
                );
                return Ok(None);
            }
        };
        log::info!(
            "[Claude] expired access_token — attempting OAuth refresh via Anthropic console (refresh_token len={})",
            refresh_token.len()
        );
        match super::claude_refresh::refresh(&refresh_token).await {
            Ok(refreshed) => {
                oauth.access_token = Some(refreshed.access_token.clone());
                oauth.refresh_token = Some(refreshed.refresh_token.clone());
                oauth.expires_at = Some(now_ms + (refreshed.expires_in as f64) * 1000.0);
                file.oauth = Some(oauth.clone());
                if let Err(e) = write_creds_atomic(&path, &file) {
                    log::warn!(
                        "[Claude] refresh succeeded but write-back to .credentials.json failed \
                         (non-fatal — token used for this cycle, will re-refresh next launch): {e}"
                    );
                } else {
                    log::info!(
                        "[Claude] refresh wrote new tokens to {} (atomic, mode 0600)",
                        path.display()
                    );
                }
                log::info!(
                    "[Claude] OAuth token refreshed via Anthropic console (expires in {}s)",
                    refreshed.expires_in
                );
            }
            Err(e) => {
                log::warn!("[Claude] OAuth refresh failed (non-fatal, falling back to skip): {e}");
                return Err(CollectorError::RefreshFailed(format!(
                    "OAuth refresh failed: {e}"
                )));
            }
        }
    } else {
        log::info!("[Claude] access_token still fresh — using cached creds without refresh");
    }

    let access_token = match oauth.access_token.as_deref().filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => {
            log::warn!(
                "[Claude] claudeAiOauth.accessToken absent or empty after refresh decision — \
                 corrupted oauth block"
            );
            return Err(CollectorError::SchemaOrIo(
                "claudeAiOauth.accessToken empty after refresh decision".into(),
            ));
        }
    };

    match fetch_usage(&access_token).await {
        Ok(usage) => {
            let snap = map_to_snapshot(&oauth, &usage);
            log::info!(
                "[Claude] collect: success — plan={} tiers={} remaining={}",
                snap.plan_type,
                snap.tiers.len(),
                snap.remaining,
            );
            Ok(Some(snap))
        }
        Err(e) => {
            log::warn!("[Claude] OAuth /usage fetch failed (non-fatal): {e}");
            Err(CollectorError::Http(format!("OAuth /usage: {e}")))
        }
    }
}

/// Atomic write to `~/.claude/.credentials.json` using the same
/// temp-then-rename pattern as `gemini.rs::write_creds_atomic`. Mode
/// 0600 set BEFORE rename so there's no permission window. The
/// `flatten extra` fields on both struct levels guarantee we round-trip
/// every key claude CLI itself writes (notably `subscriptionType` —
/// claude CLI consumes it for its own gating).
fn write_creds_atomic(target: &Path, file: &ClaudeCredentialsFile) -> Result<(), String> {
    let dir = target.parent().ok_or_else(|| "no parent dir".to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;
    let tmp = tempfile::Builder::new()
        .prefix(".credentials.")
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|e| format!("tempfile: {e}"))?;
    let text = serde_json::to_string_pretty(file).map_err(|e| format!("serialize: {e}"))?;
    {
        use std::io::Write;
        let mut f = tmp.as_file();
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        f.sync_all().map_err(|e| format!("sync: {e}"))?;
    }
    set_creds_file_mode_0600(tmp.path())?;
    tmp.persist(target)
        .map_err(|e| format!("rename: {}", e.error))?;
    Ok(())
}

#[cfg(unix)]
fn set_creds_file_mode_0600(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat: {e}"))?
        .permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod: {e}"))
}

#[cfg(not(unix))]
fn set_creds_file_mode_0600(_path: &Path) -> Result<(), String> {
    // Windows: NTFS per-user ACL on the parent ~/.claude/ dir is
    // inherited by .credentials.json — no chmod equivalent needed.
    Ok(())
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
/// `PRE_EXPIRY_BUFFER_MS` (5 min) in the future. Missing `expiresAt`
/// is treated as not-fresh (defensive — a credentials block without
/// expiry can't be safely used).
///
/// v0.4.19 — buffer bumped from 60s to 5 min so refresh fires PROACTIVELY,
/// before the token actually expires. With a 120s background sync
/// cycle, a 60s buffer left no headroom for missed ticks; 5 min
/// absorbs ~2 missed cycles. Shared with Gemini via the
/// `quota::PRE_EXPIRY_BUFFER_MS` constant so refresh timing stays
/// consistent across providers.
fn is_token_fresh(oauth: &ClaudeOAuthInner) -> bool {
    let Some(exp_ms) = oauth.expires_at else {
        return false;
    };
    let now_ms = Utc::now().timestamp_millis() as f64;
    exp_ms > now_ms + PRE_EXPIRY_BUFFER_MS
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
            extra: serde_json::Map::new(),
        }
    }

    fn oauth_with_tier(tier: Option<&str>) -> ClaudeOAuthInner {
        ClaudeOAuthInner {
            access_token: Some("sk-ant-oat01-fake".into()),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
            rate_limit_tier: tier.map(String::from),
            extra: serde_json::Map::new(),
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
        // v0.4.14: unknown field `subscriptionType` is now preserved in
        // `extra` so atomic write-back doesn't silently drop it. CodexBar
        // 82bbcde does not consume it for plan_type — we follow upstream.
        assert_eq!(
            oauth.extra.get("subscriptionType").and_then(|v| v.as_str()),
            Some("max"),
            "v0.4.14 must preserve subscriptionType in `extra` for round-trip"
        );
    }

    // v0.4.14 — atomic write-back round-trip preserves unknown keys.

    #[test]
    fn write_creds_atomic_round_trip_preserves_subscription_type() {
        // Real claude CLI writes `subscriptionType` in the file. v0.4.14's
        // refresh path mutates oauth.{accessToken,refreshToken,expiresAt}
        // and writes the file back. Without `flatten extra`, every refresh
        // would silently drop subscriptionType — claude CLI may consume
        // it for its own gating, so this is a real regression risk.
        let original = r#"{
            "claudeAiOauth": {
                "accessToken": "old-access",
                "refreshToken": "old-refresh",
                "expiresAt": 1777761408739,
                "scopes": ["user:profile"],
                "subscriptionType": "max",
                "rateLimitTier": "max_20x"
            }
        }"#;
        let mut file: ClaudeCredentialsFile = serde_json::from_str(original).unwrap();
        // Mutate the way the v0.4.14 refresh path does.
        if let Some(oauth) = file.oauth.as_mut() {
            oauth.access_token = Some("new-access".into());
            oauth.refresh_token = Some("new-refresh".into());
            oauth.expires_at = Some(1900000000000.0);
        }

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".credentials.json");
        write_creds_atomic(&path, &file).unwrap();

        // Read back and assert all fields including the unknowns survived.
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: ClaudeCredentialsFile = serde_json::from_str(&text).unwrap();
        let oauth = parsed.oauth.unwrap();
        assert_eq!(oauth.access_token.as_deref(), Some("new-access"));
        assert_eq!(oauth.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(oauth.expires_at, Some(1900000000000.0));
        assert_eq!(oauth.rate_limit_tier.as_deref(), Some("max_20x"));
        assert_eq!(
            oauth.extra.get("subscriptionType").and_then(|v| v.as_str()),
            Some("max"),
            "subscriptionType must survive atomic write-back"
        );
    }

    #[test]
    fn write_creds_atomic_preserves_top_level_unknown_keys() {
        // Defensive: if Anthropic ever adds a sibling key to claudeAiOauth
        // (telemetry, feature flags, etc.), our write-back must not drop it.
        let original = r#"{
            "claudeAiOauth": {
                "accessToken": "x",
                "expiresAt": 1777761408739,
                "rateLimitTier": "pro"
            },
            "futureTelemetryBlob": {"foo": "bar", "n": 42}
        }"#;
        let file: ClaudeCredentialsFile = serde_json::from_str(original).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".credentials.json");
        write_creds_atomic(&path, &file).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v.get("futureTelemetryBlob")
                .and_then(|f| f.get("foo"))
                .and_then(|f| f.as_str()),
            Some("bar"),
            "top-level unknown keys must survive atomic write-back"
        );
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
        // v0.4.19: buffer is now 5 min (PRE_EXPIRY_BUFFER_MS). Token
        // expiring in 4 min is INSIDE the buffer → not fresh, refresh
        // will fire proactively this cycle.
        let now_ms = Utc::now().timestamp_millis() as f64;
        let inside_buffer = oauth_with_expires_ms(Some(now_ms + 4.0 * 60.0 * 1000.0));
        assert!(
            !is_token_fresh(&inside_buffer),
            "token expiring within PRE_EXPIRY_BUFFER_MS must NOT be considered fresh"
        );
    }

    #[test]
    fn token_fresh_outside_pre_expiry_buffer() {
        // Token expiring in 6 min is OUTSIDE the 5-min buffer → still
        // fresh, refresh defers to next cycle.
        let now_ms = Utc::now().timestamp_millis() as f64;
        let outside_buffer = oauth_with_expires_ms(Some(now_ms + 6.0 * 60.0 * 1000.0));
        assert!(
            is_token_fresh(&outside_buffer),
            "token expiring outside PRE_EXPIRY_BUFFER_MS is still fresh"
        );
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

    // v0.4.20 — error-path reachability for the orchestrator's
    // CollectorError surface. The actual `collect()` end-to-end is
    // network-bound; here we pin the read_credentials seam that all
    // schema-drift error paths funnel through.

    #[test]
    fn read_credentials_returns_ok_none_on_missing_file() {
        // collect() converts this to Ok(None) — silent skip, no badge.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.json");
        let result = read_credentials(&missing).unwrap();
        assert!(
            result.is_none(),
            "missing file is the 'user not signed in' case → Ok(None)"
        );
    }

    #[test]
    fn read_credentials_returns_err_on_malformed_json() {
        // collect() converts this to Err(SchemaOrIo) — surfaces a red
        // badge with "JSON: ..." in the tooltip.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".credentials.json");
        std::fs::write(&path, r#"{not even close to json"#).unwrap();
        let result = read_credentials(&path);
        assert!(
            result.is_err(),
            "malformed JSON is the 'schema drift' case → Err(SchemaOrIo)"
        );
    }
}
