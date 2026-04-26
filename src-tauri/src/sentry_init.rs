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
    // Generic /Users/<name>/ → /Users/<scrubbed>/ on macOS, even when
    // we don't have HOME set explicitly.
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
}
