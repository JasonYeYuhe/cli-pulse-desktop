//! v0.11.0 (T2.2a) — local in-app terminal registry.
//!
//! The LOCAL terminal is a NEW path, distinct from the Supabase-driven
//! remote managed-session host (`remote/agent.rs`): the desktop user
//! spawns a passthrough CLI (their own `claude`) on THIS machine and
//! sees it stream into an xterm.js pane. It REUSES the `ConPtyTransport`
//! PTY primitive but BYPASSES the remote agent / Supabase layer entirely
//! — a separate `ConPtyTransport` instance, zero shared mutable state.
//!
//! This slice (T2.2a) is the registry + lifecycle ONLY: `start` /
//! `close` / `status` + a concurrency cap. Byte I/O (`write` / `read`)
//! is T2.2b; `resize` landed in T2.1. No frontend yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::remote::transport::{ConPtyTransport, SessionHandle, SessionTransport, TransportError};

/// Max concurrent LOCAL terminals. Each is an independent PTY + reader
/// thread + (T2.3b) a frontend poll stream, so a small cap keeps
/// resource use bounded and the UI sane.
pub const MAX_LOCAL_TERMINALS: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum TerminalError {
    #[error("too many local terminals open (max {})", MAX_LOCAL_TERMINALS)]
    CapReached,
    #[error("no local terminal with id {0}")]
    NotFound(String),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("internal: {0}")]
    Internal(String),
}

/// Returned by `start`. The frontend keys its pane on `id` and shows
/// `pid` for diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct StartInfo {
    pub id: String,
    pub pid: u32,
}

/// Returned by `status`. `running` flips false and `exit_code` fills in
/// once the child has exited (`try_wait` returns `Some`).
#[derive(Debug, Clone, Serialize)]
pub struct TerminalStatus {
    pub running: bool,
    pub exit_code: Option<i32>,
}

/// The argv for a passthrough local terminal: the user's own `claude`.
///
/// A GUI-launched Tauri app inherits a THIN PATH (no login-shell rc), so bare
/// `claude` often won't resolve. So we resolve to an absolute path — searching
/// PATH first (the shell-launched / `tauri dev` case), then common per-user +
/// system install locations. On Windows an npm-global `claude` is a `.cmd` /
/// `.ps1` shim that ConPTY's `CreateProcess` can't spawn directly, so it's
/// wrapped in `cmd /c`. Last resort: bare `claude`, so the OS gets a chance
/// and the spawn error is surfaced to the user. VERIFIED on macOS
/// (`~/.local/bin/claude` streams `claude --version`); Windows shim path is
/// VM-verified.
pub fn claude_argv() -> Vec<String> {
    match find_claude() {
        Some(path) => wrap_resolved(path),
        None => vec!["claude".to_string()],
    }
}

/// Wrap a resolved claude path into an argv, using `cmd /c` for Windows shims.
fn wrap_resolved(path: PathBuf) -> Vec<String> {
    let s = path.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        let lower = s.to_ascii_lowercase();
        if lower.ends_with(".cmd") || lower.ends_with(".bat") || lower.ends_with(".ps1") {
            return vec!["cmd".to_string(), "/c".to_string(), s];
        }
    }
    vec![s]
}

/// Executable file names to look for, per platform.
fn claude_exe_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            "claude.cmd",
            "claude.exe",
            "claude.bat",
            "claude.ps1",
            "claude",
        ]
    }
    #[cfg(not(windows))]
    {
        &["claude"]
    }
}

/// Locate an executable `claude`: PATH first, then common install dirs.
fn find_claude() -> Option<PathBuf> {
    let names = claude_exe_names();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in names {
                let cand = dir.join(name);
                if is_executable_file(&cand) {
                    return Some(cand);
                }
            }
        }
    }
    for dir in claude_candidate_dirs() {
        for name in names {
            let cand = dir.join(name);
            if is_executable_file(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

/// Common install locations to check when PATH is thin (GUI launch).
fn claude_candidate_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".local").join("bin"));
        out.push(home.join("bin"));
        out.push(home.join(".npm-global").join("bin"));
        #[cfg(windows)]
        out.push(home.join("AppData").join("Roaming").join("npm"));
    }
    #[cfg(not(windows))]
    {
        out.push(PathBuf::from("/usr/local/bin"));
        out.push(PathBuf::from("/opt/homebrew/bin"));
        out.push(PathBuf::from("/usr/bin"));
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            out.push(PathBuf::from(appdata).join("npm"));
        }
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            out.push(PathBuf::from(pf).join("nodejs"));
        }
    }
    out
}

/// True if `p` is a regular file that is executable (Unix: any exec bit;
/// Windows: existence — the extension implies executability).
fn is_executable_file(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// One registered local terminal: the PTY handle plus the first-observed
/// exit code. Caching the exit code is REQUIRED — `try_wait` nulls the
/// child on the first observed exit and returns `Some(0)` on every call
/// after, so a polling UI that reads `status` repeatedly after exit would
/// otherwise see the real code replaced by 0.
struct LocalSession {
    handle: SessionHandle,
    exit_code: Option<i32>,
}

/// Owns the local terminals. Held in Tauri state UNCONDITIONALLY (the
/// local terminal needs no pairing). The `ConPtyTransport` is a SEPARATE
/// instance from the remote agent's — the transport is stateless config
/// (default PTY size); all per-session state lives in the `SessionHandle`
/// — so the local and remote paths never share mutable state.
pub struct LocalTerminalManager {
    transport: Arc<ConPtyTransport>,
    sessions: Mutex<HashMap<String, LocalSession>>,
    /// Monotonic id source: `local-0`, `local-1`, … Unique per process.
    next_id: AtomicU64,
    /// LOCAL-only count of in-app terminals launched this process
    /// lifetime — the "in-app terminal launched vs external" telemetry
    /// (surfaced in T2.3d). A bare integer: no cwd / argv / output.
    launched: AtomicU64,
}

impl Default for LocalTerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalTerminalManager {
    pub fn new() -> Self {
        Self {
            transport: Arc::new(ConPtyTransport::new()),
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            launched: AtomicU64::new(0),
        }
    }

    /// Spawn a local terminal running `argv` in `cwd` (`env` is merged
    /// onto the parent env by the transport). Returns the new id + pid.
    /// The `terminal_start` command supplies the passthrough `claude`
    /// argv; tests supply a known short-lived command.
    ///
    /// The cap check + spawn + insert happen under one lock so the cap is
    /// atomic (two concurrent starts can't both slip past it). `start` is
    /// a one-shot openpty + spawn — not a blocking I/O loop — and is rare
    /// (a user clicking "new terminal"), so briefly holding the registry
    /// lock here does not contend with the per-frame read path (T2.2b),
    /// which clones the handle and releases the lock before any transport
    /// call.
    pub fn start(
        &self,
        argv: &[String],
        env: HashMap<String, String>,
        cwd: Option<&str>,
    ) -> Result<StartInfo, TerminalError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| TerminalError::Internal("sessions mutex poisoned".to_string()))?;
        if sessions.len() >= MAX_LOCAL_TERMINALS {
            return Err(TerminalError::CapReached);
        }
        let id = format!("local-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let handle = self.transport.start(&id, argv, env, cwd)?;
        let pid = handle.pid();
        sessions.insert(
            id.clone(),
            LocalSession {
                handle,
                exit_code: None,
            },
        );
        self.launched.fetch_add(1, Ordering::Relaxed);
        Ok(StartInfo { id, pid })
    }

    /// Close (and kill) a local terminal. Idempotent — closing an unknown
    /// id is `Ok`.
    ///
    /// Teardown ACTIVELY kills the child via `terminate` rather than relying
    /// on `HandleInner::Drop` alone. Drop fires only at the *last*
    /// `Arc<HandleInner>` clone, and an in-flight blocking `terminal_write`
    /// (on the spawn_blocking pool) holds a clone: if that write is parked on
    /// a full stdin pipe, Drop-only teardown would never run — the child +
    /// reader thread would leak and the write would never unblock (the child
    /// is never killed, so it never drains). `terminate` locks the `child`
    /// mutex (a DIFFERENT mutex than the write's `writer`, so it can't be
    /// blocked by that write); killing the child closes the slave PTY, which
    /// unblocks the write (EIO → returns 0 → its Arc drops → Drop completes).
    pub fn close(&self, id: &str) -> Result<(), TerminalError> {
        let removed = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| TerminalError::Internal("sessions mutex poisoned".to_string()))?;
            sessions.remove(id)
        };
        // Kill outside the registry lock (terminate locks only the per-handle
        // `child` mutex). Best-effort: a dead child is a no-op.
        if let Some(session) = &removed {
            let _ = self.transport.terminate(&session.handle);
        }
        drop(removed);
        Ok(())
    }

    /// Poll a terminal's liveness. Errors `NotFound` for an unknown id.
    pub fn status(&self, id: &str) -> Result<TerminalStatus, TerminalError> {
        // `try_wait` is non-blocking (trait contract — the agent calls it
        // every tick without spawn_blocking), so we poll it UNDER the
        // registry lock, which makes caching the exit code atomic and
        // race-free against a concurrent poll. Without the cache, the
        // second poll after exit would report exit_code 0 (try_wait resets
        // once it has nulled the child).
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| TerminalError::Internal("sessions mutex poisoned".to_string()))?;
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| TerminalError::NotFound(id.to_string()))?;
        if let Some(code) = session.exit_code {
            return Ok(TerminalStatus {
                running: false,
                exit_code: Some(code),
            });
        }
        let exit = self.transport.try_wait(&session.handle)?;
        if let Some(code) = exit {
            session.exit_code = Some(code); // cache the first observation
        }
        Ok(TerminalStatus {
            running: exit.is_none(),
            exit_code: exit,
        })
    }

    /// Clone the cheap Arc handle for `id`, RELEASING the registry lock
    /// before the caller touches the transport — the per-frame read/write
    /// path must never hold the lock across a transport call.
    fn handle_of(&self, id: &str) -> Result<SessionHandle, TerminalError> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| TerminalError::Internal("sessions mutex poisoned".to_string()))?;
        sessions
            .get(id)
            .map(|s| s.handle.clone())
            .ok_or_else(|| TerminalError::NotFound(id.to_string()))
    }

    /// Drain up to `max_bytes` of the terminal's buffered stdout (raw
    /// bytes — no decode/strip; xterm.js handles UTF-8 + ANSI). Empty when
    /// nothing is pending (and forever once the child has exited + drained).
    /// `NotFound` for an unknown id.
    pub fn read(&self, id: &str, max_bytes: usize) -> Result<Vec<u8>, TerminalError> {
        let handle = self.handle_of(id)?;
        Ok(self.transport.read_stdout(&handle, max_bytes)?)
    }

    /// Resize the terminal's PTY (best-effort). `NotFound` for an unknown
    /// id; a transport-level `ResizeFailed` (e.g. EIO from resizing an
    /// already-exited child) is swallowed — a per-`onResize` UI call can't
    /// act on it, and the child is gone anyway.
    pub fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), TerminalError> {
        let handle = self.handle_of(id)?;
        match self.transport.resize(&handle, rows, cols) {
            Ok(()) | Err(TransportError::ResizeFailed(_)) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The owned, `Send` `(transport, handle)` for `id`, so a caller can
    /// run the potentially-blocking `write_stdin` OFF the async runtime via
    /// `spawn_blocking` (a full stdin pipe can block if the child stops
    /// reading — T0 P0#1). `NotFound` for an unknown id.
    pub fn writable(
        &self,
        id: &str,
    ) -> Result<(Arc<ConPtyTransport>, SessionHandle), TerminalError> {
        let handle = self.handle_of(id)?;
        Ok((Arc::clone(&self.transport), handle))
    }

    /// LOCAL count of terminals launched this process lifetime (telemetry
    /// surfaced in a later slice).
    pub fn launched_count(&self) -> u64 {
        self.launched.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn open_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn sleep_argv() -> Vec<String> {
        if cfg!(target_os = "windows") {
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "ping -n 3 127.0.0.1 >nul".to_string(),
            ]
        } else {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 1".to_string(),
            ]
        }
    }

    #[test]
    fn close_unknown_id_is_ok() {
        let m = LocalTerminalManager::new();
        assert!(m.close("nope").is_ok());
    }

    #[test]
    fn status_unknown_id_is_not_found() {
        let m = LocalTerminalManager::new();
        assert!(matches!(m.status("nope"), Err(TerminalError::NotFound(_))));
    }

    #[test]
    fn read_write_resize_unknown_id_are_not_found() {
        let m = LocalTerminalManager::new();
        assert!(matches!(
            m.read("nope", 64),
            Err(TerminalError::NotFound(_))
        ));
        assert!(matches!(
            m.resize("nope", 24, 80),
            Err(TerminalError::NotFound(_))
        ));
        assert!(matches!(
            m.writable("nope"),
            Err(TerminalError::NotFound(_))
        ));
    }

    #[test]
    fn empty_argv_surfaces_transport_error_and_registers_nothing() {
        let m = LocalTerminalManager::new();
        assert!(matches!(
            m.start(&[], HashMap::new(), None),
            Err(TerminalError::Transport(TransportError::EmptyArgv))
        ));
        assert_eq!(m.open_count(), 0);
        assert_eq!(m.launched_count(), 0);
    }

    #[test]
    fn claude_argv_is_never_empty() {
        assert!(!claude_argv().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn is_executable_file_detects_a_real_binary() {
        assert!(is_executable_file(Path::new("/bin/sh")));
        assert!(!is_executable_file(Path::new("/definitely/not/here/xyz")));
        assert!(!is_executable_file(Path::new("/bin"))); // a dir, not a file
    }

    #[test]
    fn wrap_resolved_path_shape() {
        #[cfg(not(windows))]
        {
            assert_eq!(
                wrap_resolved(PathBuf::from("/usr/local/bin/claude")),
                vec!["/usr/local/bin/claude".to_string()]
            );
        }
        #[cfg(windows)]
        {
            let argv = wrap_resolved(PathBuf::from(r"C:\Users\x\AppData\Roaming\npm\claude.cmd"));
            assert_eq!(argv[0], "cmd");
            assert_eq!(argv[1], "/c");
            assert!(argv[2].ends_with("claude.cmd"));
            // A plain .exe is spawned directly (no cmd /c).
            assert_eq!(
                wrap_resolved(PathBuf::from(r"C:\tools\claude.exe")),
                vec![r"C:\tools\claude.exe".to_string()]
            );
        }
    }

    /// Env-specific: if `claude` is installed, `claude_argv` resolves it to a
    /// real executable (not the bare-`claude` last resort). `#[ignore]` — CI
    /// has no `claude`. Run locally with
    /// `cargo test -- --ignored claude_argv_resolves_installed_claude`.
    #[test]
    #[ignore]
    fn claude_argv_resolves_installed_claude() {
        let argv = claude_argv();
        eprintln!("resolved claude argv: {argv:?}");
        let last = argv.last().unwrap();
        if last != "claude" {
            assert!(
                Path::new(last).exists(),
                "resolved claude path should exist: {last}"
            );
        }
    }

    /// Real-spawn lifecycle: start → status(running) → close → gone.
    /// `#[ignore]` for the same headless-CI ConPTY flake reason as the
    /// transport real-spawn tests; run locally with
    /// `cargo test -- --ignored real_terminal_lifecycle`.
    #[test]
    #[ignore]
    fn real_terminal_lifecycle() {
        let m = LocalTerminalManager::new();
        let info = match m.start(&sleep_argv(), HashMap::new(), None) {
            Ok(i) => i,
            Err(_e) => return, // PTY alloc can fail headless; skip.
        };
        assert!(info.pid > 0);
        assert_eq!(m.open_count(), 1);
        assert_eq!(m.launched_count(), 1);
        // Fresh child: running.
        let st = m.status(&info.id).unwrap();
        assert!(st.running, "just-spawned child should be running");
        assert_eq!(st.exit_code, None);
        // Close → removed → subsequent status is NotFound (idempotent
        // second close is Ok).
        m.close(&info.id).unwrap();
        assert_eq!(m.open_count(), 0);
        assert!(matches!(
            m.status(&info.id),
            Err(TerminalError::NotFound(_))
        ));
        assert!(m.close(&info.id).is_ok());
    }

    /// Real-spawn: after a child exits, the reported exit code is STABLE
    /// across repeat `status` polls (regression guard for the cached-exit
    /// fix — raw `try_wait` returns Some(0) on the second observation).
    /// `#[ignore]` (real PTY) like the others.
    #[test]
    #[ignore]
    fn real_status_exit_code_stable_across_polls() {
        let m = LocalTerminalManager::new();
        let argv: Vec<String> = if cfg!(target_os = "windows") {
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "exit 3".to_string(),
            ]
        } else {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "exit 3".to_string(),
            ]
        };
        let info = match m.start(&argv, HashMap::new(), None) {
            Ok(i) => i,
            Err(_e) => return, // headless PTY alloc failure; skip.
        };
        // Poll until the child is observed exited (up to ~5 s).
        let started = Instant::now();
        let first: i32 = loop {
            let st = m.status(&info.id).unwrap();
            if !st.running {
                break st.exit_code.expect("an exited child has an exit code");
            }
            if started.elapsed() > Duration::from_secs(5) {
                panic!("child did not exit within 5 s");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(first, 3, "first observed exit code");
        // Every subsequent poll must report the SAME code, not 0.
        for _ in 0..3 {
            let st = m.status(&info.id).unwrap();
            assert!(!st.running);
            assert_eq!(
                st.exit_code,
                Some(3),
                "exit code must be stable across polls"
            );
        }
        m.close(&info.id).unwrap();
    }

    /// Real-spawn: `read` captures a child's output through the manager.
    /// `#[ignore]` (real PTY) like the others.
    #[test]
    #[ignore]
    fn real_manager_read_captures_output() {
        let m = LocalTerminalManager::new();
        let argv: Vec<String> = if cfg!(target_os = "windows") {
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "echo marker_xyz".to_string(),
            ]
        } else {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf 'marker_xyz\\n'".to_string(),
            ]
        };
        let info = match m.start(&argv, HashMap::new(), None) {
            Ok(i) => i,
            Err(_e) => return,
        };
        let started = Instant::now();
        let mut got: Vec<u8> = Vec::new();
        loop {
            got.extend_from_slice(&m.read(&info.id, 4096).unwrap());
            if got.windows(10).any(|w| w == b"marker_xyz") {
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            String::from_utf8_lossy(&got).contains("marker_xyz"),
            "manager.read captured: {:?}",
            String::from_utf8_lossy(&got)
        );
        m.close(&info.id).unwrap();
    }

    /// Real-spawn (unix): a byte written via `writable()`/`write_stdin`
    /// round-trips back through `cat` and is read via `read`. `cat` has no
    /// clean cmd.exe equivalent, so Windows is skipped (VM-covered).
    #[test]
    #[ignore]
    fn real_manager_write_roundtrips() {
        if cfg!(target_os = "windows") {
            return;
        }
        let m = LocalTerminalManager::new();
        let argv = vec!["/bin/sh".to_string(), "-c".to_string(), "cat".to_string()];
        let info = match m.start(&argv, HashMap::new(), None) {
            Ok(i) => i,
            Err(_e) => return,
        };
        std::thread::sleep(Duration::from_millis(100)); // let cat start
        let (transport, handle) = m.writable(&info.id).unwrap();
        let n = transport.write_stdin(&handle, b"echo_me\n").unwrap();
        assert!(n > 0, "write should send bytes");
        let started = Instant::now();
        let mut got: Vec<u8> = Vec::new();
        loop {
            got.extend_from_slice(&m.read(&info.id, 4096).unwrap());
            if got.windows(7).any(|w| w == b"echo_me") {
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            String::from_utf8_lossy(&got).contains("echo_me"),
            "written bytes should round-trip: {:?}",
            String::from_utf8_lossy(&got)
        );
        m.close(&info.id).unwrap();
    }

    /// Real-spawn regression (T2.2b review Finding 1): `close` must kill the
    /// child even when another `Arc<HandleInner>` clone is held (as an
    /// in-flight blocking write would). A held clone defeats Drop-only
    /// teardown, so `close` actively `terminate`s. `#[ignore]` (real PTY).
    #[test]
    #[ignore]
    fn real_close_kills_child_with_lingering_handle() {
        let m = LocalTerminalManager::new();
        // Long-running child so we can tell whether close actually killed it.
        let argv: Vec<String> = if cfg!(target_os = "windows") {
            vec![
                "cmd.exe".to_string(),
                "/c".to_string(),
                "ping -n 30 127.0.0.1 >nul".to_string(),
            ]
        } else {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 30".to_string(),
            ]
        };
        let info = match m.start(&argv, HashMap::new(), None) {
            Ok(i) => i,
            Err(_e) => return,
        };
        // Simulate an in-flight write task pinning an Arc<HandleInner> clone.
        let (transport, lingering) = m.writable(&info.id).unwrap();
        m.close(&info.id).unwrap();
        // The lingering handle must observe the child exit promptly — proving
        // close killed it rather than waiting for the last Arc to drop.
        let started = Instant::now();
        loop {
            if let Ok(Some(_)) = transport.try_wait(&lingering) {
                break; // killed — good
            }
            if started.elapsed() > Duration::from_secs(3) {
                panic!("close did not kill the child while a handle clone was held");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Real-spawn cap: the 5th concurrent start is rejected; closing one
    /// frees a slot. `#[ignore]` (real PTYs) like above.
    #[test]
    #[ignore]
    fn real_cap_is_enforced() {
        let m = LocalTerminalManager::new();
        let mut ids = Vec::new();
        for _ in 0..MAX_LOCAL_TERMINALS {
            match m.start(&sleep_argv(), HashMap::new(), None) {
                Ok(i) => ids.push(i.id),
                Err(_e) => return, // headless PTY alloc failure; skip.
            }
        }
        assert_eq!(m.open_count(), MAX_LOCAL_TERMINALS);
        // At cap: next start rejected.
        assert!(matches!(
            m.start(&sleep_argv(), HashMap::new(), None),
            Err(TerminalError::CapReached)
        ));
        // Free a slot → start succeeds again.
        m.close(&ids[0]).unwrap();
        let info = m.start(&sleep_argv(), HashMap::new(), None).unwrap();
        assert_eq!(m.open_count(), MAX_LOCAL_TERMINALS);
        ids.push(info.id);
        // Tidy up (drop kills children).
        for id in &ids {
            let _ = m.close(id);
        }
    }
}
