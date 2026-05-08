//! v0.9.0 — Crash-recovery mode.
//!
//! Detects repeated launch crashes and enters a degraded-feature mode
//! to break the crash loop.
//!
//! ## The v0.8.0 lesson
//!
//! v0.8.0 BEX64 crash-on-launch repeated 9+ times in 4 minutes on the
//! test VM. Each crash logged through "Background sync loop started",
//! then panic-aborted. Without intervention, the user (or VM
//! supervisor) just kept restarting. There was no app-side defense
//! against "I keep crashing the same way every time."
//!
//! ## How this module works
//!
//! Append-only JSONL at `<app_data>/crash-history.jsonl`. On every
//! startup we append a `starting` entry. After Tauri's setup hook
//! completes (i.e. control reached the main event loop), we append
//! `setup_complete`. On clean exit (Tauri::Builder::run returning
//! normally) we append `clean_exit`.
//!
//! A "crash" is detected post-hoc: a `starting` entry whose closest
//! following entry is ANOTHER `starting` (instead of
//! `setup_complete`), within `CRASH_WINDOW`. That means the prior
//! launch died before reaching the event loop.
//!
//! On every startup, we scan the recent history; if we see ≥3 crashes
//! within `CRASH_WINDOW`, we flip on RECOVERY_MODE and surface a
//! banner. Recovery mode disables specific features (agent loop, tray
//! refresh) but **keeps Sentry on** (per Gemini plan-review P2:
//! disabling telemetry during a crash loop is the wrong direction;
//! we want MORE data not less).
//!
//! ## Privacy
//!
//! `crash-history.jsonl` is local-only. Each entry is just `{ts,
//! phase, version}` — no user data, no payloads, no env. The file is
//! capped at `MAX_ENTRIES` with FIFO rotation so it never grows
//! unboundedly. The diagnostic-bundle button (v0.9.2) will ship this
//! file with explicit user consent.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// How recent a "crash" still counts toward the recovery-mode
/// threshold. v0.8.0 saw 9 crashes in 4 minutes; 5 minutes captures
/// that pattern with a comfortable margin.
const CRASH_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Number of detected crashes within `CRASH_WINDOW` that flips on
/// recovery mode. Three is small enough to react quickly to a real
/// crash loop and large enough to not false-positive on a single
/// dev-iteration crash + clean restart.
const RECOVERY_MODE_THRESHOLD: usize = 3;

/// Hard cap on `crash-history.jsonl` line count. Older entries are
/// dropped FIFO when we cross this threshold. Prevents the file from
/// growing unbounded over the lifetime of an install.
const MAX_ENTRIES: usize = 100;

/// Process-wide flag — set by `assess_recovery_mode_at_startup` if
/// the threshold is exceeded. Read by the agent loop / tray refresh
/// init paths to skip themselves.
static RECOVERY_MODE: AtomicBool = AtomicBool::new(false);

/// Lifecycle phase markers. Append-only JSONL.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Logged at the very top of `lib.rs::run`, before any other
    /// initialization. Pairs with a later `SetupComplete` on success
    /// or no entry on crash.
    Starting,
    /// Logged at the end of Tauri's `setup` hook. If we never see
    /// this after a `Starting`, that launch crashed.
    SetupComplete,
    /// Logged when Tauri's event loop returns cleanly. Optional
    /// pairing with `SetupComplete` — represents a clean shutdown.
    CleanExit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// Unix epoch seconds.
    ts: u64,
    phase: Phase,
    /// App version at the time of logging. Lets the diagnostic
    /// bundle later distinguish "v0.7.0 crashes" from "v0.9.0
    /// crashes" if a user has upgraded mid-loop.
    version: String,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn history_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("crash-history.jsonl"))
}

/// Append a single entry. Best-effort — IO failures are silently
/// dropped. The whole point of this module is observability, not a
/// new failure mode that could keep the app from launching.
fn append(phase: Phase) {
    let Some(path) = history_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let entry = Entry {
        ts: now_unix(),
        phase,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
    // Best-effort cap — read all lines, keep the last MAX_ENTRIES,
    // rewrite. Done after each append so the file never grows past
    // the cap by more than a few lines. Cheap because the file is
    // <100 lines × ~80 chars = <8 KB even at the cap.
    let _ = trim_to_cap(&path);
}

fn trim_to_cap(path: &PathBuf) -> std::io::Result<()> {
    let f = File::open(path)?;
    let lines: Vec<String> = BufReader::new(f).lines().map_while(Result::ok).collect();
    if lines.len() <= MAX_ENTRIES {
        return Ok(());
    }
    let kept = &lines[lines.len() - MAX_ENTRIES..];
    let mut f = File::create(path)?;
    for line in kept {
        writeln!(f, "{line}")?;
    }
    Ok(())
}

fn read_entries() -> Vec<Entry> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    let Ok(f) = File::open(&path) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Entry>(&l).ok())
        .collect()
}

/// Walk the entries and count "incomplete startups" — i.e. a
/// `Starting` whose nearest following entry is another `Starting`
/// (vs `SetupComplete` or `CleanExit`). Each such pair represents
/// one crashed launch.
///
/// Crashes within the last `CRASH_WINDOW` are what trigger recovery
/// mode. We use the `Starting` ts (not the next-startup ts) so the
/// window is anchored at the crash time, not at detection time.
fn count_recent_crashes(entries: &[Entry], now: u64) -> usize {
    let window_start = now.saturating_sub(CRASH_WINDOW.as_secs());
    let mut crashes = 0usize;
    for i in 0..entries.len() {
        if entries[i].phase != Phase::Starting {
            continue;
        }
        // Find the next entry. If it's another Starting, the prior
        // Starting was a crash. If it's SetupComplete or CleanExit,
        // the prior was healthy.
        let next = entries.get(i + 1);
        let crashed = match next {
            Some(n) => n.phase == Phase::Starting,
            // No next entry — this is the CURRENT launch. Don't count.
            None => false,
        };
        if crashed && entries[i].ts >= window_start {
            crashes += 1;
        }
    }
    crashes
}

/// Public API: log that we're starting. Call once at the very top
/// of `lib.rs::run`, before any other initialization.
pub fn record_startup() {
    append(Phase::Starting);
}

/// Public API: log that the Tauri setup hook completed. Call from
/// inside the setup hook just before returning Ok(()).
pub fn record_setup_complete() {
    append(Phase::SetupComplete);
}

/// Public API: log that the Tauri event loop returned cleanly.
/// Call from `run()` after `tauri::Builder::run(...)` returns.
pub fn record_clean_exit() {
    append(Phase::CleanExit);
}

/// Public API: at startup (after `record_startup` but before any
/// risky initialization), check the history and decide whether to
/// enter recovery mode. Returns true if recovery mode is now active.
///
/// Side effect: sets the process-wide RECOVERY_MODE atomic flag.
pub fn assess_recovery_mode_at_startup() -> bool {
    let entries = read_entries();
    let crashes = count_recent_crashes(&entries, now_unix());
    let active = crashes >= RECOVERY_MODE_THRESHOLD;
    RECOVERY_MODE.store(active, Ordering::Relaxed);
    if active {
        log::warn!(
            "Recovery mode ACTIVE — detected {crashes} launch crashes within {}s",
            CRASH_WINDOW.as_secs()
        );
    }
    active
}

/// Read-only accessor for code paths that want to gate themselves
/// on recovery mode.
pub fn is_in_recovery_mode() -> bool {
    RECOVERY_MODE.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: u64, phase: Phase) -> Entry {
        Entry {
            ts,
            phase,
            version: "test".to_string(),
        }
    }

    #[test]
    fn no_entries_no_crashes() {
        assert_eq!(count_recent_crashes(&[], 1000), 0);
    }

    #[test]
    fn single_starting_no_crash_counts_zero() {
        // Only one entry — that's the current launch, no prior crash
        let entries = vec![entry(1000, Phase::Starting)];
        assert_eq!(count_recent_crashes(&entries, 1000), 0);
    }

    #[test]
    fn starting_then_setup_complete_zero_crashes() {
        let entries = vec![
            entry(1000, Phase::Starting),
            entry(1010, Phase::SetupComplete),
            entry(1020, Phase::Starting), // current launch
        ];
        assert_eq!(count_recent_crashes(&entries, 1020), 0);
    }

    #[test]
    fn starting_then_starting_one_crash() {
        // First Starting died before SetupComplete — that's a crash
        let entries = vec![
            entry(1000, Phase::Starting),
            entry(1010, Phase::Starting), // current launch (the one that just appended)
        ];
        assert_eq!(count_recent_crashes(&entries, 1010), 1);
    }

    #[test]
    fn three_consecutive_starting_two_crashes() {
        // Two prior crashes + one current launch
        let entries = vec![
            entry(1000, Phase::Starting),
            entry(1010, Phase::Starting),
            entry(1020, Phase::Starting), // current
        ];
        assert_eq!(count_recent_crashes(&entries, 1020), 2);
    }

    #[test]
    fn four_consecutive_starting_three_crashes_triggers_recovery() {
        let entries = vec![
            entry(1000, Phase::Starting),
            entry(1010, Phase::Starting),
            entry(1020, Phase::Starting),
            entry(1030, Phase::Starting), // current
        ];
        let crashes = count_recent_crashes(&entries, 1030);
        assert_eq!(crashes, 3);
        assert!(crashes >= RECOVERY_MODE_THRESHOLD);
    }

    #[test]
    fn crashes_outside_window_dont_count() {
        // Three old crashes (>5 min ago) + one new healthy launch
        let now = 10_000;
        let old = now - CRASH_WINDOW.as_secs() - 100;
        let entries = vec![
            entry(old, Phase::Starting),
            entry(old + 5, Phase::Starting),
            entry(old + 10, Phase::Starting),
            entry(old + 15, Phase::Starting),
            entry(now, Phase::Starting), // current
        ];
        // Old window crashes don't count; only crashes whose ts is
        // >= window_start. Result: only the most-recent prior would
        // count, but it's still in old window → 0.
        let crashes = count_recent_crashes(&entries, now);
        assert!(crashes < RECOVERY_MODE_THRESHOLD);
    }

    #[test]
    fn mixed_healthy_and_crash_history() {
        let entries = vec![
            entry(1000, Phase::Starting),
            entry(1010, Phase::SetupComplete),
            entry(1020, Phase::CleanExit),
            entry(1030, Phase::Starting), // crash (next is Starting at 1040)
            entry(1040, Phase::Starting), // crash (next is Starting at 1050)
            entry(1050, Phase::Starting), // crash (next is Starting at 1060)
            entry(1060, Phase::Starting), // current
        ];
        // First launch was healthy (1000 → SetupComplete at 1010 →
        // CleanExit at 1020). Then 3 consecutive crashes (1030, 1040,
        // 1050 each followed by another Starting), and a 4th launch
        // (1060) that's the current run. Three crashes triggers
        // recovery mode on this launch.
        let crashes = count_recent_crashes(&entries, 1060);
        assert_eq!(crashes, 3);
        assert!(crashes >= RECOVERY_MODE_THRESHOLD);
    }

    #[test]
    fn entry_serializes_round_trip() {
        let e = entry(1000, Phase::Starting);
        let json = serde_json::to_string(&e).unwrap();
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ts, 1000);
        assert_eq!(back.phase, Phase::Starting);
    }

    #[test]
    fn phase_serializes_as_snake_case() {
        let p = Phase::SetupComplete;
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(s, "\"setup_complete\"");
    }
}
