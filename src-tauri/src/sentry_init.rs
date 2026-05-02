//! Sentry crash + error reporting — Rust counterpart to the Swift
//! `SentryLogger.swift` and Kotlin `SentryInit.kt` wrappers in the
//! sibling apps.
//!
//! DSN is read at runtime from the `CLI_PULSE_SENTRY_DSN` env var and
//! falls back to a compile-time default (none, currently). When the
//! DSN is empty, Sentry init is a no-op — events are dropped before
//! they can leave the machine. Users on dev builds run with no DSN
//! and incur zero network overhead.
//!
//! Privacy stance — matches Swift `SentryLogger` config knobs:
//!   - sendDefaultPii = false
//!   - tracesSampleRate = 0 (no perf tracing)
//!   - Client-side `before_send` scrubs the user's `$HOME` path from
//!     event payloads (so file paths read `<home>/code/foo` instead
//!     of `/Users/jason/code/foo`).
//!
//! The aggressive **field-name** scrubbing (token / secret / password
//! / api_key / supabase / claude_api / anthropic / codex / openai /
//! gemini / dsn / keychain / pairing / refresh_token / access_token /
//! id_token) is delegated to the Sentry **org-level** Data Scrubber
//! settings — same arrangement the Swift / Kotlin apps use per
//! `~/.claude/projects/.../memory/reference_sentry.md`. We don't try
//! to maintain a parallel client-side allow-list because keeping it in
//! sync across four codebases (Swift / Kotlin / Python helper / Rust
//! desktop) would be more risky than relying on the org default
//! scrubbers, which are opt-in-on by Jason's account.
//!
//! No new Sentry project is created by this code. Reuse the
//! `apple-macos` project's DSN if you want desktop events to land
//! somewhere visible — they'll be tagged `platform=desktop` so they
//! don't get confused with the Mac menu-bar app's events.

use std::sync::OnceLock;

use sentry::{ClientInitGuard, ClientOptions};

/// Holds the Sentry guard for the lifetime of the app. Drop on app
/// exit flushes pending events.
static GUARD: OnceLock<Option<ClientInitGuard>> = OnceLock::new();

/// Compile-time fallback DSN. Empty means "no Sentry by default."
/// Override at runtime via the `CLI_PULSE_SENTRY_DSN` env var.
const COMPILED_DSN: Option<&str> = option_env!("CLI_PULSE_SENTRY_DSN");

pub fn install() {
    let _ = GUARD.set(install_inner());
}

fn install_inner() -> Option<ClientInitGuard> {
    let dsn_owned: String = std::env::var("CLI_PULSE_SENTRY_DSN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| COMPILED_DSN.map(str::to_string))
        .filter(|s| !s.trim().is_empty())?;

    let release = format!("cli-pulse-desktop@{}", env!("CARGO_PKG_VERSION"));
    let environment = if cfg!(debug_assertions) {
        "dev"
    } else {
        "production"
    };

    let options = ClientOptions {
        dsn: dsn_owned.parse().ok(),
        release: Some(release.into()),
        environment: Some(environment.into()),
        send_default_pii: false,
        traces_sample_rate: 0.0,
        attach_stacktrace: true,
        before_send: Some(std::sync::Arc::new(scrub_event)),
        ..Default::default()
    };

    // The `panic` feature on sentry@0.34 auto-installs a panic hook
    // when the client initializes — no explicit register call needed.
    let guard = sentry::init(options);

    // Tag every event so the desktop project (or shared apple-macos
    // project) can distinguish desktop events from menu-bar events.
    sentry::configure_scope(|scope| {
        scope.set_tag("platform", "desktop");
        scope.set_tag("os", platform_label());
        scope.set_tag("arch", arch_label());
        scope.set_tag("app_version", env!("CARGO_PKG_VERSION"));
    });

    log::info!("sentry initialized ({environment})");
    Some(guard)
}

fn platform_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}

fn arch_label() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "other"
    }
}

/// `before_send` filter — runs in-process before any event leaves the
/// machine. Returns `None` to drop, or a (possibly modified) event.
fn scrub_event(
    mut event: sentry::protocol::Event<'static>,
) -> Option<sentry::protocol::Event<'static>> {
    if let Some(home) = std::env::var("HOME").ok().filter(|s| !s.is_empty()) {
        scrub_strings_recursive(&mut event, &home, "<home>");
    }

    // v0.3.0: redact secret-shaped tokens that may appear inside error
    // messages, breadcrumb URLs, request bodies, etc. The org-level
    // Sentry Data Scrubber catches *field-name*-based redaction, but
    // string contents can carry tokens too — e.g. an error like
    // "401 from /auth/v1/token?refresh_token=eyJhbGc..." would still
    // ship the token in the message body.
    redact_secrets_in_strings(&mut event);

    Some(event)
}

/// Walk the event JSON-ish structure and replace occurrences of
/// `needle` with `replacement` in any string field. Sentry's protocol
/// types implement Display via Debug — easiest is to round-trip via
/// JSON. Cost is negligible for the tens-of-events-per-day rate this
/// will see in practice.
fn scrub_strings_recursive(
    event: &mut sentry::protocol::Event<'static>,
    needle: &str,
    replacement: &str,
) {
    if let Ok(json) = serde_json::to_string(event) {
        if json.contains(needle) {
            let scrubbed = json.replace(needle, replacement);
            if let Ok(reconstructed) = serde_json::from_str::<sentry::protocol::Event>(&scrubbed) {
                *event = reconstructed;
            }
        }
    }
}

/// v0.3.0: redact secret-shaped tokens by pattern, not just by field
/// name. Run after `scrub_strings_recursive(home → <home>)` so paths
/// have already been normalized.
///
/// Patterns covered:
///   - JWTs:           three base64url segments separated by `.`
///   - helper secrets: `helper_<64 hex chars>`
///   - OTP query/body params for refresh_token / access_token /
///     helper_secret / pairing_code
///
/// We round-trip the event through JSON (same approach as
/// `scrub_strings_recursive`) and apply regex substitutions to every
/// string. Misses on extremely odd shapes are acceptable because the
/// org-level Data Scrubber catches the field-name path; this is a
/// belt for the suspenders.
fn redact_secrets_in_strings(event: &mut sentry::protocol::Event<'static>) {
    use once_cell::sync::Lazy;
    use regex::Regex;

    static JWT: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"\beyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+\b").unwrap()
    });
    static HELPER_SECRET: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bhelper_[0-9a-f]{64}\b").unwrap());
    static QUERY_PARAM: Lazy<Regex> = Lazy::new(|| {
        Regex::new(
            r##"(?i)(refresh_token|access_token|helper_secret|pairing_code|authorization)=[^&\s"]+"##,
        )
        .unwrap()
    });
    // v0.4.0 — Anthropic OAuth / API tokens that show up when the
    // desktop hits api.anthropic.com/api/oauth/usage. Codex review
    // flagged that the version digit count varies (saw 2 in the wild,
    // could be 1 or 3) and the rumored `sid` form may also appear in
    // error messages. Match permissively.
    static ANTHROPIC_TOKEN: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bsk-ant-(?:oat|api|sid)\d{0,3}-[A-Za-z0-9_\-]{16,}\b").unwrap());

    // v0.4.3 — additional providers + generic Bearer / Cookie redaction.
    // Codex 2026-05-02 review of v0.4.3 spec caught that:
    //  - GitHub now ships `github_pat_*` (47-char body) in addition
    //    to the legacy `gh[pousr]_` shape.
    //  - Cookie-only redaction misses `Authorization: Bearer <opaque>`
    //    for providers (e.g. Cursor) whose tokens don't match a known
    //    regex.
    static OPENAI_TOKEN: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bsk-(?:proj|svcacct)?-?[A-Za-z0-9_\-]{30,}\b").unwrap());
    static GITHUB_TOKEN_LEGACY: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b").unwrap());
    static GITHUB_PAT_NEW: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{40,90}\b").unwrap());
    static OPENROUTER_TOKEN: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bsk-or-(?:v\d+-)?[A-Za-z0-9]{40,}\b").unwrap());
    static GOOGLE_OAUTH: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\bya29\.[A-Za-z0-9_\-]{40,}\b").unwrap());
    static COOKIE_HEADER: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(?im)("?Cookie"?:\s*)[^\r\n,"]+"#).unwrap());
    static AUTH_BEARER: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(?im)("?Authorization"?:\s*)Bearer\s+[^\r\n,"]+"#).unwrap());

    let Ok(json) = serde_json::to_string(event) else {
        return;
    };
    let mut scrubbed = JWT.replace_all(&json, "<jwt-redacted>").into_owned();
    scrubbed = HELPER_SECRET
        .replace_all(&scrubbed, "<helper-secret-redacted>")
        .into_owned();
    scrubbed = QUERY_PARAM
        .replace_all(&scrubbed, "$1=<redacted>")
        .into_owned();
    scrubbed = ANTHROPIC_TOKEN
        .replace_all(&scrubbed, "<anthropic-token-redacted>")
        .into_owned();
    // v0.4.3 — order matters: OpenRouter `sk-or-...` and OpenAI
    // `sk-...` overlap on the prefix; match OpenRouter first because
    // its body length floor (40) and `or-` prefix are stricter.
    scrubbed = OPENROUTER_TOKEN
        .replace_all(&scrubbed, "<openrouter-token-redacted>")
        .into_owned();
    scrubbed = OPENAI_TOKEN
        .replace_all(&scrubbed, "<openai-token-redacted>")
        .into_owned();
    scrubbed = GITHUB_PAT_NEW
        .replace_all(&scrubbed, "<github-token-redacted>")
        .into_owned();
    scrubbed = GITHUB_TOKEN_LEGACY
        .replace_all(&scrubbed, "<github-token-redacted>")
        .into_owned();
    scrubbed = GOOGLE_OAUTH
        .replace_all(&scrubbed, "<google-oauth-redacted>")
        .into_owned();
    // Note: `${1}` (not `$1`) so the trailing alphanumerics aren't
    // interpreted as part of the capture-group name in Rust's regex
    // replace_all syntax.
    scrubbed = COOKIE_HEADER
        .replace_all(&scrubbed, "${1}<redacted>")
        .into_owned();
    scrubbed = AUTH_BEARER
        .replace_all(&scrubbed, "${1}Bearer <redacted>")
        .into_owned();

    if scrubbed != json {
        if let Ok(reconstructed) = serde_json::from_str::<sentry::protocol::Event>(&scrubbed) {
            *event = reconstructed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_without_dsn_is_a_noop() {
        // Clear env var if set, run install, ensure no crash.
        // We can't easily test that GUARD is None without exposing it,
        // but the install path running cleanly is enough.
        std::env::remove_var("CLI_PULSE_SENTRY_DSN");
        install();
        // Subsequent installs are also no-ops (OnceLock semantics)
        install();
    }

    fn scrub_str_via_event(input: &str) -> String {
        // Build a minimal event with our string in `message`, run it
        // through redact_secrets_in_strings, then read back.
        let mut event = sentry::protocol::Event::<'static> {
            message: Some(input.to_string()),
            ..Default::default()
        };
        redact_secrets_in_strings(&mut event);
        event.message.unwrap_or_default()
    }

    #[test]
    fn scrubs_jwt_in_message() {
        let s = "401 from /auth/v1/token: eyJhbGciOiJIUzI1NiIs.eyJzdWIiOjF9.AbC123-_xyz";
        let out = scrub_str_via_event(s);
        assert!(!out.contains("eyJhbGciOiJIUzI1NiIs"), "JWT survived: {out}");
        assert!(out.contains("<jwt-redacted>"), "no replacement: {out}");
    }

    #[test]
    fn scrubs_helper_secret_in_message() {
        let s = format!("device 123 with secret helper_{}", "a".repeat(64));
        let out = scrub_str_via_event(&s);
        assert!(
            !out.contains(&"a".repeat(64)),
            "helper secret survived: {out}"
        );
        assert!(out.contains("<helper-secret-redacted>"));
    }

    #[test]
    fn scrubs_query_params_in_url() {
        let s = "POST https://x.supabase.co/auth/v1/token?refresh_token=secret123&grant_type=refresh_token failed";
        let out = scrub_str_via_event(s);
        assert!(!out.contains("secret123"), "refresh_token survived: {out}");
        assert!(out.contains("refresh_token=<redacted>"));
        // Non-secret params (grant_type) survive intact.
        assert!(out.contains("grant_type=refresh_token"));
    }

    #[test]
    fn scrubs_authorization_header_value() {
        let s = "header Authorization=Bearer-eyJhbGc.eyJzdWIiOjF9.foo present";
        let out = scrub_str_via_event(s);
        assert!(!out.contains("eyJhbGc.eyJzdWIiOjF9"), "JWT survived: {out}");
    }

    #[test]
    fn benign_strings_pass_through_unchanged() {
        let s = "just a plain error message — no secrets here";
        let out = scrub_str_via_event(s);
        assert_eq!(out, s);
    }

    // v0.4.0 — Anthropic token scrubbing. Codex flagged the v0.3.0
    // regex set didn't cover sk-ant-oat..., which is what the new
    // OAuth usage path on Win/Linux carries via Authorization headers.
    #[test]
    fn scrubs_anthropic_oat_token_in_message() {
        let s = "POST /api/oauth/usage failed: sk-ant-oat01-AbCdEfGhIjKlMnOpQrSt-XyZ_-123";
        let out = scrub_str_via_event(s);
        assert!(
            !out.contains("AbCdEfGhIjKlMnOpQrSt"),
            "OAuth token survived: {out}"
        );
        assert!(out.contains("<anthropic-token-redacted>"));
    }

    #[test]
    fn scrubs_anthropic_api_token() {
        let s = "key sk-ant-api03-vWxYz1234567890abcdefghijklm leaked";
        let out = scrub_str_via_event(s);
        assert!(
            !out.contains("vWxYz1234567890abcdefghijklm"),
            "api token survived: {out}"
        );
        assert!(out.contains("<anthropic-token-redacted>"));
    }

    #[test]
    fn scrubs_anthropic_oat_in_authorization_header_value() {
        // Real shape from a logged Authorization header. The Bearer
        // token gets redacted by ANTHROPIC_TOKEN even when the
        // QUERY_PARAM regex doesn't fire (no "Authorization=" form).
        let s = r#"Authorization: Bearer sk-ant-oat01-1234567890abcdefghijklmnopqrst was rejected"#;
        let out = scrub_str_via_event(s);
        assert!(!out.contains("1234567890abcdefghijklmnopqrst"));
    }

    #[test]
    fn scrubs_unversioned_anthropic_token_form() {
        // Defensive: sk-ant-oat- without version digits (rumored
        // future form). Permissive regex catches this.
        let s = "saw sk-ant-oat-thisIsAnUnversionedTokenBody123 in the cache";
        let out = scrub_str_via_event(s);
        assert!(
            !out.contains("thisIsAnUnversionedTokenBody123"),
            "unversioned token survived: {out}"
        );
    }

    // v0.4.3 — multi-provider tokens.

    #[test]
    fn scrubs_openai_proj_token() {
        let s = "401: token=sk-proj-AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIII1234567890";
        let out = scrub_str_via_event(s);
        assert!(
            !out.contains("AAAABBBB"),
            "OpenAI proj token survived: {out}"
        );
        assert!(
            out.contains("<openai-token-redacted>"),
            "no replacement: {out}"
        );
    }

    #[test]
    fn scrubs_openai_legacy_token() {
        let s = "Authorization: Bearer sk-AAAABBBBCCCCDDDDEEEEFFFFGGGG12345678";
        let out = scrub_str_via_event(s);
        // AUTH_BEARER catches the whole header line; OPENAI_TOKEN catches
        // the token alone if rendered raw. Either way, the body must not
        // survive.
        assert!(!out.contains("AAAABBBBCCCCDDDDEEEEFFFFGGGG"));
    }

    #[test]
    fn scrubs_github_pat_legacy_format() {
        let s = "fetch failed for token ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let out = scrub_str_via_event(s);
        assert!(!out.contains("AbCdEfGhIjKlMnOp"));
        assert!(out.contains("<github-token-redacted>"));
    }

    #[test]
    fn scrubs_github_pat_new_format() {
        // 2024+ GitHub PAT: github_pat_<22 chars>_<59 chars>
        let s = "Header: github_pat_11ABCDE0Y0123456789ABC_xY1zAbCdEfGhIjKlMnOpQrStUvWxYz0123456789ABCdEfGhIj0123456789";
        let out = scrub_str_via_event(s);
        assert!(!out.contains("github_pat_11"), "PAT survived: {out}");
        assert!(out.contains("<github-token-redacted>"));
    }

    #[test]
    fn scrubs_openrouter_v1_key() {
        let s = "OPENROUTER_API_KEY=sk-or-v1-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1234";
        let out = scrub_str_via_event(s);
        assert!(!out.contains("AAAAAAAAAAAAAAAA"));
        assert!(out.contains("<openrouter-token-redacted>"));
    }

    #[test]
    fn scrubs_google_ya29_bearer() {
        let s = "Authorization: Bearer ya29.A0ARrdaM-abcDEFghijKLMNopqrSTUVwxYZ0123456789ABCDEFGHIJKLMNOPQRSTUVWX";
        let out = scrub_str_via_event(s);
        assert!(
            !out.contains("A0ARrdaM-abcDEF"),
            "ya29 token survived: {out}"
        );
    }

    #[test]
    fn scrubs_cookie_header_value() {
        // Cookie header containing an opaque session token (e.g. Cursor).
        let s = "Cookie: WorkosCursorSessionToken=user_01ABCDEF.aBcDeFgHiJ.signature123";
        let out = scrub_str_via_event(s);
        assert!(
            !out.contains("WorkosCursorSessionToken=user_"),
            "Cookie body survived: {out}"
        );
        assert!(out.contains("<redacted>"));
    }

    #[test]
    fn scrubs_generic_authorization_bearer_header() {
        // Opaque Bearer token (no prefix matching any provider regex).
        let s = "Authorization: Bearer ZXJtaW5lOnNlc3Npb24tdG9rZW4tb3BhcXVlLXZhbHVl";
        let out = scrub_str_via_event(s);
        assert!(!out.contains("ZXJtaW5lOnNlc3Npb24"));
        assert!(out.contains("Bearer <redacted>"));
    }
}
