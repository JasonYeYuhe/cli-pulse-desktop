# Dev Plan v2 — v0.9.x → v0.10.x post-incident sprint

**Date:** 2026-05-08
**Reviewers:** Gemini 3.1 Pro (this plan v2 + each per-ship diff)
**Trigger:** post-v0.8.0 BEX64 incident hardening + user-facing feature backlog.

**v2 changes from v1**: Gemini 3.1 Pro plan-review (2026-05-08) caught
2 P0 + 3 P1 + 3 P2 + 2 P3. All P0/P1 dropped or fixed; selected
P2/P3 applied. v1 ships marked `[CHANGED]` or `[DEFERRED]` below.

## Sprint thesis

The v0.8.0 BEX64 incident proved: stronger pre-promote gates, eager
Sentry, and tight Gemini diff review at each step. The product has
zero real users today (per `reference_desktop_repo.md`), so we have
freedom to ship aggressively on quality + features without breaking
anyone — *as long as* we use that freedom to make the product
compelling enough that distribution becomes worth pursuing.

This sprint is **6 ships** across v0.9.x → v0.10.1, mostly polish +
features users would actually want. v0.11.0 custom alerts is
**[DEFERRED]** to a sprint that allows Supabase schema design.

| Ship | Theme | LOC est. | Tests est. |
|---|---|---|---|
| v0.9.0 | Self-update reliability + Sentry flush + crash recovery | ~700 | ~35 |
| v0.9.1a | ConPTY agent comms scaffolding (RPC + log + diagnostic) | ~800 | ~150 |
| v0.9.1b | ConPTY transport + agent dispatch (the FFI ship) | ~1,400 | ~350 |
| v0.9.2 | Diagnostic bundle + binary polish (Sentry stays eager) | ~400 | ~20 |
| v0.10.0 | Keyboard shortcuts + date range + per-provider visibility | ~600 | ~30 |
| v0.10.1 | CSV/JSON export + provider comparison view | ~700 | ~35 |
| **Total** | — | **~4,600** | **~620** |

**[DEFERRED]** — Light theme (Gemini P0: codebase uses hardcoded
`bg-neutral-950` everywhere, not the `dark:` prefix pattern; would
need a 2-3k LOC dedicated sprint with full UI design pass).

**[DEFERRED]** — v0.11.0 custom alert rules engine (Gemini P1:
local-only persistence means duplicate notifs across devices, single
machine has limited utility; needs Supabase schema design first).

## Cross-cutting rules (from v1, hardened per Gemini review)

- **Mandatory VM smoke before promote-to-Latest.** No exceptions.
- **Gemini 3.1 Pro reviews the diff before commit** for each ship.
- **Pre-push hook** runs cargo fmt + clippy --all-targets -D warnings + cargo test + npm test + npm build.
- **No backend Supabase schema changes.** Features needing schema → defer.
- **Eager Sentry init** — never deferred. (Gemini P0: lazy-init would
  blind us to startup panics, the exact failure mode of v0.8.0.)
- **NEW (Gemini P1)**: pre-push hook adds **global** grep failing if
  `tokio::spawn(` appears in `src-tauri/src/` outside `#[cfg(test)]`.
  Replace with `tauri::async_runtime::spawn`.
- **NEW (Gemini P1)**: install a custom panic hook in v0.9.0 that
  calls `Client::close(Some(Duration::from_secs(2)))` for synchronous
  Sentry flush before chaining to the default abort handler.

## Ship 1 — v0.9.0: Self-update + Sentry-flush + crash-recovery (~700 LOC)

### Why first

Hard blocker for v0.9.1a/b. Three orthogonal stability fixes bundled
because they're small individually but need to land before any risky
ship.

### Scope

1. **Reproduce + fix v0.5.3 `os error 3`** on per-user NSIS install.
   Likely root cause: `tauri-plugin-updater`'s installer-path
   resolution doesn't account for `%LOCALAPPDATA%\CLI Pulse\` (vs
   default `%LOCALAPPDATA%\Programs\…`). Patch path resolution OR
   gracefully fall back to "click to download manually" link.
2. **Better update error messages** — categorized: "Network / disk
   full / installer corrupt / permissions / unknown". Replace raw
   OS error code in red toast.
3. **Synchronous Sentry flush before abort (Gemini P1)** — install a
   custom panic hook that:
   - Captures the panic via Sentry's auto-hook (still active)
   - Then explicitly calls `sentry::Hub::current().client().map(|c| c.close(Some(Duration::from_secs(2))))` to flush queued events
   - Then chains to the previous panic handler (which under
     `panic = "abort"` is the default abort)
   - Verify v0.8.0-class panic now reliably reaches Sentry, not by luck.
4. **Crash-recovery mode** — at startup, check
   `%LOCALAPPDATA%\dev.clipulse.desktop\crash-history.json`. If 3+
   entries within last 5 min:
   - Write a marker file
   - **Keep Sentry ON** (Gemini P2: counterproductive to disable;
     ongoing crashes need MORE telemetry not less)
   - Disable: agent loop (when v0.9.1b ships), tray refresh loop
   - Show UI banner: "Detected repeated crashes. Some features
     disabled. [Re-enable] [Send diagnostic]"
5. **Crash history schema** — append-only JSONL: `{ts, exit_code,
   last_log_lines: [...]}`. Capped at 100 entries with FIFO rotation.

### Success criteria

- Per-user NSIS install: auto-update either works or shows actionable
  error + manual-download link
- After 3 deliberate force-kills during launch, 4th launch shows
  recovery banner; Sentry still active
- Synchronous Sentry flush verified: deliberately panic in setup
  hook, confirm Sentry receives event 100% of the time over 10
  attempts (was probabilistic in v0.8.0)
- 27 → 35 backend tests (+8: crash-history append + threshold +
  panic-flush integration)

### Reviewer pre-questions for Gemini

- Crash-history file location vs keychain: `%LOCALAPPDATA%` is fine
  (mode-default per-user; crash data not credential-shaped).
- Custom panic hook ordering: capture-then-flush-then-chain. Verify
  this is the correct order with sentry-rust's hook chain semantics.
- Recovery-mode disabled-feature list: agent + tray. Add Sentry
  test event auto-emit so dashboard shows "this user is in recovery
  mode"?

---

## Ship 2 — v0.9.1a: Agent comms scaffolding (~800 LOC, no FFI)

### Split rationale (Gemini P3)

v1 plan had v0.9.1 = 2,200 LOC restoring all 4 ConPTY modules at
once. Gemini flagged this as too large for one risky ship. Split
into 2a (no FFI, just scaffolding) and 2b (FFI + transport).

v0.9.1a ships first. If it deploys cleanly (which it should — no FFI,
no risky code), 2b's risk is contained to a smaller diff.

### Scope (no FFI, no portable-pty, no windows-sys)

1. Restore `src-tauri/src/remote/{agent.rs, events.rs}` from git
   (commit `c37cec0`), but with `transport.rs` STUBBED — use a mock
   transport that returns `TransportError::Internal("v0.9.1b not
   shipped yet")` on every method.
2. Restore the 4 helper RPC wrappers in `supabase.rs` —
   `remote_helper_register_session`, `remote_helper_pull_commands`,
   `remote_helper_post_event`, `remote_helper_complete_command`.
3. Restore `remote_app_request_session_start` wrapper.
4. Restore Tauri commands: `request_remote_session_start` (which
   will register a pending row server-side, but no spawn happens),
   `agent_diagnostic`.
5. Restore the agent loop spawn in `lib.rs::run`'s setup hook,
   **using `tauri::async_runtime::spawn` (Gemini P1) — NOT
   `tokio::spawn` (the v0.8.0 bug)**.
6. **NEW kill-switch**: `CLI_PULSE_DISABLE_REMOTE_AGENT=1` env var.
   Default OFF (agent runs). Set to "1" via env to disable. Logged
   at startup so users can verify their env flag took effect.
7. **NEW pre-push grep guard (Gemini P1)**: extend
   `scripts/git-hooks/pre-push` to fail on any `tokio::spawn(`
   occurrence in `src-tauri/src/**.rs` outside `#[cfg(test)]`.

### Success criteria

- VM smoke PASS — process launches, agent loop starts, ticks every
  1s, pulls commands (gets empty list normally), no crashes
- Spawn dialog (UI) submits → server creates pending row → agent
  picks it up → returns `TransportError::Internal` →
  `complete_command` marks the row failed → user sees clear error
  in UI ("Spawn not yet supported in this build")
- 274 → 320 backend tests (+46 covering all v0.8.0 tests minus
  transport-FFI-specific)
- Pre-push grep would have FAILED on v0.8.0 commit (regression
  test for the test)

### Out of scope (v0.9.1a)

- Anything FFI: portable-pty, windows-sys, Job Object, ConPTY
  pseudoconsole spawn
- Reader thread (no master to clone from)
- Boot-time orphan reconciliation (deferred to 2b)

### Reviewer pre-questions for Gemini

- Stub-transport approach OK, or should we delay the spawn-dialog UI
  to 2b too so users don't see "fails always"? (Stub keeps the
  scaffolding integration-testable on production.)
- Default kill-switch state: OFF (agent runs) seems right since the
  whole point of 2a is to validate the agent loop. But should the
  initial Latest of v0.9.1a flip default to ON pending VM smoke?

---

## Ship 3 — v0.9.1b: ConPTY transport + FFI (~1,400 LOC)

### Why this is the highest-risk ship of the sprint

This brings back `portable-pty` + `windows-sys` linkage + Job Object
FFI + ConPTY pseudoconsole spawn. Same code surface as v0.8.0 but
WITHOUT the bug that caused v0.8.0 to crash (that was in 2a's setup
spawn, not in 2b's FFI). Still — every unsafe block is a risk.

### Scope (the FFI part)

- Restore `src-tauri/src/remote/transport.rs` from git
- Restore `portable-pty` + `windows-sys` Cargo deps with the v0.8.2
  Win32_Security feature added
- All 4 v0.8.0 Gemini plan-review fixes:
  - P0 #1 spawn_blocking around sync transport calls
  - P0 #2 0x03-byte cross-platform Ctrl-C (avoids host-kill risk)
  - P1 Job Object KILL_ON_JOB_CLOSE
  - P2 Drop on HandleInner
- All 3 v0.8.0 Gemini diff-review P1 fixes:
  - per-call `tokio::time::timeout(5s)`
  - log rotation Win write-bit
  - graceful shutdown
- **NEW**: boot-time orphan reconciliation via `sysinfo` walk —
  on agent startup, look for any `claude.exe` whose parent isn't us;
  for each one, query Supabase via `remote_app_list_sessions` and
  mark associated `remote_sessions(status='running')` rows as
  `errored`. Heuristic but defensible.
- Switch the stub transport in 2a to `ConPtyTransport::new()`.

### Success criteria

- VM smoke PASS (60s launch survival + zero new BEX64 events) —
  THIS is the v0.8.0 regression check
- Real session spawn round-trip via Mac → VM works (Block A from
  the v0.8.0 VM verify)
- Interrupt button does NOT kill `cli-pulse-desktop.exe`
  (P0 #2 verification)
- Force-kill desktop → all `claude.exe` children die within 2s
  (Job Object verification)
- 320 → 380 backend tests (+60 covering transport, FFI safety,
  Drop pattern)

### Reviewer pre-questions for Gemini

- Sysinfo orphan walk: if user has `claude` running OUTSIDE our
  managed sessions (e.g. they launched it manually), do we
  accidentally mark THAT session as errored? Need an env-marker.
- Job Object: previous Cargo.lock had `windows-sys 0.59` — confirm
  same version still works post-v0.8.2.

---

## Ship 4 — v0.9.2: Diagnostic bundle + binary polish (~400 LOC)

### v2 changes from v1

- **DROPPED (Gemini P0)**: lazy-init Sentry. Sentry stays eager.
  Compelling reason to defer Sentry is "it crashes the app at
  startup", which we have ZERO evidence of and which would itself
  be a panic Sentry should catch.
- **KEPT**: PE version metadata fix, lazy regex compile, "Send
  diagnostic bundle" button, `--self-test` CLI mode.

### Scope

1. **"Send diagnostic bundle" button** in `Settings → About`. Click
   → zip the following into `~/Downloads/cli-pulse-diag-<timestamp>.zip`:
   - `cli-pulse.log` (current full file)
   - `remote-hook.log` (current full file)
   - `diagnostic_snapshot` output (JSON pretty-printed)
   - `crash-history.json` (if exists)
   - Last 5 WER `Application Error` events for `cli-pulse-desktop` (Win)
2. **PE version metadata fix** — set MajorImageVersion via
   `winres` crate or build.rs. Verifier flagged 0.0.0.0 in v0.8.0
   WER report.
3. **Lazy-compile heavy regex** in `redaction.rs` — currently 30+
   patterns compiled via `Lazy<Regex>` at first redact call. Move
   compile off the GUI startup path (defer to first call from
   `bin/remote_hook.rs`).
4. **`cli-pulse-desktop --self-test` CLI mode** — runs `vm-smoke-launch.ps1`
   equivalent in-process, prints PASS/FAIL.

### Success criteria

- Diagnostic bundle <2 MB, contains all expected files
- PE version metadata reads correctly in Windows Properties → Details
- Self-test exits 0 on healthy install

---

## Ship 5 — v0.10.0: Keyboard shortcuts + date range + per-provider visibility (~600 LOC)

### v2 changes from v1

- **REMOVED**: light theme (Gemini P0: 2-3k LOC sprint of its own).
- **REPLACED with**: per-provider visibility toggle (hide providers
  you don't use). Smaller, clearer scope.

### Scope

1. **Custom date range picker** on Overview, Providers, Sessions
   tabs. Currently fixed at 30/7 days. Options: Today / 7d / 30d /
   90d / Custom. Persisted in localStorage per tab.
2. **Keyboard shortcuts**:
   - `Ctrl+R` rescan
   - `Ctrl+,` open Settings
   - `Ctrl+1..5` switch tabs (Overview / Providers / Sessions /
     Alerts / Settings)
   - `Esc` close modals (already partly works post-v0.6.1)
   - `Ctrl+Shift+/` opens shortcut help overlay
3. **Per-provider visibility toggle** — Settings → Appearance →
   "Visible providers". Checkboxes per provider; unchecked
   providers hidden from Overview / Providers tabs. Persisted in
   localStorage.

### Success criteria

- All three features work in en/zh-CN/ja
- Date range works without performance regression on 90-day Sessions
- Shortcut help overlay matches actual handlers (no orphan docs)

---

## Ship 6 — v0.10.1: CSV/JSON export + provider compare (~700 LOC)

### Scope

1. **Export** — Settings → Data → Export. Options:
   - Format: CSV / JSON
   - Date range: re-uses v0.10.0 picker
   - Granularity: daily / per-session
   - Include redaction: yes/no (default yes)
   - **(Gemini P3)**: token counts encoded as STRINGS in JSON to
     avoid `BigInt`/Number precision loss in JS round-trip
   - Output: file save dialog
2. **Provider comparison view** — Overview → "Compare providers"
   button. Side-by-side: cost / tokens / sessions for each provider
   over selected range. Stacked bar + table.

### Success criteria

- 90 days × 5 providers × 50 sessions = ~22,500 rows exports CSV in <2s
- JSON round-trips through `jq` without precision loss
- Provider compare renders cleanly with 0 / 1 / 5+ providers
- All copy in en/zh-CN/ja

---

## Risk register (v2)

| Risk | Probability | Mitigation |
|---|---|---|
| v0.9.1b ConPTY redo crashes again | Low (root cause confirmed + kill-switch + VM smoke + global pre-push grep + sync Sentry flush) | All 7 fixes from v0.8.0 review baked in; new env kill-switch lets users self-recover; VM smoke gate before promote |
| Custom panic hook order wrong | Low | v0.9.0 explicit test: deliberately panic in setup, verify Sentry event arrives 10/10 attempts |
| VM smoke session crashes mid-test (pattern from v0.8.x) | Medium | Three smoke paths (ps1 / brief.md / full.ps1); Jason can RDP; **(Gemini P2)**: future v0.10.x might add a headless CI integration test for ConPTY core |
| Gemini API capacity exhausted | Medium | Plan reviews can defer; per-diff reviews can fall back to Codex; never skip-to-ship |
| Per-user NSIS update fix breaks per-machine install | Medium | Test both install layouts in v0.9.0 VM smoke |
| Boot-time orphan walk false-positive on user's manual `claude` | Medium | Use env-marker `CLI_PULSE_AGENT_MANAGED=1` (Gemini suggestion); only mark sessions whose child has that env set |
| BigInt precision in JSON export | Low | Encode large integers as strings; document in JSON schema |

## Sequencing

```
v0.9.0  (3-5 days)  — self-update + Sentry flush + crash recovery
  ↓ (ship to Latest after VM smoke PASS — HARD GATE for everything below)
v0.9.1a (3-4 days)  — agent comms scaffolding (no FFI)
  ↓ (ship to Latest after VM smoke PASS)
v0.9.1b (5-7 days)  — ConPTY transport + FFI (THE BIG ONE)
  ↓ (ship to Latest after VM smoke PASS — if FAIL, revert immediately, regroup)
v0.9.2  (2-3 days)  — diagnostic bundle + binary polish
  ↓
v0.10.0 (3-4 days)  — keyboard shortcuts + date range + provider visibility
  ↓
v0.10.1 (3-4 days)  — CSV/JSON export + provider compare
```

Total: 3-4 weeks of active development. Adds ~4,600 LOC + ~620 tests
to the codebase.

## Out of scope for THIS sprint

- **Light theme** [DEFERRED, gemini P0] — needs design pass + ~2-3k LOC own sprint
- **v0.11.0 Custom alert rules** [DEFERRED, gemini P1] — needs Supabase schema + cross-team alignment
- Microsoft Store / Flathub / AUR distribution — account-level, needs Jason explicit approval
- Anonymous usage analytics opt-in — privacy review needed
- Mac team's Multi-CLI v1.14+ design — wait for them

## Memory updates after sprint

- Each ship: append to `reference_desktop_repo.md` sprint history
- v0.9.1b: update `feedback_v080_crash_on_launch_incident.md` with
  retrospective on whether v0.9.x rules prevented recurrence
- Distribution decision review (continue WATCHING, or kick off
  Microsoft Store / Flathub work)

— end of plan v2 —
