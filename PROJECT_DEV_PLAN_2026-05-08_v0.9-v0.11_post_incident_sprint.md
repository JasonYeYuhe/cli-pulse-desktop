# Dev Plan — v0.9.x → v0.11.x post-incident sprint

**Date:** 2026-05-08
**Reviewers:** Gemini 3.1 Pro (this plan + each per-ship diff)
**Trigger:** post-v0.8.0 incident hardening + the user-facing feature backlog that's been deferred while the Remote Sessions track absorbed all sprint capacity.

## Sprint thesis

The v0.8.0 BEX64 incident proved two things:
1. We need stronger pre-promote gates (mandatory VM smoke; PDB symbols; better Sentry tagging).
2. The desktop has zero real users today (per [reference_desktop_repo.md](~/.claude/projects/-Users-jason-Documents-cli-pulse/memory/reference_desktop_repo.md) — v0.7.0 NSIS = 2 downloads, both internal). That means we have **freedom to ship aggressively on quality + features** without breaking anyone, *as long as* we use that freedom to make the product compelling enough that distribution becomes worth pursuing.

This sprint is 6 ships across v0.9.x → v0.11.0, mostly polish + features users would actually want:

| Ship | Theme | LOC est. | Tests est. |
|---|---|---|---|
| v0.9.0 | Self-update reliability + crash recovery | ~600 | ~30 |
| v0.9.1 | ConPTY managed-session host (redo) | ~2,200 | ~500 |
| v0.9.2 | Diagnostic bundle + binary polish | ~500 | ~25 |
| v0.10.0 | Custom date range + light theme + keyboard shortcuts | ~900 | ~40 |
| v0.10.1 | CSV/JSON export + provider comparison view | ~700 | ~35 |
| v0.11.0 | Custom alert rules engine | ~1,000 | ~60 |
| **Total** | — | **~5,900** | **~690** |

Each ship is independently shippable, has its own VM smoke gate, and has explicit GO/NO-GO criteria. The order is **stability → infra → features**, not the reverse, because v0.8.0 proved the cost of skipping that order.

## Cross-cutting rules (apply to every ship in the sprint)

- **Mandatory VM smoke before promote-to-Latest.** No exceptions. Either via `scripts/vm-smoke-full.ps1` (single-paste) or via a fresh VM Claude session reading [scripts/VM_SMOKE_BRIEF.md](scripts/VM_SMOKE_BRIEF.md). Skipping = repeat of v0.8.0.
- **Gemini 3.1 Pro reviews the diff before commit** for each ship, not just plan. Catches the kind of one-line bugs the v0.8.0 `tokio::spawn` caused.
- **Each ship's PR-equivalent commit must pass `cargo clippy --all-targets -- -D warnings` AND the pre-push hook** (cargo fmt + cargo test + npm test + npm build). The pre-push gate caught 3 issues during v0.8.x.
- **No backend Supabase schema changes.** If a feature needs schema, it stops at "design + Mac team alignment", does not ship.
- **WATCHING-mode rules paused for this sprint.** User explicitly asked for a feature push with Gemini review at each step.

## Ship 1 — v0.9.0: Self-update reliability + crash recovery (~600 LOC)

### Why first

The v0.5.3 latent auto-update bug (`os error 3` on per-user NSIS installs, surfaced in 2026-05-06 VM verify) means users on certain install configurations cannot auto-update. If v0.8.0-class incidents recur, we can't push a hotfix to those users. **This must work BEFORE v0.9.1 ConPTY redo ships**, because v0.9.1 will be the first ship since v0.8.0 that has a non-trivial chance of needing an in-flight hotfix.

Also: the v0.8.0 BEX64 crash-on-launch loop (9+ launches in 4 min) showed that the app has no defense against repeated crashes. v0.9.0 adds a "crash recovery mode" — detect 3+ launch failures within 5 min and offer to disable the latest feature on the next launch.

### Scope

1. **Reproduce the v0.5.3 `os error 3`** — install v0.7.0 NSIS as a per-user (not per-machine) install on a fresh VM, observe whether `Settings → Check for updates` returns the same error. If yes, root cause it. (Likely: `tauri-plugin-updater`'s installer-path resolution doesn't account for per-user install layouts.)
2. **Patch path resolution**, OR fall back to graceful "click here to download manually" link in the update banner if auto-install fails.
3. **Better update error messages** — currently red toast with raw OS error. Replace with categorized: "Network error / disk full / installer corrupt / permissions / unknown".
4. **Crash-recovery mode** — at startup, check `%LOCALAPPDATA%\dev.clipulse.desktop\crash-history.json`. If 3+ entries within last 5 min, write a marker file that disables: (a) Sentry init, (b) the agent loop (when v0.9.1 ships), (c) tray refresh loop. Show a UI banner: "Detected repeated crashes. Some features disabled. [Re-enable] [Send diagnostic]".
5. **Crash history schema** — append-only JSONL: `{ts, exit_code, last_log_lines: [...]}`. Capped at 100 entries with FIFO rotation.

### Success criteria

- Per-user NSIS install on VM: `Settings → Check for updates` either (a) installs successfully, or (b) shows actionable error + manual-download link. NOT raw `os error 3`.
- After 3 deliberate crashes (kill via Task Manager during launch), 4th launch shows recovery banner with disable-features option.
- 27 → 30 backend tests (+3 covering crash-history append + threshold detection).

### Out of scope

- "Skip this version" button on the update banner (deferred to v0.10.x; needs persistent skip-list storage)
- Self-test mode (`--self-test` flag) — moved to v0.9.2

### Reviewer pre-questions for Gemini

- Crash-history is best-effort logging; should the file be in keychain-encrypted storage instead, or is `%LOCALAPPDATA%` mode-default fine?
- The disable-on-recovery list is hardcoded (Sentry / agent / tray). Should it instead be feature-flag-driven so users can choose granularly?
- Re-enable should be sticky-once or sticky-forever?

---

## Ship 2 — v0.9.1: ConPTY managed-session host REDO (~2,200 LOC)

### Why this ship is non-trivial

This is the v0.8.0 re-attempt with the actual root cause fixed (Sentry confirmed: `tokio::spawn` in Tauri's `setup` hook panics with "no reactor running"). Mac team has already shipped their `helper/transports/conpty.py` waiting for our parity, so this closes the cross-platform loop they explicitly designed for.

But it ALSO carries every risk v0.8.0 carried: portable-pty static linking, Job Object FFI, ConPTY pseudoconsole spawn, agent dispatch loop. v0.8.1 reverted all of this. v0.9.1 brings it back with the lessons applied.

### Lessons baked in from v0.8.0

| v0.8.0 issue | v0.9.1 fix |
|---|---|
| `tokio::spawn` in `setup` hook panicked | Use `tauri::async_runtime::spawn` everywhere; verify in pre-push hook via grep |
| `panic = "abort"` turned panic into BEX64 | Keep abort but add panic site smoke test (deliberately panic in setup, verify Sentry captures + crash-history records) |
| No symbols → fault offset unsymbolizable | v0.8.2 already shipped `debug = "line-tables-only"` + PDB upload; verify v0.9.1's first build emits PDB |
| No kill-switch | Add `CLI_PULSE_DISABLE_REMOTE_AGENT=1` env var — agent loop won't spawn even if paired |
| No VM smoke gate | This sprint's cross-cutting rule: mandatory VM smoke before promote |
| Single-bug post-mortem took ~30 min | v0.9.0 crash-recovery mode would have auto-mitigated by 4th relaunch |

### Scope (mostly retained from v0.8.0 plan)

- Restore `src-tauri/src/remote/{transport,agent,events}.rs` from git (commit `c37cec0`)
- All 4 Gemini plan-review fixes:
  - P0 #1 spawn_blocking around sync transport calls
  - P0 #2 0x03-byte interrupt (cross-platform Ctrl-C; avoids host-kill risk)
  - P1 Job Object KILL_ON_JOB_CLOSE for orphan auto-cleanup
  - P2 Drop on HandleInner for teardown
- All 3 Gemini diff-review P1 fixes:
  - per-call `tokio::time::timeout(5s)` around spawn_blocking
  - log rotation `set_len(0)` Windows write-bit
  - graceful shutdown via `manager.shutdown()` before loop exits
- **NEW critical fix (the actual v0.8.0 bug)**: every `tokio::spawn` inside the agent's setup-time call path → `tauri::async_runtime::spawn`
- **NEW kill-switch**: `CLI_PULSE_DISABLE_REMOTE_AGENT=1` env var. Default OFF (safe). Documented in CHANGELOG + Settings → About.
- **NEW pre-push grep**: `scripts/git-hooks/pre-push` adds a check that fails if `tokio::spawn(` appears in any `src-tauri/src/remote/` file outside `#[cfg(test)]`.
- **NEW boot-time orphan reconciliation**: agent startup uses `sysinfo` to walk processes, finds any `claude.exe` whose parent is not the current `cli-pulse-desktop.exe`, and asks Supabase to mark associated `remote_sessions` rows as `errored`. (Replaces the deferred helper-RPC approach from v0.8.0 plan.)

### Success criteria

- VM smoke PASS (60s launch survival + zero new BEX64 events) — first thing the verifier checks
- Gemini reviews diff and confirms `async_runtime::spawn` is used (NOT `tokio::spawn`)
- Pre-push grep would have FAILED on the v0.8.0 commit (regression test for the test)
- 274 → 320 backend tests (+46 covering transport, agent, events as before)
- VM verify after Latest: spawn a session via Mac → verify lifecycle round-trip

### Reviewer pre-questions for Gemini

- Boot-time orphan reconciliation via `sysinfo` is heuristic (process name match). Acceptable for v0.9.1, or wait for Mac team to add the helper-side list RPC?
- Kill-switch via env var means a user with broken agent can't fix it without restart. Should we also add a Tauri command `disable_agent_loop` that flips a runtime flag? (Probably yes for v0.9.x, no for v0.9.1.)
- The pre-push grep for `tokio::spawn` is a regression-test-for-the-test. Worth the false-positive risk on legitimate unit-test code?

---

## Ship 3 — v0.9.2: Diagnostic bundle + binary polish (~500 LOC)

### Why

If v0.9.1 ships and a future BEX64 happens, the post-mortem requires the verifier to manually collect: `cli-pulse.log`, `remote-hook.log`, WER mdmp, `diagnostic_snapshot` output, and `Cargo.lock` SHAs. This ship makes that one-click.

Binary polish is the cleanup pass: PE version metadata fix, lazy-init Sentry to shave startup time, lazy-compile heavy regex patterns.

### Scope

1. **"Send diagnostic bundle" button** in `Settings → About`. Click → zip the following into `~/Downloads/cli-pulse-diag-<timestamp>.zip`:
   - `cli-pulse.log` (full file, current)
   - `remote-hook.log` (full file, current)
   - `diagnostic_snapshot` output (JSON pretty-printed)
   - `crash-history.json` (if exists)
   - Last 5 WER `Application Error` events for `cli-pulse-desktop` (Windows only)
   - `tauri.conf.json` `version` + `Cargo.toml` `version` for sanity
2. **PE version metadata fix** — set MajorImageVersion / MinorImageVersion via `Cargo.toml` `[package.metadata.tauri]` or build.rs + `winres` crate. Verifier flagged 0.0.0.0 in WER report; cosmetic but cleaner.
3. **Lazy-init Sentry** — currently `sentry_init::install()` runs unconditionally before `tauri::Builder`. Profile shows ~50ms. Defer to first `log::error!` call OR move to a background thread.
4. **Lazy-compile heavy regex** — `redaction.rs` has 30+ regex patterns compiled via `Lazy<Regex>` at first redact call. Most users hit redaction in `bin/remote_hook.rs` only. Move regex compile off the GUI startup path.
5. **`cli-pulse-desktop --self-test` CLI mode** — runs the same checks `vm-smoke-launch.ps1` does, prints PASS/FAIL. Useful for bug reporters.

### Success criteria

- "Send diagnostic bundle" produces a <2 MB zip with all expected files
- Startup time on cold launch (Win + Linux) reduced by ≥30% vs v0.9.1
- PE version metadata reads correctly in Windows Properties → Details
- Self-test mode exits 0 on healthy install, non-zero with diagnostic message on broken

### Out of scope

- Crash dump auto-symbolization in the diagnostic bundle (would need shipping PDB inside the zip — too big)
- Sentry breadcrumbs auto-attach to bundle (privacy review needed)

---

## Ship 4 — v0.10.0: Date range picker + light theme + keyboard shortcuts (~900 LOC)

### Why

First user-facing feature ship of the sprint. These three items are the most-requested polish (per anecdata + the Mac sibling's user feedback referenced in `feedback_mac_windows_remote_track_alignment.md`).

### Scope

1. **Custom date range picker** on Overview, Providers, Sessions tabs. Currently fixed at "30 days" / "7 days". New: `[Last 7 days ▼]` dropdown with options: Today / 7d / 30d / 90d / Custom (date range picker). Persists in localStorage per tab.
2. **Light theme** — currently dark only. Add `[Dark ▼]` selector in Settings → Appearance. Options: Auto (follow OS) / Dark / Light. Tailwind already supports both via `dark:` prefix; need to invert the default and add light tokens.
3. **Keyboard shortcuts** — `Ctrl+R` rescan, `Ctrl+,` open Settings, `Ctrl+1..5` switch tabs, `Esc` close modals, `Ctrl+Shift+L` toggle theme.
4. **Shortcut help** — `Ctrl+/` opens overlay listing all shortcuts.

### Success criteria

- All three features work in en/zh-CN/ja
- Light theme passes accessibility check (4.5:1 contrast for body text)
- Keyboard shortcut overlay matches actual handlers (no orphan documentation)
- Date range works without performance regression on Sessions tab (current 24-h fixed list → 90-day worst case)

### Out of scope

- Custom theme editor (just "auto / dark / light")
- Shortcut customization (just the defaults)
- Drag-to-reorder dashboard tiles (deferred to v0.11+)

---

## Ship 5 — v0.10.1: CSV/JSON export + provider comparison view (~700 LOC)

### Why

Power users have been hitting the "I want to do X analysis on my own data" wall. Export gives them an escape hatch; comparison view is the most common in-app analysis ask.

### Scope

1. **Export** — Settings → Data → Export. Options:
   - Format: CSV / JSON
   - Date range: re-uses v0.10.0 picker
   - Granularity: daily / per-session
   - Include redaction: yes/no (default yes)
   - Output: file save dialog → user picks location
2. **Provider comparison view** — Overview → "Compare providers" button. Side-by-side: cost / tokens / sessions for each provider over the selected date range. Stacked bar chart + table.
3. **Per-day cost-per-token trend** — small tooltip on the existing Overview cards: "Claude is now $X/MTok, +Y% vs 30 days ago". Highlights pricing drift.

### Success criteria

- Export of 90 days × 5 providers × 50 sessions = ~22,500 rows produces a CSV in <2s
- JSON export round-trips through `jq` without parse errors
- Provider comparison renders with 0 / 1 / 5+ providers gracefully (empty state, single, full)
- All copy in en/zh-CN/ja

### Out of scope

- Import (re-shipping data to another machine — has privacy implications, needs design)
- Scheduled exports (would need cron-like persistence)
- Email digest export (no email service)

---

## Ship 6 — v0.11.0: Custom alert rules engine (~1,000 LOC)

### Why

Currently alerts are hardcoded (budget 80%/100%, sync failure 3× streak, etc). Users want: "alert when daily cost on Claude > $X", "alert when any session > 30 min", "alert when token usage drops by Y% vs last week" (anomaly detection for catching invisible-CLI-hang scenarios).

### Scope

1. **Rule schema** — JSON-serialisable, persisted in `HelperConfig`:
   ```json
   {
     "id": "uuid",
     "label": "Claude daily over $5",
     "metric": "cost",
     "scope": { "provider": "claude", "window": "1d" },
     "operator": ">",
     "threshold": 5.0,
     "severity": "warning",
     "enabled": true
   }
   ```
2. **Rule editor** — Settings → Alerts → Custom Rules → `[+ New rule]`. Form-driven; no code.
3. **Rule evaluator** — runs as part of `alerts::compute()` after every scan. Pure function, no I/O.
4. **Default rule library** — preset rules users can enable with one click: "Daily cost > $10", "Any session > 1 hour", "Provider request rate dropped 50%".
5. **Notification dedup** — a rule that fires every tick after threshold is annoying. Dedup: don't re-notify same rule until either (a) condition cleared then re-triggered, or (b) 24 hours elapsed.
6. **Test rule** — preview button: "Show me which past 7-day data would have triggered this rule".

### Success criteria

- 10 default rules ship with the binary, all useful
- Custom rule editor creates valid rule JSON, persists, fires correctly
- Rule eval runs in <50ms even with 50 user-defined rules
- All copy in en/zh-CN/ja

### Out of scope

- Rule sharing (export/import rule sets) — deferred
- Webhook notifications (Slack/Discord) — would need URL handling + secret management
- AI-assisted rule suggestion ("you spent unusually much on Claude today, want to alert next time?") — too ambitious for v0.11

### Reviewer pre-questions for Gemini

- Rule storage in HelperConfig means it doesn't sync across devices. Acceptable, OR worth Supabase schema design now?
- Default rule library — should they be enabled-by-default or opt-in?
- Rule severity levels (info/warning/critical) — map to existing alert taxonomy or extend?

---

## Risk register

| Risk | Probability | Mitigation |
|---|---|---|
| v0.9.1 ConPTY redo crashes again | Medium-Low (root cause confirmed + kill-switch + VM smoke gate) | All 4 fixes baked in pre-commit; kill-switch lets users self-recover; VM smoke catches before promote |
| Light theme breaks i18n strings | Low | i18n.test.ts critical-labels covers all new keys; manual visual test in en/zh-CN/ja |
| Custom alert rules engine performance | Medium | Hard cap at 50 rules; benchmark on 1k-day fixture pre-merge; flame-graph startup |
| Date range picker overruns memory on 90-day Sessions | Medium | Server-side pagination already in place; add client-side virtualization if needed |
| Gemini API capacity exhausted (happened during v0.8.1) | Medium | Plan reviews can defer to next-day if hit; per-diff reviews can fall back to Codex; never skip-to-ship |
| VM smoke session crashes mid-test (pattern from v0.8.x) | Medium | Three smoke paths (ps1 / brief.md / full.ps1); Jason can RDP in directly if all fail |

## Sequencing

```
v0.9.0 (3-5 days) — self-update reliability + crash recovery
  ↓ (ship to Latest after VM smoke PASS)
v0.9.1 (5-8 days) — ConPTY redo (THE BIG ONE)
  ↓ (ship to Latest after VM smoke PASS — if FAIL, revert immediately, regroup)
v0.9.2 (2-3 days) — diagnostic bundle + binary polish
  ↓
v0.10.0 (4-5 days) — date range + light theme + keyboard shortcuts
  ↓
v0.10.1 (3-4 days) — CSV/JSON + provider compare
  ↓
v0.11.0 (5-7 days) — custom alert rules
```

Total: 3-5 weeks of active development if no major rework.

## Out of scope for THIS sprint (but worth tracking)

- Mac team's Multi-CLI v1.14+ design (we wait for them)
- Microsoft Store / Flathub / AUR distribution (account-level action; needs Jason explicit approval)
- Scriptable CLI mode for power users (`cli-pulse-desktop --report --json`) — could fit in v0.11.x
- Anonymous usage analytics opt-in (privacy review needed)
- Per-project breakdown enhancements (already partially exists)

## Memory updates after sprint

After each ship:
- Append entry to `reference_desktop_repo.md` sprint history
- Update `MEMORY.md` if new patterns emerge worth caching
- Cross-team alignment memory only if Mac team needs to know

After sprint completes (post-v0.11.0):
- Single retrospective entry in `feedback_v080_crash_on_launch_incident.md` documenting whether the v0.9.x rules prevented a recurrence
- Distribution decision (continue to wait, or kick off Microsoft Store / Flathub work)

— end of plan —
