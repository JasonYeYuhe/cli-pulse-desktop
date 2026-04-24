//! Persistent helper config — stores device_id + helper_secret after
//! successful pairing. Lives next to the OS convention for user config:
//!
//!   macOS:   ~/Library/Application Support/dev.clipulse.desktop/config.json
//!   Linux:   ~/.config/cli-pulse-desktop/config.json   (XDG_CONFIG_HOME)
//!   Windows: %APPDATA%\dev.clipulse.desktop\config.json
//!
//! File is created with mode 0600 on Unix (windows ACLs not adjusted —
//! %APPDATA% is per-user by default). The helper_secret is sensitive
//! (grants write access to the user's devices/sessions/alerts rows via
//! helper_sync), so Sprint 1 keeps it in a mode-0600 plaintext file and
//! Sprint 2 migrates to OS keyring via `tauri-plugin-stronghold`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelperConfig {
    pub device_id: String,
    pub user_id: String,
    pub device_name: String,
    pub helper_version: String,
    pub helper_secret: String,
    /// Budget alert thresholds. `None` on any field = never alert.
    /// Added in v0.1.1 — old config files that don't have this will
    /// default to `AlertThresholds::default()` via serde default.
    #[serde(default)]
    pub thresholds: crate::alerts::AlertThresholds,
}

pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("dev.clipulse.desktop"))
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.json"))
}

pub fn load() -> anyhow::Result<Option<HelperConfig>> {
    let path = match config_path() {
        Some(p) => p,
        None => return Ok(None),
    };
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    let cfg: HelperConfig = serde_json::from_str(&text)?;
    Ok(Some(cfg))
}

pub fn save(cfg: &HelperConfig) -> anyhow::Result<()> {
    let dir = config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    fs::create_dir_all(&dir)?;
    let path = dir.join("config.json");
    let text = serde_json::to_string_pretty(cfg)?;
    fs::write(&path, text)?;
    set_private_mode(&path)?;
    Ok(())
}

pub fn clear() -> anyhow::Result<()> {
    if let Some(path) = config_path() {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_mode(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_mode(_path: &Path) -> anyhow::Result<()> {
    // On Windows, %APPDATA%\dev.clipulse.desktop is per-user by default.
    // Explicit ACL hardening ships in Sprint 2.
    Ok(())
}
