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
        assert!(!out.contains(&"a".repeat(64)), "helper secret survived: {out}");
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
}
