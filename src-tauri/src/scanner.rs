//! JSONL log scanner for Codex (OpenAI) and Claude (Anthropic) CLI tools.
//!
//! Ported from Swift `CostUsageScanner` in the macOS app. Warm scans are
//! incremental: files whose (mtime, size) are unchanged since the last
//! run are skipped entirely; files that have grown are parsed only from
//! their previous size forward (via `cache::FileAction::Incremental`).
//! State lives in per-provider JSON caches at `cache::cache_path(...)`.
//!
//! Output schema matches what the Swift scanner produces so the existing
//! Supabase RPCs (`upsert_daily_usage`) accept the payload unchanged.
//!
//! Claude cost invariant:
//! The server, Swift, Kotlin and Rust implementations all sum per-message
//! cost. NEVER compute cost from day-aggregated tokens — sonnet-4-5 and
//! sonnet-4-6 have a 200K-token tier threshold that gets wrongly crossed
//! once you aggregate. Per-message cost is accumulated into packed
//! slot [4] (`cost_nanos`, scaled by 1e9) during parse.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::cache::{self, CodexTotals, CostUsageCache, FileAction, FileEntry, Packed};
use crate::paths;
use crate::pricing;

pub const CLAUDE_MSG_BUCKET_MODEL: &str = "__claude_msg__";
const CLAUDE_COST_SCALE: f64 = 1_000_000_000.0;

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
    pub total_tokens: i64,
    pub today_key: String,
    pub days_scanned: u32,
    pub files_scanned: u32,
    /// Files present in the cache whose (mtime, size) matched and were
    /// therefore skipped entirely. Useful for benchmarking the incremental
    /// path and reported in the scan report.
    pub files_cached: u32,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub days: u32,
    pub force_rescan: bool,
    pub cache_dir: Option<PathBuf>,
    /// Test-only: override the Codex sessions roots scanned. Production
    /// callers leave this `None` so the platform-default paths are used.
    pub codex_roots_override: Option<Vec<PathBuf>>,
    /// Test-only: override the Claude projects roots scanned.
    pub claude_roots_override: Option<Vec<PathBuf>>,
    /// Test-only: pin "today" so the date-range derivation is deterministic
    /// regardless of the CI runner's clock or timezone. Production leaves
    /// this `None` and the scanner reads `chrono::Local::now()`.
    pub today_override: Option<NaiveDate>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            days: 30,
            force_rescan: false,
            cache_dir: None,
            codex_roots_override: None,
            claude_roots_override: None,
            today_override: None,
        }
    }
}

pub fn scan(days: u32) -> anyhow::Result<ScanResult> {
    scan_with_options(ScanOptions {
        days,
        ..Default::default()
    })
}

pub fn scan_with_options(opts: ScanOptions) -> anyhow::Result<ScanResult> {
    // IMPORTANT: per-event day classification (`parse_day_key_local`) converts
    // each JSONL timestamp into the *local* timezone before keying it. The
    // range filter MUST use the same local-clock anchor or events get
    // wrongly excluded near midnight in non-UTC timezones (e.g. JST users
    // between 00:00 and 09:00 local: UTC date trails local date by one,
    // so "today's" events were tagged 2026-04-25 by parse_day_key_local
    // but the filter range only ran through 2026-04-24 — entire morning of
    // usage went missing). Caught by Codex review post v0.2.1.
    let today = opts
        .today_override
        .unwrap_or_else(|| chrono::Local::now().date_naive());
    let since = today
        .checked_sub_signed(chrono::Duration::days(opts.days as i64))
        .unwrap_or(today);
    let range = DateRange {
        since_key: fmt_date(since),
        until_key: fmt_date(today),
    };
    let today_key = fmt_date(today);

    let (codex_cache, codex_files_scanned, codex_files_cached) =
        scan_codex_provider(&opts, &range)?;
    let (claude_cache, claude_files_scanned, claude_files_cached) =
        scan_claude_provider(&opts, &range)?;

    let entries = emit_entries(&codex_cache, &claude_cache, &range);

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
        days_scanned: opts.days,
        files_scanned: codex_files_scanned + claude_files_scanned,
        files_cached: codex_files_cached + claude_files_cached,
    })
}

#[derive(Debug, Clone)]
struct DateRange {
    since_key: String,
    until_key: String,
}

fn fmt_date(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

fn parse_day_key_local(ts: &str) -> Option<String> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(ts) {
        let local = dt.with_timezone(&chrono::Local);
        return Some(fmt_date(local.date_naive()));
    }
    if ts.len() >= 10 {
        if let Ok(d) = NaiveDate::parse_from_str(&ts[..10], "%Y-%m-%d") {
            return Some(fmt_date(d));
        }
    }
    None
}

fn in_range(day: &str, r: &DateRange) -> bool {
    day >= r.since_key.as_str() && day <= r.until_key.as_str()
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn file_stat(path: &Path) -> Option<(i64, i64)> {
    let meta = path.metadata().ok()?;
    let size = meta.len() as i64;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .ok()?;
    Some((mtime, size))
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ========================================================================
// Codex scanning
// ========================================================================

fn scan_codex_provider(
    opts: &ScanOptions,
    range: &DateRange,
) -> anyhow::Result<(CostUsageCache, u32, u32)> {
    let mut cache = if opts.force_rescan {
        CostUsageCache::default()
    } else {
        cache::load("codex", opts.cache_dir.as_deref())
    };

    let mut files_scanned = 0u32;
    let mut files_cached = 0u32;
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    let roots: Vec<PathBuf> = opts
        .codex_roots_override
        .clone()
        .unwrap_or_else(paths::codex_sessions_roots);
    for root in roots {
        if !root.exists() {
            continue;
        }
        for walk_entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
            if !walk_entry.file_type().is_file() {
                continue;
            }
            let p = walk_entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(file_date) = codex_date_from_path(p, &root) {
                if !in_range(&file_date, range) {
                    continue;
                }
            }
            let key = path_key(p);
            seen_paths.insert(key.clone());

            let (mtime, size) = match file_stat(p) {
                Some(s) => s,
                None => continue,
            };

            let action = cache::decide_action(cache.files.get(&key), mtime, size);
            match action {
                FileAction::Unchanged => {
                    files_cached += 1;
                    continue;
                }
                FileAction::Incremental { start_offset } => {
                    let (initial_model, initial_totals) = cache
                        .files
                        .get(&key)
                        .map(|e| (e.last_model.clone(), e.last_totals))
                        .unwrap_or((None, None));
                    let parsed =
                        parse_codex_file(p, range, start_offset, initial_model, initial_totals);
                    if !parsed.file_days.is_empty() {
                        cache::apply_file_days(&mut cache, &parsed.file_days, 1);
                    }
                    let mut merged_days = cache
                        .files
                        .get(&key)
                        .map(|e| e.days.clone())
                        .unwrap_or_default();
                    cache::merge_file_days(&mut merged_days, &parsed.file_days);
                    cache.files.insert(
                        key.clone(),
                        FileEntry {
                            mtime_unix_ms: mtime,
                            size,
                            days: merged_days,
                            parsed_bytes: Some(parsed.parsed_bytes),
                            last_model: parsed.last_model,
                            last_totals: parsed.last_totals,
                            session_id: parsed.session_id,
                        },
                    );
                    files_scanned += 1;
                }
                FileAction::FullReparse => {
                    if let Some(old) = cache.files.get(&key).cloned() {
                        cache::apply_file_days(&mut cache, &old.days, -1);
                    }
                    let parsed = parse_codex_file(p, range, 0, None, None);
                    cache::apply_file_days(&mut cache, &parsed.file_days, 1);
                    cache.files.insert(
                        key.clone(),
                        FileEntry {
                            mtime_unix_ms: mtime,
                            size,
                            days: parsed.file_days,
                            parsed_bytes: Some(parsed.parsed_bytes),
                            last_model: parsed.last_model,
                            last_totals: parsed.last_totals,
                            session_id: parsed.session_id,
                        },
                    );
                    files_scanned += 1;
                }
            }
        }
    }

    // Evict files that vanished from disk since last scan.
    let stale: Vec<String> = cache
        .files
        .keys()
        .filter(|k| !seen_paths.contains(*k))
        .cloned()
        .collect();
    for key in stale {
        if let Some(old) = cache.files.remove(&key) {
            cache::apply_file_days(&mut cache, &old.days, -1);
        }
    }

    cache::prune_days(&mut cache, &range.since_key, &range.until_key);
    cache.last_scan_unix_ms = now_unix_ms();
    if let Err(e) = cache::save("codex", &cache, opts.cache_dir.as_deref()) {
        log::warn!("cache::save(codex) failed: {e}");
    }
    Ok((cache, files_scanned, files_cached))
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

struct CodexParseResult {
    parsed_bytes: i64,
    file_days: HashMap<String, HashMap<String, Packed>>,
    last_model: Option<String>,
    last_totals: Option<CodexTotals>,
    session_id: Option<String>,
}

fn parse_codex_file(
    path: &Path,
    range: &DateRange,
    start_offset: i64,
    initial_model: Option<String>,
    initial_totals: Option<CodexTotals>,
) -> CodexParseResult {
    let mut out = CodexParseResult {
        parsed_bytes: start_offset,
        file_days: HashMap::new(),
        last_model: initial_model.clone(),
        last_totals: initial_totals,
        session_id: None,
    };

    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return out,
    };
    if start_offset > 0 && file.seek(SeekFrom::Start(start_offset as u64)).is_err() {
        return out;
    }
    let mut reader = BufReader::with_capacity(256 * 1024, file);

    let mut current_model = initial_model;
    let mut prev_total = initial_totals.unwrap_or_default();
    let mut has_prev = initial_totals.is_some();
    let mut bytes_seen: i64 = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);

    // IMPORTANT: don't use `reader.lines()` here — it strips `\r\n` AND `\n`
    // but doesn't tell us how many bytes were actually consumed. On Windows
    // CRLF JSONLs that under-counted by 1 byte per line, so the cached
    // `parsed_bytes` drifted and the next incremental scan would seek into
    // the middle of a line. read_until returns the exact byte count
    // including the terminator, which we strip ourselves.
    loop {
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => continue,
        };
        bytes_seen += n as i64;
        while matches!(buf.last(), Some(&b'\n') | Some(&b'\r')) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        let line: &str = match std::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !line.contains("\"type\":\"event_msg\"")
            && !line.contains("\"type\":\"turn_context\"")
            && !line.contains("\"type\":\"session_meta\"")
        {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if ty == "session_meta" {
            if out.session_id.is_none() {
                if let Some(payload) = obj.get("payload") {
                    out.session_id = payload
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .or_else(|| payload.get("sessionId").and_then(|v| v.as_str()))
                        .or_else(|| payload.get("id").and_then(|v| v.as_str()))
                        .map(String::from);
                }
            }
            continue;
        }

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
        if !in_range(&day, range) {
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
        let day_models = out.file_days.entry(day).or_default();
        let packed = day_models
            .entry(norm_model)
            .or_insert_with(|| vec![0, 0, 0]);
        packed[0] += d_in;
        packed[1] += d_cached.min(d_in);
        packed[2] += d_out;
    }

    out.parsed_bytes = start_offset + bytes_seen;
    out.last_model = current_model;
    out.last_totals = if has_prev { Some(prev_total) } else { None };
    out
}

// ========================================================================
// Claude scanning
// ========================================================================

fn scan_claude_provider(
    opts: &ScanOptions,
    range: &DateRange,
) -> anyhow::Result<(CostUsageCache, u32, u32)> {
    let mut cache = if opts.force_rescan {
        CostUsageCache::default()
    } else {
        cache::load("claude", opts.cache_dir.as_deref())
    };

    let mut files_scanned = 0u32;
    let mut files_cached = 0u32;
    let mut seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    let roots: Vec<PathBuf> = opts
        .claude_roots_override
        .clone()
        .unwrap_or_else(paths::claude_projects_roots);
    for root in roots {
        if !root.exists() {
            continue;
        }
        for walk_entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
            if !walk_entry.file_type().is_file() {
                continue;
            }
            let p = walk_entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let key = path_key(p);
            seen_paths.insert(key.clone());

            let (mtime, size) = match file_stat(p) {
                Some(s) => s,
                None => continue,
            };

            let action = cache::decide_action(cache.files.get(&key), mtime, size);
            match action {
                FileAction::Unchanged => {
                    files_cached += 1;
                    continue;
                }
                FileAction::Incremental { start_offset } => {
                    let parsed = parse_claude_file(p, range, start_offset);
                    if !parsed.file_days.is_empty() {
                        cache::apply_file_days(&mut cache, &parsed.file_days, 1);
                    }
                    let mut merged_days = cache
                        .files
                        .get(&key)
                        .map(|e| e.days.clone())
                        .unwrap_or_default();
                    cache::merge_file_days(&mut merged_days, &parsed.file_days);
                    cache.files.insert(
                        key.clone(),
                        FileEntry {
                            mtime_unix_ms: mtime,
                            size,
                            days: merged_days,
                            parsed_bytes: Some(parsed.parsed_bytes),
                            last_model: None,
                            last_totals: None,
                            session_id: None,
                        },
                    );
                    files_scanned += 1;
                }
                FileAction::FullReparse => {
                    if let Some(old) = cache.files.get(&key).cloned() {
                        cache::apply_file_days(&mut cache, &old.days, -1);
                    }
                    let parsed = parse_claude_file(p, range, 0);
                    cache::apply_file_days(&mut cache, &parsed.file_days, 1);
                    cache.files.insert(
                        key.clone(),
                        FileEntry {
                            mtime_unix_ms: mtime,
                            size,
                            days: parsed.file_days,
                            parsed_bytes: Some(parsed.parsed_bytes),
                            last_model: None,
                            last_totals: None,
                            session_id: None,
                        },
                    );
                    files_scanned += 1;
                }
            }
        }
    }

    let stale: Vec<String> = cache
        .files
        .keys()
        .filter(|k| !seen_paths.contains(*k))
        .cloned()
        .collect();
    for key in stale {
        if let Some(old) = cache.files.remove(&key) {
            cache::apply_file_days(&mut cache, &old.days, -1);
        }
    }

    cache::prune_days(&mut cache, &range.since_key, &range.until_key);
    cache.last_scan_unix_ms = now_unix_ms();
    if let Err(e) = cache::save("claude", &cache, opts.cache_dir.as_deref()) {
        log::warn!("cache::save(claude) failed: {e}");
    }
    Ok((cache, files_scanned, files_cached))
}

struct ClaudeParseResult {
    parsed_bytes: i64,
    file_days: HashMap<String, HashMap<String, Packed>>,
}

fn parse_claude_file(path: &Path, range: &DateRange, start_offset: i64) -> ClaudeParseResult {
    let mut out = ClaudeParseResult {
        parsed_bytes: start_offset,
        file_days: HashMap::new(),
    };

    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return out,
    };
    if start_offset > 0 && file.seek(SeekFrom::Start(start_offset as u64)).is_err() {
        return out;
    }
    let mut reader = BufReader::with_capacity(256 * 1024, file);

    let mut seen_keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut bytes_seen: i64 = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(4096);

    // CRLF-safe line iteration. See parse_codex_file for why we don't use
    // `reader.lines()`.
    loop {
        buf.clear();
        let n = match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => continue,
        };
        bytes_seen += n as i64;
        while matches!(buf.last(), Some(&b'\n') | Some(&b'\r')) {
            buf.pop();
        }
        if buf.is_empty() {
            continue;
        }
        let line: &str = match std::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let is_assistant = line.contains("\"type\":\"assistant\"");
        let is_user = line.contains("\"type\":\"user\"");
        if !is_assistant && !is_user {
            continue;
        }

        let obj: serde_json::Value = match serde_json::from_str(line) {
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
        if !in_range(&day, range) {
            continue;
        }

        if ty == "user" {
            bump_msg(&mut out.file_days, &day);
            continue;
        }

        // type == "assistant" — always count against msg bucket even
        // for streaming chunks (matches Claude Code UI semantics).
        bump_msg(&mut out.file_days, &day);

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

        // Token dedup: streaming chunks re-report cumulative usage —
        // count each (message.id, requestId) only once for tokens.
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

        // Per-message cost (bit-exact Swift parity for tiered pricing).
        let cost_nanos = pricing::claude_cost_usd(&model, input, cache_read, cache_create, output)
            .map(|c| (c * CLAUDE_COST_SCALE).round() as i64)
            .unwrap_or(0);

        let norm_model = pricing::normalize_claude_model(&model);
        let day_models = out.file_days.entry(day).or_default();
        let packed = day_models
            .entry(norm_model)
            .or_insert_with(|| vec![0, 0, 0, 0, 0, 0]);
        while packed.len() < 6 {
            packed.push(0);
        }
        packed[0] += input;
        packed[1] += cache_read;
        packed[2] += cache_create;
        packed[3] += output;
        packed[4] += cost_nanos;
        // slot 5 already bumped for this event via bump_msg? No — bump_msg
        // goes against the synthetic bucket. Per-model msg is a dedup-safe
        // counter that matches Swift's msgDelta=0 branch for token lines.
    }

    out.parsed_bytes = start_offset + bytes_seen;
    out
}

fn bump_msg(file_days: &mut HashMap<String, HashMap<String, Packed>>, day: &str) {
    let day_models = file_days.entry(day.to_string()).or_default();
    let packed = day_models
        .entry(CLAUDE_MSG_BUCKET_MODEL.to_string())
        .or_insert_with(|| vec![0, 0, 0, 0, 0, 0]);
    while packed.len() < 6 {
        packed.push(0);
    }
    packed[5] += 1;
}

// ========================================================================
// Emission — walk per-provider cache.days and build DailyEntries.
// ========================================================================

fn emit_entries(
    codex_cache: &CostUsageCache,
    claude_cache: &CostUsageCache,
    range: &DateRange,
) -> Vec<DailyEntry> {
    let mut out: Vec<DailyEntry> = Vec::new();

    // Codex: [input, cached, output] — cost computed from pricing table
    for (day, models) in &codex_cache.days {
        if !in_range(day, range) {
            continue;
        }
        for (model, packed) in models {
            let input = packed.first().copied().unwrap_or(0);
            let cached = packed.get(1).copied().unwrap_or(0);
            let output = packed.get(2).copied().unwrap_or(0);
            if input == 0 && cached == 0 && output == 0 {
                continue;
            }
            let cost = pricing::codex_cost_usd(model, input, cached, output);
            out.push(DailyEntry {
                date: day.clone(),
                provider: "Codex".into(),
                model: model.clone(),
                input_tokens: input,
                cached_tokens: cached,
                output_tokens: output,
                cost_usd: cost,
                message_count: 0,
            });
        }
    }

    // Claude: [input, cache_read, cache_create, output, cost_nanos, msgs]
    for (day, models) in &claude_cache.days {
        if !in_range(day, range) {
            continue;
        }
        for (model, packed) in models {
            let input = packed.first().copied().unwrap_or(0);
            let cache_read = packed.get(1).copied().unwrap_or(0);
            let cache_create = packed.get(2).copied().unwrap_or(0);
            let output = packed.get(3).copied().unwrap_or(0);
            let cost_nanos = packed.get(4).copied().unwrap_or(0);
            let msgs = packed.get(5).copied().unwrap_or(0);
            // Emit when there's any real token activity OR the synthetic
            // bucket has a non-zero msg count (per v1.9.4 invariant).
            if input == 0 && cache_read == 0 && cache_create == 0 && output == 0 && msgs == 0 {
                continue;
            }
            let cost = if model == CLAUDE_MSG_BUCKET_MODEL {
                None
            } else if cost_nanos > 0 {
                Some(cost_nanos as f64 / CLAUDE_COST_SCALE)
            } else {
                pricing::claude_cost_usd(model, input, cache_read, cache_create, output)
            };
            out.push(DailyEntry {
                date: day.clone(),
                provider: "Claude".into(),
                model: model.clone(),
                input_tokens: input,
                cached_tokens: cache_read + cache_create,
                output_tokens: output,
                cost_usd: cost,
                message_count: msgs,
            });
        }
    }

    out.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then(a.provider.cmp(&b.provider))
            .then(a.model.cmp(&b.model))
    });
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the timezone bug Codex caught post v0.2.1:
    /// `today` was anchored to `Utc::now()` while `today_key` and the
    /// per-event day classification used `Local::now()`. With the fix,
    /// today_key reflects whatever anchor we pin via `today_override`
    /// (in production: `chrono::Local::now()`). This is the FAST unit
    /// test — see `tests/scanner_integration.rs` for the full
    /// fixture-based regression that asserts the event survives the
    /// range filter.
    #[test]
    fn today_key_matches_today_override() {
        let tmp = std::env::temp_dir().join(format!(
            "cli-pulse-tz-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        let pinned = chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let opts = ScanOptions {
            days: 1,
            force_rescan: true,
            cache_dir: Some(tmp.clone()),
            codex_roots_override: Some(vec![tmp.join("codex_empty")]),
            claude_roots_override: Some(vec![tmp.join("claude_empty")]),
            today_override: Some(pinned),
        };
        let result = scan_with_options(opts).expect("scan should succeed");
        assert_eq!(result.today_key, "2026-01-15");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_day_key_local_handles_rfc3339() {
        // 2026-04-25T01:00:00Z: in JST (+09) → 2026-04-25 10:00 JST → "2026-04-25"
        // in PST (−07) → 2026-04-24 18:00 PST → "2026-04-24"
        // We can't pin the test TZ portably, but we can assert the function
        // returns *some* valid YYYY-MM-DD for a well-formed RFC3339 input.
        let day = parse_day_key_local("2026-04-25T01:00:00Z");
        assert!(day.is_some());
        let key = day.unwrap();
        assert_eq!(key.len(), 10);
        assert_eq!(&key[4..5], "-");
        assert_eq!(&key[7..8], "-");
    }

    #[test]
    fn parse_day_key_local_falls_back_to_prefix() {
        let day = parse_day_key_local("2026-04-25");
        assert_eq!(day.as_deref(), Some("2026-04-25"));
    }

    #[test]
    fn in_range_inclusive() {
        let r = DateRange {
            since_key: "2026-04-20".into(),
            until_key: "2026-04-25".into(),
        };
        assert!(in_range("2026-04-20", &r));
        assert!(in_range("2026-04-22", &r));
        assert!(in_range("2026-04-25", &r));
        assert!(!in_range("2026-04-19", &r));
        assert!(!in_range("2026-04-26", &r));
    }
}
