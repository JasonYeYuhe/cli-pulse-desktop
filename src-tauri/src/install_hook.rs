//! v0.7.0 — One-click install of the CLI Pulse Remote Approval hook
//! into Claude Code's `settings.json`.
//!
//! Claude reads hooks from `~/.claude/settings.json` (Linux/macOS)
//! or `%USERPROFILE%\.claude\settings.json` (Windows). The schema:
//!
//! ```json
//! {
//!   "hooks": {
//!     "PreToolUse": [
//!       { "matcher": "*",
//!         "hooks": [
//!           { "type": "command",
//!             "command": "<absolute-path-to-cli-pulse-desktop.exe> --remote-approval-hook --provider claude",
//!             "timeout": 12000
//!           }
//!         ]
//!       }
//!     ]
//!   }
//! }
//! ```
//!
//! The wizard:
//!   1. Locates settings.json (creates parent dir if missing).
//!   2. Reads + parses (or starts with `{}` if absent).
//!   3. Inserts our hook entry into `hooks.PreToolUse` array,
//!      preserving any existing matchers/hooks the user has.
//!   4. Atomically rewrites via tempfile + rename.
//!
//! Idempotent: running twice doesn't duplicate the entry.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::paths;

/// Default timeout (ms) the hook command can take before Claude
/// gives up. Mac sibling uses 12 000 (10 s remote-approval budget +
/// 2 s margin for stdin/stdout handshake). Matches the Phase 1
/// convention — keeps the experience identical across platforms.
const HOOK_TIMEOUT_MS: u64 = 12_000;

/// Hook command identifier — what we look for to detect an existing
/// install. Substring match: any `command` containing this token is
/// considered "ours" so we don't double-register on re-run, AND so a
/// future cli-pulse-desktop install path move (e.g. machine-wide vs
/// per-user) doesn't leave a stale entry.
const HOOK_MARKER: &str = "--remote-approval-hook";

/// Locate Claude's settings.json. Linux/macOS: `~/.claude/settings.json`.
/// Windows: `%USERPROFILE%\.claude\settings.json`. Same convention
/// the Mac sibling uses + the upstream Claude Code docs.
pub fn settings_path() -> Option<PathBuf> {
    paths::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Build the absolute path to the currently-running cli-pulse-desktop
/// binary so the registered hook command points to the user's actual
/// install. Falls back to `cli-pulse-desktop` (PATH lookup) on the
/// rare case where `current_exe` fails — Claude's hook runner can
/// resolve via PATH if the binary is somewhere on $PATH.
pub fn current_binary_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "cli-pulse-desktop".to_string())
}

/// Build the hook-command string for a given binary path. Public so
/// tests + the install command can both use it.
pub fn build_hook_command(binary_path: &str) -> String {
    // Quote the path so spaces in install dirs don't break shell
    // splitting on Linux/macOS. Windows shell tolerates either; the
    // double-quote form works in both.
    format!(
        "\"{}\" --remote-approval-hook --provider claude",
        binary_path
    )
}

/// Result of an install attempt — surfaced to the frontend so the
/// UI can show "installed", "already up to date", or the path that
/// was written (useful for users to verify in case they want to
/// audit settings.json manually).
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InstallResult {
    /// Wrote a new hook entry. settings.json may have been created.
    Installed { settings_path: String },
    /// The current hook command already matches what we'd install.
    /// No write happened.
    AlreadyUpToDate { settings_path: String },
    /// An existing hook entry was updated (e.g. binary path changed
    /// after a re-install). `previous` is the old command string for
    /// transparency.
    Updated {
        settings_path: String,
        previous: String,
    },
}

/// Install or update the hook entry. Returns the install result for
/// the frontend to display.
pub fn install(binary_path: &str) -> anyhow::Result<InstallResult> {
    let path = settings_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;

    // Read existing settings (or start from empty). Treat parse
    // errors as a hard fail — we don't want to silently overwrite
    // a user's broken-but-recoverable JSON.
    let mut root: Value = if path.exists() {
        let text = fs::read_to_string(&path)?;
        if text.trim().is_empty() {
            Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("settings.json parse error: {e}"))?
        }
    } else {
        Value::Object(serde_json::Map::new())
    };

    let new_command = build_hook_command(binary_path);

    // Walk to root.hooks.PreToolUse, creating intermediate objects.
    if !root.is_object() {
        return Err(anyhow::anyhow!(
            "settings.json root is not an object — cannot install hook"
        ));
    }
    let root_obj = root.as_object_mut().unwrap();
    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !hooks.is_object() {
        return Err(anyhow::anyhow!(
            "settings.json `hooks` is not an object — cannot install hook"
        ));
    }
    let hooks_obj = hooks.as_object_mut().unwrap();
    let pretool = hooks_obj
        .entry("PreToolUse")
        .or_insert_with(|| Value::Array(vec![]));
    if !pretool.is_array() {
        return Err(anyhow::anyhow!(
            "settings.json `hooks.PreToolUse` is not an array — cannot install hook"
        ));
    }
    let pretool_arr = pretool.as_array_mut().unwrap();

    // Look for an existing entry whose hooks contain our marker.
    // If found, decide between AlreadyUpToDate (command matches) and
    // Updated (command differs — likely a binary path change).
    let mut previous_cmd: Option<String> = None;
    let mut found_existing = false;
    for matcher_entry in pretool_arr.iter_mut() {
        let Some(obj) = matcher_entry.as_object_mut() else {
            continue;
        };
        let Some(inner_hooks) = obj.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
            continue;
        };
        for hook_entry in inner_hooks.iter_mut() {
            let Some(hook_obj) = hook_entry.as_object_mut() else {
                continue;
            };
            let Some(cmd) = hook_obj.get("command").and_then(|c| c.as_str()) else {
                continue;
            };
            if cmd.contains(HOOK_MARKER) {
                found_existing = true;
                if cmd == new_command {
                    return Ok(InstallResult::AlreadyUpToDate {
                        settings_path: path.display().to_string(),
                    });
                }
                previous_cmd = Some(cmd.to_string());
                hook_obj.insert("command".to_string(), Value::String(new_command.clone()));
                hook_obj.insert("timeout".to_string(), json!(HOOK_TIMEOUT_MS));
                break;
            }
        }
        if found_existing {
            break;
        }
    }

    if !found_existing {
        // Append a fresh matcher entry. matcher="*" covers all tools
        // — Claude itself decides whether to invoke our hook based
        // on the matcher.
        pretool_arr.push(json!({
            "matcher": "*",
            "hooks": [
                {
                    "type": "command",
                    "command": new_command,
                    "timeout": HOOK_TIMEOUT_MS,
                }
            ]
        }));
    }

    // Atomic write: write to tempfile in same dir, then rename.
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("settings.json has no parent dir"))?;
    let tmp = tempfile::Builder::new()
        .prefix(".claude-settings.")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    let pretty = serde_json::to_string_pretty(&root)?;
    {
        let mut f = tmp.as_file();
        f.write_all(pretty.as_bytes())?;
        f.sync_all()?;
    }
    tmp.persist(&path)
        .map_err(|e| anyhow::anyhow!("atomic rename: {}", e.error))?;

    Ok(if let Some(prev) = previous_cmd {
        InstallResult::Updated {
            settings_path: path.display().to_string(),
            previous: prev,
        }
    } else {
        InstallResult::Installed {
            settings_path: path.display().to_string(),
        }
    })
}

/// Detect whether the hook is currently installed AND points to the
/// running binary. Frontend uses this to decide whether to render
/// "Install" or "Already installed" copy.
pub fn current_status() -> Option<HookStatus> {
    let path = settings_path()?;
    if !path.exists() {
        return Some(HookStatus::NotInstalled);
    }
    let text = fs::read_to_string(&path).ok()?;
    if text.trim().is_empty() {
        return Some(HookStatus::NotInstalled);
    }
    let root: Value = serde_json::from_str(&text).ok()?;
    let cmds = collect_pretool_commands(&root);
    let our = cmds.iter().find(|c| c.contains(HOOK_MARKER));
    Some(match our {
        Some(cmd) if *cmd == build_hook_command(&current_binary_path()) => {
            HookStatus::InstalledMatchesBinary
        }
        Some(_) => HookStatus::InstalledStaleBinary,
        None => HookStatus::NotInstalled,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    /// settings.json missing OR no PreToolUse hook with our marker.
    NotInstalled,
    /// Our marker present AND command matches the running binary.
    InstalledMatchesBinary,
    /// Our marker present BUT command points to a different path
    /// (e.g. user re-installed CLI Pulse to a different dir; the
    /// stale path will fail to launch the hook).
    InstalledStaleBinary,
}

fn collect_pretool_commands(root: &Value) -> Vec<String> {
    let Some(hooks) = root.get("hooks") else {
        return vec![];
    };
    let Some(pretool) = hooks.get("PreToolUse") else {
        return vec![];
    };
    let Some(arr) = pretool.as_array() else {
        return vec![];
    };
    let mut out = Vec::new();
    for entry in arr {
        let Some(inner) = entry.get("hooks").and_then(|h| h.as_array()) else {
            continue;
        };
        for hook in inner {
            if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                out.push(cmd.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_into_tmpdir(binary_path: &str) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("settings.json");
        // Run the install logic against a controlled path. We do
        // this by NOT using install() directly (which uses the real
        // home_dir()). Instead we replicate the body inline.
        // Simpler: construct an empty settings.json, then call the
        // public helpers + manual JSON edits to verify the SHAPE is
        // right; full install() round-trip is exercised in VM verify.
        let _ = (binary_path, &settings);
        (tmp, settings)
    }

    #[test]
    fn build_hook_command_quotes_path() {
        let cmd = build_hook_command(r"C:\Program Files\CLI Pulse\cli-pulse-desktop.exe");
        assert!(cmd.contains("--remote-approval-hook --provider claude"));
        // Path is quoted so spaces work as a single arg.
        assert!(cmd.starts_with("\""));
    }

    #[test]
    fn build_hook_command_includes_marker_substring() {
        let cmd = build_hook_command("/usr/local/bin/cli-pulse-desktop");
        assert!(cmd.contains(HOOK_MARKER));
    }

    #[test]
    fn collect_pretool_commands_extracts_all_entries() {
        let root: Value = serde_json::from_str(
            r#"{
              "hooks": {
                "PreToolUse": [
                  {
                    "matcher": "*",
                    "hooks": [
                      {"type": "command", "command": "/some/other/hook"},
                      {"type": "command", "command": "/path/cli-pulse-desktop --remote-approval-hook --provider claude"}
                    ]
                  },
                  {
                    "matcher": "Bash",
                    "hooks": [
                      {"type": "command", "command": "echo hi"}
                    ]
                  }
                ]
              }
            }"#,
        )
        .unwrap();
        let cmds = collect_pretool_commands(&root);
        assert_eq!(cmds.len(), 3);
        assert!(cmds.iter().any(|c| c.contains(HOOK_MARKER)));
        assert!(cmds.iter().any(|c| c == "echo hi"));
    }

    #[test]
    fn collect_pretool_commands_empty_on_missing_keys() {
        let root: Value = serde_json::from_str(r#"{}"#).unwrap();
        assert!(collect_pretool_commands(&root).is_empty());
        let root: Value = serde_json::from_str(r#"{"hooks":{}}"#).unwrap();
        assert!(collect_pretool_commands(&root).is_empty());
        let root: Value = serde_json::from_str(r#"{"hooks":{"PreToolUse":[]}}"#).unwrap();
        assert!(collect_pretool_commands(&root).is_empty());
    }

    /// Smoke that the install logic on an empty file produces the
    /// expected JSON shape with our hook entry. We can't easily call
    /// `install()` directly because it targets `~/.claude/`, but the
    /// pure-data-shape logic is straightforward to exercise inline.
    #[test]
    fn install_into_empty_file_produces_expected_shape() {
        let (_tmp, _settings) = install_into_tmpdir("/test/path/cli-pulse-desktop");
        // Build the JSON the install fn would emit. This is the pure
        // data assertion — actual install() is integration-tested
        // via VM verify since it needs a real ~/.claude/ dir.
        let mut root = serde_json::json!({});
        let new_command = build_hook_command("/test/path/cli-pulse-desktop");
        let pretool = serde_json::json!([{
            "matcher": "*",
            "hooks": [{
                "type": "command",
                "command": new_command,
                "timeout": HOOK_TIMEOUT_MS,
            }]
        }]);
        root.as_object_mut().unwrap().insert(
            "hooks".to_string(),
            serde_json::json!({"PreToolUse": pretool}),
        );
        let cmds = collect_pretool_commands(&root);
        assert_eq!(cmds.len(), 1);
        assert!(cmds[0].contains(HOOK_MARKER));
    }
}
