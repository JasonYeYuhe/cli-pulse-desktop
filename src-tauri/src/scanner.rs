//! JSONL log scanner for Codex (OpenAI) and Claude (Anthropic) CLI tools.
//!
//! Ported from Swift `CostUsageScanner` in the macOS app. Sprint 0 does a
//! full rescan every call — no incremental offset cache yet (that lives in
//! Sprint 1). Per-day / per-model / per-provider buckets are aggregated and
//! returned as `ScanResult`.
//!
//! Output schema must match what the Swift scanner produces so the existing
//! Supabase RPCs (`upsert_daily_usage`) accept the payload unchanged.
//!
//! Key subtleties mirrored from Swift:
//! - Codex `token_count` events carry cumulative `total_token_usage`; we
//!   diff against the previous sample to get per-event deltas.
//! - Codex model resolution: use `info.model` if present, else last seen
//!   `turn_context.model`, else `"gpt-5"` fallback.
//! - Claude streams can re-report cumulative usage across chunks — dedup
//!   by `(message.id, requestId)` for TOKENS only. Message counts include
//!   every raw user+assistant event against the synthetic `__claude_msg__`
//!   bucket (Claude UI semantics — see PROJECT_FIX_v1.9.4).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::paths;
use crate::pricing;

pub const CLAUDE_MSG_BUCKET_MODEL: &str = "__claude_msg__";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyEntry {
    pub date: String,     // "YYYY-MM-DD" in local TZ
    pub provider: String, // "Codex" or "Claude"
    pub model: String,    // normalized model name
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: Option<f64>,
    pub message_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanResult {
    pub entries: Vec<DailyEntry>,
    pub total_cost_usd: f64,
    pub total_tokens: i64, // input + output only (excludes cache reads)
    pub today_key: String,
    pub days_scanned: u32,
    pub files_scanned: u32,
}

pub fn scan(days: u32) -> anyhow::Result<ScanResult> {
    let today = Utc::now().date_naive();
    let since = today
        .checked_sub_signed(chrono::Duration::days(days as i64))
        .unwrap_or(today);
    let since_key = fmt_date(since);
    let until_key = fmt_date(today);
    let today_key = fmt_date(chrono::Local::now().date_naive());

    let mut agg: HashMap<(String, String, String), Bucket> = HashMap::new();
    let mut files_scanned: u32 = 0;

    // --- Codex ---
    for root in paths::codex_sessions_roots() {
        files_scanned += scan_codex_root(&root, &since_key, &until_key, &mut agg)?;
    }

    // --- Claude ---
    for root in paths::claude_projects_roots() {
        files_scanned += scan_claude_root(&root, &since_key, &until_key, &mut agg)?;
    }

    let mut entries: Vec<DailyEntry> = agg
        .into_iter()
        .map(|((date, provider, model), b)| {
            let cost = if provider == "Codex" {
                pricing::codex_cost_usd(&model, b.input, b.cached, b.output)
            } else if model == CLAUDE_MSG_BUCKET_MODEL {
                // synthetic bucket — only msg count, no tokens, no cost
                None
            } else if b.cost_is_authoritative {
                // Per-message Claude cost was accumulated during parse
                // (see `parse_claude_file` — mirrors Swift `costNanos` slot).
                Some(b.cost_usd_accum)
            } else {
                pricing::claude_cost_usd(&model, b.input, b.cache_read, b.cache_create, b.output)
            };
            let cached = if provider == "Claude" {
                b.cache_read + b.cache_create
            } else {
                b.cached
            };
            DailyEntry {
                date,
                provider,
                model,
                input_tokens: b.input,
                cached_tokens: cached,
                output_tokens: b.output,
                cost_usd: cost,
                message_count: b.msgs,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.provider.cmp(&b.provider))
            .then(a.model.cmp(&b.model))
    });

    let total_cost_usd: f64 = entries.iter().filter_map(|e| e.cost_usd).sum();
    let total_tokens: i64 = entries
        .iter()
        .map(|e| e.input_tokens + e.output_tokens)
        .sum();

    Ok(ScanResult {
        entries,
        total_cost_usd,
        total_tokens,
        today_key,
        days_scanned: days,
        files_scanned,
    })
}

#[derive(Default)]
struct Bucket {
    input: i64,
    cached: i64,       // Codex only — single cached_input count
    cache_read: i64,   // Claude only
    cache_create: i64, // Claude only
    output: i64,
    msgs: i64,
    /// Claude: per-message cost accumulated during parse. Swift parity —
    /// tiered pricing evaluates per message (well below 200K threshold),
    /// so daily aggregates must sum per-message cost, not re-price on
    /// aggregated tokens.
    cost_usd_accum: f64,
    /// Whether cost_usd_accum is authoritative. Set true once we've added
    /// a per-message cost to the bucket. If still false at emit time we
    /// fall back to aggregate pricing (unknown model etc.).
    cost_is_authoritative: bool,
}

fn fmt_date(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

fn parse_day_key_local(ts: &str) -> Option<String> {
    // Try chrono RFC3339 / ISO 8601 (handles fractional + offset)
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        let local = dt.with_timezone(&chrono::Local);
        return Some(fmt_date(local.date_naive()));
    }
    // Fallback: "YYYY-MM-DD" prefix treated as UTC midnight
    if ts.len() >= 10 {
        if let Ok(d) = NaiveDate::parse_from_str(&ts[..10], "%Y-%m-%d") {
            return Some(fmt_date(d));
        }
    }
    None
}

fn in_range(day: &str, since: &str, until: &str) -> bool {
    day >= since && day <= until
}

fn read_jsonl_lines(path: &Path) -> anyhow::Result<BufReader<File>> {
    Ok(BufReader::with_capacity(256 * 1024, File::open(path)?))
}

// ========================================================================
// Codex scanning
// ========================================================================

fn scan_codex_root(
    root: &Path,
    since: &str,
    until: &str,
    agg: &mut HashMap<(String, String, String), Bucket>,
) -> anyhow::Result<u32> {
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0u32;
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        // Path-prefix date filter: Codex uses YYYY/MM/DD/*.jsonl layout.
        // If we can parse a date from path segments, skip early.
        if let Some(file_date) = codex_date_from_path(p, root) {
            if !in_range(&file_date, since, until) {
                continue;
            }
        }
        parse_codex_file(p, since, until, agg)?;
        count += 1;
    }
    Ok(count)
}

fn codex_date_from_path(path: &Path, root: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Vec<&str> = rel.iter().filter_map(|s| s.to_str()).collect();
    if parts.len() < 3 {
        return None;
    }
    let (y, m, d) = (parts[0], parts[1], parts[2]);
    if y.len() == 4 && m.len() == 2 && d.len() == 2 {
        Some(format!("{y}-{m}-{d}"))
    } else {
        None
    }
}

fn parse_codex_file(
    path: &Path,
    since: &str,
    until: &str,
    agg: &mut HashMap<(String, String, String), Bucket>,
) -> anyhow::Result<()> {
    let reader = match read_jsonl_lines(path) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    let mut current_model: Option<String> = None;
    let mut prev_total = CodexTotals::default();
    let mut has_prev = false;

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.is_empty() {
            continue;
        }
        // Cheap substring filter before JSON parse
        if !line.contains("\"type\":\"event_msg\"") && !line.contains("\"type\":\"turn_context\"") {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if ty == "turn_context" {
            if let Some(payload) = obj.get("payload") {
                if let Some(m) = payload.get("model").and_then(|v| v.as_str()) {
                    current_model = Some(m.to_string());
                } else if let Some(info) = payload.get("info") {
                    if let Some(m) = info.get("model").and_then(|v| v.as_str()) {
                        current_model = Some(m.to_string());
                    }
                }
            }
            continue;
        }

        if ty != "event_msg" {
            continue;
        }
        let payload = match obj.get("payload") {
            Some(p) => p,
            None => continue,
        };
        if payload.get("type").and_then(|v| v.as_str()) != Some("token_count") {
            continue;
        }
        let ts = match obj.get("timestamp").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };
        let day = match parse_day_key_local(ts) {
            Some(d) => d,
            None => continue,
        };
        if !in_range(&day, since, until) {
            continue;
        }

        let info = payload.get("info");
        let model = info
            .and_then(|i| i.get("model").and_then(|v| v.as_str()))
            .or_else(|| info.and_then(|i| i.get("model_name").and_then(|v| v.as_str())))
            .or_else(|| payload.get("model").and_then(|v| v.as_str()))
            .or_else(|| obj.get("model").and_then(|v| v.as_str()))
            .map(String::from)
            .or_else(|| current_model.clone())
            .unwrap_or_else(|| "gpt-5".to_string());

        let total = info.and_then(|i| i.get("total_token_usage"));
        let last = info.and_then(|i| i.get("last_token_usage"));
        let (d_in, d_cached, d_out) = if let Some(total) = total {
            let t_in = json_i64(total, "input_tokens");
            let t_cached = json_i64_or(total, &["cached_input_tokens", "cache_read_input_tokens"]);
            let t_out = json_i64(total, "output_tokens");
            let deltas = if has_prev {
                (
                    (t_in - prev_total.input).max(0),
                    (t_cached - prev_total.cached).max(0),
                    (t_out - prev_total.output).max(0),
                )
            } else {
                (t_in.max(0), t_cached.max(0), t_out.max(0))
            };
            prev_total = CodexTotals {
                input: t_in,
                cached: t_cached,
                output: t_out,
            };
            has_prev = true;
            deltas
        } else if let Some(last) = last {
            (
                json_i64(last, "input_tokens").max(0),
                json_i64_or(last, &["cached_input_tokens", "cache_read_input_tokens"]).max(0),
                json_i64(last, "output_tokens").max(0),
            )
        } else {
            continue;
        };

        if d_in == 0 && d_cached == 0 && d_out == 0 {
            continue;
        }

        let norm_model = pricing::normalize_codex_model(&model);
        let key = (day, "Codex".to_string(), norm_model);
        let bucket = agg.entry(key).or_default();
        bucket.input += d_in;
        bucket.cached += d_cached.min(d_in);
        bucket.output += d_out;
    }

    Ok(())
}

#[derive(Default, Clone, Copy)]
struct CodexTotals {
    input: i64,
    cached: i64,
    output: i64,
}

// ========================================================================
// Claude scanning
// ========================================================================

fn scan_claude_root(
    root: &Path,
    since: &str,
    until: &str,
    agg: &mut HashMap<(String, String, String), Bucket>,
) -> anyhow::Result<u32> {
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0u32;
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        parse_claude_file(p, since, until, agg)?;
        count += 1;
    }
    Ok(count)
}

fn parse_claude_file(
    path: &Path,
    since: &str,
    until: &str,
    agg: &mut HashMap<(String, String, String), Bucket>,
) -> anyhow::Result<()> {
    let reader = match read_jsonl_lines(path) {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };

    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        if line.is_empty() {
            continue;
        }

        let is_assistant = line.contains("\"type\":\"assistant\"");
        let is_user = line.contains("\"type\":\"user\"");
        if !is_assistant && !is_user {
            continue;
        }

        let obj: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let ts = match obj.get("timestamp").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => continue,
        };
        let day = match parse_day_key_local(ts) {
            Some(d) => d,
            None => continue,
        };
        if !in_range(&day, since, until) {
            continue;
        }

        // User events: message count only, no tokens.
        if ty == "user" {
            bump_msg(&day, agg);
            continue;
        }

        // type == "assistant": always count against msg bucket (incl. stream chunks).
        bump_msg(&day, agg);

        let message = match obj.get("message") {
            Some(m) => m,
            None => continue,
        };
        let model = match message.get("model").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => continue,
        };
        let usage = match message.get("usage") {
            Some(u) => u,
            None => continue,
        };

        // Dedup for tokens — streaming reports cumulative usage across chunks.
        let message_id = message.get("id").and_then(|v| v.as_str());
        let request_id = obj.get("requestId").and_then(|v| v.as_str());
        if let (Some(mid), Some(rid)) = (message_id, request_id) {
            let key = format!("{mid}:{rid}");
            if !seen_keys.insert(key) {
                continue;
            }
        }

        let input = json_i64(usage, "input_tokens").max(0);
        let cache_create = json_i64(usage, "cache_creation_input_tokens").max(0);
        let cache_read = json_i64(usage, "cache_read_input_tokens").max(0);
        let output = json_i64(usage, "output_tokens").max(0);
        if input == 0 && cache_create == 0 && cache_read == 0 && output == 0 {
            continue;
        }

        // Compute per-message cost now, mirroring Swift
        // `CostUsageScanner.Pricing.claudeCostUSD` at parse time. Swift
        // persists this in the cache as packed `costNanos` slot [4]; here
        // we accumulate into the day bucket directly.
        let msg_cost = pricing::claude_cost_usd(&model, input, cache_read, cache_create, output);

        let norm_model = pricing::normalize_claude_model(&model);
        let key = (day, "Claude".to_string(), norm_model);
        let bucket = agg.entry(key).or_default();
        bucket.input += input;
        bucket.cache_read += cache_read;
        bucket.cache_create += cache_create;
        bucket.output += output;
        if let Some(c) = msg_cost {
            bucket.cost_usd_accum += c;
            bucket.cost_is_authoritative = true;
        }
    }

    Ok(())
}

fn bump_msg(day: &str, agg: &mut HashMap<(String, String, String), Bucket>) {
    let key = (
        day.to_string(),
        "Claude".to_string(),
        CLAUDE_MSG_BUCKET_MODEL.to_string(),
    );
    agg.entry(key).or_default().msgs += 1;
}

// ========================================================================
// JSON helpers
// ========================================================================

fn json_i64(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn json_i64_or(v: &serde_json::Value, keys: &[&str]) -> i64 {
    for k in keys {
        if let Some(val) = v.get(*k).and_then(|x| x.as_i64()) {
            return val;
        }
    }
    0
}

// Silence "unused" for the fallback path helpers during Sprint 0.
#[allow(dead_code)]
fn utc_to_local_day(dt: DateTime<FixedOffset>) -> String {
    let utc: DateTime<Utc> = dt.with_timezone(&Utc);
    let local = chrono::Local.from_utc_datetime(&utc.naive_utc());
    fmt_date(local.date_naive())
}
