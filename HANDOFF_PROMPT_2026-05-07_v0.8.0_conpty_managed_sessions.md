# Handoff prompt — v0.8.0 ConPTY managed-session local host

Copy the section between the markers below into a fresh Mac Claude Code session at `/Users/jason/Documents/cli-pulse-desktop/`. The previous session shipped v0.5.4 → v0.7.0 (8 versions, including 1 hotfix) over two calendar days. v0.8.0 is the next planned ship: the BIGGEST single ship of the Remote Sessions track, ~1850 LOC + ~530 tests.

---

## ▼ START COPY-PASTE BELOW THIS LINE ▼

You are taking over `cli-pulse-desktop` (Tauri 2 + Rust + React, Win+Linux only) at `/Users/jason/Documents/cli-pulse-desktop/`. The previous session shipped 7 versions in two days (v0.5.4 → v0.7.0), all VM-verified clean. **Current Latest = v0.7.0.** The next ship — v0.8.0 ConPTY managed-session local host — is *fully planned + reviewed by Gemini 3.1 Pro*, but **NOT YET IMPLEMENTED**. Your job: implement v0.8.0 applying every reviewer fix. Then write a VM verify prompt for Jason to forward.

The user (Jason) operates you autonomously per `feedback_desktop_autonomy.md`. Don't ask permission for code changes / commits / version bumps / releases / hotfixes / refactors — just do them. Each ship has its own pre-push gates + Gemini review pass; don't skip those.

### 1) Ground yourself — required reading in order

**Auto-memory (already in context):**
- `feedback_desktop_autonomy.md` — your scope of authority, 3 explicit exception categories
- `feedback_vm_as_real_e2e.md` — Mac is host-managed (no creds files); VM is the real-world test target
- `feedback_vm_indicator_testing.md` — VM-side limitation patterns (RDP focus / passive idle don't trigger time-based UI)
- `feedback_vm_verify_prompt_template.md` — **standard structure for the VM prompt you'll write at the end** (Phase 0/1 + Blocks + Block F wait-for-user before deallocate)
- `feedback_gemini_review_patterns.md` — recurring catches Gemini surfaces
- `feedback_github_secret_scanner.md` — `concat!()` workaround for credential-shape literals (you'll hit this if you write tests that include token fixtures)
- `feedback_tauri2_state_guard_lifetime.md` — `app.try_state::<T>()` + `Mutex::lock()` E0597 gotcha (you may hit this in `agent.rs`)
- `feedback_mac_windows_remote_track_alignment.md` — **cross-team SOT** including Phase 4E Mac roadmap, M1-M4 backport, HOOK_MARKER namespace consideration, AND the v0.7.0 VM verify outcome + v0.8.0 Gemini findings in 修订日志
- `feedback_remote_hook_diagnostic_blind_spot.md` — `bin/remote_hook.rs` no file logging, **fold the v0.7.1 hotfix into v0.8.0 as `src/remote/log.rs` shared by hook + agent**
- `reference_desktop_repo.md` — repo location, stack, **complete sprint history through v0.7.0**, Test infrastructure section (Azure VM specifics)
- `reference_sentry.md` — Sentry org/project layout, sentry-cli usage, issue-vs-event release-filter gotcha
- `reference_supabase_creds.md` + `reference_supabase_access_token.md` — Supabase project ID + Mgmt API token
- `reference_gemini_cli.md` — `/opt/homebrew/bin/gemini` invocation pattern

**The plan + reviews on disk (READ ALL):**
- `/Users/jason/Documents/cli-pulse-desktop/PROJECT_DEV_PLAN_2026-05-07_v0.8.0_conpty_managed_sessions.md` — **the v0.8.0 plan, scope verdict GO from Gemini**, includes module decomposition, helper RPC list, sizing, out-of-scope items
- The Gemini review findings live in `feedback_mac_windows_remote_track_alignment.md`'s 修订日志 — see §3 below for the full P0/P1/P2 fix text

**Mac sibling files to read for cross-platform contract** (in `/Users/jason/Documents/cli-pulse/`):
- `helper/transports/base.py` — `SessionTransport` ABC contract (the Rust trait must mirror this)
- `helper/transports/posix_pty.py` — reference impl for the POSIX side (for understanding handle lifecycle)
- `helper/transports/conpty.py` — Mac's NotImplementedError stub explicitly waiting for THIS ship
- `helper/remote_agent.py` — `RemoteAgentManager` (port the dispatch loop + per-session state map)

### 2) The ship — v0.8.0 ConPTY managed-session local host

**Scope:** when iOS / Mac / Windows app calls `remote_app_request_session_start(p_device_id=this-windows-box, ...)`, the Windows desktop's agent loop pulls the resulting `kind='start'` command via `remote_helper_pull_commands`, spawns Claude under a ConPTY pseudoconsole, registers `remote_sessions(status='running')`, then dispatches subsequent prompt/stop/interrupt commands until the child exits or the user stops it.

**Closes the loop** the macOS team explicitly designed for. Before v0.8.0 only Mac could host managed sessions; v0.8.0 makes Windows a peer.

**New Rust modules** (per the dev plan):

```
src-tauri/src/remote/
  mod.rs          — module root
  transport.rs    — SessionTransport trait + ConPtyTransport (port of Mac ABC, portable-pty wrapper)
  agent.rs        — RemoteAgentManager + 1s tick loop
  events.rs       — Lifecycle event poster (running/stopped/errored/info)
  log.rs          — Shared file appender for hook + agent (folds in v0.7.1 hotfix scope)
src-tauri/src/bin/remote_hook.rs   — gains shared log.rs use
```

Plus 4 new helper RPC wrappers in `supabase.rs` (all already-live RPCs):
`remote_helper_register_session`, `remote_helper_pull_commands`, `remote_helper_post_event`, `remote_helper_complete_command`.

Plus 1 new Tauri command + frontend spawn dialog (~350 LOC) and ~10 i18n keys × 3 langs.

**New crate**: `portable-pty 0.8` (mature; used by VS Code / WezTerm / Alacritty). +200 KB binary, acceptable.

### 3) Reviewer findings — apply every one

Gemini 3.1 Pro review on the v0.8.0 dev plan caught **2 P0 + 1 P1 + 1 P2**. All real, all blocking. Bake them in from the first commit, not as post-impl fixes:

#### P0 #1 — Tokio executor blocking

**Bug**: `SessionTransport` trait uses sync `Result<usize>` / `Result<Vec<u8>>` returns. If `RemoteAgentManager::tick` is `async fn` running on Tauri's main runtime, calling `transport.write_stdin(data)` blocks the executor thread when the ConPTY pipe buffer is full and the child hangs.

**Fix**: wrap every sync transport call inside the async tick with `tokio::task::spawn_blocking`:

```rust
async fn dispatch_prompt(&self, sid: &str, payload: &str) -> Result<()> {
    let transport = self.transport.clone();
    let handle = self.handle_for(sid)?;
    let bytes = payload.as_bytes().to_vec();
    tokio::task::spawn_blocking(move || transport.write_stdin(&handle, &bytes))
        .await
        .map_err(|e| Error::JoinError(e))??; // double ? — outer JoinError, inner TransportError
    Ok(())
}
```

Apply to `start`, `write_stdin`, `read_stdout`(if used sync), `interrupt`, `terminate`, `close`. The ONLY non-blocking method is `try_wait` (which returns immediately).

#### P0 #2 — `GenerateConsoleCtrlEvent` kills the host process ⚠️ CRITICAL

**Bug**: SIGINT-equivalent on Windows is `GenerateConsoleCtrlEvent(CTRL_C_EVENT, target_pgid)`. Without `CREATE_NEW_PROCESS_GROUP` set when the child is spawned, the child shares the host's console process group. Sending CTRL_C **signals the desktop app itself**, instantly killing cli-pulse-desktop along with the child. The Interrupt button on the UI would terminate the user's app.

**Fix**: pass `CREATE_NEW_PROCESS_GROUP` to portable-pty's `CommandBuilder`:

```rust
use portable_pty::CommandBuilder;
let mut cmd = CommandBuilder::new("claude");
// portable-pty exposes raw process flags via this method on Windows:
#[cfg(target_os = "windows")]
{
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;
    cmd.set_creation_flags(CREATE_NEW_PROCESS_GROUP);
}
```

VERIFY this method exists on the version you're using. If portable-pty < 0.8.x doesn't expose creation_flags, EITHER bump to a version that does OR open an upstream issue and patch locally via fork. **Do not ship without this** — the bug is not subtle, the Interrupt button silently kills the app.

For the actual signal-sending side, also use `CTRL_BREAK_EVENT` (which is what `CREATE_NEW_PROCESS_GROUP` enables for sending; CTRL_C is NOT received by detached groups):

```rust
use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};
unsafe {
    if GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child_pid) == 0 {
        return Err(io::Error::last_os_error().into());
    }
}
```

Add a unit test that spawns a long-running child (e.g. `cmd.exe /c ping -n 60 localhost`), interrupts it, and asserts the child exits within 2 s while the parent test process is still alive.

#### P1 — Job Object for orphan auto-cleanup

**Bug**: my plan had heuristic process-walking on next launch. Unreliable: process names can collide, PIDs recycle, ghosts persist if the user never relaunches.

**Fix**: Windows Job Objects with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. OS kernel terminates assigned children the millisecond the desktop process exits or crashes — no heuristics needed:

```rust
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
};

// Create one job object per process (or one per app session — your call):
let job = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null()) };
let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
unsafe {
    SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as _, mem::size_of_val(&info) as u32);
    AssignProcessToJobObject(job, child_handle);
}
```

The job handle stays alive in the agent's `ManagedSession` struct. When the desktop crashes / exits cleanly / is force-killed, the job's auto-close fires and the kernel kills the children.

For boot-time state reconciliation (Gemini Q3 answer): on agent startup, query Supabase for `remote_sessions WHERE device_id=this AND status='running'`, post `kind=errored` events for each, mark them stopped server-side. Combined with Job Objects, this guarantees no orphan accumulation.

#### P2 — `SessionHandle::close` Drop pattern

**Bug**: `close(self, handle: SessionHandle)` consumes one clone but reader-thread clones keep `Arc<HandleInner>` alive — `close` doesn't actually tear down.

**Fix**: implement `Drop` on `HandleInner` (the inner struct behind the `Arc`). Drop teardown runs when the LAST clone goes out of scope:

```rust
struct HandleInner {
    pid: u32,
    job: HANDLE,
    reader_stop: Arc<AtomicBool>,
    // ...
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        self.reader_stop.store(true, Ordering::Relaxed);
        unsafe { CloseHandle(self.job); }
        // Job auto-kill fires here: children die.
    }
}
```

Remove `close` from the trait entirely OR leave it as a synonym for `drop(handle)` (more explicit).

### 4) Working pattern (matches the previous session's cadence)

1. **Pre-flight**: confirm `portable-pty 0.8.x` exposes `CommandBuilder::set_creation_flags` on Windows. If not, find a version that does (likely needs a `windows-` feature) OR plan to patch.
2. **Implement bottom-up**: `transport.rs` first (with all 4 fix patterns), then `events.rs`, then `agent.rs`, then `log.rs`, then helper RPC wrappers, then frontend.
3. **Tests as you go** — don't batch them at the end. The transport layer especially needs tests against `cmd.exe` echo / sleep targets to verify CTRL_BREAK actually works.
4. **Bump versions** in 4 places: `tauri.conf.json`, `package.json`, `src-tauri/Cargo.toml` → `npm install --package-lock-only --silent` + `cargo build --quiet`.
5. **Write CHANGELOG entry** at TOP. Cite all 4 Gemini fixes inline.
6. **Run gates**: `cargo fmt --check && cargo clippy --lib --bins -- -D warnings && cargo test --lib && npm run build && npm run test`. All must pass.
7. **Gemini review** the diff via `git diff <files> | /opt/homebrew/bin/gemini -p "..."` per `reference_gemini_cli.md`. Apply any P1+P2 fixes before commit.
8. **Commit + tag + push**. Pre-push hook re-runs gates. CI takes ~10 min.
9. **Watch CI** via Monitor — wait for Windows job conclusion. Don't poll.
10. **Promote** to Latest: `gh release edit v0.8.0 --draft=false --prerelease=false --latest`. Verify NSIS URL returns 200.
11. **Write VM verify prompt** per `feedback_vm_verify_prompt_template.md` — test-block focus areas listed in §5 below.

### 5) Things you MUST NOT get wrong

- **The P0 #2 `CREATE_NEW_PROCESS_GROUP` flag is non-negotiable**. If portable-pty doesn't expose it, FIND a way (newer version / fork / direct Win32 spawn). Shipping without it kills the app on every Interrupt click.
- **Don't preempt Codex / shell adapters** — Mac's Multi-CLI design v1.14+ is the SOT for those. v0.8.0 is Claude-only. Mac team will pull us into design review when their spec is ready.
- **Don't add backend RPCs / schema changes**. The 4 helper RPCs in §2 are already-live Mac team work.
- **Don't skip Gemini diff review per ship**. Every ship in the v0.5.x → v0.7.x sprint had at least one P0/P1 caught post-impl.
- **VM RDP focus is a known testing limitation** (per `feedback_vm_indicator_testing.md`): keyboard events (Esc / Enter / etc.) often don't reach Webview2 from the Azure RDP session. The v0.6.1 / v0.7.0 verifies hit this — Esc tests come back INCONCLUSIVE. Don't waste time debugging "Esc broken" reports unless they're reproducible from a non-RDP local Win machine.
- **The v0.7.1 hook-logging fold-in**: `bin/remote_hook.rs` currently has zero file logging — VM verify 2026-05-07 hit this exact diagnostic blind spot (medium-risk D.1 returned no `remote_permission_requests` row, can't tell if the hook fail-fast'd locally OR the server gate rejected). The `src/remote/log.rs` you ship as part of v0.8.0 must be importable by the hook binary too. Same file path: `<app_log_dir>/remote-hook.log`.

### 6) VM verify focus areas for v0.8.0

After v0.8.0 promotes to Latest, the VM verify prompt should test (in priority order):

**Block A — managed session spawn (Mac assist needed)**
- Mac iOS / Mac CLI Pulse Bar issues `remote_app_request_session_start(p_device_id=<windows-vm>, p_provider=claude, ...)`
- Within ~1 s, VM should see `remote_sessions` row with `status='running'` for that session
- spawned `claude.exe` visible in Task Manager, child of cli-pulse-desktop.exe (or detached if Job Objects took the parent slot — verify either way)

**Block B — prompt/stop/interrupt round-trip (extends v0.6.2)**
- Mac sends `prompt` command → VM Claude receives + responds
- Mac sends `interrupt` → **CRITICAL: cli-pulse-desktop must NOT die** (P0 #2 verification). Child Claude should interrupt; host stays alive.
- Mac sends `stop` → Claude exits gracefully. Job Object closes. Status=stopped event posted.

**Block C — orphan cleanup**
- Spawn a session, then force-kill cli-pulse-desktop.exe via Task Manager
- Within ~2 s the spawned `claude.exe` should also die (Job Object kill-on-close)
- Re-launch app. The orphaned `remote_sessions` row should flip to `status=errored` on agent boot.

**Block D — log file**
- After any session activity, `%LOCALAPPDATA%\dev.clipulse.desktop\logs\remote-hook.log` should exist with timestamped lines
- Run a medium-risk command via Claude (the D.1 from 2026-05-07 verify that came back blank). Now the log should show `create_request POST → status=200` OR an explicit error reason. **This closes the diagnostic blind spot.**

**Block E — regressions** (v0.7.0 hook + v0.6.x basics)

**Block F — close VM (WAIT FOR USER CONFIRMATION before `az vm deallocate`)**

### 7) When you're done

After v0.8.0 promoted to Latest:
1. Append v0.8.0 entry to `reference_desktop_repo.md` sprint history (same format as v0.7.0)
2. Update `feedback_mac_windows_remote_track_alignment.md` 修订日志 with v0.8.0 ship outcome
3. Write the VM verify prompt as a Chinese-prefixed code block, ready for Jason to copy-paste
4. Stop. Don't pre-empt v0.8.x or any new features.

If at any point you find a v0.8.0 plan assumption is wrong (like Gemini found 2 P0s in the plan), STOP and surface it before continuing. The previous session's failure mode warning still applies: shipping plausible code on top of wrong architectural assumptions is the post-many-ships risk.

### 8) Memory updates after ship

If anything novel comes up during v0.8.0 implementation (a Tauri-2/portable-pty interaction, Win32 FFI quirk, Job Object edge case), save a feedback memory under `~/.claude/projects/-Users-jason-Documents-cli-pulse/memory/` and add a one-line entry to `MEMORY.md`. See existing files for format.

Good ship.

## ▲ END COPY-PASTE ABOVE THIS LINE ▲
