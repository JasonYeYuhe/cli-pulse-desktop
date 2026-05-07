//! v0.8.1 — Remote module (post-incident reduction).
//!
//! v0.8.0 shipped a ConPTY managed-session local host (transport +
//! agent + events submodules) that crashed on launch (BEX64,
//! `STATUS_STACK_BUFFER_OVERRUN`, fault offset `0x4c9375`) on a clean
//! Windows VM. Rather than ship a half-fixed hotfix that might still
//! linger near the offending code path, v0.8.1 is a radical revert:
//! the ConPTY feature is removed entirely (modules deleted,
//! `portable-pty` + `windows-sys` deps removed, no agent loop spawn
//! in `lib.rs::run`).
//!
//! What v0.8.1 KEEPS from the v0.8.0 ship:
//! - `remote::log` — shared file appender used by `bin/remote_hook.rs`
//!   (closes the v0.7.0 diagnostic blind spot per
//!   `feedback_remote_hook_diagnostic_blind_spot.md`). Hook-side
//!   logging worked cleanly in production; this is the one piece of
//!   v0.8.0 worth preserving.
//!
//! ConPTY managed-session host returns in v0.9.x with a mandatory VM
//! smoke gate before promote-to-Latest (autonomy contract, learned
//! from the v0.8.0 incident).

pub mod log;
