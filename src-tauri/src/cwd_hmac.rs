//! v0.7.0 — Per-user secret + HMAC-SHA256 of the full cwd path.
//!
//! Hook protocol parity with the Mac sibling: when the helper
//! generates a remote permission request, it includes both
//! `cwd_basename` (last path segment, displayable) AND `cwd_hmac`
//! (HMAC-SHA256 of the full path). The HMAC lets the SERVER index
//! "same project across devices" without ever seeing the path —
//! a user with the same project checked out on Mac and Windows
//! would have matching HMACs but the path itself never crosses
//! the wire.
//!
//! Secret: 32 random bytes generated on first use, stored in the
//! OS keychain at `<service=dev.clipulse.desktop, account=cwd-hmac-secret>`.
//! Same-user-different-device case: the Mac and Windows have
//! DIFFERENT secrets → HMACs don't match → server treats them as
//! distinct. That's intentional for v0.7.0; cross-device project
//! coalescing requires syncing the secret via the device-creds
//! channel which the Mac team punted to a future iteration.
//!
//! On platforms where the keychain isn't available (headless Linux
//! without libsecret), the HMAC is `None` — server-side schema
//! tolerates null hmac (treats each request as device-local).

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::keychain;

const ACCOUNT_CWD_HMAC_SECRET: &str = "cwd-hmac-secret";

type HmacSha256 = Hmac<Sha256>;

/// Read or create the per-user HMAC secret. On first call after
/// install, generates 32 random bytes from `uuid::v4` (sufficiently
/// random for HMAC keying — uuid::v4 uses an OS RNG) and stores in
/// the keychain. Subsequent calls read the stored value.
///
/// Returns `Ok(None)` when the keychain backend is unavailable
/// (headless Linux). Caller falls back to omitting `cwd_hmac` from
/// the upload — server tolerates null and the user gets per-device
/// project tracking instead of cross-device.
pub fn load_or_create_secret() -> Result<Option<Vec<u8>>, keychain::KeychainError> {
    match keychain::read_at(ACCOUNT_CWD_HMAC_SECRET) {
        Ok(Some(hex_str)) => {
            // Stored as hex (64 chars for 32 bytes). Decode; if invalid,
            // fall through to regenerate (defensive — shouldn't happen
            // in practice).
            if let Ok(bytes) = hex_decode(&hex_str) {
                if bytes.len() == 32 {
                    return Ok(Some(bytes));
                }
            }
            log::warn!("cwd_hmac: stored secret was malformed; regenerating");
            generate_and_store()
        }
        Ok(None) => generate_and_store(),
        Err(keychain::KeychainError::NotAvailable) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Generate 32 random bytes from the OS CSPRNG via the `getrandom`
/// crate (the same crypto primitive most of our dependencies already
/// rely on). Hex-encode and write to the keychain. Returns the raw
/// bytes.
///
/// Per Gemini 3.1 Pro v0.7.0 review P2: the original implementation
/// concatenated two `uuid::v4`s, which loses ~12 bits of entropy
/// across the two UUIDs' fixed version/variant bits (4+2 fixed bits
/// per UUID × 2 = 12 fixed bits → 244 bits of randomness instead of
/// the expected 256). Practically secure either way, but
/// `getrandom::getrandom` is the idiomatic shape and produces a
/// fully uniform 32-byte slice.
fn generate_and_store() -> Result<Option<Vec<u8>>, keychain::KeychainError> {
    let mut secret = vec![0u8; 32];
    if getrandom::getrandom(&mut secret).is_err() {
        // OS RNG unavailable — extremely rare. Treat as "no secret"
        // so the caller falls through to omitting cwd_hmac, NOT a
        // weaker secret. The hook will still ship its request; just
        // without the cross-device project-matching field.
        log::warn!("cwd_hmac: getrandom failed; storing no secret this run");
        return Ok(None);
    }
    let hex_str = hex_encode(&secret);
    match keychain::store_at(ACCOUNT_CWD_HMAC_SECRET, &hex_str) {
        Ok(()) => Ok(Some(secret)),
        Err(keychain::KeychainError::NotAvailable) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Compute HMAC-SHA256 of `path` keyed by `secret`. Returns hex
/// digest (64 chars). The Mac server-side index expects hex.
///
/// Empty path or empty secret → None (matches Mac's `_hmac_path`
/// behavior — both are treated as "no project context to sign").
pub fn hmac_path(secret: &[u8], path: &str) -> Option<String> {
    if path.is_empty() || secret.is_empty() {
        return None;
    }
    let mut mac = HmacSha256::new_from_slice(secret).ok()?;
    mac.update(path.as_bytes());
    let result = mac.finalize().into_bytes();
    Some(hex_encode(&result))
}

// =============================================================
// Hex helpers — small + dependency-free. Avoids pulling the
// `hex` crate just for two formats.
// =============================================================

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn hex_decode(s: &str) -> Result<Vec<u8>, &'static str> {
    if !s.len().is_multiple_of(2) {
        return Err("odd hex length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])?;
        let lo = hex_nibble(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, &'static str> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err("invalid hex char"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_random() {
        let bytes = b"\x00\xff\xab\xcd\xde\xad\xbe\xef\x12\x34\x56\x78\x9a\xbc\xde\xf0";
        let s = hex_encode(bytes);
        assert_eq!(s.len(), bytes.len() * 2);
        let decoded = hex_decode(&s).unwrap();
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn hex_decode_uppercase_and_lowercase() {
        let hi = hex_decode("DEADbeef").unwrap();
        let lo = hex_decode("deadbeef").unwrap();
        assert_eq!(hi, lo);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_decode_rejects_invalid_chars() {
        assert!(hex_decode("ZZ").is_err());
        assert!(hex_decode("ab cd").is_err()); // space
    }

    #[test]
    fn hmac_path_empty_inputs_return_none() {
        let secret = b"some-secret-key";
        assert_eq!(hmac_path(secret, ""), None);
        assert_eq!(hmac_path(b"", "/some/path"), None);
        assert_eq!(hmac_path(b"", ""), None);
    }

    #[test]
    fn hmac_path_deterministic() {
        // Same secret + path → same digest. Same path + different
        // secret → different digest (proves keying).
        let s1 = b"secret-1";
        let s2 = b"secret-2";
        let path = "/Users/jason/Documents/myproject";
        let d1a = hmac_path(s1, path).unwrap();
        let d1b = hmac_path(s1, path).unwrap();
        let d2 = hmac_path(s2, path).unwrap();
        assert_eq!(d1a, d1b, "deterministic for same key+path");
        assert_ne!(d1a, d2, "different keys produce different digests");
    }

    #[test]
    fn hmac_path_distinguishes_paths() {
        let secret = b"shared-secret";
        let d_a = hmac_path(secret, "/projA").unwrap();
        let d_b = hmac_path(secret, "/projB").unwrap();
        assert_ne!(d_a, d_b);
    }

    #[test]
    fn hmac_path_returns_hex_64_chars() {
        // SHA-256 → 32 bytes → 64 hex chars.
        let secret = b"hello-world";
        let digest = hmac_path(secret, "/path").unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
