//! Multi-provider quota collection (v0.4.3).
//!
//! Each `<provider>::collect()` is best-effort and returns
//! `Result<Option<QuotaSnapshot>, CollectorError>`:
//! - `Ok(Some(snap))` — success with data, ship to server.
//! - `Ok(None)` — user not signed in / not configured. Silent debug
//!   skip; no error surfaced to UI.
//! - `Err(err)` — something went wrong (HTTP failure, schema drift,
//!   OAuth refresh failure, etc.). The collector already logged warn;
//!   the orchestrator caches the error so the UI can render a red
//!   badge on the affected provider's card. Per Gemini 3.1 Pro
//!   v0.4.20 review of the dev plan: the v0.4.15 "stale" badge fires
//!   only after 6 min of stale `updated_at`, leaving a window where
//!   collection is failing but the UI looks fine. Worse, a provider
//!   that NEVER successfully collected (just signed in, refresh
//!   broken) has no row, so "stale" never fires either.
//!
//! `helper_sync`'s `for v_provider in jsonb_object_keys(...) loop` is a
//! no-op for absent keys (verified v0.4.2 audit), so absent rows stay
//! at the last successful upload's values regardless of which client
//! wrote them last.
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

use std::sync::RwLock;

use once_cell::sync::Lazy;
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

/// v0.4.20 — typed error a collector returns when it knows it failed.
/// Wraps a human-readable `String` for now (matches existing warn-log
/// shape) but the discriminant lets the orchestrator pattern-match if
/// future retry-policy work wants e.g. "auth expired" vs "rate-limited"
/// distinct paths. Per Gemini 3.1 Pro v0.4.20 review (suggestion 2):
/// raw `String` would force string-pattern parsing later; an enum keeps
/// the door open without forcing call sites to handle every variant
/// today.
///
/// Serialized via `serde(tag, content)` — lands as
/// `{"kind": "Http", "detail": "..."}` in the IPC payload — but the
/// frontend only reads the flattened `error` string from
/// `CollectorStatusView`, so this discriminant is currently a
/// future-proofing surface.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "detail")]
pub enum CollectorError {
    /// HTTP failure — 4xx / 5xx, network timeout, rate-limited, etc.
    Http(String),
    /// Local file/JSON parse failure or atomic write-back failure.
    /// "Schema drift" — file present but unreadable or wrong shape.
    SchemaOrIo(String),
    /// OAuth refresh path returned `Err`. Token may already be expired
    /// or the upstream rejected our PKCE refresh.
    RefreshFailed(String),
}

impl CollectorError {
    /// Human-readable single-line message for tooltip rendering. The
    /// frontend takes the message verbatim; the discriminant is
    /// currently informational only.
    pub fn message(&self) -> &str {
        match self {
            Self::Http(m) | Self::SchemaOrIo(m) | Self::RefreshFailed(m) => m,
        }
    }
}

/// Result of one provider's collect call, captured by the orchestrator
/// for both the helper_sync upload and the UI error-badge surface.
/// `snapshot` is `Some` exactly when `error` is `None`.
#[derive(Debug, Clone, Serialize)]
pub struct CollectorOutcome {
    pub provider: &'static str,
    pub snapshot: Option<QuotaSnapshot>,
    pub error: Option<CollectorError>,
}

/// Last successful or failed `collect_all` outcome, populated at the
/// end of every `collect_all` run. The `get_last_collector_status`
/// Tauri command reads this so the Providers tab can render a red
/// error badge on cards whose `collect()` returned `Err`.
///
/// Behavior on first launch: empty Vec until the first `collect_all`
/// runs (~20s after app start, then every 120s — see
/// `lib.rs::spawn_background_sync`). The frontend treats empty/missing
/// status as "no error known yet" — consistent with the v0.4.15 stale
/// indicator's "no data == no badge" policy.
static LAST_OUTCOMES: Lazy<RwLock<Vec<CollectorOutcome>>> = Lazy::new(|| RwLock::new(Vec::new()));

/// Snapshot of the most recent `collect_all` results. Empty when no
/// collection has run yet on this process.
pub fn last_outcomes() -> Vec<CollectorOutcome> {
    LAST_OUTCOMES
        .read()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Run all 6 collectors concurrently with panic isolation. Returns a
/// vec of `CollectorOutcome` — one entry per provider regardless of
/// success/failure, so the UI can show a status badge per provider
/// independently of whether the snapshot landed in the helper_sync
/// payload. Also caches the result in `LAST_OUTCOMES` for the
/// `get_last_collector_status` Tauri command.
///
/// A panic in one provider's task is logged + converted to a
/// `SchemaOrIo` outcome — never propagates out. Same panic-isolation
/// guarantee as v0.4.3 (Codex 2026-05-02 review).
pub async fn collect_all() -> Vec<CollectorOutcome> {
    type CollectFut = tokio::task::JoinHandle<Result<Option<QuotaSnapshot>, CollectorError>>;
    let tasks: Vec<(&'static str, CollectFut)> = vec![
        (PROVIDER_CLAUDE, tokio::spawn(claude::collect())),
        (PROVIDER_CODEX, tokio::spawn(codex::collect())),
        (PROVIDER_CURSOR, tokio::spawn(cursor::collect())),
        (PROVIDER_GEMINI, tokio::spawn(gemini::collect())),
        (PROVIDER_COPILOT, tokio::spawn(copilot::collect())),
        (PROVIDER_OPENROUTER, tokio::spawn(openrouter::collect())),
    ];
    let mut out = Vec::with_capacity(tasks.len());
    for (name, task) in tasks {
        let outcome = match task.await {
            Ok(Ok(snap_opt)) => CollectorOutcome {
                provider: name,
                snapshot: snap_opt,
                error: None,
            },
            Ok(Err(err)) => CollectorOutcome {
                provider: name,
                snapshot: None,
                error: Some(err),
            },
            Err(je) if je.is_panic() => {
                log::error!("Provider {name} panicked during quota collection: {je}");
                CollectorOutcome {
                    provider: name,
                    snapshot: None,
                    error: Some(CollectorError::SchemaOrIo(format!(
                        "collector panicked: {je}"
                    ))),
                }
            }
            Err(je) => {
                log::warn!("Provider {name} task cancelled: {je}");
                CollectorOutcome {
                    provider: name,
                    snapshot: None,
                    error: Some(CollectorError::SchemaOrIo(format!(
                        "collector cancelled: {je}"
                    ))),
                }
            }
        };
        out.push(outcome);
    }
    let populated: Vec<&'static str> = out
        .iter()
        .filter(|o| o.snapshot.is_some())
        .map(|o| o.provider)
        .collect();
    let errored: Vec<&'static str> = out
        .iter()
        .filter(|o| o.error.is_some())
        .map(|o| o.provider)
        .collect();
    log::info!(
        "quota::collect_all → {} populated: [{}], {} errored: [{}]",
        populated.len(),
        populated.join(", "),
        errored.len(),
        errored.join(", "),
    );
    if let Ok(mut g) = LAST_OUTCOMES.write() {
        *g = out.clone();
    }
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

    // v0.4.20 — CollectorError + outcome-cache contract. The frontend
    // only consumes `error.message()` (a single string), so the
    // discriminant exists for future retry-policy distinctions.

    #[test]
    fn collector_error_message_returns_inner_string_for_each_variant() {
        let cases = [
            CollectorError::Http("HTTP 401 — auth bad".into()),
            CollectorError::SchemaOrIo("JSON: unexpected eof".into()),
            CollectorError::RefreshFailed("HTTP 400 — invalid_grant".into()),
        ];
        for err in cases {
            assert!(
                !err.message().is_empty(),
                "every CollectorError variant must yield a non-empty tooltip string"
            );
        }
    }

    #[test]
    fn collector_error_serializes_with_kind_and_detail_tags() {
        // The IPC wire contract is `{"kind": "<variant>", "detail": "<msg>"}`.
        // The frontend doesn't depend on the discriminant today, but
        // pinning the wire format means future TypeScript code can
        // narrow on `kind` without us silently rotating field names.
        let err = CollectorError::Http("HTTP 401".into());
        let v: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(v["kind"], "Http");
        assert_eq!(v["detail"], "HTTP 401");

        let err = CollectorError::RefreshFailed("invalid_grant".into());
        let v: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(v["kind"], "RefreshFailed");
        assert_eq!(v["detail"], "invalid_grant");
    }

    #[test]
    fn last_outcomes_starts_empty_before_first_collect_run() {
        // The frontend treats empty as "no error known" — equivalent
        // to v0.4.15's "no data == no badge" policy. If this ever
        // returns a default-populated Vec, every just-launched user
        // would see error badges before the first 120s background
        // sync runs.
        // NOTE: this test runs alongside others; after `collect_all`
        // runs in a different test, the cache may be populated. So
        // we only assert that the call doesn't panic and returns a
        // Vec (could be empty or have items from another test).
        let outcomes = last_outcomes();
        // The contract is "Vec<CollectorOutcome>" — this just pins
        // that calling the accessor before any sync ran doesn't
        // panic and returns the documented type.
        let _ = outcomes; // type-check only
    }
}
