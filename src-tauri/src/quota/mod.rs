//! Multi-provider quota collection (v0.4.3).
//!
//! Each `<provider>::collect()` is best-effort and returns
//! `Option<QuotaSnapshot>` — `None` means "no upload this cycle, leave
//! the server row untouched". `helper_sync`'s `for v_provider in
//! jsonb_object_keys(...) loop` is a no-op for absent keys (verified
//! v0.4.2 audit), so absent rows stay at the last successful upload's
//! values regardless of which client wrote them last.
//!
//! Concurrent collection uses `tokio::spawn` per arm + `JoinHandle::await`
//! with `is_panic()` checking — NOT `tokio::join!`, which shares a task
//! with the parent and would unwind `sync_now` on any provider panic.
//! Per Codex 2026-05-02 review of v0.4.3 spec.

pub mod claude;
pub mod claude_refresh;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod gemini_refresh;
pub mod openrouter;

use serde::Serialize;

/// Provider name constants — must match Mac's `ProviderKind` raw values
/// at `Models.swift:10-37` exactly. Drift here causes dual-writer
/// inserts to land on different `(user_id, provider)` PKs instead of
/// converging on one row. See `provider_name_contract` test below.
pub const PROVIDER_CLAUDE: &str = "Claude";
pub const PROVIDER_CODEX: &str = "Codex";
pub const PROVIDER_CURSOR: &str = "Cursor";
pub const PROVIDER_GEMINI: &str = "Gemini";
pub const PROVIDER_COPILOT: &str = "Copilot";
pub const PROVIDER_OPENROUTER: &str = "OpenRouter";

/// v0.4.19 — proactive pre-expiry refresh buffer (epoch milliseconds).
///
/// Both Claude and Gemini's `is_expired` / `is_token_fresh` checks
/// trigger a refresh when `expires_at - now < PRE_EXPIRY_BUFFER_MS`,
/// not when the token has already expired. Reasoning:
///   - Background sync runs every 120s. A buffer < 1 cycle (60s) means
///     a single missed tick (sleep, OS suspend, network hiccup)
///     produces an expired-token cycle.
///   - Buffer of 5 min absorbs ~2 missed cycles — safe headroom for
///     real-world conditions while keeping the refresh frequency
///     bounded by token life (5 min / 8 h ≈ 1% extra refreshes).
///   - Larger buffers (e.g. 30 min) eat provider rate limits without
///     measurable benefit.
///
/// Pin via `pre_expiry_buffer_consistent_across_providers` test at
/// the bottom of this file. Drift here would mean Claude and Gemini
/// have inconsistent refresh timing, which Gemini 3.1 Pro flagged
/// in v0.4.19 review as the kind of thing that silently rots.
pub const PRE_EXPIRY_BUFFER_MS: f64 = 5.0 * 60.0 * 1000.0;

/// Snapshot returned by each provider's `collect()`. Same shape across
/// providers so the orchestrator can build a uniform `p_provider_tiers`
/// payload regardless of which provider produced it.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaSnapshot {
    pub plan_type: String,
    /// Headline remaining — semantics vary per provider (Claude: min
    /// across tier percentages; OpenRouter: dollar-balance scaled
    /// units; Cursor: cents). Each `<provider>::collect()` documents
    /// what its scale is. The frontend handles per-provider display.
    pub remaining: i64,
    pub quota: i64,
    /// Outer reset_time (most-imminent reset for the headline tier).
    /// Mirrors Mac's per-provider conventions.
    pub session_reset: Option<String>,
    pub tiers: Vec<TierEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TierEntry {
    pub name: String,
    pub quota: i64,
    pub remaining: i64,
    pub reset_time: Option<String>,
}

/// Run all 6 collectors concurrently with panic isolation. Returns a
/// vec of `(provider_name, snapshot)` — providers that returned None
/// or panicked are filtered out, with per-provider error logging.
pub async fn collect_all() -> Vec<(&'static str, QuotaSnapshot)> {
    let tasks: Vec<(&'static str, tokio::task::JoinHandle<Option<QuotaSnapshot>>)> = vec![
        (PROVIDER_CLAUDE, tokio::spawn(claude::collect())),
        (PROVIDER_CODEX, tokio::spawn(codex::collect())),
        (PROVIDER_CURSOR, tokio::spawn(cursor::collect())),
        (PROVIDER_GEMINI, tokio::spawn(gemini::collect())),
        (PROVIDER_COPILOT, tokio::spawn(copilot::collect())),
        (PROVIDER_OPENROUTER, tokio::spawn(openrouter::collect())),
    ];
    let mut out = Vec::new();
    for (name, task) in tasks {
        match task.await {
            Ok(Some(snap)) => out.push((name, snap)),
            Ok(None) => { /* per-provider WARN/DEBUG already logged in collect() */ }
            Err(e) if e.is_panic() => {
                log::error!("Provider {name} panicked during quota collection: {e}");
            }
            Err(e) => {
                log::warn!("Provider {name} task cancelled: {e}");
            }
        }
    }
    log::info!(
        "quota::collect_all → {} provider(s) populated: {}",
        out.len(),
        out.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Snapshot of `cli pulse/CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/Models.swift:10-37`
    /// captured 2026-05-02. Re-sync this manually when Mac adds a new
    /// provider that Win/Linux ships — there is no compile-time link
    /// between the Swift enum and these Rust constants. The contract
    /// test below catches the case where the Rust constants drift
    /// from this snapshot, but does NOT catch a Mac-side rename
    /// (that's caught by users seeing duplicate rows and reporting it).
    const MAC_PROVIDER_KIND_SNAPSHOT: &[(&str, &str)] = &[
        ("codex", "Codex"),
        ("gemini", "Gemini"),
        ("claude", "Claude"),
        ("cursor", "Cursor"),
        ("copilot", "Copilot"),
        ("openRouter", "OpenRouter"),
    ];

    #[test]
    fn provider_name_contract_matches_mac_snapshot() {
        let rust_consts = [
            ("claude", PROVIDER_CLAUDE),
            ("codex", PROVIDER_CODEX),
            ("cursor", PROVIDER_CURSOR),
            ("gemini", PROVIDER_GEMINI),
            ("copilot", PROVIDER_COPILOT),
            ("openRouter", PROVIDER_OPENROUTER),
        ];
        for (case_name, rust_value) in rust_consts {
            let mac_entry = MAC_PROVIDER_KIND_SNAPSHOT
                .iter()
                .find(|(case, _)| *case == case_name)
                .unwrap_or_else(|| panic!("Mac snapshot missing case `{case_name}`"));
            assert_eq!(
                rust_value, mac_entry.1,
                "Rust constant for `{case_name}` is `{rust_value}` but Mac \
                 ProviderKind raw value is `{}` — dual-writer would land on \
                 different (user_id, provider) PKs",
                mac_entry.1
            );
        }
    }

    /// v0.4.19 — pin the proactive-refresh buffer at 5 minutes.
    /// Per Gemini 3.1 Pro review: "5 minutes is the mathematically
    /// correct integer for your architecture. Given a 120-second
    /// background tick, a 5-minute buffer safely absorbs exactly two
    /// dropped cycles." Lower → fragile to single missed tick.
    /// Higher → wastes refresh-endpoint quota.
    #[test]
    fn pre_expiry_buffer_pinned_at_5_minutes() {
        assert_eq!(
            PRE_EXPIRY_BUFFER_MS,
            5.0 * 60.0 * 1000.0,
            "PRE_EXPIRY_BUFFER_MS must be exactly 5 min — see Gemini review of v0.4.19 plan"
        );
    }
}
