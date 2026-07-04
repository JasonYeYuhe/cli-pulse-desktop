//! v0.11.0 — headless launch-smoke support.
//!
//! The CI "GUI launch-smoke" job (see `.github/workflows/ci.yml` +
//! `scripts/ci-smoke-launch.ps1`) launches the packaged **release**
//! binary with `CLI_PULSE_SMOKE_MARKER=<path>` set, waits for that file
//! to appear, then asserts the process is still alive and a top-level
//! "CLI Pulse" window exists. This module writes that marker.
//!
//! Design: the marker write is gated ENTIRELY on the env var. When it is
//! unset (every production launch) `write_ready_marker` is a no-op — no
//! file is created, no path is touched. This keeps the smoke hook
//! zero-impact for real users while giving CI a deterministic
//! "frontend actually mounted" signal that catches the two historical
//! launch-incident classes:
//!   * **crash-on-launch** (v0.8.0 BEX64 / `STATUS_STACK_BUFFER_OVERRUN`)
//!     — the process dies before the frontend can invoke the command, so
//!     the marker never appears and the CI job fails on process-death;
//!   * **white-screen** (v0.2.11) — React never mounts, the on-mount
//!     effect never fires, so the marker never appears and the CI job
//!     fails even though the process is technically alive.
//!
//! The wrong-binary class (v0.2.10 `default-run`) is covered separately
//! by the bundle-content guard in `release.yml`.

use std::ffi::OsString;
use std::io;

/// Env var the CI launch-smoke job sets to the marker file path.
pub const SMOKE_MARKER_ENV: &str = "CLI_PULSE_SMOKE_MARKER";

/// Write the launch-smoke "frontend-ready" marker IF (and only if) the
/// `CLI_PULSE_SMOKE_MARKER` env var names a non-empty path.
///
/// Returns `Ok(true)` when a marker was written, `Ok(false)` when the
/// env var is unset/empty (the production no-op path).
pub fn write_ready_marker() -> io::Result<bool> {
    write_ready_marker_to(std::env::var_os(SMOKE_MARKER_ENV))
}

/// Pure core, split out so tests exercise the gating + write without
/// mutating the process-global environment (parallel tests + the repo's
/// "never depend on ambient process state" discipline).
fn write_ready_marker_to(target: Option<OsString>) -> io::Result<bool> {
    match target {
        Some(path) if !path.is_empty() => {
            // Content is informational only — the CI job checks existence,
            // not the bytes. Stamp the version so a stale marker from a
            // previous run is visibly distinguishable in the artifact.
            std::fs::write(
                &path,
                format!("frontend-ready v{}\n", env!("CARGO_PKG_VERSION")),
            )?;
            Ok(true)
        }
        // Unset OR explicitly empty → production no-op.
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each test uses a DISTINCT filename under the temp dir so parallel
    /// test execution never collides — and no global env is touched.
    fn temp_marker(unique: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("clipulse_smoke_test_{unique}.marker"))
    }

    #[test]
    fn none_is_noop_no_file() {
        assert!(!write_ready_marker_to(None).unwrap());
    }

    #[test]
    fn empty_path_is_noop() {
        assert!(!write_ready_marker_to(Some(OsString::from(""))).unwrap());
    }

    #[test]
    fn some_path_writes_marker_with_version() {
        let path = temp_marker("writes");
        let _ = std::fs::remove_file(&path);
        let wrote = write_ready_marker_to(Some(path.clone().into_os_string())).unwrap();
        assert!(wrote, "should report a marker was written");
        let body = std::fs::read_to_string(&path).expect("marker file should exist");
        assert!(
            body.starts_with("frontend-ready v"),
            "marker body should be stamped: {body:?}"
        );
        assert!(
            body.contains(env!("CARGO_PKG_VERSION")),
            "marker should carry the crate version"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn overwrites_existing_marker() {
        let path = temp_marker("overwrite");
        std::fs::write(&path, "STALE").unwrap();
        let wrote = write_ready_marker_to(Some(path.clone().into_os_string())).unwrap();
        assert!(wrote);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("STALE"), "must overwrite, not append");
        let _ = std::fs::remove_file(&path);
    }
}
