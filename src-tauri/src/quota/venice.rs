//! Venice balance collection — port of macOS `VeniceCollector` (itself
//! derived from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET https://api.venice.ai/api/v1/billing/balance`
//! Auth: `Authorization: Bearer <apiKey>` — env `VENICE_API_KEY` /
//! `VENICE_KEY`, or the Settings-stored `venice_api_key`.
//!
//! Dual-currency balance: `balances.usd` + `balances.diem`, with an optional
//! `diemEpochAllocation` cap. Values arrive as either JSON numbers OR numeric
//! strings, so a flexible decoder coerces both (vendored from upstream).
//!
//! Scale: **cents (× 100)** — matches the Mac Venice collector (verified
//! `VeniceCollector.swift` `Int(x * 100)`), so a dual-writer converges. (Not
//! every Mac balance collector uses cents — Moonshot uses `× 100_000` — so
//! each desktop collector mirrors ITS OWN twin's scale.)
//!
//! Mapping: the DIEM balance becomes a **cap-aware** tier when
//! `diemEpochAllocation > 0` (`quota = cap`, `remaining = min(cap, diem)` — a
//! real depleting quota); otherwise a no-cap full tier (`quota == remaining`,
//! like DeepSeek). USD is always a no-cap full tier. The top-level gauge
//! mirrors the most-informative primary — the cap-aware DIEM quota if present,
//! else a positive USD balance, else a positive DIEM balance — because the
//! desktop `QuotaSnapshot` can't carry the Mac's nil gauge.

use std::time::Duration;

use serde::{Deserialize, Deserializer};

use super::{CollectorError, QuotaSnapshot, TierEntry};

const BALANCE_URL: &str = "https://api.venice.ai/api/v1/billing/balance";
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct BalanceResponse {
    #[serde(default)]
    balances: Balances,
    #[serde(
        default,
        rename = "diemEpochAllocation",
        deserialize_with = "de_flexible_f64"
    )]
    diem_epoch_allocation: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Balances {
    #[serde(default, deserialize_with = "de_flexible_f64")]
    diem: Option<f64>,
    #[serde(default, deserialize_with = "de_flexible_f64")]
    usd: Option<f64>,
}

/// Accept a JSON number, a numeric string ("7.50"), or null → `None`.
/// Non-numeric / empty strings → `None` (defensive; the Mac throws, but a
/// skipped currency is the least-disruptive desktop behavior). Vendored from
/// upstream's `decodeFlexibleDoubleIfPresent`.
fn de_flexible_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let v: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    Ok(match v {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<f64>().ok()
            }
        }
        Some(_) => None,
    })
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let token = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Venice] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_balance(&token).await?;
    let resp: BalanceResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    Ok(Some(map_to_snapshot(&resp)))
}

fn resolve_key() -> Option<String> {
    for env in ["VENICE_API_KEY", "VENICE_KEY"] {
        if let Ok(k) = std::env::var(env) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.venice_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn fetch_balance(token: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(BALANCE_URL)
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

/// USD → cents, floored at 0, saturating at `i64::MAX`. Non-finite → 0.
fn to_cents(dollars: f64) -> i64 {
    if !dollars.is_finite() || dollars <= 0.0 {
        return 0;
    }
    let c = (dollars * 100.0).round();
    if c >= i64::MAX as f64 {
        i64::MAX
    } else {
        c as i64
    }
}

fn map_to_snapshot(r: &BalanceResponse) -> QuotaSnapshot {
    let mut tiers: Vec<TierEntry> = Vec::new();

    // DIEM: cap-aware (real quota) when an allocation is present, else no-cap.
    let mut diem_primary: Option<(i64, i64)> = None;
    if let Some(diem) = r.balances.diem {
        let cents = to_cents(diem);
        match r.diem_epoch_allocation {
            Some(alloc) if alloc > 0.0 => {
                let cap = to_cents(alloc);
                let remaining = cap.min(cents);
                tiers.push(TierEntry {
                    name: "DIEM Balance".to_string(),
                    quota: cap,
                    remaining,
                    reset_time: None,
                });
                diem_primary = Some((cap, remaining));
            }
            _ => {
                if cents > 0 {
                    tiers.push(TierEntry {
                        name: "DIEM Balance".to_string(),
                        quota: cents,
                        remaining: cents,
                        reset_time: None,
                    });
                }
            }
        }
    }

    let usd_cents = r.balances.usd.map(to_cents).unwrap_or(0);
    if usd_cents > 0 {
        tiers.push(TierEntry {
            name: "USD Balance".to_string(),
            quota: usd_cents,
            remaining: usd_cents,
            reset_time: None,
        });
    }

    // Primary top-level gauge: cap-aware DIEM (real quota) > positive USD >
    // positive DIEM balance > none.
    let (quota, remaining) = if let Some((q, rem)) = diem_primary {
        (q, rem)
    } else if usd_cents > 0 {
        (usd_cents, usd_cents)
    } else {
        let diem_cents = r.balances.diem.map(to_cents).unwrap_or(0);
        (diem_cents, diem_cents)
    };

    // Readable balance line (the gauge shows raw cents). Prefer USD, then a
    // cap-aware DIEM (X / Y), then a plain DIEM balance.
    let usd = r.balances.usd.unwrap_or(0.0);
    let diem = r.balances.diem.unwrap_or(0.0);
    let status_text = if usd > 0.0 {
        Some(format!("${:.2} USD balance", usd))
    } else if let Some(alloc) = r.diem_epoch_allocation.filter(|a| *a > 0.0) {
        Some(format!("DIEM {:.2} / {:.2}", diem.max(0.0), alloc))
    } else if diem > 0.0 {
        Some(format!("DIEM {diem:.2} balance"))
    } else {
        None
    };

    QuotaSnapshot {
        status_text,
        plan_type: "API key".to_string(),
        remaining,
        quota,
        session_reset: None,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usd_and_diem_no_cap_usd_primary() {
        let r: BalanceResponse =
            serde_json::from_str(r#"{"balances":{"usd":12.34,"diem":5.0}}"#).unwrap();
        let snap = map_to_snapshot(&r);
        assert_eq!(snap.plan_type, "API key");
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "DIEM Balance");
        assert_eq!(snap.tiers[0].quota, 500); // $5.00, no-cap → quota==remaining
        assert_eq!(snap.tiers[0].remaining, 500);
        assert_eq!(snap.tiers[1].name, "USD Balance");
        assert_eq!(snap.tiers[1].quota, 1234);
        // USD is the primary top-level gauge (no cap-aware DIEM present).
        assert_eq!(snap.quota, 1234);
        assert_eq!(snap.remaining, 1234);
        assert_eq!(snap.status_text.as_deref(), Some("$12.34 USD balance"));
    }

    #[test]
    fn cap_aware_diem_is_real_quota_and_primary() {
        let r: BalanceResponse =
            serde_json::from_str(r#"{"balances":{"diem":8.0},"diemEpochAllocation":10.0}"#)
                .unwrap();
        let snap = map_to_snapshot(&r);
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "DIEM Balance");
        assert_eq!(snap.tiers[0].quota, 1000); // cap $10 → 1000 cents
        assert_eq!(snap.tiers[0].remaining, 800); // min(1000, 800)
        assert_eq!(snap.quota, 1000);
        assert_eq!(snap.remaining, 800);
    }

    #[test]
    fn diem_over_cap_is_clamped() {
        let r: BalanceResponse =
            serde_json::from_str(r#"{"balances":{"diem":15.0},"diemEpochAllocation":10.0}"#)
                .unwrap();
        let snap = map_to_snapshot(&r);
        assert_eq!(snap.tiers[0].quota, 1000);
        assert_eq!(snap.tiers[0].remaining, 1000); // min(1000, 1500)
        assert_eq!(snap.remaining, 1000);
    }

    #[test]
    fn flexible_double_accepts_numeric_strings() {
        // Values as strings; allocation "0" → not cap-aware.
        let r: BalanceResponse = serde_json::from_str(
            r#"{"balances":{"usd":"7.50","diem":"0"},"diemEpochAllocation":"0"}"#,
        )
        .unwrap();
        let snap = map_to_snapshot(&r);
        // usd "7.50" → 750 cents; diem "0" → no tier.
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "USD Balance");
        assert_eq!(snap.tiers[0].quota, 750);
        assert_eq!(snap.quota, 750);
    }

    #[test]
    fn null_and_empty_and_garbage_are_skipped() {
        // null balances, garbage allocation string → all skipped, zero gauge.
        let r: BalanceResponse = serde_json::from_str(
            r#"{"balances":{"usd":null,"diem":""},"diemEpochAllocation":"oops"}"#,
        )
        .unwrap();
        let snap = map_to_snapshot(&r);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.quota, 0);
        assert_eq!(snap.remaining, 0);
        // Fully empty object.
        let r: BalanceResponse = serde_json::from_str(r#"{}"#).unwrap();
        let snap = map_to_snapshot(&r);
        assert!(snap.tiers.is_empty());
        assert_eq!(snap.quota, 0);
    }
}
