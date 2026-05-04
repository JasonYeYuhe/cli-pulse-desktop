//! OS-native credential store wrapper.
//!
//! Service: `dev.clipulse.desktop`
//! Accounts (one entry per account name):
//!   - `supabase-refresh-token` — v0.3.0 OTP refresh token
//!   - `cursor-cookie`, `copilot-token`, `openrouter-api-key`,
//!     `openrouter-base-url` — v0.4.16 provider creds (migrated from
//!     the v0.4.6 plaintext provider_creds.json)
//!
//! Backends (selected at compile time via `keyring` features):
//!   - macOS  → Keychain (`apple-native`)
//!   - Windows → Credential Manager (`windows-native`)
//!   - Linux  → Secret Service via libsecret (`linux-native-sync-persistent`)
//!
//! **Fail-closed on Linux without Secret Service for OTP.** Codex review
//! of the v0.3.0 plan correctly flagged that machine-id-derived
//! "encryption" is not security — `/etc/machine-id` is an identifier,
//! not a secret. So for the OTP refresh token the keychain is the only
//! storage path. v0.4.16 provider creds DO have a file fallback (plaintext
//! mode-0600), preserving v0.4.6 behavior on platforms where keychain
//! isn't available — see `provider_creds.rs`.
//!
//! `.deb` / `.rpm` package metadata declares `libsecret-1-0` as a runtime
//! dependency so default installs already have it; the failure only
//! fires on minimal headless Linux without a desktop environment.

use keyring::Entry;
use thiserror::Error;

const SERVICE: &str = "dev.clipulse.desktop";
const ACCOUNT_REFRESH_TOKEN: &str = "supabase-refresh-token";

#[derive(Debug, Error)]
pub enum KeychainError {
    /// Backend missing (e.g. Linux without libsecret), or platform
    /// returned a "no backend available" error.
    #[error("OS keychain not available on this platform")]
    NotAvailable,

    /// Generic error from the underlying `keyring` crate. Message is
    /// already user-readable (the crate provides decent context).
    #[error("keychain error: {0}")]
    Backend(String),
}

fn entry_for(account: &str) -> Result<Entry, KeychainError> {
    Entry::new(SERVICE, account).map_err(map_err)
}

/// Translate a `keyring::Error` into our typed error. The crate
/// surfaces backend-missing situations through `PlatformFailure` with
/// an underlying error; we treat anything that smells like
/// "no backend / no secret service" as `NotAvailable` so the UI
/// can show the install hint.
fn map_err(e: keyring::Error) -> KeychainError {
    use keyring::Error as KE;
    match e {
        KE::PlatformFailure(inner) => {
            let msg = inner.to_string();
            // libsecret / dbus-secret-service surfaces "could not connect
            // to dbus" or "secret service is not available" when the
            // backend is missing. Heuristic match — broad enough to
            // catch all relevant cases without false-positiving on
            // genuine transient platform errors.
            let lower = msg.to_lowercase();
            if lower.contains("dbus")
                || lower.contains("secret service")
                || lower.contains("not available")
            {
                KeychainError::NotAvailable
            } else {
                KeychainError::Backend(msg)
            }
        }
        KE::NoStorageAccess(inner) => KeychainError::Backend(inner.to_string()),
        KE::Invalid(field, msg) => KeychainError::Backend(format!("invalid {field}: {msg}")),
        other => KeychainError::Backend(other.to_string()),
    }
}

// ---- Generic API (used by provider_creds.rs since v0.4.16) ----

/// Set the value at `<service=dev.clipulse.desktop, account=<name>>`.
/// Returns `Err(NotAvailable)` if the backend is missing — caller
/// decides whether to fail-closed (OTP) or fall back to file
/// (provider creds, v0.4.6 behavior).
pub fn store_at(account: &str, value: &str) -> Result<(), KeychainError> {
    entry_for(account)?.set_password(value).map_err(map_err)
}

/// Read the value at the given account. `Ok(None)` for "entry doesn't
/// exist" (a normal state for never-set creds), `Err(NotAvailable)`
/// for "OS keychain backend missing".
pub fn read_at(account: &str) -> Result<Option<String>, KeychainError> {
    match entry_for(account)?.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err(e)),
    }
}

/// Delete the entry at `account`. A missing entry is treated as success
/// (the goal "no value in keychain" is already achieved).
pub fn delete_at(account: &str) -> Result<(), KeychainError> {
    match entry_for(account)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_err(e)),
    }
}

/// Probe whether the keychain backend is available on this host.
/// Cheap operation — creates an entry handle and immediately calls
/// `get_password` against a probe account, treating `NoEntry` (the
/// expected response) as "available" and `PlatformFailure / DBus not
/// running` etc. as "unavailable". Used by `provider_creds.rs` at
/// startup to decide whether to migrate from file → keychain.
pub fn is_available() -> bool {
    read_at("__probe__").is_ok()
}

// ---- v0.3.0 OTP refresh-token wrappers (kept for backward compat) ----

pub fn store_refresh_token(token: &str) -> Result<(), KeychainError> {
    store_at(ACCOUNT_REFRESH_TOKEN, token)
}

pub fn read_refresh_token() -> Result<Option<String>, KeychainError> {
    read_at(ACCOUNT_REFRESH_TOKEN)
}

pub fn delete_refresh_token() -> Result<(), KeychainError> {
    delete_at(ACCOUNT_REFRESH_TOKEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip on a real keychain. Skipped on CI where the test
    /// runner has no keychain access; the `set/get/delete` cycle is
    /// idempotent so it's safe to run on a developer machine.
    #[test]
    #[ignore = "needs OS keychain access; run manually with `cargo test -- --ignored`"]
    fn round_trip_real_keychain() {
        // Use a unique sentinel value so we don't clobber a real token.
        let probe = format!(
            "test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );

        // Make sure the slot starts empty (or pre-existing).
        let pre = read_refresh_token().expect("read should not error");

        store_refresh_token(&probe).expect("store");
        let got = read_refresh_token().expect("read").expect("some");
        assert_eq!(got, probe);

        delete_refresh_token().expect("delete");
        let after = read_refresh_token().expect("read after delete");
        assert_eq!(after, None);

        // Restore any pre-existing token so we don't break a real session.
        if let Some(restored) = pre {
            store_refresh_token(&restored).expect("restore");
        }
    }
}
