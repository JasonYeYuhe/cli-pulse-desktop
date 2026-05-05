//! v0.5.2 — top-projects aggregation. Mac sibling parity surface:
//! Mac's `OverviewTab.swift:607` renders `TopProjectsList` from
//! `DashboardSummary.top_projects`, but the desktop's server-side
//! `dashboard_summary` RPC doesn't return that field (verified via
//! Supabase MCP on 2026-05-05). Per the v0.5.0 dev plan v2: source
//! the data from the existing `sessions` table client-side rather
//! than asking for a backend schema change.
//!
//! Algorithm: group by `project`, sum `estimated_cost`, count
//! sessions, take max `last_active_at`. Sort by total cost desc.
//! Truncate to `limit`.
//!
//! Sessions with null `project` are aggregated under the
//! `<unknown>` bucket — the v0.4.x scanner can't always resolve
//! a project (e.g. helper-launched sessions, whoami=root sessions
//! on Linux). Surfacing a single "unknown" row is more honest
//! than silently dropping that cost from the total.

use serde::Serialize;
use std::collections::HashMap;

use crate::supabase::SessionRow;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TopProject {
    pub project: String,
    pub cost_usd: f64,
    pub session_count: usize,
    /// Most recent `last_active_at` across sessions in this bucket.
    /// ISO-8601 string from the server. Empty string if all sessions
    /// in the bucket had a null timestamp (shouldn't happen — the
    /// server populates `last_active_at` on insert — but guarded).
    pub last_active: String,
}

/// Bucket label used when a session row has `project = NULL`. Keep
/// in sync with the frontend so the UI can pretty-print this label
/// (`overview.top_projects_unknown` i18n key) instead of literally
/// rendering the angle brackets.
pub const UNKNOWN_PROJECT: &str = "<unknown>";

pub fn aggregate_top_projects(rows: &[SessionRow], limit: usize) -> Vec<TopProject> {
    // (cost, session_count, max_last_active)
    type Acc = (f64, usize, String);
    let mut by_project: HashMap<String, Acc> = HashMap::new();

    for row in rows {
        let project = row
            .project
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(UNKNOWN_PROJECT)
            .to_string();
        let cost = row.estimated_cost.unwrap_or(0.0);
        let entry = by_project.entry(project).or_insert((0.0, 0, String::new()));
        entry.0 += cost;
        entry.1 += 1;
        // String comparison works for ISO-8601 timestamps because the
        // format is lexicographically sortable. No chrono parse needed.
        if row.last_active_at > entry.2 {
            entry.2.clear();
            entry.2.push_str(&row.last_active_at);
        }
    }

    let mut out: Vec<TopProject> = by_project
        .into_iter()
        .map(
            |(project, (cost_usd, session_count, last_active))| TopProject {
                project,
                cost_usd,
                session_count,
                last_active,
            },
        )
        .collect();
    // Stable order: highest cost first; on tie, more recent
    // last_active wins (recent activity ranks above stale rollups);
    // on a double-tie, fall back to project-name lexicographic so
    // the rendered order is deterministic across app launches
    // (HashMap iteration order is randomized; per Gemini 3.1 Pro
    // v0.5.2 review P2). Without the project-name fallback, two
    // projects with identical cost AND identical last_active
    // (e.g. both empty) would visually swap on each restart.
    out.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.last_active.cmp(&a.last_active))
            .then_with(|| a.project.cmp(&b.project))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(project: Option<&str>, cost: f64, last_active: &str) -> SessionRow {
        SessionRow {
            project: project.map(|s| s.to_string()),
            estimated_cost: Some(cost),
            requests: None,
            last_active_at: last_active.to_string(),
            total_usage: None,
        }
    }

    #[test]
    fn aggregate_groups_by_project_and_sums_cost() {
        let rows = vec![
            row(Some("alpha"), 1.5, "2026-05-05T10:00:00Z"),
            row(Some("alpha"), 2.0, "2026-05-05T11:00:00Z"),
            row(Some("beta"), 5.0, "2026-05-05T09:00:00Z"),
        ];
        let out = aggregate_top_projects(&rows, 5);
        assert_eq!(out.len(), 2);
        // beta is bigger cost (5.0) — first.
        assert_eq!(out[0].project, "beta");
        assert!((out[0].cost_usd - 5.0).abs() < 0.001);
        assert_eq!(out[0].session_count, 1);
        // alpha second, summed cost.
        assert_eq!(out[1].project, "alpha");
        assert!((out[1].cost_usd - 3.5).abs() < 0.001);
        assert_eq!(out[1].session_count, 2);
        // Max last_active across the bucket.
        assert_eq!(out[1].last_active, "2026-05-05T11:00:00Z");
    }

    #[test]
    fn null_project_falls_into_unknown_bucket() {
        let rows = vec![
            row(None, 1.0, "2026-05-05T10:00:00Z"),
            row(Some(""), 2.0, "2026-05-05T11:00:00Z"), // empty string also unknown
            row(Some("real"), 3.0, "2026-05-05T09:00:00Z"),
        ];
        let out = aggregate_top_projects(&rows, 5);
        assert_eq!(out.len(), 2);
        // unknown sums 1.0 + 2.0 = 3.0, real = 3.0 → tied. Tie-break
        // on max last_active: unknown 11:00 > real 09:00 → unknown
        // wins. Documents that the bucket label sorts naturally.
        assert_eq!(out[0].project, UNKNOWN_PROJECT);
        assert!((out[0].cost_usd - 3.0).abs() < 0.001);
        assert_eq!(out[0].session_count, 2);
        assert_eq!(out[1].project, "real");
        assert!((out[1].cost_usd - 3.0).abs() < 0.001);
    }

    #[test]
    fn empty_input_returns_empty_vec() {
        let out = aggregate_top_projects(&[], 5);
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn truncates_to_limit() {
        let rows: Vec<SessionRow> = (0..10)
            .map(|i| {
                row(
                    Some(&format!("project-{i}")),
                    (10 - i) as f64,
                    "2026-05-05T10:00:00Z",
                )
            })
            .collect();
        let out = aggregate_top_projects(&rows, 5);
        assert_eq!(out.len(), 5);
        // Highest cost first — project-0 is most expensive (10.0).
        assert_eq!(out[0].project, "project-0");
        assert_eq!(out[4].project, "project-4");
    }

    #[test]
    fn null_estimated_cost_treated_as_zero() {
        let rows = vec![
            SessionRow {
                project: Some("alpha".into()),
                estimated_cost: None,
                requests: None,
                last_active_at: "2026-05-05T10:00:00Z".to_string(),
                total_usage: None,
            },
            row(Some("alpha"), 5.0, "2026-05-05T11:00:00Z"),
        ];
        let out = aggregate_top_projects(&rows, 5);
        assert_eq!(out.len(), 1);
        // First row contributed 0 cost but counts as a session.
        assert!((out[0].cost_usd - 5.0).abs() < 0.001);
        assert_eq!(out[0].session_count, 2);
    }
}
