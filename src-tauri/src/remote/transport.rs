//! v0.9.1a — Stub transport for ConPTY managed-session host.
//!
//! v0.8.0 shipped a portable-pty / Job Object / ConPTY pseudoconsole
//! implementation that crashed on launch (BEX64) due to an unrelated
//! `tokio::spawn` bug in the agent loop's spawn site. v0.8.1 reverted
//! the entire transport module + the FFI deps (`portable-pty`,
//! `windows-sys`).
//!
//! v0.9.1a ships the agent **scaffolding** without the FFI — same
//! trait + types as v0.8.0, but `StubTransport` returns
//! `TransportError::Internal` from every method. This lets us:
//!
//! 1. Validate the agent's tick loop / RPC dispatch / event posting
//!    on its own, without the FFI complications that masked the
//!    v0.8.0 bug for hours.
//! 2. Give users a "not yet supported" path through the spawn-dialog
//!    UI: dialog submits → server creates pending row → agent picks
//!    up the `start` command → stub returns Internal →
//!    `complete_command` marks the row failed → UI shows the error.
//! 3. Land the kill-switch env var (`CLI_PULSE_DISABLE_REMOTE_AGENT=1`)
//!    and the boot-time recovery-mode gating with a smaller diff.
//!
//! v0.9.1b adds the actual ConPTY + Job Object FFI on top, swapping
//! `StubTransport` for `ConPtyTransport`. Same trait, smaller blast
//! radius per Gemini 3.1 Pro plan-review P3.

use std::collections::HashMap;

/// Errors a transport can emit. The agent maps these onto either a
/// `kind=errored` lifecycle event (transient) or a `failed` command
/// completion (recoverable). Same shape as v0.8.0 so v0.9.1b can
/// drop in without breaking the agent's match arms.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("argv must not be empty")]
    EmptyArgv,
    #[error("PTY allocate failed: {0}")]
    PtyAllocFailed(String),
    #[error("spawn failed: {0}")]
    SpawnFailed(String),
    #[error("write failed: {0}")]
    WriteFailed(String),
    #[error("read failed: {0}")]
    ReadFailed(String),
    #[error("interrupt failed: {0}")]
    InterruptFailed(String),
    #[error("terminate failed: {0}")]
    TerminateFailed(String),
    #[error("handle has no live state")]
    HandleClosed,
    #[error("internal: {0}")]
    Internal(String),
}

/// Opaque token returned by `start()`. v0.9.1a's stub-transport
/// version is much simpler than v0.8.0's — no Arc<HandleInner>, no
/// Drop pattern, no Job Object handle. `start()` never actually
/// succeeds in v0.9.1a so the handle is mostly a type-system
/// placeholder.
///
/// v0.9.1b will reintroduce the Arc<HandleInner> pattern with the
/// reader thread + Job Object + portable-pty integration.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    pub session_id: String,
    /// Placeholder PID. Always 0 in v0.9.1a (stub never spawns).
    pid: u32,
}

impl SessionHandle {
    pub fn pid(&self) -> u32 {
        self.pid
    }
}

/// PTY transport contract. Same trait shape as v0.8.0 so v0.9.1b
/// can swap implementations without rewiring the agent.
///
/// All methods are synchronous (matches Mac's
/// `helper/transports/base.py::SessionTransport`). The agent wraps
/// each call in `spawn_blocking` so a blocked transport call never
/// parks Tauri's tokio runtime — even though the stub doesn't block,
/// the wrapper stays in place for v0.9.1b drop-in compatibility.
pub trait SessionTransport: Send + Sync {
    fn start(
        &self,
        session_id: &str,
        argv: &[String],
        env: HashMap<String, String>,
        cwd: Option<&str>,
    ) -> Result<SessionHandle, TransportError>;

    fn write_stdin(&self, handle: &SessionHandle, data: &[u8]) -> Result<usize, TransportError>;

    fn read_stdout(
        &self,
        handle: &SessionHandle,
        max_bytes: usize,
    ) -> Result<Vec<u8>, TransportError>;

    fn interrupt(&self, handle: &SessionHandle) -> Result<(), TransportError>;

    fn terminate(&self, handle: &SessionHandle) -> Result<(), TransportError>;

    fn try_wait(&self, handle: &SessionHandle) -> Result<Option<i32>, TransportError>;
}

/// v0.9.1a stub. Every method returns `Internal("...not implemented yet")`.
/// Tracks calls for tests + observability; the agent's
/// `complete_command` path will mark each pulled `start` command as
/// failed with the stub's error message, surfacing in the UI as
/// "Spawn not yet supported in this build".
pub struct StubTransport {
    /// Optional reason override for tests that want a specific
    /// error message. Default: a fixed v0.9.1a-specific string.
    reason: String,
}

impl Default for StubTransport {
    fn default() -> Self {
        Self {
            reason: "v0.9.1a stub transport — ConPTY spawn not yet implemented in this build (planned for v0.9.1b)".to_string(),
        }
    }
}

impl StubTransport {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn with_reason(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn internal_err(&self) -> TransportError {
        TransportError::Internal(self.reason.clone())
    }
}

impl SessionTransport for StubTransport {
    fn start(
        &self,
        _session_id: &str,
        argv: &[String],
        _env: HashMap<String, String>,
        _cwd: Option<&str>,
    ) -> Result<SessionHandle, TransportError> {
        // Validate argv early — agent test would expect this even for
        // a stub.
        if argv.is_empty() {
            return Err(TransportError::EmptyArgv);
        }
        Err(self.internal_err())
    }

    fn write_stdin(&self, _h: &SessionHandle, _data: &[u8]) -> Result<usize, TransportError> {
        Err(self.internal_err())
    }

    fn read_stdout(
        &self,
        _h: &SessionHandle,
        _max_bytes: usize,
    ) -> Result<Vec<u8>, TransportError> {
        Err(self.internal_err())
    }

    fn interrupt(&self, _h: &SessionHandle) -> Result<(), TransportError> {
        Err(self.internal_err())
    }

    fn terminate(&self, _h: &SessionHandle) -> Result<(), TransportError> {
        Err(self.internal_err())
    }

    fn try_wait(&self, _h: &SessionHandle) -> Result<Option<i32>, TransportError> {
        // No live process — return Some(0) so the agent's exit-observer
        // doesn't loop forever waiting on a child that can't exist.
        // Defensive: if start() never succeeded for this handle, the
        // agent shouldn't be calling try_wait on it anyway, but if it
        // does, we don't want to leave it polling forever.
        Ok(Some(0))
    }
}

/// Test-only: construct a synthetic SessionHandle with a given pid
/// and session id. Used by agent unit tests that need a live-looking
/// handle without going through `start()`.
#[cfg(test)]
pub(crate) fn synthetic_handle(session_id: impl Into<String>, pid: u32) -> SessionHandle {
    SessionHandle {
        session_id: session_id.into(),
        pid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_argv_rejected_even_on_stub() {
        let t = StubTransport::new();
        let result = t.start("sid", &[], HashMap::new(), None);
        assert!(matches!(result, Err(TransportError::EmptyArgv)));
    }

    #[test]
    fn start_returns_internal_with_helpful_message() {
        let t = StubTransport::new();
        let result = t.start("sid", &["claude".to_string()], HashMap::new(), None);
        match result {
            Err(TransportError::Internal(msg)) => {
                assert!(msg.contains("v0.9.1a"), "should mention version: {msg}");
                assert!(msg.contains("v0.9.1b"), "should mention next ship: {msg}");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn all_methods_return_internal_consistently() {
        let t = StubTransport::new();
        let h = synthetic_handle("sid", 0);
        assert!(matches!(
            t.write_stdin(&h, b"x"),
            Err(TransportError::Internal(_))
        ));
        assert!(matches!(
            t.read_stdout(&h, 4096),
            Err(TransportError::Internal(_))
        ));
        assert!(matches!(t.interrupt(&h), Err(TransportError::Internal(_))));
        assert!(matches!(t.terminate(&h), Err(TransportError::Internal(_))));
    }

    #[test]
    fn try_wait_returns_some_to_avoid_infinite_observer_loop() {
        // Defensive: stub never spawns, so try_wait shouldn't claim
        // "still running" — that would make the agent's exit-observer
        // poll forever.
        let t = StubTransport::new();
        let h = synthetic_handle("sid", 0);
        assert_eq!(t.try_wait(&h).unwrap(), Some(0));
    }

    #[test]
    fn handle_pid_is_zero_for_stub_handles() {
        let h = synthetic_handle("sid", 0);
        assert_eq!(h.pid(), 0);
    }

    #[test]
    fn custom_reason_propagates_through_internal_error() {
        let t = StubTransport::with_reason("custom test message");
        match t.start("sid", &["x".to_string()], HashMap::new(), None) {
            Err(TransportError::Internal(msg)) => assert_eq!(msg, "custom test message"),
            other => panic!("expected Internal with custom msg, got {other:?}"),
        }
    }

    #[test]
    fn dyn_session_transport_object_safe() {
        // Trait must be object-safe so Arc<dyn SessionTransport>
        // works (the agent stores the transport this way).
        let t: std::sync::Arc<dyn SessionTransport> = std::sync::Arc::new(StubTransport::new());
        let result = t.start("sid", &["claude".to_string()], HashMap::new(), None);
        assert!(matches!(result, Err(TransportError::Internal(_))));
    }
}
