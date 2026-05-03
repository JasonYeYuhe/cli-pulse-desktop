//! Gemini quota collection — port of macOS
//! `Collectors/GeminiCollector.swift`, file-only path.
//!
//! Auth source: `~/.gemini/oauth_creds.json` only. v0.4.3 SKIPS the
//! macOS Keychain priority that Mac's `GeminiOAuthManager.swift` uses,
//! since `ASWebAuthenticationSession` + `SecItem` Keychain APIs are
//! macOS-native. Cross-platform OAuth (browser redirect listener +
//! `keyring` crate) deferred to v0.4.5+.
//!
//! Token refresh: NOT in v0.4.3. If `expiry_date < now()`, silent
//! skip + DEBUG log. User must run `gemini` CLI to refresh the file.
//! v0.4.4 will add explicit `CollectorStatus::Expired` state so the
//! UI can render an actionable warning (Gemini 3.1 Pro 2026-05-02
//! review).
//!
//! Endpoints (both POST, JSON body, 10s timeout):
//! - `https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist`
//!   for tier id + project discovery.
//! - `https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota`
//!   for per-model bucket data.
//!
//! Tiers emitted: by model family — "Pro", "Flash", "Flash Lite"
//! (preferred order) followed by any unknown families alphabetically.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::{QuotaSnapshot, TierEntry};

const TIER_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const QUOTA_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const PREFERRED_FAMILIES: &[&str] = &["Pro", "Flash", "Flash Lite"];

#[derive(Debug, Clone, Deserialize)]
struct CredsFile {
    #[serde(default)]
    access_token: Option<String>,
    /// Epoch milliseconds (Mac line 102-104).
    #[serde(default)]
    expiry_date: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LoadCodeAssistResponse {
    #[serde(rename = "currentTier", default)]
    current_tier: Option<TierBlock>,
    /// Either a string OR an object with `projectId` / `id`. Mac line 173-181.
    #[serde(rename = "cloudaicompanionProject", default)]
    project: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct TierBlock {
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct QuotaResponse {
    #[serde(default)]
    buckets: Vec<QuotaBucket>,
    /// Some Mac responses include resetTime at the root as a fallback
    /// (Mac line 222-226). Same key fallback chain.
    #[serde(default, alias = "reset_time", alias = "quotaResetTime")]
    reset_time: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct QuotaBucket {
    #[serde(rename = "modelId")]
    model_id: String,
    #[serde(rename = "remainingFraction", default = "default_fraction_one")]
    remaining_fraction: f64,
    #[serde(
        default,
        alias = "reset_time",
        alias = "resetAt",
        alias = "quotaResetTime"
    )]
    reset_time: Option<String>,
}

fn default_fraction_one() -> f64 {
    1.0
}

#[derive(Debug)]
struct TierInfo {
    tier_id: Option<String>,
    project_id: Option<String>,
}

/// Collect Gemini quota from `~/.gemini/oauth_creds.json`. Returns
/// `None` if file absent / token expired / HTTP fails — `[Gemini]`
/// log line in each case.
///
/// Log levels (v0.4.4 align with claude/codex):
///   - file absent / token expired / no access_token: `debug!` (expected)
///   - file present but JSON parse fails: `warn!` (schema drift)
pub async fn collect() -> Option<QuotaSnapshot> {
    let path = match creds_path() {
        Some(p) => p,
        None => {
            log::debug!("[Gemini] could not resolve home dir — skipping");
            return None;
        }
    };
    let creds = match read_creds(&path) {
        Ok(Some(c)) => c,
        Ok(None) => {
            log::debug!("[Gemini] oauth_creds.json absent — run `gemini` CLI to authenticate");
            return None;
        }
        Err(e) => {
            log::warn!("[Gemini] oauth_creds.json parse failed (non-fatal): {e}");
            return None;
        }
    };
    let token = creds.access_token?;
    if let Some(exp_ms) = creds.expiry_date {
        let now_ms = chrono::Utc::now().timestamp_millis() as f64;
        if exp_ms < now_ms {
            log::debug!(
                "[Gemini] oauth_creds.json expiry_date past now() — run `gemini` CLI to refresh \
                 (active OAuth refresh deferred to v0.4.5+)"
            );
            return None;
        }
    }

    let tier_info = fetch_tier(&token).await.unwrap_or(TierInfo {
        tier_id: None,
        project_id: None,
    });

    let quota = match fetch_quota(&token, tier_info.project_id.as_deref()).await {
        Ok(q) => q,
        Err(e) => {
            log::warn!("[Gemini] retrieveUserQuota fetch failed (non-fatal): {e}");
            return None;
        }
    };

    Some(map_to_snapshot(&tier_info, &quota))
}

fn creds_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".gemini").join("oauth_creds.json"))
}

/// Read + parse oauth_creds.json with three-state outcome:
/// - `Ok(Some(creds))` — file present and JSON parsed.
/// - `Ok(None)` — file absent (legitimate "user not signed in" skip).
/// - `Err(msg)` — file present but read or JSON parse failed (schema drift).
fn read_creds(path: &Path) -> Result<Option<CredsFile>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("JSON: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("IO: {e}")),
    }
}

async fn fetch_tier(token: &str) -> Result<TierInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let body = serde_json::json!({
        "metadata": {"ideType": "GEMINI_CLI", "pluginType": "GEMINI"}
    });
    let resp = client
        .post(TIER_URL)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let parsed: LoadCodeAssistResponse = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    Ok(parse_tier_info(&parsed))
}

fn parse_tier_info(raw: &LoadCodeAssistResponse) -> TierInfo {
    let tier_id = raw.current_tier.as_ref().and_then(|t| t.id.clone());
    let project_id = match &raw.project {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Object(obj)) => obj
            .get("projectId")
            .and_then(|v| v.as_str().map(String::from))
            .or_else(|| obj.get("id").and_then(|v| v.as_str().map(String::from))),
        _ => None,
    };
    TierInfo {
        tier_id,
        project_id,
    }
}

async fn fetch_quota(token: &str, project_id: Option<&str>) -> Result<QuotaResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let body = if let Some(pid) = project_id {
        serde_json::json!({"project": pid})
    } else {
        serde_json::json!({})
    };
    let resp = client
        .post(QUOTA_URL)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    resp.json::<QuotaResponse>()
        .await
        .map_err(|e| format!("parse: {e}"))
}

fn map_to_snapshot(tier: &TierInfo, quota: &QuotaResponse) -> QuotaSnapshot {
    // Group buckets by family, keep lowest remaining fraction per family
    // (most-constrained dimension wins). Mac line 246-255.
    let mut family_best: BTreeMap<String, (f64, Option<String>)> = BTreeMap::new();
    for bucket in &quota.buckets {
        let family = classify_model(&bucket.model_id);
        let reset = bucket
            .reset_time
            .clone()
            .or_else(|| quota.reset_time.clone());
        family_best
            .entry(family.to_string())
            .and_modify(|existing| {
                if bucket.remaining_fraction < existing.0 {
                    *existing = (bucket.remaining_fraction, reset.clone());
                }
            })
            .or_insert((bucket.remaining_fraction, reset));
    }

    let mut tiers = Vec::with_capacity(family_best.len());
    // Emit preferred families first, then unknown alphabetical.
    for family in PREFERRED_FAMILIES {
        if let Some((fraction, reset)) = family_best.remove(*family) {
            tiers.push(TierEntry {
                name: family.to_string(),
                quota: 100,
                remaining: ((fraction * 100.0).round() as i64).clamp(0, 100),
                reset_time: reset,
            });
        }
    }
    for (family, (fraction, reset)) in family_best {
        tiers.push(TierEntry {
            name: family,
            quota: 100,
            remaining: ((fraction * 100.0).round() as i64).clamp(0, 100),
            reset_time: reset,
        });
    }

    let plan_type = match tier.tier_id.as_deref() {
        Some("standard-tier") => "Paid",
        Some("free-tier") => "Free",
        Some("legacy-tier") => "Legacy",
        _ => "Unknown",
    }
    .to_string();

    // Outer remaining/reset: prefer first emitted tier (matches Mac
    // line 291-297: Pro > Flash > Flash Lite > min across all).
    let outer = tiers.first().cloned();
    let outer_remaining = outer.as_ref().map(|t| t.remaining).unwrap_or(100);
    let outer_reset = outer.and_then(|t| t.reset_time);

    QuotaSnapshot {
        plan_type,
        remaining: outer_remaining,
        quota: 100,
        session_reset: outer_reset,
        tiers,
    }
}

fn classify_model(model_id: &str) -> &'static str {
    let l = model_id.to_lowercase();
    if l.contains("flash-lite") || l.contains("flash_lite") {
        "Flash Lite"
    } else if l.contains("flash") {
        "Flash"
    } else if l.contains("pro") {
        "Pro"
    } else {
        // Mac falls back to raw model id; for the static-str return
        // here we'd need owned strings. Use a generic bucket so the
        // upload doesn't drop the data — actual classification of
        // niche models is a v0.4.4 polish item.
        "Other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_model_buckets() {
        assert_eq!(classify_model("gemini-2.5-flash-lite"), "Flash Lite");
        assert_eq!(classify_model("Gemini-2.0-Flash"), "Flash");
        assert_eq!(classify_model("gemini-1.5-pro-001"), "Pro");
        assert_eq!(classify_model("flash_lite-experimental"), "Flash Lite");
        assert_eq!(classify_model("gemini-3-something-new"), "Other");
    }

    #[test]
    fn parse_quota_response_with_buckets() {
        let json = r#"{
            "buckets": [
                {"modelId": "gemini-2.5-flash", "remainingFraction": 0.83, "resetTime": "2026-05-09T00:00:00Z"},
                {"modelId": "gemini-1.5-pro-001", "remainingFraction": 0.5, "resetTime": "2026-05-09T00:00:00Z"}
            ]
        }"#;
        let q: QuotaResponse = serde_json::from_str(json).unwrap();
        assert_eq!(q.buckets.len(), 2);
        assert_eq!(q.buckets[0].model_id, "gemini-2.5-flash");
    }

    #[test]
    fn snapshot_orders_pro_flash_flash_lite_then_unknown() {
        let tier = TierInfo {
            tier_id: Some("standard-tier".into()),
            project_id: None,
        };
        let q = QuotaResponse {
            buckets: vec![
                QuotaBucket {
                    model_id: "gemini-2.5-flash-lite".into(),
                    remaining_fraction: 0.9,
                    reset_time: None,
                },
                QuotaBucket {
                    model_id: "weird-model".into(),
                    remaining_fraction: 0.3,
                    reset_time: None,
                },
                QuotaBucket {
                    model_id: "gemini-pro-1.5".into(),
                    remaining_fraction: 0.5,
                    reset_time: None,
                },
                QuotaBucket {
                    model_id: "gemini-2.5-flash".into(),
                    remaining_fraction: 0.7,
                    reset_time: None,
                },
            ],
            reset_time: None,
        };
        let snap = map_to_snapshot(&tier, &q);
        assert_eq!(snap.tiers[0].name, "Pro");
        assert_eq!(snap.tiers[1].name, "Flash");
        assert_eq!(snap.tiers[2].name, "Flash Lite");
        assert_eq!(snap.tiers[3].name, "Other");
        assert_eq!(snap.plan_type, "Paid");
    }

    #[test]
    fn family_lowest_fraction_wins() {
        // Two flash models: 0.4 and 0.7 → emit 0.4 (most constrained).
        let q = QuotaResponse {
            buckets: vec![
                QuotaBucket {
                    model_id: "gemini-flash-fast".into(),
                    remaining_fraction: 0.7,
                    reset_time: None,
                },
                QuotaBucket {
                    model_id: "gemini-flash-slow".into(),
                    remaining_fraction: 0.4,
                    reset_time: None,
                },
            ],
            reset_time: None,
        };
        let snap = map_to_snapshot(
            &TierInfo {
                tier_id: None,
                project_id: None,
            },
            &q,
        );
        assert_eq!(snap.tiers[0].name, "Flash");
        assert_eq!(snap.tiers[0].remaining, 40);
    }

    #[test]
    fn plan_type_buckets() {
        let q = QuotaResponse::default();
        for (input, expected) in [
            ("standard-tier", "Paid"),
            ("free-tier", "Free"),
            ("legacy-tier", "Legacy"),
            ("something-new", "Unknown"),
        ] {
            let snap = map_to_snapshot(
                &TierInfo {
                    tier_id: Some(input.into()),
                    project_id: None,
                },
                &q,
            );
            assert_eq!(snap.plan_type, expected);
        }
    }

    #[test]
    fn parse_tier_info_project_string_or_object() {
        let s = LoadCodeAssistResponse {
            current_tier: Some(TierBlock {
                id: Some("standard-tier".into()),
            }),
            project: Some(serde_json::json!("my-proj-id")),
        };
        let info = parse_tier_info(&s);
        assert_eq!(info.tier_id.as_deref(), Some("standard-tier"));
        assert_eq!(info.project_id.as_deref(), Some("my-proj-id"));

        let o = LoadCodeAssistResponse {
            current_tier: None,
            project: Some(serde_json::json!({"projectId": "from-obj", "id": "alt"})),
        };
        let info = parse_tier_info(&o);
        assert_eq!(info.project_id.as_deref(), Some("from-obj"));

        let alt = LoadCodeAssistResponse {
            current_tier: None,
            project: Some(serde_json::json!({"id": "fallback-only"})),
        };
        let info = parse_tier_info(&alt);
        assert_eq!(info.project_id.as_deref(), Some("fallback-only"));
    }

    #[test]
    fn bucket_reset_time_falls_back_to_global() {
        let q = QuotaResponse {
            buckets: vec![QuotaBucket {
                model_id: "gemini-pro".into(),
                remaining_fraction: 0.5,
                reset_time: None, // bucket has none — fallback to global
            }],
            reset_time: Some("2026-05-09T00:00:00Z".into()),
        };
        let snap = map_to_snapshot(
            &TierInfo {
                tier_id: None,
                project_id: None,
            },
            &q,
        );
        assert_eq!(
            snap.tiers[0].reset_time.as_deref(),
            Some("2026-05-09T00:00:00Z")
        );
    }
}
