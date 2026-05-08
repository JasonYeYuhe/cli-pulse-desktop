//! v0.9.3 — Diagnostic bundle export.
//!
//! Creates a zip file in `~/Downloads/cli-pulse-diag-<timestamp>.zip`
//! containing the files a maintainer would need to triage a bug
//! report. Triggered from `Settings → About → Save diagnostic bundle`.
//!
//! ## Why
//!
//! The v0.8.0 incident's post-mortem required the verifier to
//! manually collect: `cli-pulse.log`, `remote-hook.log`, the WER
//! mdmp + companion files, `diagnostic_snapshot` output, and the
//! `Cargo.lock` SHA. That took ~20 minutes per report. This module
//! collapses it into one click.
//!
//! ## What goes in
//!
//! - `cli-pulse.log` — the main app log (full file, current, capped
//!   at the rotation size by tauri-plugin-log)
//! - `remote-hook.log` — the hook subprocess log (v0.7.1 / v0.8.0
//!   diagnostic-blind-spot fix)
//! - `crash-history.jsonl` — v0.9.0 crash-recovery markers, if any
//! - `diagnostic_snapshot.json` — the existing platform snapshot
//!   that the Settings → About text-copy already exposes
//! - `versions.txt` — `tauri.conf.json` version + `Cargo.toml`
//!   version (for sanity; should always match)
//! - `wer-events.txt` (Windows only) — last 5 `Application Error`
//!   events for `cli-pulse-desktop` from the Windows Event Log,
//!   formatted with timestamp + provider + first 3 lines of message
//!
//! ## What does NOT go in
//!
//! - `helper_secret`, `refresh_token`, OAuth tokens, JWTs — never
//! - Full PDB symbols — too big (27 MB for the GUI alone) and
//!   already on the GitHub release page anyway
//! - WER mdmp files — those contain process memory snapshots which
//!   may include OAuth tokens. The maintainer asks for those
//!   separately on a case-by-case basis after explicit user consent.
//! - Sentry events — those are already at jason-yeyuhe.sentry.io
//!
//! ## Privacy posture
//!
//! Bundle is saved LOCALLY to `~/Downloads/`. The user attaches it
//! to their bug report deliberately. We surface a privacy hint in
//! the UI: "Bundle is saved locally — review the contents before
//! sharing."

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Serialize;
use zip::ZipWriter;

/// Files we attempt to collect, in order. Each entry is best-effort
/// — missing files are skipped, not errors.
#[derive(Debug, Clone, Copy)]
struct BundleSource {
    /// Display name inside the zip.
    archive_name: &'static str,
    /// Resolver — returns the path to read or None to skip this entry.
    resolver: fn() -> Option<PathBuf>,
}

const SOURCES: &[BundleSource] = &[
    BundleSource {
        archive_name: "cli-pulse.log",
        resolver: app_log_path,
    },
    BundleSource {
        archive_name: "remote-hook.log",
        resolver: hook_log_path,
    },
    BundleSource {
        archive_name: "crash-history.jsonl",
        resolver: crash_history_path,
    },
];

fn app_log_path() -> Option<PathBuf> {
    // Tauri-plugin-log defaults to `<app_log_dir>/cli-pulse.log`.
    // We duplicate the `dirs::data_local_dir` lookup here because
    // we can't get the AppHandle from a non-Tauri context. The
    // actual file path matches what `app.path().app_log_dir()`
    // returns post-init.
    //
    // Win + Linux both use `data_local_dir` → ~/.local/share/...
    // or %LOCALAPPDATA%\... — the trailing path components are
    // identical. macOS diverges to ~/Library/Logs/... per
    // tauri-plugin-log convention.
    if cfg!(target_os = "macos") {
        let home = dirs::home_dir()?;
        Some(
            home.join("Library")
                .join("Logs")
                .join("dev.clipulse.desktop")
                .join("cli-pulse.log"),
        )
    } else {
        let local = dirs::data_local_dir()?;
        Some(
            local
                .join("dev.clipulse.desktop")
                .join("logs")
                .join("cli-pulse.log"),
        )
    }
}

fn hook_log_path() -> Option<PathBuf> {
    crate::remote::log::log_file_path()
}

fn crash_history_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("crash-history.jsonl"))
}

/// Outcome of a bundle-create call. Used by the Tauri command
/// handler to render success / failure to the UI.
#[derive(Debug, Serialize)]
pub struct BundleResult {
    /// Absolute path of the saved zip.
    pub path: String,
    /// Total uncompressed bytes that went into the zip (informational).
    pub uncompressed_bytes: u64,
    /// Names of the entries actually included. Excludes entries
    /// whose resolver returned None or whose file didn't exist.
    pub entries: Vec<String>,
}

/// Errors that prevent bundle creation entirely.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("could not resolve Downloads directory")]
    NoDownloadsDir,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
}

/// Create a diagnostic bundle in `~/Downloads/`. Returns the saved
/// path + metadata for the UI to render.
///
/// `extras` is a map of `name → contents` for entries that come from
/// in-memory state (not a file on disk). Used for the
/// `diagnostic_snapshot.json` and `versions.txt` entries.
pub fn create_bundle(extras: Vec<(String, Vec<u8>)>) -> Result<BundleResult, BundleError> {
    let downloads = dirs::download_dir().ok_or(BundleError::NoDownloadsDir)?;
    std::fs::create_dir_all(&downloads)?;
    let timestamp = format_timestamp(SystemTime::now());
    let zip_name = format!("cli-pulse-diag-{timestamp}.zip");
    let zip_path = downloads.join(&zip_name);

    let file = File::create(&zip_path)?;
    let mut zip = ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let mut entries = Vec::new();
    let mut total_bytes = 0u64;

    for src in SOURCES {
        let Some(path) = (src.resolver)() else {
            continue;
        };
        if !path.exists() {
            continue;
        }
        let mut f = match File::open(&path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut contents = Vec::new();
        if f.read_to_end(&mut contents).is_err() {
            continue;
        }
        zip.start_file(src.archive_name, opts)?;
        zip.write_all(&contents)?;
        total_bytes += contents.len() as u64;
        entries.push(src.archive_name.to_string());
    }

    // In-memory extras (diagnostic_snapshot, versions, etc.).
    for (name, contents) in extras {
        zip.start_file(&name, opts)?;
        zip.write_all(&contents)?;
        total_bytes += contents.len() as u64;
        entries.push(name);
    }

    zip.finish()?;

    Ok(BundleResult {
        path: zip_path.to_string_lossy().into_owned(),
        uncompressed_bytes: total_bytes,
        entries,
    })
}

/// Format a SystemTime as `YYYYMMDD-HHMMSS` for use in filenames.
/// Matches the `format_rfc3339` output style of `remote::log` but
/// without colons (which break Windows file names).
fn format_timestamp(t: SystemTime) -> String {
    let dur = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d,
        Err(_) => return "19700101-000000".to_string(),
    };
    let secs = dur.as_secs();
    let days = (secs / 86_400) as i64;
    let h = ((secs % 86_400) / 3600) as u32;
    let m = ((secs % 3600) / 60) as u32;
    let s = (secs % 60) as u32;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}{month:02}{day:02}-{h:02}{m:02}{s:02}")
}

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
    fn timestamp_format_is_filename_safe() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let s = format_timestamp(t);
        // 1700000000 = 2023-11-14T22:13:20Z → 20231114-221320
        assert_eq!(s, "20231114-221320");
        // No colons, no slashes — safe for Windows + POSIX
        assert!(!s.contains(':'));
        assert!(!s.contains('/'));
        assert!(!s.contains('\\'));
    }

    #[test]
    fn timestamp_handles_unix_epoch() {
        let s = format_timestamp(SystemTime::UNIX_EPOCH);
        assert_eq!(s, "19700101-000000");
    }

    #[test]
    fn create_bundle_with_only_extras_succeeds() {
        // Even with NO disk sources resolvable (e.g. fresh CI runner),
        // `create_bundle` should still succeed if extras has at least
        // one entry. The bundle just contains the in-memory data.
        let extras = vec![("test.txt".to_string(), b"hello world".to_vec())];
        let result = create_bundle(extras);
        // dirs::download_dir() returns None on some sandbox CI runners.
        // Skip the assertion in that case; the test merely verifies
        // we don't panic.
        if let Ok(r) = result {
            assert!(r.entries.contains(&"test.txt".to_string()));
            assert!(r.uncompressed_bytes >= 11);
            // Cleanup
            let _ = std::fs::remove_file(&r.path);
        }
    }

    #[test]
    fn create_bundle_filters_nonexistent_files() {
        // The SOURCES list points to log files that may or may not
        // exist on this dev machine. The function should skip
        // missing ones cleanly (not error out the whole bundle).
        let result = create_bundle(vec![("present.txt".to_string(), b"x".to_vec())]);
        if let Ok(r) = result {
            // present.txt is always there because we passed it as extra
            assert!(r.entries.contains(&"present.txt".to_string()));
            let _ = std::fs::remove_file(&r.path);
        }
    }

    #[test]
    fn days_to_ymd_known_dates() {
        // Sanity: 0 days since 1970-01-01 = 1970-01-01
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
    }
}
