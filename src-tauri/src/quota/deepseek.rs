//! DeepSeek quota collection — port of macOS `DeepSeekCollector.swift`
//! (itself derived from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET https://api.deepseek.com/user/balance`
//! Auth: `Authorization: Bearer <apiKey>` — env `DEEPSEEK_API_KEY` /
//! `DEEPSEEK_KEY`, or the Settings-stored `deepseek_api_key`.
//!
//! Pure-balance (no quota gauge): DeepSeek exposes current balances per
//! currency, not a usage/limit split. Each positive-balance currency becomes a
//! tier (cents; `quota == remaining` — a balance isn't a depleting quota). The
//! top-level gauge mirrors the primary balance so the Providers overall bar
//! reads "you have $X" (the desktop `QuotaSnapshot` can't carry the Mac's nil
//! gauge; a full bar of the current balance is the least-wrong mapping).

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const BALANCE_URL: &str = "https://api.deepseek.com/user/balance";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct BalanceResponse {
    #[serde(default)]
    #[allow(dead_code)] // parsed for completeness; not surfaced in the gauge
    is_available: bool,
    #[serde(default)]
    balance_infos: Vec<BalanceInfo>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BalanceInfo {
    #[serde(default)]
    currency: String,
    // DeepSeek encodes balances as STRINGS ("5.43") — parse defensively.
    #[serde(default)]
    total_balance: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Balance {
    currency: String,
    total: f64,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let api_key = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[DeepSeek] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_balance(&api_key).await?;
    let resp: BalanceResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    Ok(Some(map_to_snapshot(&parse_balances(&resp))))
}

fn resolve_key() -> Option<String> {
    for env in ["DEEPSEEK_API_KEY", "DEEPSEEK_KEY"] {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.deepseek_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_balance(api_key: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(BALANCE_URL)
        .header("Authorization", format!("Bearer {api_key}"))
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

/// Parse string-encoded balances → typed `f64`. Skips a currency whose
/// `total_balance` isn't numeric (defensive; the Mac's documented port-gotcha).
fn parse_balances(r: &BalanceResponse) -> Vec<Balance> {
    r.balance_infos
        .iter()
        .filter_map(|b| {
            Some(Balance {
                currency: b.currency.clone(),
                total: b.total_balance.trim().parse().ok()?,
            })
        })
        .collect()
}

/// USD-positive → any-positive → USD → first (verbatim from CodexBar/Mac).
fn select_primary(balances: &[Balance]) -> Option<&Balance> {
    balances
        .iter()
        .find(|b| b.currency == "USD" && b.total > 0.0)
        .or_else(|| balances.iter().find(|b| b.total > 0.0))
        .or_else(|| balances.iter().find(|b| b.currency == "USD"))
        .or_else(|| balances.first())
}

fn to_cents(dollars: f64) -> i64 {
    let c = (dollars * 100.0).round();
    if c <= 0.0 {
        0
    } else if c >= i64::MAX as f64 {
        i64::MAX
    } else {
        c as i64
    }
}

fn map_to_snapshot(balances: &[Balance]) -> QuotaSnapshot {
    let tiers: Vec<TierEntry> = balances
        .iter()
        .filter(|b| b.total > 0.0)
        .map(|b| {
            let cents = to_cents(b.total);
            TierEntry {
                name: format!("{} Balance", b.currency),
                quota: cents,
                remaining: cents,
                reset_time: None,
            }
        })
        .collect();
    let primary = select_primary(balances);
    let primary_cents = primary.map(|b| to_cents(b.total)).unwrap_or(0);
    // Readable balance line (the gauge shows raw cents) — "$12.34 balance" for
    // USD, "88.00 CNY balance" otherwise.
    let status_text = primary.map(|b| {
        if b.currency == "USD" {
            format!("${:.2} balance", b.total.max(0.0))
        } else {
            format!("{:.2} {} balance", b.total.max(0.0), b.currency)
        }
    });
    QuotaSnapshot {
        status_text,
        plan_type: "API key".to_string(),
        remaining: primary_cents,
        quota: primary_cents,
        session_reset: None,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "is_available": true,
        "balance_infos": [
            {"currency":"USD","total_balance":"12.34","granted_balance":"2.00","topped_up_balance":"10.34"},
            {"currency":"CNY","total_balance":"88.00","granted_balance":"0","topped_up_balance":"88.00"}
        ]
    }"#;

    #[test]
    fn parses_string_balances() {
        let r: BalanceResponse = serde_json::from_str(SAMPLE).unwrap();
        let b = parse_balances(&r);
        assert_eq!(b.len(), 2);
        assert!((b[0].total - 12.34).abs() < 1e-9);
        assert_eq!(b[1].currency, "CNY");
    }

    #[test]
    fn skips_non_numeric_balance() {
        let r: BalanceResponse = serde_json::from_str(
            r#"{"balance_infos":[{"currency":"USD","total_balance":"oops"}]}"#,
        )
        .unwrap();
        assert!(parse_balances(&r).is_empty());
    }

    #[test]
    fn selects_usd_positive_primary() {
        let b = parse_balances(&serde_json::from_str::<BalanceResponse>(SAMPLE).unwrap());
        assert_eq!(select_primary(&b).unwrap().currency, "USD");
        // any-positive when USD is zero
        let alt = vec![
            Balance {
                currency: "USD".into(),
                total: 0.0,
            },
            Balance {
                currency: "CNY".into(),
                total: 5.0,
            },
        ];
        assert_eq!(select_primary(&alt).unwrap().currency, "CNY");
    }

    #[test]
    fn snapshot_per_currency_tiers_and_primary_gauge() {
        let snap = map_to_snapshot(&parse_balances(
            &serde_json::from_str::<BalanceResponse>(SAMPLE).unwrap(),
        ));
        assert_eq!(snap.plan_type, "API key");
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "USD Balance");
        assert_eq!(snap.tiers[0].quota, 1234); // $12.34 → cents
        assert_eq!(snap.tiers[0].remaining, 1234); // balance isn't depleting
                                                   // Top-level gauge mirrors the USD-positive primary.
        assert_eq!(snap.quota, 1234);
        assert_eq!(snap.remaining, 1234);
        // Readable balance line (the gauge shows raw cents).
        assert_eq!(snap.status_text.as_deref(), Some("$12.34 balance"));
    }

    #[test]
    fn empty_balances_zero_gauge() {
        let snap = map_to_snapshot(&[]);
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
        assert!(snap.tiers.is_empty());
    }
}
