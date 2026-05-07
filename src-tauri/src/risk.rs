//! v0.7.0 — Rust port of `helper/provider_adapters/claude.py`'s risk
//! classifier. Used by the Windows-side hook emission binary
//! (`bin/remote_hook.rs`) to decide whether a Claude PermissionRequest
//! should round-trip to the remote-approval channel or fall back
//! immediately to the local prompt (HIGH).
//!
//! Risk scale mirrors the Mac sibling exactly:
//!   * **LOW** — read-only tools (Read, Glob, Grep, WebFetch,
//!     WebSearch, TodoRead, ListMcpResources). Approving these
//!     remotely is generally safe.
//!   * **MEDIUM** — anything else by default (Edit, Write, MCP tool
//!     calls, unrecognised tool names). Server-side approve scope is
//!     limited to "once" for these.
//!   * **HIGH** — Bash commands containing dangerous tokens
//!     (rm -rf, sudo, mkfs, dd if=, fork bomb, shutdown, reboot,
//!     killall, chmod 777 /, curl, wget, ssh, scp, rsync, history
//!     -c, kextload, csrutil). The hook binary fail-closes locally on
//!     HIGH — never round-trips to the remote channel.
//!
//! These are the same tokens Mac flags. Tracked future work
//! (PROJECT_DEV_PLAN_2026-04-29 #9): sensitive-filename blocklist
//! (`.env`, `id_rsa`, `*.pem`, `credentials.json`, `~/.aws/credentials`)
//! that should escalate `cat <file>` from MEDIUM to HIGH. Out of
//! scope for v0.7.0; ports as v0.7.x once Mac ships the same.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    pub fn as_str(&self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Medium => "medium",
            Risk::High => "high",
        }
    }
}

/// Tools that read state but don't change it. Approving these
/// remotely is low risk — we still let the user approve, but we
/// won't auto-deny them.
const LOW_RISK_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "TodoRead",
    "ListMcpResources",
];

/// Classify the risk of a PermissionRequest. Mirrors Mac's
/// `_classify_risk` semantics with v0.7.0 hardening: high-risk
/// matching now normalizes whitespace before comparison so
/// `rm  -rf` (extra space), `rm\t-rf` (tab), `rm   -r   -f /tmp`
/// (split flags) all classify as HIGH (Gemini 3.1 Pro v0.7.0
/// review P1).
///
/// The Mac sibling does naive substring; we go a step further on
/// the desktop because the LLM driving the Bash command is
/// statistically more likely to emit non-canonical whitespace than
/// a human typing in a real terminal. The Mac team can mirror this
/// hardening back via redaction.py if they want; either way the
/// bar is "fail-closed for clearly-destructive operations" not
/// "decide all possible bash invocations correctly."
///
/// `tool_input` is the raw JSON object from the hook input. For
/// `Bash` tool the relevant field is `command` (string); for
/// non-Bash the input shape varies but risk is always MEDIUM
/// unless the tool is in the LOW_RISK_TOOLS set.
pub fn classify_risk(tool_name: &str, tool_input: &Value) -> Risk {
    if LOW_RISK_TOOLS.contains(&tool_name) {
        return Risk::Low;
    }
    if tool_name == "Bash" {
        let command = tool_input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if is_high_risk_bash(command) {
            return Risk::High;
        }
        return Risk::Medium;
    }
    // Edit / Write / MCP / unknown — default medium.
    Risk::Medium
}

/// Whitespace-normalized high-risk classifier for Bash commands.
/// Splits on ANY whitespace (spaces, tabs, newlines), then checks:
///   * Token-level: any whitespace-separated word matches a danger
///     keyword (sudo / mkfs / shutdown / reboot / killall /
///     kextload / csrutil / curl / wget / ssh / scp / rsync /
///     dd — also `rm` when paired with destructive flags)
///   * Pair-level: `rm` followed by any token containing `r` AND
///     `f` flags (covers `rm -rf`, `rm -fr`, `rm -r -f`, `rm -rfv`,
///     etc.)
///   * Substring fallback: fork bomb signature, `chmod 777 /`,
///     `history -c`
///
/// Reasoning: substring matching alone misses `rm  -rf` (double
/// space), `rm\t-rf` (tab), `rm -r -f` (split flags). Token-based
/// matching is whitespace-tolerant by construction.
fn is_high_risk_bash(command: &str) -> bool {
    // Substring matchers — for patterns that span multiple tokens
    // OR contain whitespace as a structural element (fork bomb).
    const SUBSTRING_DANGER: &[&str] = &[
        ":(){ :|:& };:", // fork bomb (whitespace IS the signature)
        "chmod 777 /",   // root-only chmod 777
        "history -c",
    ];
    for s in SUBSTRING_DANGER {
        if command.contains(s) {
            return true;
        }
    }

    // Whitespace-split token list. `tokens` is the bare alphanumeric
    // command words; `flag_tokens` includes flags so we can match
    // `rm <something with r and f flags>`.
    let tokens: Vec<&str> = command.split_whitespace().collect();

    // Single-token danger keywords. `sudo` / `curl` / `wget` /
    // `ssh` / `scp` / `rsync` / `mkfs` / `shutdown` / `reboot` /
    // `killall` / `kextload` / `csrutil` / `dd` are dangerous as
    // bare commands. Match exact token (not prefix) to avoid
    // false positives on `sudoer-config-tool` etc.
    const SINGLE_TOKEN_DANGER: &[&str] = &[
        "sudo", "mkfs", "shutdown", "reboot", "killall", "kextload", "csrutil", "curl", "wget",
        "ssh", "scp", "rsync",
    ];
    for tok in &tokens {
        if SINGLE_TOKEN_DANGER.contains(tok) {
            return true;
        }
        // `dd if=...` — dd is dangerous when invoked with the
        // `if=` source-input flag. Bare `dd` without args isn't
        // useful so we treat any `dd` token as suspect when the
        // command also contains `if=`.
        if *tok == "dd" && command.contains("if=") {
            return true;
        }
    }

    // Pair-level: `rm` followed by any token containing both `r`
    // and `f` flags (covers -rf / -fr / -rfv / -r -f variants).
    for i in 0..tokens.len() {
        if tokens[i] == "rm" {
            // Single-token form `rm -rf <path>`: next token has both
            // r and f.
            if let Some(next) = tokens.get(i + 1) {
                let stripped = next.trim_start_matches('-');
                if stripped.contains('r') && stripped.contains('f') {
                    return true;
                }
            }
            // Split-flag form `rm -r -f <path>`: any later flag
            // tokens collectively contain r and f.
            let mut has_r = false;
            let mut has_f = false;
            for tok in &tokens[(i + 1).min(tokens.len())..] {
                if !tok.starts_with('-') {
                    break; // hit the operand; stop scanning flags
                }
                let stripped = tok.trim_start_matches('-');
                if stripped.contains('r') {
                    has_r = true;
                }
                if stripped.contains('f') {
                    has_f = true;
                }
            }
            if has_r && has_f {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn low_risk_tools_classify_low() {
        for tool in LOW_RISK_TOOLS {
            assert_eq!(classify_risk(tool, &json!({})), Risk::Low, "tool={}", tool);
        }
    }

    #[test]
    fn unknown_tool_defaults_medium() {
        assert_eq!(classify_risk("MyMcpTool", &json!({})), Risk::Medium);
        assert_eq!(classify_risk("Edit", &json!({})), Risk::Medium);
        assert_eq!(classify_risk("Write", &json!({})), Risk::Medium);
    }

    #[test]
    fn bash_with_safe_command_is_medium() {
        // Reading a file is medium under Bash — Read tool would be low,
        // but a bash `cat` is not specifically called out as safe.
        assert_eq!(
            classify_risk("Bash", &json!({"command": "ls -la"})),
            Risk::Medium
        );
        assert_eq!(
            classify_risk("Bash", &json!({"command": "cat README.md"})),
            Risk::Medium
        );
        assert_eq!(
            classify_risk("Bash", &json!({"command": "git status"})),
            Risk::Medium
        );
    }

    #[test]
    fn bash_rm_rf_is_high() {
        assert_eq!(
            classify_risk("Bash", &json!({"command": "rm -rf /tmp/junk"})),
            Risk::High
        );
        assert_eq!(
            classify_risk("Bash", &json!({"command": "rm -fr /var/log/*"})),
            Risk::High
        );
    }

    #[test]
    fn bash_sudo_is_high() {
        assert_eq!(
            classify_risk("Bash", &json!({"command": "sudo apt-get install"})),
            Risk::High
        );
        // Trailing sudo — `find . | sudo xargs rm` etc.
        assert_eq!(
            classify_risk("Bash", &json!({"command": "find . -type f | sudo cat"})),
            Risk::High
        );
    }

    #[test]
    fn bash_curl_is_high_outbound_exfil_concern() {
        assert_eq!(
            classify_risk("Bash", &json!({"command": "curl https://evil.com"})),
            Risk::High
        );
        assert_eq!(
            classify_risk("Bash", &json!({"command": "wget http://example.com/file"})),
            Risk::High
        );
    }

    #[test]
    fn bash_ssh_scp_is_high() {
        assert_eq!(
            classify_risk("Bash", &json!({"command": "ssh user@host"})),
            Risk::High
        );
        assert_eq!(
            classify_risk("Bash", &json!({"command": "scp ./file user@host:/tmp"})),
            Risk::High
        );
        assert_eq!(
            classify_risk("Bash", &json!({"command": "rsync -av ./ remote:/dest"})),
            Risk::High
        );
    }

    #[test]
    fn bash_chmod_777_root_is_high() {
        assert_eq!(
            classify_risk("Bash", &json!({"command": "chmod 777 / -R"})),
            Risk::High
        );
        // chmod 777 of a non-root path — currently NOT high (Mac too).
        // A user setting permissive perms on a project file is medium.
        assert_eq!(
            classify_risk("Bash", &json!({"command": "chmod 777 ./file.sh"})),
            Risk::Medium
        );
    }

    #[test]
    fn bash_fork_bomb_is_high() {
        assert_eq!(
            classify_risk("Bash", &json!({"command": " :(){ :|:& };:"})),
            Risk::High
        );
    }

    #[test]
    fn bash_with_no_command_field_is_medium() {
        // Missing or non-string command — graceful degrade to medium
        // (the request still round-trips; the user just sees a less-
        // specific summary).
        assert_eq!(classify_risk("Bash", &json!({})), Risk::Medium);
        assert_eq!(
            classify_risk("Bash", &json!({"command": null})),
            Risk::Medium
        );
        assert_eq!(classify_risk("Bash", &json!({"command": 42})), Risk::Medium);
    }

    // v0.7.0 Gemini P1 — whitespace-tolerant high-risk matching.
    #[test]
    fn bash_rm_rf_with_extra_whitespace_is_high() {
        // Double space, tabs, split flags — all should classify HIGH.
        for cmd in &[
            "rm  -rf /tmp/junk",  // double space
            "rm\t-rf\t/tmp",      // tab
            "rm -r -f /tmp/junk", // split flags
            "rm -rfv /tmp",       // -rfv (r + f + v)
            "rm -fr /var/log",    // -fr
            "  rm -rf /",         // leading whitespace
        ] {
            assert_eq!(
                classify_risk("Bash", &json!({"command": cmd})),
                Risk::High,
                "should be HIGH: {:?}",
                cmd
            );
        }
    }

    #[test]
    fn bash_rm_without_destructive_flags_is_medium() {
        // `rm file.txt` — no -r or -f. Treat as medium (still
        // destructive but recoverable for a single file).
        assert_eq!(
            classify_risk("Bash", &json!({"command": "rm file.txt"})),
            Risk::Medium
        );
        // `rm -i file.txt` — interactive flag. Not high-risk.
        assert_eq!(
            classify_risk("Bash", &json!({"command": "rm -i file.txt"})),
            Risk::Medium
        );
    }

    #[test]
    fn bash_token_match_doesnt_false_positive_on_substrings() {
        // `sudoer-config-tool` should NOT match `sudo` token —
        // single-token danger requires exact token equality.
        assert_eq!(
            classify_risk(
                "Bash",
                &json!({"command": "./sudoer-config-tool --validate"})
            ),
            Risk::Medium
        );
        // `forecast-curl-stats` should NOT match `curl`.
        assert_eq!(
            classify_risk("Bash", &json!({"command": "./forecast-curl-stats"})),
            Risk::Medium
        );
    }

    #[test]
    fn bash_dd_only_high_with_if_flag() {
        // bare `dd` is dangerous with `if=`; without it it's just a
        // filename. Avoids false-positive on `cd dd-folder/`.
        assert_eq!(
            classify_risk("Bash", &json!({"command": "dd if=/dev/zero of=/tmp/file"})),
            Risk::High
        );
        assert_eq!(
            classify_risk("Bash", &json!({"command": "ls dd-folder/"})),
            Risk::Medium
        );
    }

    #[test]
    fn risk_as_str_matches_server_enum_values() {
        // The server's CHECK constraint on remote_permission_requests.risk
        // accepts exactly these three values. Drift here would fail the
        // RPC at runtime.
        assert_eq!(Risk::Low.as_str(), "low");
        assert_eq!(Risk::Medium.as_str(), "medium");
        assert_eq!(Risk::High.as_str(), "high");
    }
}
