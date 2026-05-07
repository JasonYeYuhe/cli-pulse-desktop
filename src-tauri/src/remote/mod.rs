//! v0.8.0 — Remote Sessions local-host module.
//!
//! Slice 4 of the Remote Sessions track: this Windows / Linux desktop
//! becomes a peer to the Mac helper for hosting managed Claude
//! sessions that another device's UI is driving.
//!
//! Module layout (per `PROJECT_DEV_PLAN_2026-05-07_v0.8.0_conpty_managed_sessions.md`):
//! - `transport` — `SessionTransport` trait + `ConPtyTransport` impl
//!   (portable-pty wrapper, Job Object on Windows, write-0x03-for-
//!   interrupt cross-platform pattern).
//! - `agent` — `RemoteAgentManager` (per-session state map + 1s tick
//!   loop pulling commands via `remote_helper_pull_commands` and
//!   dispatching to the transport).
//! - `events` — lifecycle event poster (status / info / orphan-cleanup).
//! - `log` — shared file appender used by both the agent AND the
//!   `remote_hook.rs` subprocess. Folds in the v0.7.1 hotfix scope
//!   from `feedback_remote_hook_diagnostic_blind_spot.md`.

pub mod agent;
pub mod events;
pub mod log;
pub mod transport;

pub use agent::{spawn_agent_loop, AgentDiagnostic, AgentHandle, RemoteAgentManager};
pub use transport::{ConPtyTransport, SessionHandle, SessionTransport, TransportError};
