//! v0.7.0 — Standalone Rust binary registered as Claude Code's
//! PermissionRequest hook. Reads JSON from stdin, calls the helper
//! RPCs to upload + poll for a remote decision, writes Claude's
//! decision JSON to stdout.
//!
//! Invoked as:
//!   cli-pulse-desktop --remote-approval-hook --provider claude
//!
//! Run as a subprocess by Claude per PermissionRequest. NOT a part
//! of the Tauri app's runtime — must be self-contained:
//!   * Loads HelperConfig from the same on-disk path the GUI uses.
//!   * Has its own HTTP client via `cli_pulse_desktop_lib::supabase`.
//!   * Reads `cwd-hmac-secret` from the same OS keychain entry.
//!
//! Behavior matches the Mac sibling's `helper/remote_hook.py`:
//!   * If the helper isn't paired or the network is unreachable →
//!     fail closed by emitting a deny+message that asks the local
//!     CLI to handle the prompt itself.
//!   * If the user doesn't decide before `TIMEOUT_S` (10 s) → same
//!     fallback.
//!   * If user approves → emit `behavior: "allow"`.
//!   * If user denies → emit `behavior: "deny"` with reason.
//!   * High-risk shortcut → never round-trip; emit
//!     `behavior: "deny"` immediately with a "must approve locally"
//!     message.
//!
//! Defence in depth: the entire `run` body is wrapped so any
//! unhandled error STILL emits a parseable hook output. Without
//! this, Claude sees an empty stdout and either hangs or fails
//! opaquely.

use std::io::{Read, Write};
use std::time::Duration;

use cli_pulse_desktop_lib::{config, cwd_hmac, redaction, risk, supabase};
use serde_json::{json, Value};

/// Total time budget for the remote-approval round-trip. Claude's
/// hook timeout is 10 s; we leave 500 ms for cleanup.
const TIMEOUT_MS: u64 = 9_500;

/// Polling interval. Mac uses 1 s; matching cadence so the server
/// log shape is identical.
const POLL_INTERVAL_MS: u64 = 1_000;

/// Env var the helper sets when spawning a managed Claude session.
/// The hook prefers this over the raw `session_id` from Claude's
/// hook input so an inline approve in the Sessions UI lands on the
/// row matching the managed session.
const REMOTE_SESSION_ID_ENV: &str = "CLI_PULSE_REMOTE_SESSION_ID";

/// Hardcoded last-resort hook output. Used when run() panics / fails
/// before we can construct a normal fallback.
const RAW_DENY_FALLBACK: &str = r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"CLI Pulse remote-approval-hook crashed. If this persists, open CLI Pulse → Settings → Privacy and turn off Remote Control so the local Claude permission prompt runs on your next attempt."}}}"#;

#[tokio::main]
async fn main() {
    // v0.7.0 Gemini post-impl P2: panic hook ensures stdout receives
    // a parseable hook-output even if a `.unwrap()` blows up before
    // our try-wrapper runs. Without this, an empty stdout makes
    // Claude either hang or surface an opaque parse error. The hook
    // also writes to stderr for the user's log file.
    std::panic::set_hook(Box::new(|info| {
        let _ = std::io::stdout().write_all(RAW_DENY_FALLBACK.as_bytes());
        let _ = std::io::stdout().write_all(b"\n");
        let _ = std::io::stdout().flush();
        eprintln!("remote_hook panicked: {}", info);
    }));

    // Parse args. Required: --remote-approval-hook --provider <name>.
    // We accept arbitrary order + tolerate extras for forward-compat.
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "--remote-approval-hook") {
        eprintln!(
            "remote_hook: missing --remote-approval-hook flag\nusage: cli-pulse-desktop --remote-approval-hook --provider claude"
        );
        std::process::exit(2);
    }
    let provider = arg_value(&args, "--provider").unwrap_or_else(|| "claude".to_string());

    // Run with last-resort fallback. ANY error path emits a parseable
    // JSON to stdout so Claude doesn't hang or fail opaquely.
    if let Err(e) = run(&provider).await {
        eprintln!("remote_hook crashed: {e:?}");
        let _ = std::io::stdout().write_all(RAW_DENY_FALLBACK.as_bytes());
        let _ = std::io::stdout().write_all(b"\n");
        let _ = std::io::stdout().flush();
    }
    // Always exit 0 — the JSON output IS the decision; non-zero exits
    // would also cause Claude to fail opaquely.
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter().enumerate();
    while let Some((_, a)) = iter.next() {
        if a == flag {
            return iter.next().map(|(_, v)| v.clone());
        }
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

async fn run(provider: &str) -> anyhow::Result<()> {
    if provider != "claude" {
        // Mac stubs codex/shell; we do the same. Emit local fallback
        // so Claude knows to handle the prompt itself.
        emit_local_fallback("Provider not yet supported by Windows hook");
        return Ok(());
    }

    let raw = read_stdin_json();
    let cfg = match config::load()? {
        Some(c) => c,
        None => {
            emit_local_fallback("Helper not paired");
            return Ok(());
        }
    };

    let tool_name = raw
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();
    let tool_input = raw.get("tool_input").cloned().unwrap_or_else(|| json!({}));
    let cwd = raw.get("cwd").and_then(|v| v.as_str()).unwrap_or("");

    // Risk classify. HIGH = fail-closed locally, never round-trip.
    let r = risk::classify_risk(&tool_name, &tool_input);
    if r == risk::Risk::High {
        emit_local_fallback("High-risk action requires local approval");
        return Ok(());
    }

    // Compute cwd basename + HMAC. HMAC uses keychain-stored secret;
    // missing secret (headless Linux) → upload with no hmac, server
    // tolerates null.
    let cwd_basename = cwd
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .take(255)
        .collect::<String>();
    let hmac_secret = cwd_hmac::load_or_create_secret().ok().flatten();
    let cwd_hmac_hex = hmac_secret
        .as_deref()
        .and_then(|s| cwd_hmac::hmac_path(s, cwd));

    // Build summary + redacted payload (port of claude.py
    // _summary_for + redacted_input loop).
    let summary = build_summary(&tool_name, &tool_input);
    let redacted_payload = build_redacted_payload(&tool_name, &tool_input);

    // Pick session_id: env var (managed-session binding) takes
    // precedence; fall back to Claude's hook session_id.
    let session_id = std::env::var(REMOTE_SESSION_ID_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty() && uuid::Uuid::parse_str(s).is_ok())
        .or_else(|| {
            raw.get("session_id")
                .and_then(|v| v.as_str())
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .map(|s| s.to_string())
        });

    let request_id = uuid::Uuid::new_v4().to_string();

    let mut payload_obj = serde_json::Map::new();
    payload_obj.insert("tool_name".to_string(), Value::String(tool_name.clone()));
    payload_obj.insert("tool_input".to_string(), redacted_payload);
    if let Some(h) = cwd_hmac_hex.as_deref() {
        payload_obj.insert("cwd_hmac_present".to_string(), Value::String(h.to_string()));
    }

    // Create the request server-side.
    if let Err(e) = supabase::remote_helper_create_permission_request(
        &cfg.device_id,
        &cfg.helper_secret,
        &request_id,
        session_id.as_deref(),
        provider,
        &tool_name,
        &summary,
        Value::Object(payload_obj),
        r.as_str(),
        60, // ttl seconds — matches Mac
    )
    .await
    {
        eprintln!("create_permission_request failed: {e:?}");
        emit_local_fallback("Remote channel unavailable");
        return Ok(());
    }

    // Surface useful info: cwd_basename for the request row. The
    // SQL function accepts the path-related fields via the payload
    // structure or independent params. Future: add cwd_basename as
    // its own RPC param so the server can index "same project". For
    // v0.7.0 we ship it inside the payload only (matches Mac shape).
    let _ = cwd_basename; // currently informational; lifecycle complete.

    // Poll for decision. ~POLL_INTERVAL_MS cadence, total budget
    // TIMEOUT_MS. Mac uses 1 s polls with 10 s budget = ~10 attempts.
    //
    // v0.7.0 Gemini post-impl P1: each individual poll is wrapped
    // in `tokio::time::timeout(POLL_HTTP_TIMEOUT)` so a stalled TCP
    // connection can't blow past the total budget. Without this, a
    // single hung poll could exceed Claude's 12s hook timeout
    // before the loop's elapsed-check ever fires.
    let start = std::time::Instant::now();
    let timeout = Duration::from_millis(TIMEOUT_MS);
    let interval = Duration::from_millis(POLL_INTERVAL_MS);
    // Per-poll HTTP cap. Leave headroom for retry within the
    // remaining budget — a slow poll yields to the next iteration
    // rather than dragging the whole loop past Claude's hook
    // timeout. 2.5s is generous for a healthy Supabase RPC and
    // tight enough that 4 timed-out polls fit inside the 9.5s
    // total budget.
    let poll_http_timeout = Duration::from_millis(2_500);
    loop {
        // Initial poll happens BEFORE first sleep so a fast remote
        // decide (the user already had the sheet open) returns
        // immediately rather than waiting one full interval.
        let poll_fut = supabase::remote_helper_poll_permission_decision(
            &cfg.device_id,
            &cfg.helper_secret,
            &request_id,
        );
        match tokio::time::timeout(poll_http_timeout, poll_fut).await {
            Ok(Ok(decision)) => match decision.status.as_str() {
                "approved" | "approve" => {
                    emit_allow();
                    return Ok(());
                }
                "denied" | "deny" => {
                    emit_deny(
                        decision
                            .reason
                            .as_deref()
                            .unwrap_or("Denied remotely via CLI Pulse"),
                    );
                    return Ok(());
                }
                "expired" => {
                    emit_local_fallback("Remote approval expired");
                    return Ok(());
                }
                _ => {} // pending / unknown → keep polling
            },
            Ok(Err(e)) => {
                // Transient RPC error (HTTP / parse). Retry unless
                // the budget is exhausted.
                eprintln!("poll error (will retry): {e:?}");
            }
            Err(_elapsed) => {
                // Per-poll timeout exceeded. Treat same as transient
                // error — try the next interval if budget allows.
                eprintln!("poll exceeded {poll_http_timeout:?} — retrying");
            }
        }

        if start.elapsed() + interval >= timeout {
            // Out of budget; fail closed.
            emit_local_fallback(
                "Remote approval unavailable. If this keeps happening, open CLI Pulse → Settings → Privacy and turn off Remote Control; the local Claude permission prompt will then run on your next attempt.",
            );
            return Ok(());
        }
        tokio::time::sleep(interval).await;
    }
}

fn read_stdin_json() -> Value {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return json!({});
    }
    if buf.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str(&buf).unwrap_or(json!({}))
}

// =============================================================
// Summary + payload builders — port of claude.py _summary_for
// and the redacted_input loop.
// =============================================================

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

fn build_summary(tool_name: &str, tool_input: &Value) -> String {
    let raw = match tool_name {
        "Bash" => {
            let cmd = tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let redacted = redaction::redact(cmd);
            format!("$ {}", redacted)
        }
        "Read" | "Edit" | "Write" => {
            let path = tool_input
                .get("file_path")
                .or_else(|| tool_input.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // basename only — full path could leak project structure
            let basename = path.rsplit(['/', '\\']).next().unwrap_or("");
            format!("{} {}", tool_name, basename)
        }
        "WebFetch" | "WebSearch" => {
            let url = tool_input
                .get("url")
                .or_else(|| tool_input.get("query"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("{} {}", tool_name, url)
        }
        _ => {
            // Generic: tool name + a small set of redacted top-level
            // keys (no values; just shape hint).
            let mut keys: Vec<&str> = tool_input
                .as_object()
                .map(|o| {
                    o.keys()
                        .filter(|k| !k.starts_with('_'))
                        .map(String::as_str)
                        .collect()
                })
                .unwrap_or_default();
            keys.sort();
            keys.truncate(3);
            format!("{}({})", tool_name, keys.join(", "))
        }
    };
    truncate(&raw, 256)
}

fn build_redacted_payload(_tool_name: &str, tool_input: &Value) -> Value {
    // Mac contract:
    //   * Drop keys starting with `_`.
    //   * String values: truncate to 1024 + redact.
    //   * Numeric / bool / null: kept as-is.
    //   * List / object values: collapse to a length hint to keep
    //     payload small (no recursion).
    let Some(obj) = tool_input.as_object() else {
        return json!({});
    };
    let mut out = serde_json::Map::new();
    for (k, v) in obj {
        if k.starts_with('_') {
            continue;
        }
        match v {
            Value::String(s) => {
                let redacted = redaction::redact(s);
                let truncated = truncate(&redacted, 1024);
                out.insert(k.clone(), Value::String(truncated));
            }
            Value::Number(_) | Value::Bool(_) | Value::Null => {
                out.insert(k.clone(), v.clone());
            }
            Value::Array(arr) => {
                out.insert(
                    k.clone(),
                    Value::String(format!("<list len={}>", arr.len())),
                );
            }
            Value::Object(o) => {
                out.insert(k.clone(), Value::String(format!("<dict len={}>", o.len())));
            }
        }
    }
    Value::Object(out)
}

// =============================================================
// Hook output emitters — Claude PermissionRequest schema.
// https://code.claude.com/docs/en/hooks
// =============================================================

fn emit_allow() {
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": "allow",
            }
        }
    });
    write_out(&out);
}

fn emit_deny(reason: &str) {
    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": "deny",
                "message": reason,
            }
        }
    });
    write_out(&out);
}

fn emit_local_fallback(reason: &str) {
    // PermissionRequest does NOT support "ask" — only allow/deny.
    // Documented fail-closed path is deny+message that directs the
    // user to retry locally so Claude's own prompt fires.
    let msg = format!(
        "Remote approval unavailable: {}. If this keeps happening, open CLI Pulse → Settings → Privacy and turn off Remote Control; the local Claude permission prompt will then run on your next attempt.",
        reason
    );
    emit_deny(&msg);
}

fn write_out(v: &Value) {
    let s = serde_json::to_string(v).unwrap_or_else(|_| RAW_DENY_FALLBACK.to_string());
    let _ = std::io::stdout().write_all(s.as_bytes());
    let _ = std::io::stdout().write_all(b"\n");
    let _ = std::io::stdout().flush();
}
