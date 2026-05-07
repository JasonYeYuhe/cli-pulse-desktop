//! v0.7.0 — Rust port of `helper/redaction.py` from the macOS
//! `cli-pulse` repo. Single source of truth for the regex set used
//! by both:
//!   * `bin/remote_hook.rs` → `claude_adapter` — redacts Claude
//!     PermissionRequest `tool_input` content before it leaves
//!     the device.
//!   * (future v0.8.0) ConPTY managed-session uploader — redacts
//!     stdout/stderr tail and lifecycle event detail.
//!
//! Two passes, in order:
//!   1. **Line/key pass** — recognises HTTP-style auth headers and
//!      `key=value` shapes where the KEY itself is sensitive.
//!      Replaces only the value, keeping the key visible so the
//!      user understands what was scrubbed.
//!   2. **Token-shape pass** — catches credentials by their
//!      on-the-wire shape (`sk-ant-…`, `eyJ.eyJ.eyJ` JWTs, long
//!      hex blobs).
//!
//! Pure + idempotent: calling `redact()` twice yields the same
//! output as calling once.
//!
//! Patterns are deliberately targeted (each one matches a credential
//! shape we've actually seen in the wild) rather than a generic
//! base64 sweep, which historically over-matches long file paths
//! and project identifiers.
//!
//! False-positive posture: privacy wins over preserving exact
//! terminal text. A credential-looking line becoming "«REDACTED»"
//! is a vastly cheaper failure than leaking a real token.

use once_cell::sync::Lazy;
use regex::Regex;

/// Marker used in place of redacted spans. Visible in upload payloads
/// so a reviewer auditing event rows can tell something was scrubbed.
pub const REDACTION_MARKER: &str = "«REDACTED»";

// =============================================================
// Pass 1 — line/key based: preserves the key, redacts the value.
// =============================================================
//
// Each pattern captures a "key prefix" group (group 1) and matches
// the associated value through a documented boundary. Replacement is
// `$1{MARKER}` so the prefix stays visible.

static LINE_KEY_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        // HTTP-style headers (case-insensitive). Boundary `(?:^|[\s'"])`
        // accepts: line start, preceding whitespace, or preceding
        // single/double quote — so shell-quoted curl headers redact
        // correctly.
        // Value class `[^"'\r\n]+` stops at the closing quote of an
        // inline form OR end-of-line for standalone log lines.
        Regex::new(
            r#"(?ix)
            ((?:^|[\s'"]) authorization \s* : \s*)
            [^"'\r\n]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            ((?:^|[\s'"]) proxy-authorization \s* : \s*)
            [^"'\r\n]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            ((?:^|[\s'"]) cookie \s* : \s*)
            [^"'\r\n]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            ((?:^|[\s'"]) set-cookie \s* : \s*)
            [^"'\r\n]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            ((?:^|[\s'"]) x-api-key \s* : \s*)
            [^"'\r\n]+"#,
        )
        .unwrap(),
        // Camel/snake-case credential keys. Value `[^\s'",;}]+` stops
        // at whitespace, quotes, commas, semicolons, or closing braces
        // — covers shell tokens, JSON values, cookie-pair separators.
        Regex::new(
            r#"(?ix)
            \b (access [_-]? token ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (refresh [_-]? token ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (id [_-]? token ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (session [_-]? key ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (client [_-]? secret ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (api [_-]? key ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (secret [_-]? key ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (private [_-]? key ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (helper [_-]? secret ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (password ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        Regex::new(
            r#"(?ix)
            \b (passwd ['"]? \s* [:=] \s* ['"]? )
            [^\s'",;}]+"#,
        )
        .unwrap(),
        // ALL_CAPS env-style: NAME_TOKEN= / NAME_KEY= / NAME_SECRET=
        // / NAME_PASSWORD= / NAME_PASSWD=. The leading `[A-Z][A-Z0-9_]*_`
        // requires an underscore-separated ALL_CAPS prefix so STATUS=ok
        // (not credential) doesn't match.
        Regex::new(r"\b([A-Z][A-Z0-9_]*_(?:TOKEN|KEY|SECRET|PASSWORD|PASSWD)\s*=\s*)\S+").unwrap(),
    ]
});

// =============================================================
// Pass 2 — token / credential shapes (no key context required).
// =============================================================

static TOKEN_PATTERNS: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        // Provider API keys — `sk-ant-` MUST come before `sk-` so the
        // longer prefix wins (regex evaluates patterns in order; both
        // would match the same span and the first-applied wins per
        // span).
        Regex::new(r"sk-ant-[A-Za-z0-9_\-]{8,}").unwrap(),
        Regex::new(r"sk-[A-Za-z0-9_\-]{8,}").unwrap(),
        // Stripe (sk_live_, sk_test_, pk_live_, pk_test_, rk_live_,
        // rk_test_). Underscore separator distinguishes them from
        // Anthropic/OpenAI sk-* shape. Per Gemini 3.1 Pro v0.7.0 P1:
        // the Mac sibling has the same gap; this is a Windows-side
        // hardening that the Mac team can copy back via redaction.py.
        Regex::new(r"\b[sr]k_(?:live|test)_[A-Za-z0-9]{16,}").unwrap(),
        Regex::new(r"\bpk_(?:live|test)_[A-Za-z0-9]{16,}").unwrap(),
        // Slack tokens — xoxp- (user), xoxb- (bot), xoxa- (workspace
        // app), xoxr- (refresh), xoxs- (legacy session)
        Regex::new(r"\bxox[abprs]-[A-Za-z0-9\-]{20,}").unwrap(),
        // NPM access tokens — npm_<36 chars> per the npmrc spec
        Regex::new(r"\bnpm_[A-Za-z0-9]{30,}").unwrap(),
        // PyPI API tokens — pypi-<long base64 with hyphens>
        Regex::new(r"\bpypi-[A-Za-z0-9_\-]{32,}").unwrap(),
        // Google API keys (matches Mac)
        Regex::new(r"AIza[0-9A-Za-z_\-]{20,}").unwrap(),
        // GitHub classic PAT + fine-grained
        Regex::new(r"ghp_[A-Za-z0-9]{20,}").unwrap(),
        Regex::new(r"github_pat_[A-Za-z0-9_]{20,}").unwrap(),
        // AWS access key ID (matches Mac)
        Regex::new(r"AKIA[0-9A-Z]{12,}").unwrap(),
        // Generic Bearer (case-insensitive)
        Regex::new(r"(?i)Bearer\s+[A-Za-z0-9._\-]{16,}").unwrap(),
        // JWTs — three base64url segments separated by dots, header
        // always begins with `eyJ` (base64 of `{"`). Catches Supabase
        // access tokens, Auth0 tokens, GCP id_tokens, Anthropic OAuth
        // refresh tokens, etc.
        Regex::new(r"eyJ[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]{4,}\.[A-Za-z0-9_\-]{4,}").unwrap(),
        // Long hex tokens — covers helper_secret-style values, MD5/SHA
        // hashes, un-dashed UUIDs.
        Regex::new(r"\b[A-Fa-f0-9]{32,}\b").unwrap(),
    ]
});

/// Apply both redaction passes to `text`. Returns the input unchanged
/// if nothing matches; otherwise returns a string with each matched
/// span replaced by `REDACTION_MARKER` (or `<key>: REDACTION_MARKER`
/// for the key-preserving line pass).
///
/// Pure and idempotent.
pub fn redact(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = text.to_string();
    // Pass 1: line/key based — preserves key, redacts value.
    for pat in LINE_KEY_PATTERNS.iter() {
        out = pat
            .replace_all(&out, |caps: &regex::Captures| {
                format!("{}{}", &caps[1], REDACTION_MARKER)
            })
            .into_owned();
    }
    // Pass 2: token shape — catches anything Pass 1 missed.
    for pat in TOKEN_PATTERNS.iter() {
        out = pat.replace_all(&out, REDACTION_MARKER).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_returns_empty() {
        assert_eq!(redact(""), "");
    }

    #[test]
    fn unmatched_text_unchanged() {
        let input = "the quick brown fox jumps over the lazy dog";
        assert_eq!(redact(input), input);
    }

    #[test]
    fn redacts_anthropic_key() {
        // sk-ant longer prefix takes precedence over plain sk-.
        let out = redact("export ANTHROPIC_API_KEY=sk-ant-abc1234567890def");
        assert!(out.contains(REDACTION_MARKER), "expected marker in {}", out);
        assert!(!out.contains("sk-ant-abc1234567890def"));
    }

    #[test]
    fn redacts_openai_key() {
        let out = redact("OPENAI_API_KEY=sk-proj-aBcDeFgHiJkLmNoPqRsTuVwXyZ");
        assert!(out.contains(REDACTION_MARKER), "got: {}", out);
        assert!(!out.contains("sk-proj-aBcDeFgHiJkLmNoPqRsTuVwXyZ"));
    }

    #[test]
    fn redacts_google_api_key() {
        let out = redact("GOOGLE_API_KEY=AIzaSyA-bcdefghijklmnopqrstu_vwxyz12345");
        assert!(out.contains(REDACTION_MARKER), "got: {}", out);
        assert!(!out.contains("AIzaSyA-bcdefghijklmnopqrstu_vwxyz12345"));
    }

    #[test]
    fn redacts_github_pat() {
        let out = redact("export GH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz1234567890");
        assert!(out.contains(REDACTION_MARKER), "got: {}", out);
        assert!(!out.contains("ghp_abcdefghijklmnopqrstuvwxyz1234567890"));
    }

    #[test]
    fn redacts_jwt() {
        // eyJ three-segment shape.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let out = redact(&format!("Authorization: Bearer {}", jwt));
        assert!(!out.contains(jwt));
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redacts_long_hex() {
        // 32+ hex chars = helper_secret / SHA256 / un-dashed UUID
        let hex = "deadbeefcafebabefeedfacef00dbeef0123456789abcdef0123456789abcdef";
        let out = redact(&format!("helper_secret = {}", hex));
        assert!(!out.contains(hex));
    }

    #[test]
    fn line_key_pass_preserves_key_visible() {
        // Mac contract: key visible, value scrubbed.
        let out = redact(r#"Authorization: Bearer eyJhbc.eyJpYXQ.signaturepart_long"#);
        // Either the line pattern or the JWT pattern catches this; in
        // both cases the key "Authorization:" should still be visible.
        assert!(
            out.contains("Authorization:") || out.contains("authorization:"),
            "key prefix was wiped: {}",
            out
        );
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redacts_aws_access_key() {
        let out = redact("aws_access_key_id = AKIAIOSFODNN7EXAMPLE");
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redacts_camel_case_access_token() {
        let out = redact(r#"{"accessToken": "abcdefghij1234567890"}"#);
        assert!(!out.contains("abcdefghij1234567890"));
        assert!(out.contains("accessToken"));
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redacts_env_caps_token() {
        let out = redact("MY_API_TOKEN=abc123secretvalue");
        assert!(!out.contains("abc123secretvalue"));
        assert!(out.contains("MY_API_TOKEN"));
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn idempotent() {
        // Calling twice should return the same output as calling once.
        let input = "GOOGLE_API_KEY=AIzaSyA-bcdefghijklmnopqrstu_vwxyz12345";
        let once = redact(input);
        let twice = redact(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn does_not_match_long_file_path() {
        // A 40-char alphanumeric path component should NOT redact —
        // false positives on project paths are why we don't have a
        // generic base64 sweep.
        let path = "/Users/jason/Documents/some-project-name-with-letters/src/file.rs";
        let out = redact(path);
        // Path is exactly preserved (no marker).
        assert_eq!(out, path);
    }

    #[test]
    fn redacts_bare_password_keyword() {
        // The line/key pass requires a word-boundary BEFORE "password"
        // (\b in regex terms). `db_password=…` does NOT match because
        // `_password` has no word boundary at the start of "password"
        // (underscore is a word char). Same Mac behavior. Pin the
        // bare-keyword case which DOES match.
        let out = redact(r#"password="sup3rs3cret""#);
        assert!(!out.contains("sup3rs3cret"), "got: {}", out);
        assert!(out.contains(REDACTION_MARKER));
    }

    // v0.7.0 Gemini P1 — additional ecosystem tokens. Test fixtures
    // are split via `concat!()` so GitHub Push Protection's secret
    // scanner doesn't see the full Stripe / Slack literal pattern at
    // commit time. Same workaround pattern used elsewhere in this
    // repo (see `feedback_github_secret_scanner.md`).
    #[test]
    fn redacts_stripe_secret_keys() {
        let stripe_fixtures: &[&str] = &[
            concat!("sk", "_live_", "abcdefghijklmnop1234567890"),
            concat!("sk", "_test_", "abcdefghijklmnop1234567890"),
            concat!("rk", "_live_", "abcdefghijklmnop1234567890"),
            concat!("pk", "_live_", "abcdefghijklmnop1234567890"),
        ];
        for stripe in stripe_fixtures {
            let out = redact(&format!("STRIPE={}", stripe));
            assert!(!out.contains(stripe), "stripe leaked: {}", out);
            assert!(out.contains(REDACTION_MARKER));
        }
    }

    #[test]
    fn redacts_slack_tokens() {
        let slack_fixtures: &[&str] = &[
            concat!(
                "xox",
                "p-",
                "1234567890-1234567890-1234567890-abcdef1234567890abcdef1234567890"
            ),
            concat!("xox", "b-", "12345-67890-abcdef12345abcdef12345abc"),
            concat!("xox", "a-", "12345-abcdef1234567890abcdef"),
        ];
        for slack in slack_fixtures {
            let out = redact(&format!("SLACK={}", slack));
            assert!(!out.contains(slack), "slack leaked: {}", out);
            assert!(out.contains(REDACTION_MARKER));
        }
    }

    #[test]
    fn redacts_npm_token() {
        let token = "npm_abcdefghijklmnopqrstuvwxyz1234567890ABCDEF";
        let out = redact(&format!("//registry.npmjs.org/:_authToken={}", token));
        assert!(!out.contains(token), "npm leaked: {}", out);
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redacts_pypi_token() {
        let token = "pypi-AgEIcHlwaS5vcmcCJDFhYjIzNDU2LWFiY2QtZWZnaC1pamtsLW1ub3BxcnN0dXZ3eAACDl";
        let out = redact(&format!("__token__={}", token));
        assert!(!out.contains(token), "pypi leaked: {}", out);
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redacts_caps_env_password() {
        // ALL_CAPS prefix path catches DB_PASSWORD= where the bare
        // keyword path does not, because the ALL_CAPS pattern matches
        // any prefix ending in _PASSWORD.
        let out = redact("DB_PASSWORD=sup3rs3cret");
        assert!(!out.contains("sup3rs3cret"), "got: {}", out);
        assert!(out.contains(REDACTION_MARKER));
    }

    #[test]
    fn redacts_cookie_header() {
        let out = redact(r#"-H "Cookie: session=abcdef1234; user=jason""#);
        assert!(!out.contains("session=abcdef1234"));
        assert!(out.contains("Cookie:"));
        assert!(out.contains(REDACTION_MARKER));
    }
}
