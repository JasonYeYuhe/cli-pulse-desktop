//! Client-computed alerts fed into `helper_sync.p_alerts`.
//!
//! Budget alerts are evaluated here (not via server RPC) because the
//! desktop has access to precise local scan data — including today's
//! partial cost — before it's uploaded to `daily_usage_metrics`. This
//! matches how the iOS/macOS apps show "today's spend" on the Overview
//! card: precise, immediate, not waiting on server roll-up.
//!
//! Sessions-based alerts (CPU spike, long-running) mirror the Python
//! helper's `collect_alerts` function in `helper/system_collector.py`.
//!
//! Output format: each Alert is a JSON object matching the columns
//! `helper_sync` expects in its `p_alerts` array (see Swift / Python
//! implementations + `backend/supabase/helper_rpc.sql`).

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

use crate::scanner::ScanResult;
use crate::sessions::{LiveSession, SessionsSnapshot};

/// Matches `public.alerts` columns + helper_sync insert order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    #[serde(rename = "type")]
    pub alert_type: String,
    pub severity: String, // "Info" | "Warning" | "Critical"
    pub title: String,
    pub message: String,
    pub created_at: String, // RFC3339
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_project_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_device_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>, // "device" | "session" | "project" | "budget"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grouping_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppression_key: Option<String>,
}

/// User-tunable thresholds. Each `None` = never alert for this.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertThresholds {
    pub daily_budget_usd: Option<f64>,
    pub weekly_budget_usd: Option<f64>,
    #[serde(default = "default_cpu_threshold")]
    pub cpu_spike_pct: f32,
}

fn default_cpu_threshold() -> f32 {
    80.0
}

impl Default for AlertThresholds {
    fn default() -> Self {
        Self {
            daily_budget_usd: None,
            weekly_budget_usd: None,
            cpu_spike_pct: 80.0,
        }
    }
}

/// Compute client-side alerts from (scan, sessions). Idempotent: same
/// inputs + same day → same `suppression_key`, so `helper_sync`'s
/// `on conflict (id, user_id) do update` refreshes the row instead of
/// spawning a duplicate.
pub fn compute(
    scan: &ScanResult,
    sessions: &SessionsSnapshot,
    thresholds: &AlertThresholds,
    device_name: Option<&str>,
) -> Vec<Alert> {
    let now = Utc::now();
    let now_iso = now.to_rfc3339();
    let today_key = scan.today_key.clone();
    let mut out: Vec<Alert> = Vec::new();

    // --- Budget: daily ---
    if let Some(daily_limit) = thresholds.daily_budget_usd {
        if daily_limit > 0.0 {
            let today_cost = today_cost(scan);
            if today_cost > daily_limit {
                out.push(Alert {
                    id: format!("budget-daily-{today_key}"),
                    alert_type: "Daily Budget Exceeded".into(),
                    severity: "Warning".into(),
                    title: format!("Daily budget exceeded — ${today_cost:.2}"),
                    message: format!(
                        "Today's spend of ${today_cost:.2} is above your daily budget of ${daily_limit:.2}."
                    ),
                    created_at: now_iso.clone(),
                    related_project_id: None,
                    related_project_name: None,
                    related_session_id: None,
                    related_session_name: None,
                    related_provider: None,
                    related_device_name: device_name.map(String::from),
                    source_kind: Some("budget".into()),
                    source_id: Some(format!("daily:{today_key}")),
                    grouping_key: Some("budget:daily".into()),
                    suppression_key: Some(format!("budget-daily:{today_key}")),
                });
            }
        }
    }

    // --- Budget: weekly (rolling 7d) ---
    if let Some(weekly_limit) = thresholds.weekly_budget_usd {
        if weekly_limit > 0.0 {
            let week_cost = last_7_days_cost(scan, &now);
            if week_cost > weekly_limit {
                let week_label = iso_week_label(&now);
                out.push(Alert {
                    id: format!("budget-weekly-{week_label}"),
                    alert_type: "Weekly Budget Exceeded".into(),
                    severity: "Warning".into(),
                    title: format!("Weekly budget exceeded — ${week_cost:.2}"),
                    message: format!(
                        "Last 7 days of spend totals ${week_cost:.2}, above your weekly budget of ${weekly_limit:.2}."
                    ),
                    created_at: now_iso.clone(),
                    related_project_id: None,
                    related_project_name: None,
                    related_session_id: None,
                    related_session_name: None,
                    related_provider: None,
                    related_device_name: device_name.map(String::from),
                    source_kind: Some("budget".into()),
                    source_id: Some(format!("weekly:{week_label}")),
                    grouping_key: Some("budget:weekly".into()),
                    suppression_key: Some(format!("budget-weekly:{week_label}")),
                });
            }
        }
    }

    // --- Per-session CPU spikes ---
    for s in &sessions.sessions {
        if s.cpu_usage >= thresholds.cpu_spike_pct {
            out.push(cpu_spike_alert(s, &now_iso, device_name));
        }
    }

    // Server caps at 500; we stay well below.
    out.truncate(20);
    out
}

fn cpu_spike_alert(s: &LiveSession, now_iso: &str, device_name: Option<&str>) -> Alert {
    Alert {
        id: format!("session-spike-{}", s.id),
        alert_type: "Usage Spike".into(),
        severity: "Warning".into(),
        title: format!("{} is consuming high CPU", short_name(&s.name)),
        message: format!(
            "Process CPU is {:.1}% for {} in {}.",
            s.cpu_usage, s.provider, s.project
        ),
        created_at: now_iso.to_string(),
        related_project_id: Some(project_id(&s.project)),
        related_project_name: Some(s.project.clone()),
        related_session_id: Some(s.id.clone()),
        related_session_name: Some(s.name.clone()),
        related_provider: Some(s.provider.clone()),
        related_device_name: device_name.map(String::from),
        source_kind: Some("session".into()),
        source_id: Some(s.id.clone()),
        grouping_key: Some(format!("usage:{}", s.provider)),
        suppression_key: Some(format!(
            "usage-spike:{}:{}",
            s.id,
            device_name.unwrap_or("desktop")
        )),
    }
}

fn today_cost(scan: &ScanResult) -> f64 {
    let today = &scan.today_key;
    scan.entries
        .iter()
        .filter(|e| &e.date == today && e.model != crate::scanner::CLAUDE_MSG_BUCKET_MODEL)
        .filter_map(|e| e.cost_usd)
        .sum()
}

fn last_7_days_cost(scan: &ScanResult, now: &DateTime<Utc>) -> f64 {
    let cutoff = now
        .checked_sub_signed(chrono::Duration::days(7))
        .map(|dt| dt.date_naive())
        .unwrap_or_else(|| now.date_naive());
    scan.entries
        .iter()
        .filter(|e| e.model != crate::scanner::CLAUDE_MSG_BUCKET_MODEL)
        .filter_map(|e| {
            let d = chrono::NaiveDate::parse_from_str(&e.date, "%Y-%m-%d").ok()?;
            if d >= cutoff {
                e.cost_usd
            } else {
                None
            }
        })
        .sum()
}

fn iso_week_label(d: &DateTime<Utc>) -> String {
    let iso = d.iso_week();
    format!("{:04}-W{:02}", iso.year(), iso.week())
}

fn short_name(name: &str) -> String {
    if name.chars().count() <= 32 {
        name.to_string()
    } else {
        let truncated: String = name.chars().take(29).collect();
        format!("{truncated}...")
    }
}

fn project_id(project: &str) -> String {
    let mut out = String::with_capacity(project.len());
    let mut last = '-';
    for c in project.to_lowercase().chars() {
        let ch = if c.is_alphanumeric() { c } else { '-' };
        if ch == '-' && last == '-' {
            continue;
        }
        out.push(ch);
        last = ch;
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "local-workspace".into()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{DailyEntry, ScanResult};
    use crate::sessions::{LiveSession, SessionsSnapshot};

    fn dummy_scan(today: &str, cost_today: f64, cost_yesterday: f64) -> ScanResult {
        ScanResult {
            entries: vec![
                DailyEntry {
                    date: today.into(),
                    provider: "Claude".into(),
                    model: "claude-sonnet-4-6".into(),
                    input_tokens: 10,
                    cached_tokens: 0,
                    output_tokens: 10,
                    cost_usd: Some(cost_today),
                    message_count: 0,
                },
                DailyEntry {
                    date: "2026-04-23".into(),
                    provider: "Claude".into(),
                    model: "claude-sonnet-4-6".into(),
                    input_tokens: 10,
                    cached_tokens: 0,
                    output_tokens: 10,
                    cost_usd: Some(cost_yesterday),
                    message_count: 0,
                },
            ],
            total_cost_usd: cost_today + cost_yesterday,
            total_tokens: 40,
            today_key: today.into(),
            days_scanned: 2,
            files_scanned: 1,
            files_cached: 0,
        }
    }

    fn dummy_sessions(count: usize, cpu: f32) -> SessionsSnapshot {
        let sessions = (0..count)
            .map(|i| LiveSession {
                id: format!("proc-{i}"),
                name: format!("claude --cli-{i}"),
                provider: "Claude".into(),
                project: "demo".into(),
                status: "Running".into(),
                total_usage: 1000,
                exact_cost: None,
                requests: 5,
                error_count: 0,
                collection_confidence: "high".into(),
                started_at: "2026-04-24T12:00:00Z".into(),
                last_active_at: "2026-04-24T13:00:00Z".into(),
                cpu_usage: cpu,
                memory_mb: 100,
                pids: vec![i as u32],
                command: format!("claude-{i}"),
            })
            .collect();
        SessionsSnapshot {
            sessions,
            total_processes_seen: 100,
            matched_before_dedup: count,
            collected_at: "2026-04-24T13:00:00Z".into(),
        }
    }

    #[test]
    fn daily_budget_breach_fires_once() {
        let scan = dummy_scan("2026-04-24", 75.0, 10.0);
        let sess = dummy_sessions(0, 0.0);
        let th = AlertThresholds {
            daily_budget_usd: Some(50.0),
            ..AlertThresholds::default()
        };
        let alerts = compute(&scan, &sess, &th, None);
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].alert_type, "Daily Budget Exceeded");
        assert_eq!(
            alerts[0].suppression_key.as_deref(),
            Some("budget-daily:2026-04-24")
        );
    }

    #[test]
    fn daily_budget_under_limit_no_alert() {
        let scan = dummy_scan("2026-04-24", 10.0, 5.0);
        let sess = dummy_sessions(0, 0.0);
        let th = AlertThresholds {
            daily_budget_usd: Some(50.0),
            ..AlertThresholds::default()
        };
        let alerts = compute(&scan, &sess, &th, None);
        assert_eq!(alerts.len(), 0);
    }

    #[test]
    fn cpu_spike_above_threshold_triggers() {
        let scan = dummy_scan("2026-04-24", 1.0, 1.0);
        let sess = dummy_sessions(2, 85.0);
        let th = AlertThresholds::default();
        let alerts = compute(&scan, &sess, &th, None);
        assert_eq!(alerts.len(), 2);
        assert_eq!(alerts[0].alert_type, "Usage Spike");
        assert_eq!(alerts[0].severity, "Warning");
    }

    #[test]
    fn cpu_below_threshold_no_alert() {
        let scan = dummy_scan("2026-04-24", 1.0, 1.0);
        let sess = dummy_sessions(3, 40.0);
        let th = AlertThresholds::default();
        let alerts = compute(&scan, &sess, &th, None);
        assert_eq!(alerts.len(), 0);
    }

    #[test]
    fn msg_bucket_not_counted_in_budget_cost() {
        let mut scan = dummy_scan("2026-04-24", 10.0, 5.0);
        scan.entries.push(DailyEntry {
            date: "2026-04-24".into(),
            provider: "Claude".into(),
            model: "__claude_msg__".into(),
            input_tokens: 0,
            cached_tokens: 0,
            output_tokens: 0,
            cost_usd: None,
            message_count: 500,
        });
        let sess = dummy_sessions(0, 0.0);
        let th = AlertThresholds {
            daily_budget_usd: Some(8.0),
            ..AlertThresholds::default()
        };
        let alerts = compute(&scan, &sess, &th, None);
        // Today cost = 10 (msg bucket excluded) > 8 → one alert.
        assert_eq!(alerts.len(), 1);
    }

    #[test]
    fn project_id_sanitizes_safely() {
        assert_eq!(project_id("Hello World"), "hello-world");
        assert_eq!(project_id("!!!"), "local-workspace");
        assert_eq!(project_id("foo / bar"), "foo-bar");
    }
}
