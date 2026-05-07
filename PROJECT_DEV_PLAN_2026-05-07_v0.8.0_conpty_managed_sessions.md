# Dev Plan — v0.8.0 ConPTY Managed-Session Local Host

**Date:** 2026-05-07
**Type:** new feature, minor version bump (v0.7.0 → v0.8.0)
**Reviewers:** Gemini 3.1 Pro (plan + diff)
**Trigger:** Mac team's `helper/transports/conpty.py` stub explicitly waited for the cli-pulse-desktop track. Slice 1-3 (v0.6.0/v0.6.2/v0.7.0) shipped the app + hook surfaces; Slice 4 closes the loop by making Windows able to actually HOST a managed Claude session that another device's UI is driving.

## What this ships (Slice 4)

When the user (from their iOS app or another Mac) opens the Sessions UI and clicks "Spawn Claude session on Windows-Box":
1. iOS calls `remote_app_request_session_start(p_device_id, p_provider="claude", ...)` server-side
2. Server inserts `remote_sessions(status='pending')` + `remote_session_commands(kind='start', payload=…)`
3. **(this slice)** Windows desktop's agent loop pulls the `start` command via `remote_helper_pull_commands`, spawns Claude under ConPTY, sets `CLI_PULSE_REMOTE_SESSION_ID` env var, registers session via `remote_helper_register_session(status='running')`
4. iOS sees status flip to `running`; user can now Send / Stop / Interrupt via `remote_app_send_command` (already shipped in v0.6.2)
5. **(this slice)** Windows agent receives the prompt commands, writes to ConPTY stdin
6. **(this slice)** PermissionRequests fired by the spawned Claude inherit the env var → bound to the managed session in `remote_permission_requests.session_id` → user's iOS sees them as "this session's pending approvals"

This makes Windows a peer to Mac in the local-host role. Before this slice, only Mac could host managed sessions (since only Mac has `helper/transports/posix_pty.py` shipped).

## Module decomposition

### New Rust modules

```
src-tauri/src/remote/
  mod.rs          — module root, re-exports
  transport.rs    — SessionTransport trait + ConPtyTransport (port of Mac SessionTransport ABC + portable-pty wrapper)
  agent.rs        — RemoteAgentManager (port of Mac RemoteAgentManager; per-session state map + tick loop)
  events.rs       — Event batcher + uploader (lifecycle status rows now; stdout tail in v0.8.x)
  log.rs          — Shared file logger for both bin/remote_hook.rs AND the agent (folds in
                    feedback_remote_hook_diagnostic_blind_spot's v0.7.1 hotfix scope)
```

Plus 4 new helper RPC wrappers in `src-tauri/src/supabase.rs` (all already-live RPCs):
- `remote_helper_register_session(p_device_id, p_helper_secret, p_session_id, p_provider, p_cwd_basename, p_cwd_hmac, p_client_label)`
- `remote_helper_pull_commands(p_device_id, p_helper_secret, p_max)`
- `remote_helper_post_event(p_device_id, p_helper_secret, p_session_id, p_seq, p_kind, p_payload)`
- `remote_helper_complete_command(p_device_id, p_helper_secret, p_command_id, p_status, p_error)`

### portable-pty crate

Use `portable-pty 0.8` (mature, used by VS Code's terminal, alacritty, wezterm). Cross-platform — Win10+ ConPTY, Win older WinPTY fallback (we don't need that since v0.7.0 already requires modern Tauri 2 / Webview2 = Win10+).

Surface used:
- `native_pty_system()` → `PtySystem` trait object (auto-picks ConPTY/WinPTY)
- `PtySystem::openpty(PtySize { rows, cols, pixel_width, pixel_height })` → `(MasterPty, SlavePty)`
- `MasterPty::spawn_command(CommandBuilder)` → `Box<dyn Child + Send>`
- `MasterPty::take_writer() -> Box<dyn Write + Send>` (stdin)
- `MasterPty::try_clone_reader() -> Box<dyn Read + Send>` (stdout)
- `Child::kill()` + `Child::wait()`

For SIGINT-equivalent on Windows: `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)` against the child process group ID. portable-pty doesn't expose this directly — we use the `windows-sys` crate (already a transitive dep via `keyring`) for the FFI.

## Cross-platform contract (matches Mac `SessionTransport` ABC)

```rust
pub trait SessionTransport: Send + Sync {
    fn start(&self, session_id: &str, argv: &[String], env: HashMap<String, String>, cwd: Option<&str>) -> Result<SessionHandle>;
    fn write_stdin(&self, handle: &SessionHandle, data: &[u8]) -> Result<usize>;
    fn read_stdout(&self, handle: &SessionHandle, max_bytes: usize) -> Result<Vec<u8>>;
    fn try_wait(&self, handle: &SessionHandle) -> Result<Option<i32>>; // non-blocking
    fn interrupt(&self, handle: &SessionHandle) -> Result<()>;
    fn terminate(&self, handle: &SessionHandle) -> Result<()>;
    fn close(&self, handle: SessionHandle) -> Result<()>;
}

pub struct SessionHandle {
    pub session_id: String,
    inner: Arc<HandleInner>, // ConPTY-specific state
}
```

`SessionHandle` is `Send + Sync + Clone` so the agent loop can hand a clone to a per-session reader thread.

Mac's `read_stdout` is blocking-up-to-timeout via `select()`. For Windows we use `try_clone_reader` on a dedicated `std::thread::spawn` per session that pumps into a `tokio::sync::mpsc::channel`, and `read_stdout` is non-blocking (drains channel buffer).

## Agent tick design

`RemoteAgentManager` runs as a `tokio::spawn` task in the Tauri app's runtime, **separate from** the existing `spawn_background_sync` (which runs every 120s for helper_sync). Cadence: 1s — same as Mac.

```rust
pub struct RemoteAgentManager {
    transport: Arc<dyn SessionTransport>,
    sessions: Arc<Mutex<HashMap<String, ManagedSession>>>,
    config: HelperConfig,  // device_id + helper_secret
    stop: Arc<AtomicBool>,
}

impl RemoteAgentManager {
    pub async fn tick(&self, max_commands: u32) -> Result<()> {
        // 1. Pull pending commands via remote_helper_pull_commands
        // 2. Dispatch by kind: start | prompt | stop | interrupt
        // 3. For each running session: try_wait; if exited, post status event,
        //    complete the originating start_command, drop from map
        // 4. Drain per-session reader channels (in-memory only for v0.8.0 —
        //    stdout/stderr upload deferred to v0.8.x)
    }
}
```

Stop-responsive shutdown: same pattern as `spawn_background_sync` (v0.4.23 — 100ms `poll_stop_signal`). When stop flag fires:
- Loop exits
- All managed sessions get `transport.terminate(&handle)` → graceful exit
- Final lifecycle event posted: `kind=stopped`, `payload="app_shutdown"`

## Event flow

For v0.8.0 we ship LIFECYCLE EVENTS ONLY (matches Mac iter1):
- `kind=running` posted when session start succeeds
- `kind=stopped` posted when child exits cleanly
- `kind=errored` posted when transport raises or child exits non-zero
- `kind=info` posted for app-shutdown / orphan-cleanup

Stdout/stderr tail upload is **explicitly deferred to v0.8.x** (Mac iter1 also defers — they have the 4 KB cap plumbing in but don't use it). Capping to lifecycle keeps the v0.8.0 wire-shape simple + matches Mac for cross-platform parity.

## Frontend additions

### Sessions tab — "Start new session" CTA

Add a button at the top of `RemoteSessionsSection` (only visible when `enabled === true && remoteSessions.length === 0` OR a "+ Spawn" button when there's at least one session). Click:
- Opens a small dialog: "Spawn a Claude session in this directory"
- User enters cwd basename (or it auto-detects via the OS file picker)
- Calls `request_remote_session_start` Tauri command (wraps `remote_app_request_session_start`)
- The session row appears in the list (status=pending) within 1 tick
- When the agent picks it up + transport.start succeeds, status flips to running

NOTE: per the plan-from-Mac, `request_session_start` is the APP-SIDE RPC. So this button on Windows sends a `start` command to **whichever paired device hosts that cwd-hash** — which could be the SAME Windows machine if the user is on Windows. The agent loop detects "this device received a start command for itself" and spawns locally. Cross-device routing is via cwd_hmac matching (deferred to a future iter; in v0.8.0 the user explicitly picks WHERE to spawn via a device dropdown).

### Settings → About — agent diagnostic snapshot

Show:
- "Managed sessions running: N"
- "Agent loop last tick: N seconds ago"
- "Sessions hosted lifetime: N"

For v0.8.0 just append to the existing `diagnostic_snapshot` Tauri command.

## i18n (~10 new keys × 3 languages)

```
remote.session_start_button: "Start new session"
remote.session_start_dialog_title: "Spawn a Claude session"
remote.session_start_cwd_label: "Working directory"
remote.session_start_cwd_picker: "Browse…"
remote.session_start_provider_label: "Provider"
remote.session_start_submit: "Start"
remote.session_start_processing: "Starting…"
remote.session_start_failed: "Couldn't start session: {{err}}"
settings.agent_status_heading: "Agent diagnostic"
settings.agent_status_running: "{{count}} managed session(s) running"
settings.agent_status_last_tick: "Last tick {{age}} ago"
```

## Tests target

- Backend: ~30 new tests
  - transport: 8 tests covering start / write / read / interrupt / terminate / close + error paths (run against real ConPTY in CI? Or feature-gate to a Mock impl for CI portability?)
  - agent: 12 tests covering pull → dispatch → state-map updates, lifecycle event ordering, stop-responsive shutdown
  - events: 5 tests covering seq increment, batch shape
  - log: 3 tests covering file rotation + concurrent appender
  - 2 tests on the existing helper-RPC wrappers' wire shape pin
- Frontend: ~5 tests
  - i18n pin every new key in critical-labels
  - 2 component tests for the spawn dialog state machine

Total tests: 249 → ~285 (+36).

## Reviewer questions for Gemini

1. **Tick cadence vs latency**: 1s tick cadence means Send commands take ~500ms median to dispatch. Does that feel responsive enough for an interactive session, or should the agent maintain a long-poll connection to `remote_helper_pull_commands`?
2. **Reader thread per session**: each managed session gets a dedicated OS thread for `read_stdout`. With ~20 sessions max (server cap), that's 20 threads. Acceptable on Windows or worth sharing a single reader-loop with a `select`-style pollset?
3. **Crash recovery**: if the Tauri app crashes while sessions are running, the child Claude processes are orphaned. On next launch the agent sees those `remote_sessions` rows as `status=running` but no transport handles. Should we (a) heuristically re-detect orphans via `sysinfo::Process` walking, (b) post `kind=errored` on every running-row that lacks a handle, (c) leave them stale and rely on the server-side retention cron (60d) to clean up?
4. **portable-pty vs hand-rolled ConPTY**: portable-pty pulls in `tokio` features we already have, plus a `bitflags` dep. ~200 KB binary growth. Acceptable, or should we hand-roll the Win32 FFI (saves binary, costs ~300 LOC of unsafe + Windows-only code)?
5. **Bin remote_hook.rs file logging**: I plan to put the shared log writer in `src/remote/log.rs` and call it from both the agent AND the hook binary. Hook binary is a separate process so cross-process file appender needs `OpenOptions::append(true)` + an OS-level append (Windows: `FILE_APPEND_DATA`, atomic by spec). Confirm this is the right pattern.
6. **Spawn UX**: my design has the user pick "where to spawn" via a device dropdown. For solo single-Windows users, this is annoying boilerplate. Should there be a "default to this device" shortcut, OR fall back to "spawn on the device that registered this cwd_hmac most recently"?

## Sizing

| Component | LOC | Tests |
|---|---|---|
| `remote/transport.rs` (ConPtyTransport + Win32 FFI for SIGINT) | 350 | 100 |
| `remote/agent.rs` (RemoteAgentManager + tick loop) | 500 | 200 |
| `remote/events.rs` (lifecycle event poster) | 150 | 60 |
| `remote/log.rs` (shared file logger for hook + agent) | 100 | 40 |
| `supabase.rs` (4 new helper RPC wrappers) | 200 | 80 |
| `bin/remote_hook.rs` (use the shared log + folds in v0.7.1 hook-logging hotfix scope) | 30 | 0 |
| `lib.rs` (Tauri command + spawn agent loop in setup) | 80 | 20 |
| Frontend: Spawn dialog + diagnostic display | 350 | 30 |
| i18n × 3 langs | 90 | — |
| **Total** | **~1850** | **~530** |

Larger than v0.7.0 (~1800 LOC + 600 tests). Sized as the "biggest single ship" of the Remote Sessions track.

## Out of scope (defer to v0.8.x or later)

- Stdout/stderr tail upload via `remote_helper_post_event` (Mac iter1 also defers)
- Cross-device automatic routing via cwd_hmac matching (Mac team punted; user picks device explicitly in the spawn dialog)
- Codex / shell adapters (waiting on Mac v1.14+ Multi-CLI design)
- Long-poll instead of 1s tick (depends on Gemini Q1 outcome)
- Win older / WinPTY fallback (we require Win10+; portable-pty falls back automatically anyway)

## Risks

1. **portable-pty interaction with Tauri's tokio runtime.** The crate uses its own thread pool for child management. If those threads conflict with Tauri's runtime (e.g. WebView event loop blocking), we'd see UI jank. Mitigation: VM verify on a real Win machine; if jank shows, switch to a dedicated `tokio::runtime::Builder::new_current_thread()` for the agent.
2. **Orphan child processes on crash.** Without orphan cleanup, a user who force-kills the desktop while sessions are running ends up with `claude.exe` ghosts. Mitigation: heuristic Q3 above.
3. **ConPTY behavior differences vs PosixPty.** Mac PosixPty has `start_new_session=True` for SIGINT-via-pgid; Windows has no exact analog (process groups behave differently). The substitute is `GenerateConsoleCtrlEvent` against the child's PID. Mitigation: explicit test for "interrupt during a long-running output flush works".

## What v0.8.0 explicitly does NOT do

- Stdout streaming to UI (deferred — the data plumbing works but iter1 doesn't surface it; matches Mac iter1)
- Multiple providers (codex / shell still stubbed)
- Auto-spawn / agent-autonomous-loop (only triggered by app-side `start` command, never by the helper itself)

— end of plan —
