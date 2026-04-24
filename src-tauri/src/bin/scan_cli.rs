//! Smoke-test binary — runs the scanner against the local machine's
//! ~/.claude and ~/.codex logs and prints a summary. Used for parity
//! checking against the Swift `CostUsageScanner` in the macOS app.
//!
//! Usage:
//!   cargo run --bin scan_cli -- [days]         # defaults to 30
//!   cargo run --bin scan_cli -- 7 json         # JSON output

use cli_pulse_desktop_lib::scanner;
use std::env;

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let days: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let fmt = args.next().unwrap_or_else(|| "pretty".to_string());

    let result = scanner::scan(days)?;

    if fmt == "json" {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    println!(
        "Scanned {} files over last {} days. Total cost: ${:.4}, total I/O tokens: {}",
        result.files_scanned, result.days_scanned, result.total_cost_usd, result.total_tokens
    );
    println!("Today = {}\n", result.today_key);

    // Group by date for a compact printout
    let mut by_date: std::collections::BTreeMap<&str, Vec<&scanner::DailyEntry>> =
        std::collections::BTreeMap::new();
    for e in &result.entries {
        by_date.entry(e.date.as_str()).or_default().push(e);
    }
    for (date, entries) in by_date.iter().rev().take(7) {
        let day_cost: f64 = entries.iter().filter_map(|e| e.cost_usd).sum();
        let day_tokens: i64 = entries
            .iter()
            .map(|e| e.input_tokens + e.output_tokens)
            .sum();
        let msgs: i64 = entries
            .iter()
            .filter(|e| e.model == scanner::CLAUDE_MSG_BUCKET_MODEL)
            .map(|e| e.message_count)
            .sum();
        println!(
            "  {}  ${:8.4}  {:>12} tokens  {:>6} msgs  ({} models)",
            date,
            day_cost,
            day_tokens,
            msgs,
            entries
                .iter()
                .filter(|e| e.model != scanner::CLAUDE_MSG_BUCKET_MODEL)
                .count()
        );
    }

    Ok(())
}
