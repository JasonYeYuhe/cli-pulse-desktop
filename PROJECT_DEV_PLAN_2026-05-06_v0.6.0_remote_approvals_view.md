# Dev Plan — v0.6.0 Remote Approvals (view + decide on Windows desktop)

**Date:** 2026-05-06
**Type:** new feature, minor version bump (v0.5.7 → v0.6.0)
**Reviewers:** Gemini 3.1 Pro (plan + diff)
**Trigger:** user directive 2026-05-06 — replicate the macOS "session control" feature on Windows. macOS team has Phase 1 (Remote Approvals) + Phase 2 iter1 (Sessions Input) shipped, with `helper/transports/conpty.py` explicitly stubbed to wait for the cli-pulse-desktop track. Backend is fully live (5 tables + 6 app RPCs). User said: 你全权负责 设计开发.

## Scope decision: thin slice first

The macOS Remote Sessions feature is genuinely large — 4 phases, ~3-4k LOC of Python helper + Swift UI + ConPTY infrastructure. Dropping the whole port into one commit would multi-session; iterating in slices keeps each ship reviewable + revertible.

**Slice ordering, smallest-first:**

| Slice | Capability | Wires up | Estimate |
|---|---|---|---|
| **1 (this plan, v0.6.0)** | Decide approvals + view managed sessions on Windows | App-side RPCs only | ~700 LOC + 200 tests |
| 2 (v0.6.1+) | Send prompts / stop / interrupt managed sessions | `remote_app_send_command` | ~300 LOC |
| 3 (v0.7.0) | Hook integration: Windows Claude Code can EMIT pending approvals | New `bin/remote_hook.rs` + `install-claude-hook` wizard | ~1200 LOC |
| 4 (v0.8.0) | ConPTY managed-session HOST: Windows app can spawn + drive Claude sessions locally per remote command | Rust `portable-pty` + agent manager | ~1500 LOC |

**Why this ordering:**
1. Slice 1 has standalone value the day it ships — user at Windows can approve/deny requests originating from their Mac/iOS. Closes one of the highest-friction loops in the existing macOS feature.
2. Slice 1 uses ONLY the read-side app RPCs (already authenticated via existing `with_user_jwt` plumbing) and the `user_settings.remote_control_enabled` PATCH. No new auth shapes, no new dependencies.
3. Slices 2-4 each add a clearly-scoped new capability without re-architecting Slice 1. The wire shapes Slice 1 introduces (`RemotePermissionRequest`, `RemoteSession`) are stable and reused.

## What v0.6.0 ships

### Backend additions (Rust, src-tauri)

#### `supabase.rs` — 5 new wrappers around existing live RPCs

```rust
pub struct RemotePermissionRequest {
    pub id: String,
    pub session_id: Option<String>,
    pub device_id: String,
    pub device_name: Option<String>,
    pub provider: String,
    pub tool_name: String,
    pub summary: String,
    pub risk: String,        // "low" / "medium" / "high"
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
}

pub struct RemoteSession {
    pub id: String,
    pub device_id: String,
    pub device_name: Option<String>,
    pub provider: String,
    pub cwd_basename: String,
    pub cwd_hmac: Option<String>,
    pub status: String,        // "pending" / "running" / "stopped" / "errored"
    pub client_label: Option<String>,
    pub created_at: String,
    pub last_event_at: Option<String>,
}

pub async fn remote_list_pending_approvals(user_jwt: &str) -> SupabaseResult<Vec<RemotePermissionRequest>>;
pub async fn remote_decide_permission(
    request_id: &str,
    decision: &str,         // "approve" | "deny"
    scope: &str,            // "once" | "alwaysSession"
    decided_by_device_id: Option<&str>,
    user_jwt: &str,
) -> SupabaseResult<()>;
pub async fn remote_list_sessions(user_jwt: &str) -> SupabaseResult<Vec<RemoteSession>>;
pub async fn get_remote_control_setting(user_id: &str, user_jwt: &str) -> SupabaseResult<bool>;  // PostgREST GET against user_settings
pub async fn set_remote_control_setting(user_id: &str, enabled: bool, user_jwt: &str) -> SupabaseResult<()>;  // PATCH
```

Wire shapes mirror Swift `Models.swift:867-1005` exactly (snake_case JSON, optional fields where Mac uses Optional<>). `decided_by_device_id` is the desktop's own `device_id` from `HelperConfig` — lets the server-side audit log identify which device made the call.

`get_remote_control_setting` does PostgREST GET against `user_settings?user_id=eq.<uid>&select=remote_control_enabled` (mirrors Swift `APIClient.swift:712-731` shape). On 404 / empty array → return `false` (default OFF, matches Mac).

`set_remote_control_setting` does PostgREST PATCH against `user_settings?user_id=eq.<uid>` with body `{"remote_control_enabled": <bool>}`. The server-side `_remote_authenticate_helper_gated` re-checks this on every helper RPC, so flipping off severs the channel immediately.

#### `lib.rs` — 5 new Tauri commands (all wrap above with `with_user_jwt`)

```rust
#[tauri::command] async fn get_remote_pending_approvals() -> Result<Vec<RemotePermissionRequest>, String>;
#[tauri::command] async fn decide_remote_approval(request_id: String, decision: String, scope: String) -> Result<(), String>;
#[tauri::command] async fn list_remote_sessions() -> Result<Vec<RemoteSession>, String>;
#[tauri::command] async fn get_remote_control_setting() -> Result<bool, String>;
#[tauri::command] async fn set_remote_control_setting(enabled: bool) -> Result<(), String>;
```

All 5 unpaired-state-safe: return `Ok(empty)` / `Ok(false)` when `config::load()` returns `None`. Same pattern as `get_top_projects` / `get_server_alerts`.

Decide passes `decided_by_device_id = Some(cfg.device_id)` so the server's audit row attributes it correctly.

### Frontend additions (React, src/App.tsx)

#### Header: pending-approvals badge

Right of the existing "Update available" banner / before the `PairBadge`:

```tsx
{remoteControlEnabled && pendingCount > 0 && (
  <button onClick={() => setShowApprovalsSheet(true)} className="...amber...">
    🔔 {pendingCount} pending
  </button>
)}
```

State at App-level. Polls `get_remote_pending_approvals` every 30s when `remoteControlEnabled === true`.

#### `RemoteApprovalsSheet` component

Modal-style sheet (mounts as overlay when `showApprovalsSheet === true`). Three states:
- **Disabled** (toggle off): hint card "Remote Control is off. Enable in Settings → Privacy."
- **Empty** (toggle on, no pending): "No approvals waiting."
- **List**: scrollable list of `RemotePermissionRequest` rows.

Each row:
- Header: `device_name` (or "Unknown device") + "·" + `provider` (Claude / Codex / shell)
- Body: `tool_name` (mono) + `summary` (wrapped)
- Risk pill: green (low) / amber (medium) / red (high)
- Age: "{N}m ago" computed from `created_at` (uses existing `time.unit_*` i18n keys)
- Buttons:
  - **Approve** — emerald; **disabled when `risk === "high"`** (matches Mac Phase 1)
  - **Deny** — neutral
  - Both call `decide_remote_approval(id, "approve"|"deny", "once")`. Optimistic remove from list.

Tooltip on disabled high-risk Approve: localized "High-risk requests must be approved on the originating device."

#### Settings → Privacy section

New section above the existing Danger Zone (visually less-scary than danger but more-cautious than Integrations). Layout:

```
Privacy & Remote Control
  Allow this account to participate in cross-device approvals.
  When enabled, your other devices' Claude / Codex hooks can ask you
  to approve permission requests from this Windows app.
  
  [Toggle]  Remote Control     OFF / ON
            (consent dialog on first ON)
  
  Status:   Server reports remote_control_enabled = <true|false>.
            Refreshed [N]s ago.
```

Toggle-on-first-time fires a **consent dialog** (matches Mac `AdvancedSection`):
- Title: "Enable Remote Control?"
- Body: privacy posture summary (default OFF, gated server-side, no transcripts uploaded, high-risk fail-closed). Three bullet points.
- Buttons: **Cancel** | **Enable**

### Sessions tab — managed sessions read-only section

Above the existing Activity Timeline + process snapshot:

```
Active managed sessions  (cross-device)
┌──────────────────────────────────────────┐
│ Mac (Jason's MBP) · Claude · my-project  │
│ Running for 5m · last event 2s ago      │
└──────────────────────────────────────────┘
```

Each row from `list_remote_sessions()`. No buttons in v0.6.0 — just visibility. Send/stop come in v0.6.1.

Empty state: "No managed sessions running." Hidden entirely when `remoteControlEnabled === false`.

### i18n (en/zh-CN/ja, ~25 new keys)

```
remote.title: "Remote Approvals"
remote.heading_pending: "Pending approvals"
remote.empty_pending: "No approvals waiting."
remote.disabled_hint: "Remote Control is off. Enable in Settings → Privacy."

remote.row_unknown_device: "Unknown device"
remote.risk_low: "low risk"
remote.risk_medium: "medium risk"
remote.risk_high: "high risk"
remote.high_risk_blocked_tooltip: "High-risk requests must be approved on the originating device."
remote.approve_button: "Approve"
remote.deny_button: "Deny"
remote.approve_processing: "Approving…"
remote.deny_processing: "Denying…"
remote.action_failed: "Action failed: {{err}}"
remote.age_ago: "{{age}} ago"

remote.badge_pending_count_one: "{{count, number}} pending"
remote.badge_pending_count_other: "{{count, number}} pending"

remote.sessions_heading: "Active managed sessions"
remote.sessions_empty: "No managed sessions running."

settings.privacy_heading: "Privacy & Remote Control"
settings.privacy_body: "Allow this account to participate in cross-device approvals. When enabled, your other devices' Claude / Codex hooks can ask you to approve permission requests from this Windows app."
settings.privacy_toggle_label: "Remote Control"
settings.privacy_status_on: "Server reports remote_control_enabled = true."
settings.privacy_status_off: "Server reports remote_control_enabled = false."
settings.privacy_status_refreshed: "Refreshed {{age}} ago."
settings.privacy_consent_title: "Enable Remote Control?"
settings.privacy_consent_body_b1: "Default OFF. Server-side gate enforced on every helper RPC."
settings.privacy_consent_body_b2: "Provider keys, tokens, transcripts, full project paths are never uploaded."
settings.privacy_consent_body_b3: "High-risk requests (rm -rf, sudo, curl, ssh, scp, chmod 777, etc.) cannot be approved remotely — they always fall through to the originating device's local prompt."
settings.privacy_consent_enable_button: "Enable Remote Control"
```

i18n.test.ts: pin every new key in the critical-labels list.

### Tests

Backend (~10 new tests):
- 5 supabase response-shape pins (one per RPC wrapper) — round-trip JSON via `serde_json::from_value` to ensure schema drift is caught
- 1 `decide_remote_approval` decision-string validation (only "approve" / "deny" accepted)
- 2 risk-classification helpers (low/medium/high → CSS variant)
- 1 unpaired-state path returns Ok(empty)
- 1 `get_remote_control_setting` 404 returns false (no row exists yet for new accounts)

Frontend (~6 new tests):
- 3 i18n pins (one per language: critical-labels list grows)
- 1 high-risk Approve button is disabled
- 1 optimistic remove on Approve (badge count drops immediately)
- 1 toggle-on-first-time fires consent dialog

## Reviewer questions (for Gemini 3.1 Pro)

1. **Slicing decision:** should v0.6.0 ship just APP-side view+decide, or should I attempt to also include the Windows-side hook emission (Slice 3 from the table above) so users on Windows can ALSO trigger approvals? Slice 3 is ~1200 LOC additional + a Windows ConPTY-adjacent shell-out story; my read is "no, keep v0.6.0 thin". Confirm or push back.

2. **Sessions tab read-only:** v0.6.0 shows running managed sessions but no Send / Stop buttons. Is that a coherent UX — does the visible-but-not-actionable state confuse users? Or should I either omit the section entirely OR include the buttons?

3. **Consent dialog vs simple toggle:** Mac uses a full consent dialog on first-enable; the toggle is a 3-state machine (off / saving / on). Is there value in matching Mac exactly, or is a simpler "switch + below-the-fold disclosure" enough for the Windows audience that's a power-user subset?

4. **Risk-pill color choice:** green for low risk could read as "approved" rather than "low risk". Is amber/grey for low-risk safer? Mac uses what color scheme?

5. **`set_remote_control_setting` write race:** two devices simultaneously toggling — last-write-wins on PATCH is fine for boolean state. But the badge count updates locally pre-patch (optimistic) — should the optimistic update revert on PATCH error, or just let the next 30s poll re-sync?

6. **`decide_remote_approval` race:** another device approves the same request between our list_pending and our user clicking Approve. The RPC will return RPC error (request already decided). Do we surface this as a toast ("Already decided on another device — refreshing list") or a silent reload?

## Files this plan would touch

| File | Change |
|---|---|
| `src-tauri/src/supabase.rs` | +5 wrappers + 2 structs (~150 LOC) |
| `src-tauri/src/lib.rs` | +5 Tauri commands + handler registration (~80 LOC) |
| `src/App.tsx` | RemoteApprovalsSheet + header badge + Settings privacy section + sessions read-only section (~400 LOC) |
| `src/locales/{en,zh-CN,ja}.json` | ~25 keys × 3 = 75 entries |
| `src/i18n.test.ts` | critical-labels list grows by 25 keys |
| 3 manifests + 2 lock files | version bump 0.5.7 → 0.6.0 |
| `CHANGELOG.md` | 0.6.0 entry citing macOS-team handoff context |
| `reference_desktop_repo.md` | append v0.6.0 sprint entry + Slices 2-4 roadmap |

## Risks

- **Multi-device wire-format drift.** Mac may evolve `RemotePermissionRequest` shape (e.g. `device_name` was added in v0.32). Defensive: every Optional<String> on Mac → `Option<String>` with `#[serde(default)]` on Rust side. Skipped fields don't break decode.
- **Polling cost.** 30s polling at every paired desktop adds ~10 RPS at scale. Mitigation: use 60s when no pending was seen on the last 3 polls (exponential back-off), 30s when there are pending. Defer if simpler is fine for v0.6.0.
- **Consent-dialog L10n drift.** The privacy-posture body has 3 bullets that mention specific Phase 1 invariants. Those need to stay accurate as backend evolves. Mitigation: keep the keys generic ("server-side gate enforced", not "v0.27 gate enforced") so they don't need updates per backend version bump.

## What v0.6.0 explicitly does NOT do

- Hook emission from Windows (Slice 3, future v0.7.0)
- Send / Stop / Interrupt managed sessions (Slice 2, future v0.6.1)
- ConPTY local-spawn host (Slice 4, future v0.8.0)
- APNs push notification (Mac iter v0.32, requires Apple Developer setup — not relevant to Windows-only)
- Codex / shell hook adapters (Mac stubs them too in Phase 1)

— end of plan —
