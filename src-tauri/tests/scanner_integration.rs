//! End-to-end integration tests for the scanner against synthetic
//! JSONL fixtures.
//!
//! These tests deliberately avoid touching the user's real
//! `~/.claude/projects/` or `~/.codex/sessions/` — every fixture is
//! built fresh into a temp directory, scanned via
//! `ScanOptions::{codex,claude}_roots_override`, and asserted in
//! detail. Tests run in CI on all four platforms (Win+Linux × x64+ARM)
//! so any platform-specific JSONL parsing regression gets caught.
//!
//! Highest-value coverage in this file:
//! - Bit-exact Claude per-message cost summation (the v0.1.3 invariant)
//! - Codex cumulative `total_token_usage` delta math
//! - Token dedup by `(message.id, requestId)` for streaming chunks
//! - The `__claude_msg__` synthetic-bucket counts both user + assistant
//!   events (incl. dedup'd streaming chunks)
//! - Date-range filtering with `today_override` (would have caught the
//!   v0.2.2 timezone bug)

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::NaiveDate;
use cli_pulse_desktop_lib::scanner::{self, DailyEntry, ScanOptions, CLAUDE_MSG_BUCKET_MODEL};

/// Build an isolated scratch dir under tmp/ that's removed when the
/// returned `TempEnv` goes out of scope. Each test gets its own.
struct TempEnv {
    pub root: PathBuf,
    pub codex_root: PathBuf,
    pub claude_root: PathBuf,
    pub cache_dir: PathBuf,
}

impl TempEnv {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "cli-pulse-int-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let codex_root = root.join("codex");
        let claude_root = root.join("claude");
        let cache_dir = root.join("cache");
        fs::create_dir_all(&codex_root).unwrap();
        fs::create_dir_all(&claude_root).unwrap();
        fs::create_dir_all(&cache_dir).unwrap();
        Self {
            root,
            codex_root,
            claude_root,
            cache_dir,
        }
    }

    fn write_codex(&self, year: &str, month: &str, day: &str, name: &str, body: &str) -> PathBuf {
        let dir = self.codex_root.join(year).join(month).join(day);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    fn write_claude(&self, project: &str, name: &str, body: &str) -> PathBuf {
        let dir = self.claude_root.join(project);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        fs::write(&p, body).unwrap();
        p
    }

    fn options(&self, days: u32, today: Option<NaiveDate>) -> ScanOptions {
        ScanOptions {
            days,
            force_rescan: true,
            cache_dir: Some(self.cache_dir.clone()),
            codex_roots_override: Some(vec![self.codex_root.clone()]),
            claude_roots_override: Some(vec![self.claude_root.clone()]),
            today_override: today,
        }
    }
}

impl Drop for TempEnv {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Convenience: pick the entry matching (date, provider, model) or
/// fail loudly with a useful message.
fn pick<'a>(entries: &'a [DailyEntry], date: &str, provider: &str, model: &str) -> &'a DailyEntry {
    entries
        .iter()
        .find(|e| e.date == date && e.provider == provider && e.model == model)
        .unwrap_or_else(|| {
            let listing: Vec<String> = entries
                .iter()
                .map(|e| format!("({}, {}, {})", e.date, e.provider, e.model))
                .collect();
            panic!(
                "no entry for ({date}, {provider}, {model}). Got: {:?}",
                listing
            )
        })
}

// ========================================================================
// Codex — cumulative total_token_usage delta math
// ========================================================================

const CODEX_THREE_TURNS: &str = r#"{"type":"session_meta","timestamp":"2026-04-25T10:00:00Z","payload":{"session_id":"sess-1"}}
{"type":"turn_context","timestamp":"2026-04-25T10:00:01Z","payload":{"model":"gpt-5","info":{}}}
{"type":"event_msg","timestamp":"2026-04-25T10:00:02Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":0,"output_tokens":500},"model":"gpt-5"}}}
{"type":"event_msg","timestamp":"2026-04-25T10:00:10Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1800,"cached_input_tokens":600,"output_tokens":900},"model":"gpt-5"}}}
{"type":"event_msg","timestamp":"2026-04-25T10:00:20Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":3000,"cached_input_tokens":1200,"output_tokens":1500},"model":"gpt-5"}}}
"#;

#[test]
fn codex_three_turns_yields_cumulative_totals() {
    let env = TempEnv::new("codex_three");
    env.write_codex("2026", "04", "25", "session.jsonl", CODEX_THREE_TURNS);

    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(1, Some(today))).unwrap();

    let e = pick(&result.entries, "2026-04-25", "Codex", "gpt-5");
    // 1st turn: +1000 input, +0 cached, +500 output (no prior baseline)
    // 2nd turn: total goes 1000→1800 input (+800), 0→600 cached (+600), 500→900 output (+400)
    // 3rd turn: total 1800→3000 (+1200), 600→1200 (+600), 900→1500 (+600)
    // Sum: input 1000+800+1200 = 3000, cached 0+600+600 = 1200, output 500+400+600 = 1500
    assert_eq!(e.input_tokens, 3000);
    assert_eq!(e.output_tokens, 1500);
    // `cached` capped at min(cached_delta, input_delta) per slot — all three
    // hold (cached_delta <= input_delta), so sum is 1200.
    assert_eq!(e.cached_tokens, 1200);
}

#[test]
fn codex_pricing_applied_to_aggregated_tokens() {
    let env = TempEnv::new("codex_pricing");
    env.write_codex("2026", "04", "25", "s.jsonl", CODEX_THREE_TURNS);
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(1, Some(today))).unwrap();

    let e = pick(&result.entries, "2026-04-25", "Codex", "gpt-5");
    // gpt-5 rates: input $1.25/M, cached $0.125/M, output $10/M
    // (3000 - 1200) non-cached × 1.25e-6 = $0.00225
    //  1200 cached × 0.125e-6 = $0.00015
    //  1500 output × 10e-6 = $0.015
    // total = 0.0174
    let cost = e.cost_usd.expect("Codex gpt-5 has a price");
    assert!(
        (cost - 0.0174).abs() < 1e-9,
        "expected $0.0174, got ${cost}"
    );
}

// ========================================================================
// Claude — per-message cost, msg bucket, dedup
// ========================================================================

const CLAUDE_TIERED_BIG_MSG: &str = r#"{"type":"user","timestamp":"2026-04-25T11:00:00Z"}
{"type":"assistant","timestamp":"2026-04-25T11:00:05Z","requestId":"r1","message":{"id":"m1","model":"claude-sonnet-4-6","usage":{"input_tokens":250000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":1000}}}
"#;

#[test]
fn claude_per_message_cost_under_aggregation_threshold() {
    let env = TempEnv::new("claude_tiered");
    env.write_claude("proj", "session.jsonl", CLAUDE_TIERED_BIG_MSG);
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(1, Some(today))).unwrap();

    let e = pick(&result.entries, "2026-04-25", "Claude", "claude-sonnet-4-6");
    assert_eq!(e.input_tokens, 250000);
    assert_eq!(e.output_tokens, 1000);

    // 250K input on sonnet-4-6: 200K @ $3/M base + 50K @ $6/M tiered = $0.6 + $0.3 = $0.9
    // 1000 output: 1000 @ $15/M = $0.015
    // total = 0.915
    let cost = e.cost_usd.expect("sonnet-4-6 priced");
    assert!(
        (cost - 0.915).abs() < 1e-6,
        "expected $0.915 (per-message tiered), got ${cost}"
    );
}

const CLAUDE_TWO_SMALL_MSGS_NO_TIER: &str = r#"{"type":"user","timestamp":"2026-04-25T12:00:00Z"}
{"type":"assistant","timestamp":"2026-04-25T12:00:02Z","requestId":"r1","message":{"id":"m1","model":"claude-sonnet-4-6","usage":{"input_tokens":150000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":500}}}
{"type":"user","timestamp":"2026-04-25T12:01:00Z"}
{"type":"assistant","timestamp":"2026-04-25T12:01:02Z","requestId":"r2","message":{"id":"m2","model":"claude-sonnet-4-6","usage":{"input_tokens":150000,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":500}}}
"#;

#[test]
fn claude_two_small_messages_stay_under_tier_threshold() {
    // The v0.1.3 invariant: cost is sum of per-message cost. Two 150K
    // messages each price at $3/M ($0.45) — total $0.90 input cost.
    // BUG (would have shipped without per-message accumulation): sum
    // tokens first → 300K, then price tiered → 200K @ $3/M + 100K @ $6/M
    // = $1.20 input cost. Wrong by 33%.
    let env = TempEnv::new("claude_two_msgs");
    env.write_claude("proj", "session.jsonl", CLAUDE_TWO_SMALL_MSGS_NO_TIER);
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(1, Some(today))).unwrap();

    let e = pick(&result.entries, "2026-04-25", "Claude", "claude-sonnet-4-6");
    assert_eq!(e.input_tokens, 300000);
    assert_eq!(e.output_tokens, 1000);

    // input: 0.90 (two flat-rate messages), output: 1000 @ $15/M = $0.015
    let cost = e.cost_usd.expect("sonnet-4-6 priced");
    assert!(
        (cost - 0.915).abs() < 1e-6,
        "per-message tier semantic broken: expected $0.915, got ${cost}"
    );
}

const CLAUDE_STREAMING_DEDUP: &str = r#"{"type":"user","timestamp":"2026-04-25T13:00:00Z"}
{"type":"assistant","timestamp":"2026-04-25T13:00:01Z","requestId":"r1","message":{"id":"m1","model":"claude-haiku-4-5","usage":{"input_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":50}}}
{"type":"assistant","timestamp":"2026-04-25T13:00:02Z","requestId":"r1","message":{"id":"m1","model":"claude-haiku-4-5","usage":{"input_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":50}}}
{"type":"assistant","timestamp":"2026-04-25T13:00:03Z","requestId":"r1","message":{"id":"m1","model":"claude-haiku-4-5","usage":{"input_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":50}}}
"#;

#[test]
fn claude_streaming_chunks_deduped_for_tokens_but_counted_for_msgs() {
    // Three streaming chunks of the same (message.id=m1, requestId=r1) —
    // tokens should count exactly once; msg bucket should count all 4
    // events (1 user + 3 assistant chunks).
    let env = TempEnv::new("claude_dedup");
    env.write_claude("proj", "session.jsonl", CLAUDE_STREAMING_DEDUP);
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(1, Some(today))).unwrap();

    let e = pick(&result.entries, "2026-04-25", "Claude", "claude-haiku-4-5");
    assert_eq!(e.input_tokens, 100, "tokens NOT deduped");
    assert_eq!(e.output_tokens, 50, "tokens NOT deduped");

    let m = pick(
        &result.entries,
        "2026-04-25",
        "Claude",
        CLAUDE_MSG_BUCKET_MODEL,
    );
    // 1 user + 3 assistant streaming events = 4 msgs against synthetic bucket
    assert_eq!(m.message_count, 4);
}

// ========================================================================
// Date range — TIMEZONE bug regression (v0.2.2)
// ========================================================================

const CLAUDE_LATE_NIGHT: &str = r#"{"type":"user","timestamp":"2026-04-25T05:00:00+09:00"}
{"type":"assistant","timestamp":"2026-04-25T05:00:01+09:00","requestId":"r1","message":{"id":"m1","model":"claude-haiku-4-5","usage":{"input_tokens":100,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":50}}}
"#;

#[test]
fn timezone_anchor_uses_today_override_consistently() {
    // Regression for v0.2.2: prior to the fix, `today` was anchored on
    // Utc::now() while parse_day_key_local converted timestamps to local.
    // Here we pin `today_override` to 2026-04-25 — the event timestamp
    // resolves to 2026-04-25 in JST (UTC+9) but to 2026-04-24 in UTC.
    // With a consistent local anchor + override the event is in range.
    let env = TempEnv::new("tz_anchor");
    env.write_claude("proj", "session.jsonl", CLAUDE_LATE_NIGHT);
    // Ask for today=2026-04-25 (matches the local-frame day of the event)
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(1, Some(today))).unwrap();

    assert_eq!(result.today_key, "2026-04-25");
    let _e = pick(&result.entries, "2026-04-25", "Claude", "claude-haiku-4-5");
    // Event survived the range filter — ✓
}

// ========================================================================
// Aggregate sanity
// ========================================================================

#[test]
fn empty_roots_yields_empty_result_no_panics() {
    let env = TempEnv::new("empty");
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(7, Some(today))).unwrap();
    assert_eq!(result.entries.len(), 0);
    assert_eq!(result.total_cost_usd, 0.0);
    assert_eq!(result.total_tokens, 0);
    assert_eq!(result.files_scanned, 0);
    assert_eq!(result.files_cached, 0);
}

#[test]
fn out_of_range_files_excluded() {
    // File dated 2026-01-01 with today=2026-04-25 days=7 → out of range.
    let env = TempEnv::new("out_of_range");
    env.write_codex(
        "2026",
        "01",
        "01",
        "old.jsonl",
        // New Year content shouldn't appear in an end-of-April scan.
        r#"{"type":"event_msg","timestamp":"2026-01-01T12:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":0,"output_tokens":500},"model":"gpt-5"}}}"#,
    );
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(7, Some(today))).unwrap();
    let codex_entries: Vec<&DailyEntry> = result
        .entries
        .iter()
        .filter(|e| e.provider == "Codex")
        .collect();
    assert!(
        codex_entries.is_empty(),
        "out-of-range file leaked through: {codex_entries:?}"
    );
}

#[test]
fn cache_makes_repeat_scans_idempotent() {
    let env = TempEnv::new("cache_idempotent");
    env.write_claude("proj", "s.jsonl", CLAUDE_TWO_SMALL_MSGS_NO_TIER);
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();

    // First scan — cold (force_rescan=true so we ignore any stale cache).
    let r1 = scanner::scan_with_options(env.options(1, Some(today))).unwrap();
    // Second scan — warm. NB: env.options uses force_rescan=true; flip it
    // to false so the cache actually gets reused.
    let mut warm_opts = env.options(1, Some(today));
    warm_opts.force_rescan = false;
    let r2 = scanner::scan_with_options(warm_opts).unwrap();

    assert_eq!(
        r1.total_cost_usd, r2.total_cost_usd,
        "warm scan must not change totals"
    );
    assert_eq!(r1.total_tokens, r2.total_tokens);
    // Warm scan: file cached = 1, scanned = 0
    assert_eq!(r2.files_cached, 1);
    assert_eq!(r2.files_scanned, 0);
}

#[test]
fn codex_event_grouped_by_local_date_in_user_tz() {
    // Ensure that a single file with events on TWO local days produces
    // entries for both days.
    let env = TempEnv::new("two_days");
    env.write_codex(
        "2026",
        "04",
        "24",
        "day1.jsonl",
        r#"{"type":"event_msg","timestamp":"2026-04-24T10:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":50},"model":"gpt-5"}}}
"#,
    );
    env.write_codex(
        "2026",
        "04",
        "25",
        "day2.jsonl",
        r#"{"type":"event_msg","timestamp":"2026-04-25T10:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200,"cached_input_tokens":0,"output_tokens":100},"model":"gpt-5"}}}
"#,
    );
    let today = NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
    let result = scanner::scan_with_options(env.options(7, Some(today))).unwrap();

    let _d1 = pick(&result.entries, "2026-04-24", "Codex", "gpt-5");
    let _d2 = pick(&result.entries, "2026-04-25", "Codex", "gpt-5");

    // Verify the two-day total = 100+200 input, 50+100 output across the matching entries.
    let by_day: HashMap<String, &DailyEntry> = result
        .entries
        .iter()
        .filter(|e| e.provider == "Codex")
        .map(|e| (e.date.clone(), e))
        .collect();
    assert_eq!(by_day["2026-04-24"].input_tokens, 100);
    assert_eq!(by_day["2026-04-25"].input_tokens, 200);
}
