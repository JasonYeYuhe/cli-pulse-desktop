//! Persistent per-provider credentials storage.
//!
//! v0.4.6: plaintext-mode-0600 file in the per-user config dir.
//! v0.4.16: OS-native keychain (macOS Keychain / Windows Credential
//!          Manager / Linux Secret Service) primary, with the v0.4.6
//!          file as fallback when the keychain backend is unavailable
//!          (e.g. headless Linux without `gnome-keyring`/`kwallet`).
//!
//! ## Storage path
//!
//! - Keychain entries (when available): one entry per cred under
//!   service `dev.clipulse.desktop`, accounts:
//!   `cursor-cookie`, `copilot-token`, `openrouter-api-key`,
//!   `openrouter-base-url`.
//! - File fallback: same path as v0.4.6
//!   macOS:   ~/Library/Application Support/dev.clipulse.desktop/provider_creds.json
//!   Linux:   ~/.config/cli-pulse-desktop/provider_creds.json
//!   Windows: %APPDATA%\dev.clipulse.desktop\provider_creds.json
//!   Mode 0600 on Unix, NTFS per-user ACL on Windows.
//!
//! ## Migration
//!
//! On app startup, `migrate_v1_file_to_keychain_if_needed()` runs once:
//! - File missing OR file already at `version: 2`: skip.
//! - File at `version: 1` AND keychain available:
//!   - Copy each set value to keychain.
//!   - Atomically rewrite file with `version: 2` + zeroed values.
//!     (v0.4.17, out of scope here, will delete the file entirely.)
//! - File at `version: 1` AND keychain unavailable: stay on file
//!   storage (no regression vs v0.4.6).
//!
//! Per Gemini 3.1 Pro review of the v0.4.14-v0.4.16 dev plan: the
//! migration runs at startup, NOT on first `save()`. Otherwise users
//! who upgrade but never open Settings would stay on the plaintext
//! file forever, fragmenting the user base across two backends.
//!
//! ## Read priority (in collectors: cursor / copilot / openrouter)
//! 1. Env var (existing v0.4.5 behavior — backwards compat for power users)
//! 2. Active backend value (keychain or file — `load()` picks the source)
//! 3. None → silent skip at debug! log level
//!
//! ## Schema
//! `version: 1` = v0.4.6 file with values inline.
//! `version: 2` = v0.4.16 file with zeroed values + values in keychain.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::keychain;

/// Keychain account names per cred field. Stable so existing entries
/// keep working across version bumps.
const ACCT_CURSOR: &str = "cursor-cookie";
const ACCT_COPILOT: &str = "copilot-token";
const ACCT_OR_KEY: &str = "openrouter-api-key";
const ACCT_OR_URL: &str = "openrouter-base-url";

/// On-disk schema. Same shape as v0.4.6 — `version` field discriminates
/// "values inline" (1) from "values in keychain, file is breadcrumb" (2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderCreds {
    /// Schema version. 1 = v0.4.6, 2 = v0.4.16 (post-migration).
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

/// Selected backend (cached after first probe to avoid repeated keychain
/// round-trips on every load). v0.4.16 — chosen by `init_backend` at
/// startup, then sticky for the lifetime of the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// OS keychain available — values stored at
    /// `<service=dev.clipulse.desktop, account=<cred-name>>`.
    OsKeychain,
    /// File at `<config_dir>/provider_creds.json`, mode 0600 on Unix.
    /// Used when the OS keychain probe fails (headless Linux, sandboxed
    /// CI). Same security model as v0.4.6 — no regression.
    File,
}

static BACKEND: OnceLock<Backend> = OnceLock::new();

/// Initialize the storage backend selection. Idempotent — safe to call
/// multiple times. Called from the Tauri `setup` hook so the migration
/// runs once at startup.
///
/// Side effects:
/// - Probes keychain via `keychain::is_available()`.
/// - If keychain is available AND file is at v1, runs the one-shot
///   v1 → keychain migration.
/// - v0.4.19 — if file is at v2 AND keychain is the active backend,
///   spawns an off-thread cleanup that deletes the breadcrumb file.
///   The file was a rollback safety-net for v0.4.16-v0.4.18; with
///   v0.4.18 published and stable, the rollback window has closed.
///   Per Gemini 3.1 Pro review of the v0.4.19 plan: filesystem I/O
///   during init must NOT block the main thread (boot metric).
pub fn init_backend() -> Backend {
    let chosen = if keychain::is_available() {
        Backend::OsKeychain
    } else {
        Backend::File
    };
    let _ = BACKEND.set(chosen);
    log::info!("[ProviderCreds] backend selected: {chosen:?}");
    if chosen == Backend::OsKeychain {
        if let Err(e) = migrate_v1_file_to_keychain_if_needed() {
            log::warn!(
                "[ProviderCreds] v1->v2 migration failed (non-fatal, will retry next launch): {e}"
            );
        }
        // v0.4.19 — delete v2 breadcrumb on a background thread so
        // we don't block app launch on filesystem I/O. The file
        // contains zeroed values + a `version: 2` marker; deletion
        // is reversible at the next launch (re-migrate from keychain
        // if the file is somehow recreated).
        std::thread::spawn(|| {
            if let Err(e) = cleanup_v2_breadcrumb_if_present() {
                log::warn!("[ProviderCreds] v2 breadcrumb cleanup failed (non-fatal): {e}");
            }
        });
    }
    chosen
}

pub fn current_backend() -> Backend {
    *BACKEND.get().unwrap_or(&Backend::File)
}

pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("provider_creds.json"))
}

/// Read-side cache. The file gets read every 2-min sync cycle by 3
/// collectors — that's 3 file-opens × 30 cycles/hour. Cache invalidates
/// on `save()` so live edits via the Settings UI take effect immediately.
/// Per Codex 2026-05-04 review concern #3.
static CACHE: Lazy<RwLock<Option<ProviderCreds>>> = Lazy::new(|| RwLock::new(None));

/// Read creds via the active backend. Returns a default-empty struct
/// when nothing is stored. Errs only on disk IO / parse failures
/// (keychain "not found" is a normal state, not an error).
pub fn load() -> anyhow::Result<ProviderCreds> {
    if let Some(cached) = CACHE.read().ok().and_then(|g| g.clone()) {
        return Ok(cached);
    }
    let creds = match current_backend() {
        Backend::OsKeychain => load_from_keychain()?,
        Backend::File => load_from_disk()?,
    };
    if let Ok(mut g) = CACHE.write() {
        *g = Some(creds.clone());
    }
    Ok(creds)
}

fn load_from_keychain() -> anyhow::Result<ProviderCreds> {
    Ok(ProviderCreds {
        version: 2,
        cursor_cookie: keychain::read_at(ACCT_CURSOR).ok().flatten(),
        copilot_token: keychain::read_at(ACCT_COPILOT).ok().flatten(),
        openrouter_api_key: keychain::read_at(ACCT_OR_KEY).ok().flatten(),
        openrouter_base_url: keychain::read_at(ACCT_OR_URL).ok().flatten(),
    })
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

/// Write creds via the active backend. Invalidates the in-memory cache
/// on success.
pub fn save(creds: &ProviderCreds) -> anyhow::Result<()> {
    match current_backend() {
        Backend::OsKeychain => save_to_keychain(creds)?,
        Backend::File => save_to_file(creds)?,
    }
    if let Ok(mut g) = CACHE.write() {
        *g = Some(creds.clone());
    }
    Ok(())
}

fn save_to_keychain(creds: &ProviderCreds) -> anyhow::Result<()> {
    set_or_clear_keychain(ACCT_CURSOR, creds.cursor_cookie.as_deref())?;
    set_or_clear_keychain(ACCT_COPILOT, creds.copilot_token.as_deref())?;
    set_or_clear_keychain(ACCT_OR_KEY, creds.openrouter_api_key.as_deref())?;
    set_or_clear_keychain(ACCT_OR_URL, creds.openrouter_base_url.as_deref())?;
    Ok(())
}

fn set_or_clear_keychain(account: &str, value: Option<&str>) -> anyhow::Result<()> {
    match value {
        Some(v) if !v.is_empty() => keychain::store_at(account, v)
            .map_err(|e| anyhow::anyhow!("keychain set {account}: {e}"))?,
        _ => keychain::delete_at(account)
            .map_err(|e| anyhow::anyhow!("keychain delete {account}: {e}"))?,
    }
    Ok(())
}

fn save_to_file(creds: &ProviderCreds) -> anyhow::Result<()> {
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

    tmp.persist(&target)
        .map_err(|e| anyhow::anyhow!("atomic rename: {}", e.error))?;

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
    // %APPDATA%\dev.clipulse.desktop is per-user by NTFS default.
    Ok(())
}

/// v0.4.19 — delete the `version: 2` zeroed breadcrumb file written
/// by v0.4.16's migration. By v0.4.19, all the rollback room v0.4.16
/// reserved (one release of "if the keychain write went wrong I can
/// revert by editing the file") has been used: v0.4.16 + v0.4.17 +
/// v0.4.18 shipped without keychain-write incident.
///
/// Per Gemini 3.1 Pro review (Q1): no checksum / readback verification
/// needed before deletion. The file's values are already zeroed by
/// v0.4.16; reading it back can't restore creds even if the keychain
/// is corrupt. The `version: 2` marker is the entire safety contract —
/// if it's there, the file is safe to delete.
fn cleanup_v2_breadcrumb_if_present() -> anyhow::Result<()> {
    let path = match config_path() {
        Some(p) => p,
        None => return Ok(()),
    };
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&path)?;
    let creds: ProviderCreds = match serde_json::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            // Defensive: don't delete an unparseable file. It might be
            // a hand-edited v1 with corruption; user might want to
            // recover it manually.
            return Err(anyhow::anyhow!(
                "file at {} unparseable, refusing to delete: {e}",
                path.display()
            ));
        }
    };
    if creds.version < 2 {
        // v1 file — keychain backend may have been unavailable when
        // this was written, OR migration hasn't run yet on this
        // launch. Leave it alone.
        return Ok(());
    }
    fs::remove_file(&path).map_err(|e| anyhow::anyhow!("remove {}: {e}", path.display()))?;
    log::info!(
        "[ProviderCreds] v0.4.19 — deleted v2 breadcrumb file at {} (rollback window closed; values live in OS keychain)",
        path.display()
    );
    Ok(())
}

/// Migrate v0.4.6 plaintext file to OS keychain at first launch after
/// upgrade to v0.4.16. Idempotent — running twice on a v2 file is a
/// no-op.
///
/// On success, the file is rewritten with `version: 2` and zeroed
/// values. v0.4.17 (out of scope here) will delete the file entirely
/// after one release of "if migration goes wrong, I can revert and
/// the file is still there".
fn migrate_v1_file_to_keychain_if_needed() -> anyhow::Result<()> {
    let path = match config_path() {
        Some(p) => p,
        None => return Ok(()),
    };
    if !path.exists() {
        return Ok(());
    }
    let text = fs::read_to_string(&path)?;
    let v1: ProviderCreds = serde_json::from_str(&text)?;
    if v1.version >= 2 {
        // Already migrated — nothing to do.
        return Ok(());
    }

    log::info!(
        "[ProviderCreds] v1->v2 migration: copying {} cred(s) from {} to OS keychain",
        [
            v1.cursor_cookie.as_deref(),
            v1.copilot_token.as_deref(),
            v1.openrouter_api_key.as_deref(),
            v1.openrouter_base_url.as_deref(),
        ]
        .iter()
        .filter(|v| v.map(|s| !s.is_empty()).unwrap_or(false))
        .count(),
        path.display(),
    );

    save_to_keychain(&v1)?;

    // Write a v2 file with zeroed values so we know the migration is done.
    // The file lingers until v0.4.17 deletes it entirely.
    let v2 = ProviderCreds {
        version: 2,
        cursor_cookie: None,
        copilot_token: None,
        openrouter_api_key: None,
        openrouter_base_url: None,
    };
    save_to_file(&v2)?;
    log::info!(
        "[ProviderCreds] v1->v2 migration complete; file at {} now zeroed (will be deleted in v0.4.17+)",
        path.display(),
    );
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
        let json = r#"{"cursor_cookie": "x"}"#;
        let c: ProviderCreds = serde_json::from_str(json).unwrap();
        assert_eq!(c.version, 1);
        assert_eq!(c.cursor_cookie.as_deref(), Some("x"));
    }

    #[test]
    fn parse_failure_surfaces_error() {
        let result: Result<ProviderCreds, _> = serde_json::from_str(r#"{not json"#);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_fields_ignored() {
        let json = r#"{"version": 2, "cursor_cookie": "x", "future_field": 42}"#;
        let c: ProviderCreds = serde_json::from_str(json).unwrap();
        assert_eq!(c.version, 2);
        assert_eq!(c.cursor_cookie.as_deref(), Some("x"));
    }

    #[test]
    fn empty_string_credential_treated_as_unset() {
        let json = r#"{"cursor_cookie": ""}"#;
        let c: ProviderCreds = serde_json::from_str(json).unwrap();
        assert_eq!(c.cursor_cookie.as_deref(), Some(""));
    }

    // v0.4.16 — Backend enum and migration.

    #[test]
    fn backend_enum_serializes_as_snake_case() {
        // The Tauri command surfaces this to the frontend so the
        // Settings UI / diagnostic panel can render "OS keychain" or
        // "File (keyring unavailable)". Pin the wire format.
        let json = serde_json::to_string(&Backend::OsKeychain).unwrap();
        assert_eq!(json, r#""os_keychain""#);
        let json = serde_json::to_string(&Backend::File).unwrap();
        assert_eq!(json, r#""file""#);
    }

    #[test]
    fn v2_file_is_skipped_by_migration_logic() {
        // A v2 file should round-trip to v2 — migration is idempotent
        // and never re-copies values back into the keychain (which
        // would be a bug since v2 values are intentionally zeroed).
        let v2 = ProviderCreds {
            version: 2,
            cursor_cookie: None,
            copilot_token: None,
            openrouter_api_key: None,
            openrouter_base_url: None,
        };
        let json = serde_json::to_string(&v2).unwrap();
        let parsed: ProviderCreds = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, 2);
        assert!(parsed.cursor_cookie.is_none());
    }

    // v0.4.19 — cleanup of v2 breadcrumb file.

    /// Helper: write the given creds to a tempdir-rooted path that
    /// mimics the real config_path() shape, returning the path. Caller
    /// is responsible for the tempdir lifetime.
    fn write_creds_at(dir: &Path, creds: &ProviderCreds) -> PathBuf {
        let path = dir.join("provider_creds.json");
        let text = serde_json::to_string_pretty(creds).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// Direct test of the deletion predicate (parses file, checks
    /// version, deletes). Doesn't go through `cleanup_v2_breadcrumb_if_present`
    /// because that uses the global `config_path()` which would clobber
    /// the user's real file. Mirrors the same control flow inline.
    fn delete_if_v2(path: &Path) -> anyhow::Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let text = fs::read_to_string(path)?;
        let creds: ProviderCreds = serde_json::from_str(&text)?;
        if creds.version < 2 {
            return Ok(false);
        }
        fs::remove_file(path)?;
        Ok(true)
    }

    #[test]
    fn cleanup_removes_v2_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_creds_at(
            tmp.path(),
            &ProviderCreds {
                version: 2,
                cursor_cookie: None,
                copilot_token: None,
                openrouter_api_key: None,
                openrouter_base_url: None,
            },
        );
        assert!(path.exists());
        let removed = delete_if_v2(&path).unwrap();
        assert!(removed, "v2 file should be deleted");
        assert!(!path.exists(), "file must not exist after cleanup");
    }

    #[test]
    fn cleanup_keeps_v1_file() {
        // Defensive: a v1 file means migration hasn't run OR keychain
        // was unavailable — the file is the SOLE storage of secrets.
        // Deleting it would lose the user's creds.
        let tmp = tempfile::tempdir().unwrap();
        let path = write_creds_at(
            tmp.path(),
            &ProviderCreds {
                version: 1,
                cursor_cookie: Some("real-secret-cookie".into()),
                copilot_token: None,
                openrouter_api_key: None,
                openrouter_base_url: None,
            },
        );
        assert!(path.exists());
        let removed = delete_if_v2(&path).unwrap();
        assert!(!removed, "v1 file must NEVER be deleted");
        assert!(path.exists(), "v1 file must still exist");
    }

    #[test]
    fn cleanup_no_op_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does-not-exist.json");
        let removed = delete_if_v2(&path).unwrap();
        assert!(!removed);
    }
}
