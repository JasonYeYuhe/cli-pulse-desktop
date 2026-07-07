//! Kilo credit + subscription usage — port of macOS `KiloCollector`.
//!
//! Endpoint: `GET https://app.kilo.ai/api/trpc/user.getCreditBlocks,kiloPass.getState?batch=1`
//! (a tRPC **batch** call — two procedures in one request, response is a JSON
//! array of two `{result:{data:{json:…}}}` envelopes).
//! Auth: `Authorization: Bearer <token>` — env `KILO_API_KEY`, the Settings
//! `kilo_api_key`, or the Kilo CLI session file `~/.local/share/kilo/auth.json`
//! (`json.kilo.access`).
//!
//! Money arrives as **micro-USD** integers (`amount_mUsd` / `balance_mUsd`). We
//! mirror the Mac's display convention: micro-USD → USD (÷1e6) → **display
//! units at $1 = 100_000** (`Int(usd * 100_000)`, truncating toward zero like
//! Swift `Int()`), so a dual-writer (Mac + desktop, same key) converges on the
//! shared `(user_id, provider)` row. The readable dollar figure lives in
//! `status_text` ("$remaining / $total") since the raw ×100_000 units are
//! meaningless on their own.

use std::time::Duration;

use serde_json::Value;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const BATCH_URL: &str = "https://app.kilo.ai/api/trpc/user.getCreditBlocks,kiloPass.getState";
// tRPC batch input: procedure 0 and 1 each take a null json arg.
const BATCH_INPUT: &str = r#"{"0":{"json":null},"1":{"json":null}}"#;
const TIMEOUT: Duration = Duration::from_secs(15);
const MICRO_USD: f64 = 1_000_000.0;
const DISPLAY_SCALE: f64 = 100_000.0; // $1 = 100_000 display units

#[derive(Debug, Clone, Default)]
struct KiloUsage {
    credits_total_mu: f64,
    credits_remaining_mu: f64,
    sub_usage_usd: Option<f64>,
    sub_base_usd: Option<f64>,
    sub_bonus_usd: Option<f64>,
    tier: Option<String>,
    next_billing: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_token() {
        Some(t) => t,
        None => {
            log::debug!("[Kilo] no API key (env / Settings / CLI file) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_batch(&token).await?;
    let usage = parse_response(&body)?;
    Ok(Some(map_to_snapshot(&usage)))
}

fn resolve_token() -> Option<String> {
    if let Ok(k) = std::env::var("KILO_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    if let Some(k) = crate::provider_creds::load()
        .ok()
        .and_then(|c| c.kilo_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(k);
    }
    // Kilo CLI session file fallback: ~/.local/share/kilo/auth.json → kilo.access
    token_from_cli_file()
}

fn token_from_cli_file() -> Option<String> {
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())?;
    let path = std::path::Path::new(&home).join(".local/share/kilo/auth.json");
    let data = std::fs::read_to_string(path).ok()?;
    let json: Value = serde_json::from_str(&data).ok()?;
    json.pointer("/kilo/access")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

async fn fetch_batch(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(BATCH_URL)
        .query(&[("batch", "1"), ("input", BATCH_INPUT)])
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| CollectorError::Http(format!("request: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(CollectorError::Http(format!("HTTP {}", status.as_u16())));
    }
    resp.text()
        .await
        .map_err(|e| CollectorError::Http(format!("body: {e}")))
}

fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

fn parse_response(body: &str) -> Result<KiloUsage, CollectorError> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| CollectorError::SchemaOrIo(format!("parse: {e}")))?;
    let arr = v
        .as_array()
        .ok_or_else(|| CollectorError::SchemaOrIo("expected JSON array".into()))?;

    let mut usage = KiloUsage::default();

    // Procedure 0: user.getCreditBlocks → sum amount_mUsd / balance_mUsd.
    if let Some(blocks) = arr
        .first()
        .and_then(|p| p.pointer("/result/data/json/creditBlocks"))
        .and_then(Value::as_array)
    {
        for b in blocks {
            usage.credits_total_mu += num(b, "amount_mUsd").unwrap_or(0.0);
            usage.credits_remaining_mu += num(b, "balance_mUsd").unwrap_or(0.0);
        }
    }

    // Procedure 1: kiloPass.getState → subscription usage.
    if let Some(sub) = arr
        .get(1)
        .and_then(|p| p.pointer("/result/data/json/subscription"))
    {
        usage.sub_usage_usd = num(sub, "currentPeriodUsageUsd");
        usage.sub_base_usd = num(sub, "currentPeriodBaseCreditsUsd");
        usage.sub_bonus_usd = num(sub, "currentPeriodBonusCreditsUsd");
        usage.tier = sub
            .get("tier")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty());
        usage.next_billing = ["nextBillingAt", "nextRenewalAt", "renewsAt"]
            .iter()
            .find_map(|k| sub.get(*k).and_then(Value::as_str))
            .map(str::to_string)
            .filter(|s| !s.is_empty());
    }

    Ok(usage)
}

fn map_to_snapshot(k: &KiloUsage) -> QuotaSnapshot {
    let credits_total = k.credits_total_mu / MICRO_USD;
    let credits_remaining = k.credits_remaining_mu / MICRO_USD;
    let units = |usd: f64| (usd * DISPLAY_SCALE) as i64;

    let mut tiers: Vec<TierEntry> = Vec::new();
    if credits_total > 0.0 {
        tiers.push(TierEntry {
            name: "Credits".to_string(),
            quota: units(credits_total),
            remaining: units(credits_remaining),
            reset_time: None,
        });
    }
    if let (Some(base), Some(bonus)) = (k.sub_base_usd, k.sub_bonus_usd) {
        let sub_total = base + bonus;
        let sub_used = k.sub_usage_usd.unwrap_or(0.0);
        let sub_remaining = (sub_total - sub_used).max(0.0);
        tiers.push(TierEntry {
            name: "Kilo Pass".to_string(),
            quota: units(sub_total),
            remaining: units(sub_remaining),
            reset_time: k.next_billing.clone(),
        });
    }

    let plan = match k.tier.as_deref() {
        Some("tier_19") => "Starter".to_string(),
        Some("tier_49") => "Pro".to_string(),
        Some("tier_199") => "Expert".to_string(),
        Some(t) => t.to_string(),
        None => "Credits".to_string(),
    };

    QuotaSnapshot {
        status_text: Some(format!("${credits_remaining:.2} / ${credits_total:.2}")),
        plan_type: plan,
        remaining: units(credits_remaining),
        quota: if credits_total > 0.0 {
            units(credits_total)
        } else {
            0
        },
        session_reset: k.next_billing.clone(),
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"[
        {"result":{"data":{"json":{"creditBlocks":[
            {"amount_mUsd":10000000,"balance_mUsd":7500000},
            {"amount_mUsd":5000000,"balance_mUsd":5000000}
        ]}}}},
        {"result":{"data":{"json":{"subscription":{
            "tier":"tier_49",
            "currentPeriodUsageUsd":3.0,
            "currentPeriodBaseCreditsUsd":19.0,
            "currentPeriodBonusCreditsUsd":1.0,
            "nextBillingAt":"2026-08-01T00:00:00Z"
        }}}}}
    ]"#;

    #[test]
    fn sums_credit_blocks_and_scales_to_display_units() {
        let u = parse_response(FULL).unwrap();
        // total = 15 USD, remaining = 12.5 USD.
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.quota, 1_500_000); // 15 * 100_000
        assert_eq!(snap.remaining, 1_250_000); // 12.5 * 100_000
        assert_eq!(snap.plan_type, "Pro"); // tier_49
        assert_eq!(snap.status_text.as_deref(), Some("$12.50 / $15.00"));
        assert_eq!(snap.session_reset.as_deref(), Some("2026-08-01T00:00:00Z"));
    }

    #[test]
    fn builds_credits_and_kilo_pass_tiers() {
        let snap = map_to_snapshot(&parse_response(FULL).unwrap());
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Credits");
        assert_eq!(snap.tiers[0].quota, 1_500_000);
        // Kilo Pass: base+bonus = 20 USD, used 3 → remaining 17.
        assert_eq!(snap.tiers[1].name, "Kilo Pass");
        assert_eq!(snap.tiers[1].quota, 2_000_000);
        assert_eq!(snap.tiers[1].remaining, 1_700_000);
        assert_eq!(
            snap.tiers[1].reset_time.as_deref(),
            Some("2026-08-01T00:00:00Z")
        );
    }

    #[test]
    fn credits_only_no_subscription() {
        let body = r#"[{"result":{"data":{"json":{"creditBlocks":[
            {"amount_mUsd":2000000,"balance_mUsd":500000}
        ]}}}},{"result":{"data":{"json":{}}}}]"#;
        let snap = map_to_snapshot(&parse_response(body).unwrap());
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.plan_type, "Credits"); // no tier
        assert_eq!(snap.status_text.as_deref(), Some("$0.50 / $2.00"));
    }

    #[test]
    fn unknown_tier_passes_through_and_empty_credits_zero_quota() {
        let body = r#"[{"result":{"data":{"json":{"creditBlocks":[]}}}},
            {"result":{"data":{"json":{"subscription":{"tier":"tier_999"}}}}}]"#;
        let snap = map_to_snapshot(&parse_response(body).unwrap());
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.plan_type, "tier_999");
        assert!(snap.tiers.is_empty());
    }

    #[test]
    fn non_array_body_is_schema_error() {
        assert!(matches!(
            parse_response(r#"{"not":"an array"}"#),
            Err(CollectorError::SchemaOrIo(_))
        ));
    }
}
