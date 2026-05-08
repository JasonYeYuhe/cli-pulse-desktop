//! v0.8.0 — Lifecycle event poster for managed sessions.
//!
//! Mirrors the Mac sibling's iter1 event surface: only `kind=status`
//! and `kind=info` are emitted by v0.8.0. Stdout/stderr tail upload
//! is deferred to v0.8.x (plan §"Out of scope") to keep the wire
//! shape simple and match the Mac iter1 contract.
//!
//! ### Status event semantics — load-bearing
//!
//! Server-side `remote_helper_post_event` only transitions
//! `remote_sessions.status` when:
//!   * `p_kind = 'status'`
//!   * `p_payload IN ('stopped', 'errored')` — exact match required.
//!
//! Lifecycle context (exit code, "child gone" hint, spawn-failure
//! reason) goes via `kind=info` so a typo in the status payload
//! doesn't silently leave the row stuck on `running`. Mac's
//! `_post_status` does the same defensive check; we mirror it here.
//!
//! ### Sequence numbers
//!
//! Each session has its own dense seq counter starting at 1. Counters
//! reset on session respawn (the seq column isn't unique per session
//! in the schema). The agent calls `next_seq(session_id)` before
//! every post and the result is the value sent for `p_seq`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::supabase::{remote_helper_post_event, SupabaseError};

/// Server-side cap on `remote_session_events.payload`. Matches the
/// SQL CHECK on `length(payload) <= 4096`. We cap a hair under so a
/// UTF-8 boundary fix can't push payloads over. v0.8.0 only emits
/// `kind=info` (which uses INFO_PAYLOAD_CAP_CHARS) and `kind=status`
/// (fixed payload string) so this cap is unused this iter; v0.8.x
/// stdout tail upload will reference it. Kept here as the canonical
/// constant so future code doesn't redefine it.
#[allow(dead_code)]
const EVENT_PAYLOAD_CAP_CHARS: usize = 4000;

/// Hard cap for `kind=info` rows. Smaller than stdout because info
/// is inherently short — anything longer probably has a stack trace
/// we'd rather elide. Mac's helper uses 1024.
const INFO_PAYLOAD_CAP_CHARS: usize = 1024;

/// Per-session event sequence counters. Held in a Mutex<HashMap<>>
/// so the agent loop can hand the same `EventPoster` to multiple
/// dispatch paths without serialising via channels. Counter resets
/// when a session is removed from the map (drop on stop / errored /
/// observe-exit).
pub struct EventPoster {
    device_id: String,
    helper_secret: String,
    seq: Mutex<HashMap<String, i64>>,
}

impl EventPoster {
    pub fn new(device_id: String, helper_secret: String) -> Self {
        Self {
            device_id,
            helper_secret,
            seq: Mutex::new(HashMap::new()),
        }
    }

    /// Increment + return the next seq for `session_id`. Starts at 1.
    fn next_seq(&self, session_id: &str) -> i64 {
        let mut map = match self.seq.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(), // poisoned: keep going, the data is fine
        };
        let entry = map.entry(session_id.to_string()).or_insert(0);
        *entry += 1;
        *entry
    }

    /// Forget a session's seq counter. Called when a session has
    /// terminated and the row is removed from the agent's map. Avoids
    /// growing the HashMap unboundedly across long-lived helper
    /// processes.
    pub fn forget(&self, session_id: &str) {
        if let Ok(mut map) = self.seq.lock() {
            map.remove(session_id);
        }
    }

    /// Post a `kind=status` lifecycle event. Payload must be exactly
    /// `"stopped"` or `"errored"` for the SQL gate to transition
    /// `remote_sessions.status` — defensive check rejects anything
    /// else with a noisy log line.
    pub async fn post_status(
        &self,
        session_id: &str,
        status: StatusKind,
    ) -> Result<(), SupabaseError> {
        let payload = status.as_str();
        let seq = self.next_seq(session_id);
        remote_helper_post_event(
            &self.device_id,
            &self.helper_secret,
            session_id,
            seq,
            "status",
            payload,
        )
        .await
    }

    /// Post a `kind=running` lifecycle event. Different from the
    /// `'pending' → 'running'` transition (which the SQL function
    /// `remote_helper_register_session` performs); this `kind=info`
    /// emission is for downstream observers that subscribe to
    /// session events. v0.8.0 keeps it as `info` rather than
    /// `running` because the server-side gate only honours `status`
    /// payloads in `('stopped', 'errored')` per the Mac iter1
    /// contract — a `'running'` status row would be ignored
    /// anyway, so we use `info` for the lifecycle hint.
    pub async fn post_info(&self, session_id: &str, detail: &str) -> Result<(), SupabaseError> {
        if detail.is_empty() {
            return Ok(());
        }
        let payload = truncate_chars(detail, INFO_PAYLOAD_CAP_CHARS);
        let seq = self.next_seq(session_id);
        remote_helper_post_event(
            &self.device_id,
            &self.helper_secret,
            session_id,
            seq,
            "info",
            &payload,
        )
        .await
    }
}

/// Allowed values for `kind=status` payload. The enum exists so a
/// caller can't accidentally hand a freeform string; the SQL gate
/// requires exact matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Stopped,
    Errored,
}

impl StatusKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StatusKind::Stopped => "stopped",
            StatusKind::Errored => "errored",
        }
    }
}

/// Cap to N chars without mid-codepoint truncation. Mirrors the
/// Mac helper's `_safe_truncate_utf8` behaviour.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::with_capacity(max);
    for (i, c) in s.chars().enumerate() {
        if i >= max - 1 {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_kind_payload_strings() {
        assert_eq!(StatusKind::Stopped.as_str(), "stopped");
        assert_eq!(StatusKind::Errored.as_str(), "errored");
    }

    #[test]
    fn next_seq_increments_per_session() {
        let p = EventPoster::new("dev".to_string(), "secret".to_string());
        assert_eq!(p.next_seq("a"), 1);
        assert_eq!(p.next_seq("a"), 2);
        assert_eq!(p.next_seq("b"), 1);
        assert_eq!(p.next_seq("a"), 3);
    }

    #[test]
    fn forget_resets_per_session() {
        let p = EventPoster::new("dev".to_string(), "secret".to_string());
        assert_eq!(p.next_seq("a"), 1);
        assert_eq!(p.next_seq("a"), 2);
        p.forget("a");
        // After forget, next_seq starts over at 1.
        assert_eq!(p.next_seq("a"), 1);
    }

    #[test]
    fn truncate_handles_under_cap() {
        assert_eq!(truncate_chars("hello", 100), "hello");
        assert_eq!(truncate_chars("", 5), "");
    }

    #[test]
    fn truncate_caps_long_strings() {
        let s = "a".repeat(EVENT_PAYLOAD_CAP_CHARS + 100);
        let truncated = truncate_chars(&s, EVENT_PAYLOAD_CAP_CHARS);
        assert_eq!(truncated.chars().count(), EVENT_PAYLOAD_CAP_CHARS);
        assert!(truncated.ends_with('…'));
    }

    #[test]
    fn truncate_respects_utf8_boundaries() {
        // Multi-byte characters must not be split mid-codepoint.
        let s = "あ".repeat(10);
        let truncated = truncate_chars(&s, 5);
        assert_eq!(truncated.chars().count(), 5);
        assert!(truncated.ends_with('…'));
        // The string IS valid UTF-8 — would panic on display
        // otherwise.
        assert!(!truncated.is_empty());
    }
}
