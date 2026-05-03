//! Persistent per-provider credentials storage (v0.4.6).
//!
//! Lives next to the existing `config.json` in the same OS-conventional
//! per-user config directory:
//!   macOS:   ~/Library/Application Support/dev.clipulse.desktop/provider_creds.json
//!   Linux:   ~/.config/cli-pulse-desktop/provider_creds.json   (XDG_CONFIG_HOME)
//!   Windows: %APPDATA%\dev.clipulse.desktop\provider_creds.json
//!
//! Same security model as `config.json` (which holds `helper_secret`):
//! file mode 0600 on Unix, per-user `%APPDATA%` ACL default on Windows.
//! True OS keychain / `tauri-plugin-stronghold` migration tracked for
//! v0.4.7+ — v0.4.6 is mode-0600 plaintext, sufficient for indie commercial
//! app first iteration per Gemini 2026-05-04 review #1 (no-peek SHIP-IT).
//!
//! Atomic write: write to temp file in same dir → set 0600 → rename
//! (atomic on POSIX; Windows replaces existing target with the new file
//! contents in a single ReplaceFile call). The mode is set BEFORE rename
//! so there's no window where the live file has default permissions.
//!
//! Read priority in collectors (cursor / copilot / openrouter):
//!   1. Env var (existing v0.4.5 behavior — backwards compat for power users)
//!   2. provider_creds.json value
//!   3. None → silent skip at debug! log level
//!
//! Schema versioned (`version: 1`) so v0.4.7+ stronghold migration can
//! branch on the version field.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::config::config_dir;

/// On-disk schema. All credential fields are `Option<String>`; absent or
/// empty-string is the same "not configured" semantic. The `version`
/// field future-proofs for v0.4.7+ stronghold migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCreds {
    /// Schema version — currently always 1. v0.4.7+ may branch on this.
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub cursor_cookie: Option<String>,
    #[serde(default)]
    pub copilot_token: Option<String>,
    #[serde(default)]
    pub openrouter_api_key: Option<String>,
    /// Optional override for OpenRouter's `OPENROUTER_API_URL` env. NOT
    /// secret — exposed plaintext in `ProviderCredsView`.
    #[serde(default)]
    pub openrouter_base_url: Option<String>,
}

fn default_version() -> u32 {
    1
}

impl Default for ProviderCreds {
    fn default() -> Self {
        Self {
            version: default_version(),
            cursor_cookie: None,
            copilot_token: None,
            openrouter_api_key: None,
            openrouter_base_url: None,
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("provider_creds.json"))
}

/// Read-side cache. The file gets read every 2-min sync cycle by 3
/// collectors — that's 3 file-opens × 30 cycles/hour. Anti-virus
/// scanning or slow disks would compound. Cache invalidates on
/// `save()` so live edits via the Settings UI take effect immediately.
/// Per Codex 2026-05-04 review concern #3.
static CACHE: Lazy<RwLock<Option<ProviderCreds>>> = Lazy::new(|| RwLock::new(None));

/// Read from disk, returning a default-empty struct if file is absent.
/// Returns `Err` only on actual IO / parse failure (so collectors can
/// distinguish "user has no file" from "file is corrupt"). Uses an
/// in-memory cache invalidated by `save()`.
pub fn load() -> anyhow::Result<ProviderCreds> {
    if let Some(cached) = CACHE.read().ok().and_then(|g| g.clone()) {
        return Ok(cached);
    }
    let creds = load_from_disk()?;
    if let Ok(mut g) = CACHE.write() {
        *g = Some(creds.clone());
    }
    Ok(creds)
}

fn load_from_disk() -> anyhow::Result<ProviderCreds> {
    let path = match config_path() {
        Some(p) => p,
        None => return Ok(ProviderCreds::default()),
    };
    if !path.exists() {
        return Ok(ProviderCreds::default());
    }
    let text = fs::read_to_string(&path)?;
    let creds: ProviderCreds = serde_json::from_str(&text)?;
    Ok(creds)
}

/// Atomic write. Sequence:
///   1. Create temp file in same dir as target (so rename is same-fs).
///   2. Write content.
///   3. Set mode 0600 on temp file (BEFORE rename — no permission window).
///   4. Persist (rename to target). On POSIX rename is atomic; on Windows
///      `tempfile::NamedTempFile::persist` uses ReplaceFile internally.
///   5. Invalidate the in-memory cache.
pub fn save(creds: &ProviderCreds) -> anyhow::Result<()> {
    let dir = config_dir().ok_or_else(|| anyhow::anyhow!("no config dir"))?;
    fs::create_dir_all(&dir)?;
    let target = dir.join("provider_creds.json");

    let tmp = tempfile::Builder::new()
        .prefix(".provider_creds.")
        .suffix(".tmp")
        .tempfile_in(&dir)?;

    let text = serde_json::to_string_pretty(creds)?;
    {
        use std::io::Write;
        let mut f = tmp.as_file();
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }

    set_private_mode(tmp.path())?;

    // Persist (atomic-ish rename). On POSIX this is rename(2) atomic.
    // On Windows tempfile uses ReplaceFile or MoveFileExW. If the target
    // is being read by another process at the exact same moment, Windows
    // may return AccessDenied; we surface that as anyhow::Error and the
    // Tauri command frontend renders a toast.
    tmp.persist(&target)
        .map_err(|e| anyhow::anyhow!("atomic rename: {}", e.error))?;

    // Invalidate cache so the next collector read picks up the new value.
    if let Ok(mut g) = CACHE.write() {
        *g = Some(creds.clone());
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
    // %APPDATA%\dev.clipulse.desktop is per-user by NTFS default. v0.4.7+
    // may add explicit ACL hardening alongside stronghold migration.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_empty() {
        let c = ProviderCreds::default();
        let json = serde_json::to_string(&c).unwrap();
        let back: ProviderCreds = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 1);
        assert!(back.cursor_cookie.is_none());
        assert!(back.copilot_token.is_none());
        assert!(back.openrouter_api_key.is_none());
        assert!(back.openrouter_base_url.is_none());
    }

    #[test]
    fn round_trip_populated() {
        let c = ProviderCreds {
            version: 1,
            cursor_cookie: Some("WorkosCursorSessionToken=abc".into()),
            copilot_token: Some("ghp_test".into()),
            openrouter_api_key: Some("sk-or-v1-test".into()),
            openrouter_base_url: Some("https://custom.example.com".into()),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: ProviderCreds = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.cursor_cookie.as_deref(),
            Some("WorkosCursorSessionToken=abc")
        );
        assert_eq!(back.copilot_token.as_deref(), Some("ghp_test"));
        assert_eq!(back.openrouter_api_key.as_deref(), Some("sk-or-v1-test"));
        assert_eq!(
            back.openrouter_base_url.as_deref(),
            Some("https://custom.example.com")
        );
    }

    #[test]
    fn missing_version_defaults_to_1() {
        // v0.4.5 had no provider_creds.json; if a v0.4.7+ user somehow
        // hand-edits the file and drops the version field, parse must
        // not fail.
        let json = r#"{"cursor_cookie": "x"}"#;
        let c: ProviderCreds = serde_json::from_str(json).unwrap();
        assert_eq!(c.version, 1);
        assert_eq!(c.cursor_cookie.as_deref(), Some("x"));
    }

    #[test]
    fn parse_failure_surfaces_error() {
        // Malformed JSON — load_from_disk returns Err so Tauri command
        // can show "config corrupt" toast rather than silently
        // overwriting with an empty default.
        let result: Result<ProviderCreds, _> = serde_json::from_str(r#"{not json"#);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_fields_ignored() {
        // Forward compat: v0.4.7+ may add new fields. Old v0.4.6 client
        // reading newer file should not fail.
        let json = r#"{"version": 2, "cursor_cookie": "x", "future_field": 42}"#;
        let c: ProviderCreds = serde_json::from_str(json).unwrap();
        assert_eq!(c.version, 2);
        assert_eq!(c.cursor_cookie.as_deref(), Some("x"));
    }

    #[test]
    fn empty_string_credential_treated_as_unset() {
        // We accept empty-string from the UI as a "clear" signal in the
        // Tauri command (set_provider_creds maps "" → None). The
        // disk shape uses Option<String>, so empty-string in JSON becomes
        // Some("") and collectors filter on `!is_empty()`. Verify the
        // Option deserializer doesn't conflate "" with None.
        let json = r#"{"cursor_cookie": ""}"#;
        let c: ProviderCreds = serde_json::from_str(json).unwrap();
        assert_eq!(c.cursor_cookie.as_deref(), Some(""));
    }
}
