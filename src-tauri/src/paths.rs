//! Cross-platform resolution of Claude and Codex log roots.
//!
//! On macOS/Linux: `$HOME/.claude/projects`, `$HOME/.config/claude/projects`,
//!                 `$HOME/.codex/sessions`, `$HOME/.codex/archived_sessions`.
//! On Windows: `%USERPROFILE%\.claude\projects` etc. — dirs::home_dir()
//! already handles this.
//!
//! On Windows we ALSO scan the home dirs of running WSL distros (via
//! [`crate::wsl`]), so usage from Claude Code / Codex run *inside* WSL merges
//! into the same totals. No-op on macOS/Linux and on Windows without a running
//! distro.
//!
//! Honors `CODEX_HOME` env var (set by the Codex CLI to override).

use std::env;
use std::path::{Path, PathBuf};

use crate::wsl;

pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

/// The `.claude` log roots under a given home directory (native or a WSL home).
fn claude_roots_for_home(home: &Path) -> [PathBuf; 2] {
    [
        home.join(".config").join("claude").join("projects"),
        home.join(".claude").join("projects"),
    ]
}

pub fn claude_projects_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.extend(claude_roots_for_home(&home));
    }
    // Append the same roots inside each running WSL distro's home(s).
    for wsl_home in wsl::wsl_home_roots() {
        roots.extend(claude_roots_for_home(&wsl_home));
    }
    roots
}

pub fn codex_sessions_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    // A `CODEX_HOME` override replaces only the NATIVE codex root; WSL distros
    // still use their own `~/.codex`, so they are appended regardless below.
    if let Ok(codex_home) = env::var("CODEX_HOME") {
        let codex_home = codex_home.trim();
        if !codex_home.is_empty() {
            let base = PathBuf::from(codex_home);
            roots.push(base.join("sessions"));
            roots.push(base.join("archived_sessions"));
        }
    }
    if roots.is_empty() {
        if let Some(home) = home_dir() {
            roots.push(home.join(".codex").join("sessions"));
            roots.push(home.join(".codex").join("archived_sessions"));
        }
    }
    for wsl_home in wsl::wsl_home_roots() {
        roots.push(wsl_home.join(".codex").join("sessions"));
        roots.push(wsl_home.join(".codex").join("archived_sessions"));
    }
    roots
}
