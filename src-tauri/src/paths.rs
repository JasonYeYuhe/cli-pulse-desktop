//! Cross-platform resolution of Claude and Codex log roots.
//!
//! On macOS/Linux: `$HOME/.claude/projects`, `$HOME/.config/claude/projects`,
//!                 `$HOME/.codex/sessions`, `$HOME/.codex/archived_sessions`.
//! On Windows: `%USERPROFILE%\.claude\projects` etc. — dirs::home_dir()
//! already handles this.
//!
//! Honors `CODEX_HOME` env var (set by the Codex CLI to override).

use std::env;
use std::path::PathBuf;

pub fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

pub fn claude_projects_roots() -> Vec<PathBuf> {
    let home = match home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };
    vec![
        home.join(".config").join("claude").join("projects"),
        home.join(".claude").join("projects"),
    ]
}

pub fn codex_sessions_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(codex_home) = env::var("CODEX_HOME") {
        let codex_home = codex_home.trim();
        if !codex_home.is_empty() {
            let base = PathBuf::from(codex_home);
            roots.push(base.join("sessions"));
            roots.push(base.join("archived_sessions"));
            return roots;
        }
    }
    if let Some(home) = home_dir() {
        roots.push(home.join(".codex").join("sessions"));
        roots.push(home.join(".codex").join("archived_sessions"));
    }
    roots
}
