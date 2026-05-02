//! OS-native credential store wrapper for the v0.3.0 OTP refresh token.
//!
//! Service: `dev.clipulse.desktop`
//! Account: `supabase-refresh-token`
//!
//! Backends (selected at compile time via `keyring` features):
//!   - macOS  → Keychain (`apple-native`)
//!   - Windows → Credential Manager (`windows-native`)
//!   - Linux  → Secret Service via libsecret (`linux-native-sync-persistent`)
//!
//! **Fail-closed on Linux without Secret Service.** Codex review of the
//! v0.3.0 plan correctly flagged that machine-id-derived "encryption" is
//! not security — `/etc/machine-id` is an identifier, not a secret. An
//! attacker who reads the encrypted file can usually derive the key.
//! Refresh tokens grant ~1 week of account access; storing them with weak
//! crypto is worse than asking the user to install libsecret. So when
//! the platform backend is missing, we surface a clear error and the UI
//! offers the existing pair-from-Mac flow as a workaround.
//!
//! `.deb` / `.rpm` package metadata declares `libsecret-1-0` as a runtime
//! dependency so default installs already have it; the failure only
//! fires on minimal headless Linux without a desktop environment.

use keyring::Entry;
use thiserror::Error;

const SERVICE: &str = "dev.clipulse.desktop";
const ACCOUNT: &str = "supabase-refresh-token";

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

fn entry() -> Result<Entry, KeychainError> {
    Entry::new(SERVICE, ACCOUNT).map_err(map_err)
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

pub fn store_refresh_token(token: &str) -> Result<(), KeychainError> {
    entry()?.set_password(token).map_err(map_err)
}

/// Read the refresh token. Returns `Ok(None)` if no entry exists,
/// `Err(NotAvailable)` if the backend is missing.
pub fn read_refresh_token() -> Result<Option<String>, KeychainError> {
    match entry()?.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err(e)),
    }
}

/// Delete the refresh token. Best-effort — a missing entry is treated
/// as success (the goal "no token in keychain" is already achieved).
pub fn delete_refresh_token() -> Result<(), KeychainError> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_err(e)),
    }
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
