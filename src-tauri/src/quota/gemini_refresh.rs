//! Active Gemini OAuth refresh (v0.4.7, expanded in v0.4.9).
//!
//! Gemini CLI writes `~/.gemini/oauth_creds.json` with access_token +
//! refresh_token + expiry_date (epoch ms). When the access token expires
//! (~8 hours after issue), v0.4.6 silently skipped collection until the
//! user re-ran `gemini` CLI to refresh.
//!
//! v0.4.7 actively refreshes by extracting Gemini CLI's hardcoded OAuth
//! client_id + client_secret (not secrets per RFC 6749 §2.2 — already
//! shipped in the user's local CLI binary) and POSTing to Google's
//! `https://oauth2.googleapis.com/token` endpoint.
//!
//! v0.4.9 fixes the discovery path. The original v0.4.7 mirrored
//! macOS CodexBar's approach: look for a literal `oauth2.js` source
//! file at known npm/Homebrew/Nix paths. That works for Homebrew
//! installs and old gemini-cli versions, but **modern @google/gemini-cli
//! npm releases are esbuild-bundled** — there's no standalone
//! `oauth2.js`; the OAuth code lives inside hashed chunks like
//! `bundle/gemini-3OZCG3O2.js`. v0.4.9 adds:
//!
//!   1. Direct `oauth2.js` path (legacy / source layout) — first.
//!   2. Walk `<gemini-cli-root>/bundle/*.js` — scan each chunk file for
//!      the OAuth pair.
//!   3. Walk `<gemini-cli-root>/dist/**/*.js` as a final fallback.
//!
//! Plus a value-pattern regex fallback (`<digits>-...apps.google...com`
//! and `GOCSPX-...{20,}`) for when minification strips the named
//! constants `OAUTH_CLIENT_ID` / `OAUTH_CLIENT_SECRET`.
//!
//! VM verification of v0.4.7 caught the gap (Windows user with
//! gemini-cli installed at `%APPDATA%\npm\node_modules\@google\gemini-
//! cli\bundle\gemini-*.js` — the v0.4.7 path search found nothing
//! because it looked for a file that doesn't exist in modern bundled
//! releases).
//!
//! Best-effort: if no candidate path matches OR the refresh API
//! fails, fall back to v0.4.6 silent-skip behavior. No regression.

use std::path::PathBuf;
use std::time::Duration;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

const TOKEN_REFRESH_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);

/// Legacy direct paths for the unbundled TS-compiled `oauth2.js` file.
/// Probed in order against each install root. Modern monorepo
/// `@google/gemini-cli` 0.4x installs have:
///   - `@google/gemini-cli-core/dist/src/code_assist/oauth2.js`
///     (sibling package — npm 7+ hoists workspace deps to the same
///     `node_modules` parent)
///   - `@google/gemini-cli/node_modules/@google/gemini-cli-core/...`
///     (deep nesting — pnpm/yarn-workspaces / older npm install
///     strategies)
///   - `@google/gemini-cli/dist/src/code_assist/oauth2.js`
///     (legacy/source layout — Homebrew, older releases)
const LEGACY_OAUTH2_JS_REL_PATHS: &[&str] = &[
    "dist/src/code_assist/oauth2.js",
    "node_modules/@google/gemini-cli-core/dist/src/code_assist/oauth2.js",
];

/// Subdirectories within a `gemini-cli` package root to scan for `.js`
/// files when the legacy `oauth2.js` path doesn't exist. Order matters:
/// `bundle/` is the primary location for esbuild-bundled releases.
const SEARCH_SUBDIRS: &[&str] = &["bundle", "dist", "lib"];

/// Hard cap to avoid scanning unrelated huge JS files. Real bundle
/// chunks are well under 10 MB; anything bigger is unlikely to be the
/// gemini-cli OAuth code path.
const MAX_JS_FILE_SIZE: u64 = 10 * 1024 * 1024;

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
    let (client_id, client_secret, discovered_path) = find_oauth_credentials_with_path()
        .ok_or_else(|| {
            "Gemini CLI OAuth credentials not found — checked legacy oauth2.js path and \
             bundle/dist/lib subdirs of all known npm/Homebrew/Nix install roots"
                .to_string()
        })?;
    log::info!(
        "[Gemini] refresh: discovered OAuth client creds at {} (client_id starts {}…)",
        discovered_path.display(),
        client_id.chars().take(12).collect::<String>(),
    );
    post_refresh(&client_id, &client_secret, refresh_token).await
}

/// Walk all candidate gemini-cli install roots and locate the OAuth
/// credential pair. Tries the legacy `oauth2.js` file first (Homebrew /
/// older releases), then walks `<root>/bundle/*.js`, then
/// `<root>/dist/*.js` and `<root>/lib/*.js`. Returns the first
/// `(client_id, client_secret)` pair found in a single file.
///
/// v0.4.9: previously only the legacy direct path was checked, which
/// missed modern esbuild-bundled @google/gemini-cli npm releases where
/// the OAuth code is in hashed chunks under `bundle/`.
///
/// v0.4.10: returns the discovered path alongside the creds so the
/// caller can log it. Also emits per-root probe lines at INFO when
/// nothing matches, so VM verification can pinpoint which roots were
/// even tried (vs. silently ignored because the env var wasn't set).
fn find_oauth_credentials_with_path() -> Option<(String, String, PathBuf)> {
    let roots = collect_gemini_cli_roots();
    log::info!(
        "[Gemini] refresh: probing {} candidate gemini-cli install root(s)",
        roots.len()
    );
    for root in &roots {
        // Try every legacy direct path first — fastest hit when the
        // unbundled TS-compiled oauth2.js is on disk (gemini-cli-core
        // package, or sibling-hoisted at the gemini-cli root, or
        // workspace-nested under gemini-cli/node_modules/...).
        for rel in LEGACY_OAUTH2_JS_REL_PATHS {
            let legacy = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            if let Ok(content) = std::fs::read_to_string(&legacy) {
                if let Some((id, secret)) = extract_oauth_credentials(&content) {
                    log::info!(
                        "[Gemini] refresh: found OAuth pair via legacy direct path {}",
                        legacy.display()
                    );
                    return Some((id, secret, legacy));
                }
            }
        }

        // Walk known JS-containing subdirs for the bundled releases.
        // Recursive in v0.4.11 (was 1-level in v0.4.10 — see
        // scan_dir_recursive).
        for subdir in SEARCH_SUBDIRS {
            let dir = root.join(subdir);
            if let Some((id, secret, hit)) = scan_dir_for_credentials(&dir) {
                return Some((id, secret, hit));
            }
        }
    }
    log::info!(
        "[Gemini] refresh: no OAuth client creds found in any of {} root(s) — first roots tried: {:?}",
        roots.len(),
        roots
            .iter()
            .take(3)
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
    );
    None
}

/// Maximum directory depth for recursive .js scans. Tuned to reach
/// `dist/src/code_assist/oauth2.js` (depth 3 below `<root>/dist`)
/// without descending arbitrarily into vendored sub-packages.
const MAX_SCAN_DEPTH: u32 = 4;

/// Recursively scan `.js` files under `dir` for the OAuth pair.
/// Returns the first match. Skips:
/// - Nested `node_modules/` subdirs (avoid expanding into transitive
///   deps where the same OAuth pattern probably won't match anyway).
/// - Files larger than `MAX_JS_FILE_SIZE`.
/// - Recursion past `MAX_SCAN_DEPTH`.
///
/// v0.4.11 — was non-recursive in v0.4.10 (one level only), which
/// missed `dist/src/code_assist/oauth2.js` in the gemini-cli-core
/// package because the .js file is 3 levels deep under `<root>/dist`.
/// VM verification of v0.4.10 reported 76 .js files scanned in
/// `<root>/bundle` (the bundled chunks correctly walked) but 0 in
/// the gemini-cli-core `dist/` because v0.4.10 didn't descend into
/// `dist/src/code_assist/`.
fn scan_dir_for_credentials(dir: &std::path::Path) -> Option<(String, String, PathBuf)> {
    let mut scanned = 0u32;
    let hit = scan_dir_recursive(dir, 0, &mut scanned);
    if let Some((id, secret, path)) = hit {
        log::info!(
            "[Gemini] refresh: found OAuth pair in {} (after scanning {} .js file(s) recursively under {})",
            path.display(),
            scanned,
            dir.display()
        );
        return Some((id, secret, path));
    }
    if scanned > 0 {
        log::info!(
            "[Gemini] refresh: scanned {} .js file(s) recursively in {} — no OAuth pair matched",
            scanned,
            dir.display()
        );
    }
    None
}

fn scan_dir_recursive(
    dir: &std::path::Path,
    depth: u32,
    scanned: &mut u32,
) -> Option<(String, String, PathBuf)> {
    if depth > MAX_SCAN_DEPTH {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            // Skip vendored deps — they don't ship gemini-cli's OAuth pair
            // and recursing into them is the easy way to turn this into
            // an O(N) filesystem walk on every refresh attempt.
            if path.file_name().and_then(|n| n.to_str()) == Some("node_modules") {
                continue;
            }
            subdirs.push(path);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if meta.len() > MAX_JS_FILE_SIZE {
                continue;
            }
        }
        *scanned += 1;
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Some((id, secret)) = extract_oauth_credentials(&content) {
                return Some((id, secret, path));
            }
        }
    }
    // Files first, then descend (depth-first into each subdir in
    // alphabetical order — ReadDir order is platform-defined, so we
    // get reproducible behavior regardless of FS ordering).
    subdirs.sort();
    for sub in subdirs {
        if let Some(hit) = scan_dir_recursive(&sub, depth + 1, scanned) {
            return Some(hit);
        }
    }
    None
}

/// Top-level gemini-cli install roots to search. Plain dirs (no
/// trailing `oauth2.js`); the caller appends the relevant subpath.
fn collect_gemini_cli_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if cfg!(target_os = "windows") {
        // npm default global: %APPDATA%\npm\node_modules\@google\gemini-cli
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let appdata = PathBuf::from(appdata);
            roots.push(
                appdata
                    .join("npm")
                    .join("node_modules")
                    .join("@google")
                    .join("gemini-cli"),
            );
            // Some npm setups hoist top-level @google deps next to /npm.
            roots.push(
                appdata
                    .join("npm")
                    .join("node_modules")
                    .join("@google")
                    .join("gemini-cli-core"),
            );
        }
        // Less common but valid: nodejs global bundled with %PROGRAMFILES%.
        if let Some(programfiles) = std::env::var_os("PROGRAMFILES") {
            roots.push(
                PathBuf::from(programfiles)
                    .join("nodejs")
                    .join("node_modules")
                    .join("@google")
                    .join("gemini-cli"),
            );
        }
    } else {
        // Mac / Linux common npm-global lib roots (Homebrew, system).
        let lib_roots = ["/usr/local/lib", "/usr/lib", "/opt/homebrew/lib"];
        for base in &lib_roots {
            roots.push(PathBuf::from(base).join("node_modules/@google/gemini-cli"));
            roots.push(PathBuf::from(base).join("node_modules/@google/gemini-cli-core"));
        }
        // User-local npm prefixes
        if let Some(home) = dirs::home_dir() {
            for user_path in &[".npm-global/lib", ".local/lib", ".nvm/versions/node"] {
                roots.push(home.join(user_path).join("node_modules/@google/gemini-cli"));
                roots.push(
                    home.join(user_path)
                        .join("node_modules/@google/gemini-cli-core"),
                );
            }
        }
    }

    roots
}

fn extract_oauth_credentials(content: &str) -> Option<(String, String)> {
    // Named-constant form first — matches Homebrew / source-layout
    // releases where the JS still contains literal:
    //   const OAUTH_CLIENT_ID = '...';
    //   const OAUTH_CLIENT_SECRET = '...';
    static NAMED_ID_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"OAUTH_CLIENT_ID\s*=\s*['"]([\w\-\.]+)['"]"#).unwrap());
    static NAMED_SECRET_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"OAUTH_CLIENT_SECRET\s*=\s*['"]([\w\-]+)['"]"#).unwrap());

    if let (Some(id), Some(secret)) = (
        NAMED_ID_REGEX
            .captures(content)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string())),
        NAMED_SECRET_REGEX
            .captures(content)
            .and_then(|c| c.get(1).map(|m| m.as_str().to_string())),
    ) {
        return Some((id, secret));
    }

    // Value-pattern fallback for esbuild-bundled releases where the
    // constant names have been stripped/renamed by minification but
    // the literal string values remain. The patterns are narrow enough
    // to avoid most false positives:
    //   - Google OAuth client IDs follow:
    //     <9-12 digit project number>-<random>.apps.googleusercontent.com
    //   - Google OAuth client secrets are GOCSPX-<22-char random>
    //     (we require {20,} to skip too-short matches).
    static VALUE_ID_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(\d{9,12}-[\w]+\.apps\.googleusercontent\.com)"#).unwrap());
    static VALUE_SECRET_REGEX: Lazy<Regex> =
        Lazy::new(|| Regex::new(r#"(GOCSPX-[\w\-]{20,})"#).unwrap());

    let id = VALUE_ID_REGEX
        .captures(content)?
        .get(1)?
        .as_str()
        .to_string();
    let secret = VALUE_SECRET_REGEX
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
    fn collect_gemini_cli_roots_returns_at_least_one() {
        // Smoke: the candidate root list shouldn't be empty regardless of OS.
        let roots = collect_gemini_cli_roots();
        assert!(!roots.is_empty());
    }

    // v0.4.9 — bundle/minified chunk discovery.

    #[test]
    fn extract_credentials_from_minified_bundle_chunk() {
        // Real bundle chunk shape: variable names have been stripped /
        // renamed by esbuild minification. Only the literal string
        // values remain. v0.4.9 value-pattern fallback handles this.
        let content = r#"
            var Bn={};Bn.client_id="681255809395-test123.apps.googleusercontent.com";
            var Cn={secret:"GOCSPX-bundled-test-secret-12345"};
        "#;
        let (id, secret) = extract_oauth_credentials(content).unwrap();
        assert_eq!(id, "681255809395-test123.apps.googleusercontent.com");
        assert_eq!(secret, "GOCSPX-bundled-test-secret-12345");
    }

    #[test]
    fn value_fallback_rejects_too_short_secret() {
        // Defensive: GOCSPX- followed by < 20 chars is unlikely to be a
        // real Google client secret. Tightening to {20,} avoids
        // matching unrelated GOCSPX-prefixed substrings.
        let content = r#"
            const X = "1234567890-foo.apps.googleusercontent.com";
            const Y = "GOCSPX-short";
        "#;
        assert!(extract_oauth_credentials(content).is_none());
    }

    #[test]
    fn value_fallback_rejects_non_googleusercontent_domain() {
        // The id regex requires `.apps.googleusercontent.com` suffix,
        // so unrelated `<digits>-<word>.apps.example.com` shapes don't
        // false-match.
        let content = r#"
            const X = "1234567890-foo.apps.example.com";
            const Y = "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-12345";
        "#;
        assert!(extract_oauth_credentials(content).is_none());
    }

    #[test]
    fn named_constant_takes_priority_over_value_fallback() {
        // Both forms present: named constants should win, since they
        // unambiguously identify the OAuth pair (versus value-pattern
        // matching which could pick up the wrong pair from a file
        // containing multiple `<digits>-...apps.googleusercontent.com`
        // strings — e.g. comments documenting other OAuth clients).
        let content = r#"
            // Some other Google API: 999999999999-other.apps.googleusercontent.com
            const OAUTH_CLIENT_ID = '111111111111-real.apps.googleusercontent.com';
            const OAUTH_CLIENT_SECRET = 'GOCSPX-real-secret-here';
        "#;
        let (id, _) = extract_oauth_credentials(content).unwrap();
        // Named regex captures the const value, not the comment value.
        assert_eq!(id, "111111111111-real.apps.googleusercontent.com");
    }

    // v0.4.11 — recursive scan + multiline-assignment shape.

    #[test]
    fn extract_credentials_from_multiline_assignment() {
        // Upstream `gemini-cli-core/dist/src/code_assist/oauth2.js`
        // emits multi-line const assignment after TS->JS compile:
        //   const OAUTH_CLIENT_ID =
        //       '<value>';
        // Rust regex `\s*` matches across newlines by default, so the
        // existing regex SHOULD handle this — this test pins that
        // contract so a future regex tweak can't silently break it.
        let content = "const OAUTH_CLIENT_ID =\n    '681255809395-multi.apps.googleusercontent.com';\nconst OAUTH_CLIENT_SECRET =\n    'GOCSPX-multi-line-here';\n";
        let (id, secret) = extract_oauth_credentials(content).unwrap();
        assert_eq!(id, "681255809395-multi.apps.googleusercontent.com");
        assert_eq!(secret, "GOCSPX-multi-line-here");
    }

    #[test]
    fn scan_dir_recursively_finds_creds_three_levels_deep() {
        // Synthetic gemini-cli-core install layout:
        //   <tmp>/dist/src/code_assist/oauth2.js
        // v0.4.10's non-recursive scan_dir_for_credentials would miss
        // this because <tmp>/dist itself contains no .js files.
        // v0.4.11 walks 3 levels in.
        let tmp = tempfile::tempdir().unwrap();
        let deep = tmp.path().join("dist").join("src").join("code_assist");
        std::fs::create_dir_all(&deep).unwrap();
        let oauth_file = deep.join("oauth2.js");
        std::fs::write(
            &oauth_file,
            "const OAUTH_CLIENT_ID = '111111111111-x.apps.googleusercontent.com';\n\
             const OAUTH_CLIENT_SECRET = 'GOCSPX-deeply-nested-secret-12345';\n",
        )
        .unwrap();
        // Also put a non-matching .js file at the dist top-level to
        // mimic the gemini-cli-core layout where dist/index.js exists
        // but doesn't carry the OAuth pair.
        std::fs::write(tmp.path().join("dist").join("index.js"), "// no creds").unwrap();

        let scan_root = tmp.path().join("dist");
        let hit = scan_dir_for_credentials(&scan_root);
        assert!(
            hit.is_some(),
            "recursive scan should reach oauth2.js 3 dirs deep"
        );
        let (id, secret, path) = hit.unwrap();
        assert_eq!(id, "111111111111-x.apps.googleusercontent.com");
        assert_eq!(secret, "GOCSPX-deeply-nested-secret-12345");
        assert_eq!(path, oauth_file);
    }

    #[test]
    fn scan_dir_recursive_skips_node_modules() {
        // Recursing into transitive deps' node_modules is the easy
        // way to turn this into a 1000-file walk. Verify we skip.
        let tmp = tempfile::tempdir().unwrap();
        let nm = tmp
            .path()
            .join("node_modules")
            .join("some-dep")
            .join("dist");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(
            nm.join("oauth2.js"),
            "const OAUTH_CLIENT_ID = '999999999999-x.apps.googleusercontent.com';\n\
             const OAUTH_CLIENT_SECRET = 'GOCSPX-should-not-be-found-12345';\n",
        )
        .unwrap();
        let hit = scan_dir_for_credentials(tmp.path());
        assert!(
            hit.is_none(),
            "recursive scan must skip node_modules/ — found {:?}",
            hit.as_ref().map(|(_, _, p)| p.display().to_string())
        );
    }

    #[test]
    fn scan_dir_recursive_returns_none_for_missing_dir() {
        // Probing a non-existent directory must not panic — common case
        // is the user has gemini-cli but not gemini-cli-core, or vice
        // versa, so half of the candidate roots are missing on every run.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(scan_dir_for_credentials(&missing).is_none());
    }
}
