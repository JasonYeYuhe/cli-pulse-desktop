//! v0.8.0 — Shared file logger for the Tauri agent + the
//! `bin/remote_hook.rs` subprocess.
//!
//! Folds in the v0.7.1 hotfix scope per
//! `feedback_remote_hook_diagnostic_blind_spot.md`. Before this, the
//! hook subprocess emitted everything to stderr — Claude Code's
//! `Stdio::piped()` ate the buffer and the user/dev had zero forensics
//! when a hook silently failed. The 2026-05-07 VM verify hit that exact
//! blind spot: medium-risk D.1 round-trip showed claude executing the
//! command after 13 s but `remote_permission_requests` had 0 rows for
//! that device — couldn't tell if the hook fail-fast'd, the network
//! dropped, or the helper_secret was rejected.
//!
//! This logger writes to a process-wide file:
//!
//!   Windows: %LOCALAPPDATA%\dev.clipulse.desktop\logs\remote-hook.log
//!   Linux:   ~/.local/share/dev.clipulse.desktop/logs/remote-hook.log
//!   macOS:   ~/Library/Logs/dev.clipulse.desktop/remote-hook.log
//!
//! Both the hook binary AND the agent loop append here. Cross-process
//! `OpenOptions::append(true)` is atomic per write on both platforms
//! (Windows uses `FILE_APPEND_DATA` semantics, POSIX guarantees
//! `O_APPEND` writes <PIPE_BUF). One file makes correlation trivial:
//! grep a session_id and you see both the hook decision flow and the
//! agent dispatch flow interleaved by wall-clock time.
//!
//! Rotation: at 5 MB the next write truncates and starts fresh. We do
//! NOT keep a rolling N-file backup because the hook subprocess can't
//! coordinate rename races without a lock file, and the diagnostic
//! posture says "last few minutes of context is enough; if you need
//! more, you have a different problem." Same threshold as
//! `tauri-plugin-log`'s default.
//!
//! Privacy: the log file is local-only, NEVER uploaded server-side.
//! The hook + agent code redacts before logging where applicable, but
//! the rule of thumb is: don't log full payloads, don't log full
//! cwd paths, don't log full helper_secret. Log enough to debug
//! without enough to leak.

use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

use once_cell::sync::OnceCell;

/// Hard cap on log file size. At this point the next write truncates
/// the file and starts from zero. Matches `tauri-plugin-log`'s default
/// so users don't have to learn two thresholds.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Writer state: single Mutex around the open file handle. The hook
/// subprocess opens this lazily on first log call, the agent opens
/// once during setup. The Mutex serialises writes within a single
/// process; cross-process serialisation is provided by the OS append
/// semantics (write <PIPE_BUF on POSIX, atomic FILE_APPEND_DATA on
/// Windows).
static WRITER: OnceCell<Mutex<File>> = OnceCell::new();

/// Resolve the log file path. Honors the same per-OS convention as
/// `tauri-plugin-log`'s `LogDir` target so users find both files
/// next to each other:
///
/// - Windows: %LOCALAPPDATA%\dev.clipulse.desktop\logs\remote-hook.log
/// - Linux:   ~/.local/share/dev.clipulse.desktop/logs/remote-hook.log
/// - macOS:   ~/Library/Logs/dev.clipulse.desktop/remote-hook.log
///
/// Returns None on truly headless setups where neither `dirs::data_local_dir`
/// nor `dirs::home_dir` resolves — in that case logging is a no-op and
/// the caller's `try_init` returns Err so the agent / hook can fall
/// through to whatever it does without logs.
pub fn log_file_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        // macOS: ~/Library/Logs/dev.clipulse.desktop/
        let home = dirs::home_dir()?;
        Some(
            home.join("Library")
                .join("Logs")
                .join("dev.clipulse.desktop")
                .join("remote-hook.log"),
        )
    } else if cfg!(target_os = "windows") {
        // Windows: %LOCALAPPDATA%\dev.clipulse.desktop\logs\
        let local = dirs::data_local_dir()?;
        Some(
            local
                .join("dev.clipulse.desktop")
                .join("logs")
                .join("remote-hook.log"),
        )
    } else {
        // Linux + other Unix: ~/.local/share/dev.clipulse.desktop/logs/
        let local = dirs::data_local_dir()?;
        Some(
            local
                .join("dev.clipulse.desktop")
                .join("logs")
                .join("remote-hook.log"),
        )
    }
}

/// Initialise the writer. Idempotent: subsequent calls are no-ops if
/// the writer is already set. Returns Err on the rare path where
/// neither the data-local nor home dir resolves — caller should treat
/// logging as best-effort and continue.
pub fn try_init() -> std::io::Result<()> {
    if WRITER.get().is_some() {
        return Ok(());
    }
    let path = match log_file_path() {
        Some(p) => p,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no data-local or home directory resolvable",
            ));
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // v0.8.0 Gemini diff review P1 #2 — `.write(true)` is REQUIRED on
    // Windows so `set_len(0)` (the rotation truncate path in
    // `log_line` below) can actually take effect. Without it,
    // `OpenOptions::append(true)` alone grants only FILE_APPEND_DATA;
    // `File::set_len` calls SetEndOfFile which needs FILE_WRITE_DATA
    // / GENERIC_WRITE. Clippy flags this as "ineffective" because
    // `append(true)` is supposed to imply append-only, but on Windows
    // we genuinely need both bits set; allow the lint with a pin.
    #[allow(clippy::ineffective_open_options)]
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(&path)?;
    let _ = WRITER.set(Mutex::new(file));
    Ok(())
}

/// Append one log line. Format: `<RFC3339-utc> <component> <level> <message>\n`.
///
/// Truncates the file if it has grown past `MAX_LOG_BYTES` since the
/// last write — checked on each call so a long-running agent never
/// exceeds the cap by more than one chunk.
///
/// Best-effort: I/O failures are silently dropped. The whole point of
/// this module is to add visibility, not to introduce a new failure
/// mode that could hang the hook or the agent. The hook binary cannot
/// crash because Claude's PermissionRequest hook would interpret an
/// empty stdout as deny + opaque error.
pub fn log_line(component: &str, level: &str, message: &str) {
    let Some(writer) = WRITER.get() else {
        // try_init never called, or it failed. Drop silently.
        return;
    };
    let Ok(mut guard) = writer.lock() else {
        return;
    };
    // Rotation guard: stat the file via the open handle and truncate
    // if past the cap. A small race with a concurrent process is fine
    // — the file is append-only so the worst case is two processes
    // both deciding to truncate at the same moment, which yields a
    // freshly-truncated file (the desired state). Lost log data in
    // that race is bounded to whatever was within MAX_LOG_BYTES.
    if let Ok(meta) = guard.metadata() {
        if meta.len() > MAX_LOG_BYTES {
            let _ = guard.set_len(0);
            let _ = guard.seek(std::io::SeekFrom::Start(0));
        }
    }
    let line = format!(
        "{ts} {component} {level} {message}\n",
        ts = format_rfc3339(SystemTime::now()),
        component = component,
        level = level,
        message = message,
    );
    let _ = guard.write_all(line.as_bytes());
    let _ = guard.flush();
}

/// Emit an info-level log line.
pub fn info(component: &str, message: &str) {
    log_line(component, "INFO", message);
}

/// Emit a warning-level log line.
pub fn warn(component: &str, message: &str) {
    log_line(component, "WARN", message);
}

/// Emit an error-level log line.
pub fn error(component: &str, message: &str) {
    log_line(component, "ERROR", message);
}

/// Format a SystemTime as a compact RFC3339-ish UTC string. Avoids the
/// chrono dep here so this module compiles in the bin (which deliberately
/// keeps deps narrow). Format is `YYYY-MM-DDTHH:MM:SSZ`.
fn format_rfc3339(t: SystemTime) -> String {
    let dur = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return "1970-01-01T00:00:00Z".to_string(),
    };
    let secs = dur.as_secs();
    // UTC days since 1970-01-01.
    let days = (secs / 86_400) as i64;
    let h = ((secs % 86_400) / 3600) as u32;
    let m = ((secs % 3600) / 60) as u32;
    let s = (secs % 60) as u32;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days-since-1970 to (year, month, day). Pure-Rust calendar
/// math; cycles every 400 years because Gregorian leap rules. Good
/// from year 0 to year 9999; we'll be long retired before that
/// matters.
fn days_to_ymd(mut days: i64) -> (i64, u32, u32) {
    days += 719_468;
    let era = if days >= 0 {
        days / 146_097
    } else {
        (days - 146_096) / 146_097
    };
    let doe = (days - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_format_is_correct_shape() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let s = format_rfc3339(t);
        // 1700000000 = 2023-11-14T22:13:20Z
        assert_eq!(s, "2023-11-14T22:13:20Z");
    }

    #[test]
    fn rfc3339_handles_unix_epoch() {
        let s = format_rfc3339(SystemTime::UNIX_EPOCH);
        assert_eq!(s, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn log_path_resolvable_on_supported_platforms() {
        // We can't assert the exact path (varies per host) but we can
        // assert SOMETHING resolves on the host this test runs on,
        // and that the trailing component is `remote-hook.log`.
        let path = log_file_path().expect("data-local dir should resolve in CI");
        assert_eq!(path.file_name().unwrap(), "remote-hook.log");
    }

    #[test]
    fn try_init_creates_parent_dirs() {
        // Best-effort: try_init might fail in sandbox CI without home
        // dir, but if it succeeds the directory must exist after.
        if try_init().is_ok() {
            let path = log_file_path().unwrap();
            assert!(
                path.parent().unwrap().exists(),
                "parent dir should exist after init"
            );
        }
    }
}
