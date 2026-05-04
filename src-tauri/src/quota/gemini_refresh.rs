//! Active Gemini OAuth refresh (v0.4.7).
//!
//! Gemini CLI writes `~/.gemini/oauth_creds.json` with access_token +
//! refresh_token + expiry_date (epoch ms). When the access token expires
//! (~8 hours after issue), v0.4.6 silently skipped collection until the
//! user re-ran `gemini` CLI to refresh.
//!
//! v0.4.7 actively refreshes by:
//!   1. Locating Gemini CLI's bundled `oauth2.js` file.
//!   2. Regex-extracting `OAUTH_CLIENT_ID` and `OAUTH_CLIENT_SECRET`
//!      (these are the values Gemini CLI uses internally; not secrets
//!      per RFC 6749 §2.2 — already shipped in the user's local CLI
//!      binary, we just re-use them).
//!   3. POSTing to `https://oauth2.googleapis.com/token` with
//!      `grant_type=refresh_token`.
//!   4. Caller updates `~/.gemini/oauth_creds.json` atomically with the
//!      new access_token + expiry_date.
//!
//! Mirrors macOS CodexBar `GeminiStatusProbe.swift:520-600` (commit
//! 82bbcde, 2026-05-02). Path candidates cover npm, Homebrew, Nix.
//!
//! Best-effort: if `oauth2.js` can't be found OR the refresh API fails,
//! fall back to v0.4.6 silent-skip behavior. No regression.

use std::path::PathBuf;
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

const TOKEN_REFRESH_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);
const OAUTH2_JS_RELATIVE: &str =
    "node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js";

#[derive(Debug, Clone)]
pub struct RefreshedTokens {
    pub access_token: String,
    /// Optional new refresh_token (Google may rotate).
    pub refresh_token: Option<String>,
    /// Seconds from now when the access_token expires (per Google's
    /// OAuth 2.0 RFC 6749 §5.1).
    pub expires_in: u64,
    /// Optional new id_token.
    pub id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleRefreshResponse {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

/// Try to refresh a Gemini access token using the user's local
/// gemini-cli OAuth credentials. Returns `Err(reason)` if any step
/// fails — caller logs at warn! and falls back to silent-skip.
pub async fn refresh(refresh_token: &str) -> Result<RefreshedTokens, String> {
    let oauth2_js_path = find_oauth2_js().ok_or_else(|| {
        "Gemini CLI's oauth2.js not found in known npm/Homebrew/Nix paths".to_string()
    })?;
    let content = std::fs::read_to_string(&oauth2_js_path)
        .map_err(|e| format!("read {}: {e}", oauth2_js_path.display()))?;
    let (client_id, client_secret) = extract_oauth_credentials(&content).ok_or_else(|| {
        "OAUTH_CLIENT_ID / OAUTH_CLIENT_SECRET regex didn't match — gemini-cli internal layout \
         may have changed; refresh disabled until upstream regex updated"
            .to_string()
    })?;

    post_refresh(&client_id, &client_secret, refresh_token).await
}

fn find_oauth2_js() -> Option<PathBuf> {
    collect_candidate_paths().into_iter().find(|p| p.exists())
}

fn collect_candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let sub = OAUTH2_JS_RELATIVE.replace('/', std::path::MAIN_SEPARATOR_STR);

    if cfg!(target_os = "windows") {
        // npm default global on Win: %APPDATA%\npm\node_modules\@google\gemini-cli
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let appdata = PathBuf::from(appdata);
            // Nested layout (npm install -g @google/gemini-cli puts the
            // package's deps under its own node_modules/).
            paths.push(
                appdata
                    .join("npm")
                    .join("node_modules")
                    .join("@google")
                    .join("gemini-cli")
                    .join(&sub),
            );
            // Flat layout (sibling): some npm versions hoist deps.
            paths.push(appdata.join("npm").join(&sub));
        }
    } else {
        // Mac / Linux common npm-global lib roots
        let lib_roots = ["/usr/local/lib", "/usr/lib", "/opt/homebrew/lib"];
        for base in &lib_roots {
            paths.push(
                PathBuf::from(base)
                    .join("node_modules/@google/gemini-cli")
                    .join(OAUTH2_JS_RELATIVE),
            );
            // Sibling-layout fallback
            paths.push(PathBuf::from(base).join(OAUTH2_JS_RELATIVE));
        }
        // User-local npm prefixes
        if let Some(home) = dirs::home_dir() {
            for user_path in &[".npm-global/lib", ".local/lib"] {
                paths.push(
                    home.join(user_path)
                        .join("node_modules/@google/gemini-cli")
                        .join(OAUTH2_JS_RELATIVE),
                );
                paths.push(home.join(user_path).join(OAUTH2_JS_RELATIVE));
            }
        }
    }

    paths
}

fn extract_oauth_credentials(content: &str) -> Option<(String, String)> {
    static CLIENT_ID_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"OAUTH_CLIENT_ID\s*=\s*['"]([\w\-\.]+)['"]"#).unwrap());
    static CLIENT_SECRET_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"OAUTH_CLIENT_SECRET\s*=\s*['"]([\w\-]+)['"]"#).unwrap());

    let id = CLIENT_ID_REGEX
        .captures(content)?
        .get(1)?
        .as_str()
        .to_string();
    let secret = CLIENT_SECRET_REGEX
        .captures(content)?
        .get(1)?
        .as_str()
        .to_string();
    Some((id, secret))
}

async fn post_refresh(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<RefreshedTokens, String> {
    let client = reqwest::Client::builder()
        .timeout(REFRESH_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let body = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let resp = client
        .post(TOKEN_REFRESH_ENDPOINT)
        .form(&body)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let snippet: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect();
        return Err(format!("HTTP {} — {}", status.as_u16(), snippet));
    }

    let parsed: GoogleRefreshResponse = resp.json().await.map_err(|e| format!("parse: {e}"))?;

    Ok(RefreshedTokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_in: parsed.expires_in,
        id_token: parsed.id_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_credentials_from_real_shape() {
        // Synthetic snippet matching gemini-cli-core's oauth2.js shape.
        // Real values look like: 681255809395-...apps.googleusercontent.com
        // and GOCSPX-... — we don't ship them, just the regex.
        let content = r#"
            // ... lots of code ...
            const OAUTH_CLIENT_ID = '681255809395-abc.apps.googleusercontent.com';
            const OAUTH_CLIENT_SECRET = 'GOCSPX-fake-secret-here';
            // more code
        "#;
        let (id, secret) = extract_oauth_credentials(content).unwrap();
        assert_eq!(id, "681255809395-abc.apps.googleusercontent.com");
        assert_eq!(secret, "GOCSPX-fake-secret-here");
    }

    #[test]
    fn extract_credentials_handles_double_quotes() {
        let content = r#"const OAUTH_CLIENT_ID = "id-with-double-quotes"; const OAUTH_CLIENT_SECRET = "GOCSPX-secret";"#;
        let (id, secret) = extract_oauth_credentials(content).unwrap();
        assert_eq!(id, "id-with-double-quotes");
        assert_eq!(secret, "GOCSPX-secret");
    }

    #[test]
    fn extract_credentials_returns_none_when_missing() {
        let content = "no oauth credentials in here";
        assert!(extract_oauth_credentials(content).is_none());
    }

    #[test]
    fn extract_credentials_returns_none_when_only_id_present() {
        // Defensive: if oauth2.js shape changes such that only one
        // constant is found, refuse — don't try to refresh with a
        // half-extracted value pair (would 401 anyway, just fail loud).
        let content = "const OAUTH_CLIENT_ID = 'only-id';";
        assert!(extract_oauth_credentials(content).is_none());
    }

    #[test]
    fn parse_google_refresh_response_minimal() {
        let json =
            r#"{"access_token":"new-token","expires_in":3599,"token_type":"Bearer","scope":"..."}"#;
        let r: GoogleRefreshResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.access_token, "new-token");
        assert_eq!(r.expires_in, 3599);
        assert!(r.refresh_token.is_none());
        assert!(r.id_token.is_none());
    }

    #[test]
    fn parse_google_refresh_response_with_rotated_tokens() {
        // Google sometimes rotates refresh_token + id_token alongside
        // access_token. Caller must persist whichever fields came back.
        let json = r#"{"access_token":"new-access","expires_in":3599,"refresh_token":"new-refresh","id_token":"new-id"}"#;
        let r: GoogleRefreshResponse = serde_json::from_str(json).unwrap();
        assert_eq!(r.access_token, "new-access");
        assert_eq!(r.refresh_token.as_deref(), Some("new-refresh"));
        assert_eq!(r.id_token.as_deref(), Some("new-id"));
    }

    #[test]
    fn collect_candidate_paths_returns_at_least_one() {
        // Smoke: the path list shouldn't be empty regardless of OS.
        let paths = collect_candidate_paths();
        assert!(!paths.is_empty());
    }
}
