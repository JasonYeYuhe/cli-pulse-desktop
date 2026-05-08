//! v0.9.1a — Remote module (post-incident return, scaffolding only).
//!
//! ## History
//!
//! v0.8.0 shipped a ConPTY managed-session local host (transport +
//! agent + events submodules) that crashed on launch (BEX64,
//! `STATUS_STACK_BUFFER_OVERRUN`). Sentry confirmed the root cause:
//! `tokio::spawn` from inside Tauri's `setup` hook, which runs
//! OUTSIDE any tokio runtime context — panic with "no reactor
//! running, must be called from the context of a Tokio 1.x runtime"
//! → `panic = "abort"` → process termination.
//!
//! v0.8.1 reverted the entire feature (modules deleted,
//! `portable-pty` + `windows-sys` deps removed). v0.9.1a brings back
//! the SCAFFOLDING with the bug fixed (`tauri::async_runtime::spawn`
//! instead of `tokio::spawn`) but uses a STUB transport instead of
//! the FFI-heavy ConPTY one. v0.9.1b will swap the stub for the
//! real `ConPtyTransport` (separate ship, smaller blast radius per
//! Gemini plan-review P3).
//!
//! ## What this module provides (v0.9.1a)
//!
//! - `transport` — `SessionTransport` trait, `SessionHandle`, and
//!   `StubTransport` (every method returns `TransportError::Internal`)
//! - `agent` — `RemoteAgentManager` + 1 s tick loop pulling commands
//!   via `remote_helper_pull_commands`, dispatching, posting events
//! - `events` — `EventPoster` for lifecycle status / info events
//! - `log` — shared file appender used by both the agent AND
//!   `bin/remote_hook.rs` (the v0.7.1 diagnostic-blind-spot fix that
//!   v0.8.1 preserved through the revert)
//!
//! v0.9.1a behavior: when the user clicks "+ Start new session" in
//! the Sessions tab UI:
//! 1. Server creates a `remote_sessions(status='pending')` row +
//!    `kind='start'` command
//! 2. The agent loop pulls it on next tick (~1 s)
//! 3. `transport.start()` (the stub) returns
//!    `TransportError::Internal("v0.9.1a stub transport — ...")`
//! 4. Agent posts a `kind=errored` lifecycle event + a `kind=info`
//!    detail event with the stub's reason string
//! 5. Agent calls `remote_helper_complete_command(failed)` so the
//!    queue advances
//! 6. UI sees the row flip from `pending` → `errored` and shows the
//!    "not yet supported" reason

pub mod agent;
pub mod events;
pub mod log;
pub mod transport;

pub use agent::{spawn_agent_loop, AgentDiagnostic, AgentHandle, RemoteAgentManager};
pub use transport::{ConPtyTransport, SessionHandle, SessionTransport, TransportError};
