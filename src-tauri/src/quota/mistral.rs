//! Mistral month-to-date spend collection — port of macOS `MistralCollector`
//! (itself derived from steipete/CodexBar, MIT).
//!
//! Mistral is pay-as-you-go — no quota cap, no credit balance — so this is a
//! **status-only** provider carrying an EXACT month-to-date SPEND (the
//! value-add), computed from `tokens × price`. Status line:
//! `"€X.XXXX this month · N tokens"`.
//!
//! Endpoint: `GET admin.mistral.ai/api/billing/v2/usage?month&year` (current
//! UTC month/year). Auth: **cookie-session** — the `Cookie:` header must
//! contain an `ory_session_*` cookie; a `csrftoken` cookie (if present) is
//! echoed as `X-CSRFTOKEN`. From env `MISTRAL_COOKIE` / `MISTRAL_SESSION_TOKEN`
//! or the Settings-stored `mistral_cookie`. Manual paste only.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Datelike;
use serde::Deserialize;

use super::{CollectorError, QuotaSnapshot};

const USAGE_URL: &str = "https://admin.mistral.ai/api/billing/v2/usage";
const TIMEOUT: Duration = Duration::from_secs(15);

// ── Vendored billing tree (only the cost/token-aggregation fields kept) ──

#[derive(Debug, Clone, Default, Deserialize)]
struct BillingResponse {
    #[serde(default)]
    completion: Option<Category>,
    #[serde(default)]
    ocr: Option<Category>,
    #[serde(default)]
    connectors: Option<Category>,
    #[serde(default)]
    audio: Option<Category>,
    #[serde(default, rename = "libraries_api")]
    libraries_api: Option<LibrariesCategory>,
    #[serde(default, rename = "fine_tuning")]
    fine_tuning: Option<FineTuningCategory>,
    #[serde(default, rename = "end_date")]
    end_date: Option<String>,
    #[serde(default, rename = "currency_symbol")]
    currency_symbol: Option<String>,
    #[serde(default)]
    prices: Vec<Price>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Category {
    #[serde(default)]
    models: HashMap<String, ModelUsage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LibrariesCategory {
    #[serde(default)]
    pages: Option<Category>,
    #[serde(default)]
    tokens: Option<Category>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FineTuningCategory {
    #[serde(default)]
    training: HashMap<String, ModelUsage>,
    #[serde(default)]
    storage: HashMap<String, ModelUsage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ModelUsage {
    #[serde(default)]
    input: Vec<Entry>,
    #[serde(default)]
    output: Vec<Entry>,
    #[serde(default)]
    cached: Vec<Entry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Entry {
    #[serde(default, rename = "billing_metric")]
    billing_metric: Option<String>,
    #[serde(default, rename = "billing_group")]
    billing_group: Option<String>,
    #[serde(default)]
    value: Option<i64>,
    #[serde(default, rename = "value_paid")]
    value_paid: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Price {
    #[serde(default, rename = "billing_metric")]
    billing_metric: Option<String>,
    #[serde(default, rename = "billing_group")]
    billing_group: Option<String>,
    #[serde(default)]
    price: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct MistralUsage {
    total_cost: f64,
    currency_symbol: String,
    total_tokens: i64,
    end_date: Option<String>,
}

pub async fn collect() -> Result<Option<QuotaSnapshot>, CollectorError> {
    let cookie = match resolve_cookie() {
        Some(c) => c,
        None => {
            log::debug!("[Mistral] no session cookie (env or Settings UI) — skipping");
            return Ok(None);
        }
    };
    let (has_session, csrf) = session_and_csrf(&cookie);
    if !has_session {
        return Err(CollectorError::SchemaOrIo(
            "Mistral: cookie has no ory_session_* (not signed in)".to_string(),
        ));
    }
    let body = fetch_usage(&cookie, csrf.as_deref()).await?;
    let usage = parse_usage(&body)?;
    Ok(Some(map_to_snapshot(&usage)))
}

fn resolve_cookie() -> Option<String> {
    for env in ["MISTRAL_COOKIE", "MISTRAL_SESSION_TOKEN"] {
        if let Ok(c) = std::env::var(env) {
            let c = c.trim().to_string();
            if !c.is_empty() {
                return Some(c);
            }
        }
    }
    crate::provider_creds::load()
        .ok()
        .and_then(|c| c.mistral_cookie)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// `(an ory_session_* cookie is present, the csrftoken value if any)`.
fn session_and_csrf(raw: &str) -> (bool, Option<String>) {
    let mut has_session = false;
    let mut csrf = None;
    for part in raw.split(';') {
        let mut it = part.splitn(2, '=');
        let (Some(name), Some(value)) = (it.next(), it.next()) else {
            continue;
        };
        let name = name.trim();
        if name.starts_with("ory_session_") {
            has_session = true;
        }
        if name.eq_ignore_ascii_case("csrftoken") {
            let v = value.trim();
            if !v.is_empty() {
                csrf = Some(v.to_string());
            }
        }
    }
    (has_session, csrf)
}

async fn fetch_usage(cookie: &str, csrf: Option<&str>) -> Result<String, CollectorError> {
    let now = chrono::Utc::now();
    let (month, year) = (now.month(), now.year());
    let client = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| CollectorError::Http(format!("client build: {e}")))?;
    let mut req = client
        .get(USAGE_URL)
        .query(&[("month", month.to_string()), ("year", year.to_string())])
        .header("Accept", "*/*")
        .header("Cookie", cookie)
        .header("Referer", "https://admin.mistral.ai/organization/usage")
        .header("Origin", "https://admin.mistral.ai");
    if let Some(csrf) = csrf {
        req = req.header("X-CSRFTOKEN", csrf);
    }
    let resp = req
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

/// `metric::group → price`; a `None` group defaults to "" so a flat-priced
/// category still matches.
fn build_price_index(prices: &[Price]) -> HashMap<String, f64> {
    let mut index = HashMap::new();
    for p in prices {
        if let (Some(metric), Some(price_str)) = (&p.billing_metric, &p.price) {
            if let Ok(value) = price_str.trim().parse::<f64>() {
                index.insert(
                    format!("{metric}::{}", p.billing_group.as_deref().unwrap_or("")),
                    value,
                );
            }
        }
    }
    index
}

fn price_for(entry: &Entry, prices: &HashMap<String, f64>) -> f64 {
    match &entry.billing_metric {
        Some(metric) => *prices
            .get(&format!(
                "{metric}::{}",
                entry.billing_group.as_deref().unwrap_or("")
            ))
            .unwrap_or(&0.0),
        None => 0.0,
    }
}

fn parse_usage(body: &str) -> Result<MistralUsage, CollectorError> {
    let resp: BillingResponse =
        serde_json::from_str(body).map_err(|e| CollectorError::Http(format!("parse: {e}")))?;
    let prices = build_price_index(&resp.prices);
    let mut cost = 0.0_f64;
    let mut tokens = 0_i64;

    let mut accumulate = |models: &HashMap<String, ModelUsage>, counts_tokens: bool| {
        for model in models.values() {
            for entry in model.input.iter().chain(&model.output).chain(&model.cached) {
                let units = entry.value_paid.or(entry.value).unwrap_or(0);
                cost += units as f64 * price_for(entry, &prices);
                if counts_tokens {
                    tokens += units;
                }
            }
        }
    };

    if let Some(c) = &resp.completion {
        accumulate(&c.models, true);
    }
    for cat in [&resp.ocr, &resp.connectors, &resp.audio]
        .into_iter()
        .flatten()
    {
        accumulate(&cat.models, false);
    }
    if let Some(lib) = &resp.libraries_api {
        if let Some(pages) = &lib.pages {
            accumulate(&pages.models, false);
        }
        if let Some(toks) = &lib.tokens {
            accumulate(&toks.models, true);
        }
    }
    if let Some(ft) = &resp.fine_tuning {
        accumulate(&ft.training, false);
        accumulate(&ft.storage, false);
    }

    Ok(MistralUsage {
        total_cost: cost.max(0.0), // clamp refund/credit adjustments
        currency_symbol: resp.currency_symbol.unwrap_or_else(|| "€".to_string()),
        total_tokens: tokens.max(0),
        end_date: resp.end_date.filter(|s| !s.is_empty()),
    })
}

/// Integer with thousands separators (e.g. `1234567` → `"1,234,567"`).
fn group_thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let bytes = digits.as_bytes();
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn map_to_snapshot(u: &MistralUsage) -> QuotaSnapshot {
    let mut status = format!(
        "{}{:.4} this month",
        u.currency_symbol,
        u.total_cost.max(0.0)
    );
    if u.total_tokens > 0 {
        status.push_str(&format!(" · {} tokens", group_thousands(u.total_tokens)));
    }
    // Status-only exact spend — no quota gauge.
    QuotaSnapshot {
        status_text: Some(status),
        plan_type: "Pay-as-you-go".to_string(),
        remaining: 0,
        quota: 0,
        session_reset: u.end_date.clone(),
        tiers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_and_csrf_detects_ory_and_extracts_csrf() {
        let (has, csrf) = session_and_csrf("ory_session_abc123=xyz; csrftoken=tok987; other=1");
        assert!(has);
        assert_eq!(csrf.as_deref(), Some("tok987"));
        // No ory_session → not signed in.
        let (has, _) = session_and_csrf("session=foo; csrftoken=t");
        assert!(!has);
    }

    #[test]
    fn aggregates_cost_and_tokens_from_billing_tree() {
        // completion: 1000 input units @ 0.001 + 500 output @ 0.002.
        // Price index keys: metric::group.
        let body = r#"{
            "currency_symbol": "€",
            "end_date": "2026-07-31T23:59:59Z",
            "prices": [
                {"billing_metric":"input_tokens","billing_group":"large","price":"0.001"},
                {"billing_metric":"output_tokens","billing_group":"large","price":"0.002"}
            ],
            "completion": {"models": {"mistral-large": {
                "input":  [{"billing_metric":"input_tokens","billing_group":"large","value":1000}],
                "output": [{"billing_metric":"output_tokens","billing_group":"large","value_paid":500}]
            }}}
        }"#;
        let u = parse_usage(body).unwrap();
        // cost = 1000*0.001 + 500*0.002 = 1.0 + 1.0 = 2.0
        assert!((u.total_cost - 2.0).abs() < 1e-9);
        // tokens counted only from completion: 1000 + 500 = 1500
        assert_eq!(u.total_tokens, 1500);
        assert_eq!(u.currency_symbol, "€");
        let snap = map_to_snapshot(&u);
        assert_eq!(snap.plan_type, "Pay-as-you-go");
        assert_eq!(snap.quota, 0);
        assert_eq!(
            snap.status_text.as_deref(),
            Some("€2.0000 this month · 1,500 tokens")
        );
        assert_eq!(snap.session_reset.as_deref(), Some("2026-07-31T23:59:59Z"));
    }

    #[test]
    fn value_paid_preferred_over_value_and_ocr_not_counted_for_tokens() {
        let body = r#"{
            "prices":[{"billing_metric":"pages","billing_group":"","price":"0.01"}],
            "ocr":{"models":{"ocr-1":{"input":[{"billing_metric":"pages","value":10,"value_paid":4}]}}}
        }"#;
        let u = parse_usage(body).unwrap();
        // value_paid (4) preferred → cost 4*0.01 = 0.04; ocr tokens NOT counted.
        assert!((u.total_cost - 0.04).abs() < 1e-9);
        assert_eq!(u.total_tokens, 0);
    }

    #[test]
    fn missing_price_zero_cost_and_no_token_suffix_for_non_completion() {
        // No price → cost 0. `ocr` doesn't count tokens (so no token suffix);
        // missing currency → default "€".
        let u =
            parse_usage(r#"{"ocr":{"models":{"m":{"input":[{"billing_metric":"x","value":9}]}}}}"#)
                .unwrap();
        assert_eq!(u.total_cost, 0.0);
        assert_eq!(u.total_tokens, 0);
        assert_eq!(u.currency_symbol, "€");
        assert_eq!(
            map_to_snapshot(&u).status_text.as_deref(),
            Some("€0.0000 this month")
        );
    }

    #[test]
    fn group_thousands_formats() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(1500), "1,500");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }
}
