//! Moonshot (Kimi developer platform) balance collection — port of macOS
//! `MoonshotCollector` (itself derived from steipete/CodexBar, MIT).
//!
//! Endpoint: `GET {base}/v1/users/me/balance`
//! Auth: `Authorization: Bearer <apiKey>` — env `MOONSHOT_API_KEY` or the
//! Settings-stored `moonshot_api_key`.
//! Base: env `MOONSHOT_API_BASE` (default `https://api.moonshot.ai`; China
//! users set `https://api.moonshot.cn`).
//!
//! This is the DEVELOPER-platform balance (api-key), DISTINCT from the
//! consumer Kimi chat app (cookie). Pure uncapped USD balance, like DeepSeek.
//!
//! Response: `{ "code": 0, "status": true, "scode": "...",
//! "data": { "available_balance": f64, "voucher_balance": f64,
//! "cash_balance": f64 } }`. The Chinese-API success gate is `code == 0 &&
//! status == true` (faithful to upstream) — anything else is a failure.
//!
//! ## Unit scale — why `× 100_000`, not cents
//! The Mac Moonshot collector encodes USD as `units = round(usd * 100_000)`
//! and writes that to the shared `(user_id, "Moonshot")` row. To keep a
//! dual-writer (Mac + desktop, same account + key) converging on identical
//! numbers — instead of flip-flopping the displayed balance by 1000× every
//! sync — the desktop mirrors that SAME scale. (Contrast DeepSeek, whose Mac
//! collector uses cents `× 100`; each desktop collector matches its own Mac
//! twin's scale.) The desktop frontend renders tier counts raw, so a readable
//! "$X.XX" needs the status-text concept the snapshot doesn't yet have — a
//! known pre-existing limitation for all balance providers.

use std::time::Duration;

use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot, TierEntry};

const DEFAULT_BASE: &str = "https://api.moonshot.ai";
const USD_UNIT_SCALE: f64 = 100_000.0;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Deserialize)]
struct BalanceResponse {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    status: bool,
    #[serde(default)]
    #[allow(dead_code)] // surfaced only in the gate-failure message
    scode: Option<String>,
    #[serde(default)]
    data: BalanceData,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BalanceData {
    #[serde(default, rename = "available_balance")]
    available_balance: f64,
    #[serde(default, rename = "voucher_balance")]
    voucher_balance: f64,
    #[serde(default, rename = "cash_balance")]
    cash_balance: f64,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let key = match resolve_key() {
        Some(k) => k,
        None => {
            log::debug!("[Moonshot] no API key (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let body = fetch_balance(&key).await?;
    let resp: BalanceResponse =
        serde_json::from_str(&body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    check_gate(&resp)?;
    Ok(Some(map_to_snapshot(&resp.data)))
}

fn resolve_key() -> Option<String> {
    if let Ok(k) = std::env::var("MOONSHOT_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.moonshot_api_key)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn resolve_base() -> String {
    std::env::var("MOONSHOT_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

fn balance_url() -> String {
    format!("{}/v1/users/me/balance", resolve_base())
}

async fn fetch_balance(key: &str) -> Result<String, CollectorError> {
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let resp = client
        .get(balance_url())
        .header("Authorization", format!("Bearer {key}"))
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

/// Chinese-API success gate: `code == 0 && status == true`. Faithful to the
/// Mac collector — a 200 with `code != 0` is a business-logic failure, not a
/// valid balance.
fn check_gate(r: &BalanceResponse) -> Result<(), CollectorError> {
    if r.code == 0 && r.status {
        Ok(())
    } else {
        Err(CollectorError::SchemaOrIo(format!(
            "Moonshot gate failed: code {}, scode {}",
            r.code,
            r.scode.as_deref().unwrap_or("?")
        )))
    }
}

/// USD → integer units (`round(usd * 100_000)`), floored at 0 and saturating
/// at `i64::MAX`. Non-finite / non-positive → 0.
fn units(usd: f64) -> i64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    let u = (usd * USD_UNIT_SCALE).round();
    if u >= i64::MAX as f64 {
        i64::MAX
    } else {
        u as i64
    }
}

fn map_to_snapshot(d: &BalanceData) -> QuotaSnapshot {
    // Positive voucher / cash components as informational full tiers
    // (quota == remaining). Negative cash (deficit) is skipped — a negative
    // tier breaks the bar (the Mac's C-11 lesson).
    let mut tiers: Vec<TierEntry> = Vec::new();
    for (name, usd) in [("Voucher", d.voucher_balance), ("Cash", d.cash_balance)] {
        let u = units(usd);
        if u > 0 {
            tiers.push(TierEntry {
                name: name.to_string(),
                quota: u,
                remaining: u,
                reset_time: None,
            });
        }
    }
    // Uncapped balance ⇒ full-bar gauge of the available balance (the desktop
    // snapshot can't carry the Mac's nil gauge — same convention as DeepSeek).
    let available = units(d.available_balance);
    // Readable balance line (mirrors the Mac's "Balance: $X" status_text) — the
    // gauge shows raw ×100_000 units, so this is how the user reads the dollars.
    let mut status = format!("Balance: ${:.2}", d.available_balance.max(0.0));
    if d.cash_balance < 0.0 {
        status.push_str(&format!(" · ${:.2} in deficit", d.cash_balance.abs()));
    }
    QuotaSnapshot {
        status_text: Some(status),
        plan_type: "API key".to_string(),
        remaining: available,
        quota: available,
        session_reset: None,
        tiers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "code": 0,
        "status": true,
        "scode": "0x0",
        "data": {"available_balance": 5.0, "voucher_balance": 2.0, "cash_balance": 3.0}
    }"#;

    #[test]
    fn gate_ok_maps_available_and_component_tiers() {
        let r: BalanceResponse = serde_json::from_str(SAMPLE).unwrap();
        assert!(check_gate(&r).is_ok());
        let snap = map_to_snapshot(&r.data);
        assert_eq!(snap.plan_type, "API key");
        // $5.00 → 500_000 units.
        assert_eq!(snap.remaining, 500_000);
        assert_eq!(snap.quota, 500_000);
        assert_eq!(snap.tiers.len(), 2);
        assert_eq!(snap.tiers[0].name, "Voucher");
        assert_eq!(snap.tiers[0].quota, 200_000);
        assert_eq!(snap.tiers[0].remaining, 200_000);
        assert_eq!(snap.tiers[1].name, "Cash");
        assert_eq!(snap.tiers[1].quota, 300_000);
        assert_eq!(snap.status_text.as_deref(), Some("Balance: $5.00"));
    }

    #[test]
    fn gate_rejects_nonzero_code_and_false_status() {
        let r: BalanceResponse =
            serde_json::from_str(r#"{"code":1,"status":true,"data":{}}"#).unwrap();
        assert!(check_gate(&r).is_err());
        let r: BalanceResponse =
            serde_json::from_str(r#"{"code":0,"status":false,"data":{}}"#).unwrap();
        assert!(check_gate(&r).is_err());
    }

    #[test]
    fn skips_negative_cash_tier() {
        let r: BalanceResponse = serde_json::from_str(
            r#"{"code":0,"status":true,"data":{"available_balance":2.0,"voucher_balance":2.0,"cash_balance":-1.5}}"#,
        )
        .unwrap();
        let snap = map_to_snapshot(&r.data);
        // Only the positive Voucher tier survives; deficit cash is dropped.
        assert_eq!(snap.tiers.len(), 1);
        assert_eq!(snap.tiers[0].name, "Voucher");
        assert_eq!(snap.remaining, 200_000); // available $2.00
    }

    #[test]
    fn units_scale_floor_and_nonfinite() {
        assert_eq!(units(1.0), 100_000);
        assert_eq!(units(0.0), 0);
        assert_eq!(units(-4.0), 0);
        assert_eq!(units(f64::NAN), 0);
        assert_eq!(units(f64::INFINITY), 0); // non-finite guarded before scale
    }

    #[test]
    fn empty_data_zero_gauge_no_tiers() {
        let r: BalanceResponse =
            serde_json::from_str(r#"{"code":0,"status":true,"data":{}}"#).unwrap();
        let snap = map_to_snapshot(&r.data);
        assert_eq!(snap.remaining, 0);
        assert_eq!(snap.quota, 0);
        assert!(snap.tiers.is_empty());
    }

    #[test]
    fn balance_url_default_path() {
        // env-free assertion — default base, no env set in this test binary.
        assert!(balance_url().ends_with("/v1/users/me/balance"));
        assert!(balance_url().starts_with("https://"));
    }
}
