# v0.3.0 — Direct Email Sign-In for Tauri Desktop

**Status:** spec — pending sign-off.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-02), with input from Codex GPT-5.4 + Gemini 3.1 Pro.
**Tracks:** [PROJECT_FIX backlog item — pairing UX gap surfaced 2026-05-01 during v0.2.11 VM verification].

## 1. Problem

Pure Windows / Linux users cannot onboard. The 6-digit pairing code that the
Tauri desktop helper consumes is generated **only** by the macOS menu bar app
(`generatePairingCode()` is called only from `PairingSection.swift:46` —
verified by repo-wide grep). The iOS app has no "Add Device" UI despite the
desktop telling users to use it. The desktop's locale strings literally point
users to a non-existent iOS surface.

v0.2.13 patches the misleading copy. v0.3.0 fixes the actual architectural gap.

## 2. Goal

Same email/account login syncs across all platforms. macOS / Windows / Linux
collect; iOS / Android view. Standard Notion / Linear / 1Password model.

After v0.3.0:

```
                 Supabase auth.users
                        │
   ┌─────────┬──────────┼──────────┬─────────┐
 macOS     Windows    Linux       iOS     Android
 (sign in once on each device with the same email)
```

The helper still runs as a daemon with a long-lived `device_id + helper_secret`
(same as today). What changes is **how that helper_secret is provisioned**:
instead of consuming a Mac-issued 6-digit code, the user signs in once with
their email, and the desktop mints its own helper credential against the
user's session.

## 3. Decision (with rationale)

**Primary auth method: Supabase email OTP (`signInWithOtp` + email-code verify).**

Three independent reviews converged here after divergence:

| Reviewer | Initial leaning | Final |
|---|---|---|
| Claude Opus 4.7 (this) | A2 (loopback OAuth + PKCE) | OTP — Gemini's case is stronger |
| Codex GPT-5.4 | A2 (loopback OAuth + PKCE), OTP as fallback | OTP first acceptable; OAuth later |
| Gemini 3.1 Pro | OTP — primary | OTP — primary |

**Why OTP over loopback OAuth (A2) for v0.3.0:**

1. **Windows Defender firewall prompts** — first-launch `127.0.0.1:N` bind on
   Windows commonly triggers "Allow this app through firewall?" UAC prompts
   that scare non-technical users into denying. Slack, Discord, and others
   have hit this in the wild.
2. **Supabase `redirect_uri` allowlist** — GoTrue requires exact-match
   `redirect_uri` registration, no port wildcards (claim from Gemini;
   directionally consistent with GoTrue's docs around the `additionalRedirectUrls`
   config key). A2 would force us to either pin a single port (collision risk)
   or pre-register a fixed fallback set.
3. **Headless Linux** — no browser available, OAuth dies; OTP works via email
   only. Server / VPS users are a real audience for a CLI-tracking helper.
4. **Code volume** — OTP path is ~3 REST calls in Rust. Loopback OAuth needs
   `tiny_http`, PKCE state machine, callback handler, port discovery, and OAuth
   provider selection UI. ~5x more code, ~5x more failure modes.
5. **OAuth providers (Apple / Google / GitHub) can be added in v0.3.1** as a
   power-user upgrade once the OTP foundation is in. Not blocking.

**Token refresh strategy: pre-emptive near-expiry refresh + 401 retry once.**
Codex's call (Gemini's pure 401-only would cause every helper_sync to incur
one failed RPC before the refresh kicks in). On refresh-token 401 (i.e., the
refresh token itself died), clear keychain + transition UI to "Session
Expired — sign in again" (Gemini's UX call, kept).

**Server-side: add new RPC `register_desktop_helper()`. Don't modify
`helper_sync`.** Codex's call. Strictly additive, zero risk to existing
Mac one-click flow. helper_sync continues using `device_id +
helper_secret` indefinitely.

**Keep `pair_helper` / device_id pairing path.** Both reviewers agreed.
Mac users have a working paid product; breaking changes are bad. Demote to
"Advanced" disclosure under Settings, but never remove.

## 4. Architecture

### Auth flow comparison

**Today (paired helper only):**
```
User opens Mac app → generatePairingCode (auth.uid()) → display 6-digit code
                  ↓
          (user manually copies)
                  ↓
Tauri desktop: pair_device(code) → register_helper RPC (anon-key + code)
            → returns device_id + helper_secret → save to HelperConfig
            → all subsequent helper_sync uses device_id (anon-key)
```

**v0.3.0 (new path, parallel to old):**
```
User opens Tauri desktop → enters email → /auth/v1/otp (email, create_user: true)
                          → email arrives with 6-digit code
                          → user enters code → /auth/v1/verify (email + token)
                          → returns access_token + refresh_token
                          → register_desktop_helper RPC (with user JWT)
                          → returns device_id + helper_secret
                          → save HelperConfig (helper_secret) + Keychain (refresh_token)
                          → discard access_token (helper_sync uses helper_secret)
                          → all subsequent helper_sync uses device_id (unchanged)

[Refresh token used only when re-running register_desktop_helper or for
account-management RPCs we may add later. Sign-out is local-only —
clears keychain + HelperConfig regardless of refresh-token state.]
```

**Account creation is allowed on desktop.** OTP request uses
`create_user: true` (Supabase default). A pure Windows user with no
prior Mac/iOS install can sign up directly from desktop — that's the
core onboarding gap this spec closes. If the email is already
registered (from Mac/iOS), the same flow signs the user in to that
account. No "must sign up elsewhere first" friction.

**Old path stays:**
```
Tauri desktop Settings → Advanced → "Pair from Mac" → existing pair_device flow
```

### What gets persisted where

| Data | Storage | Lifetime |
|---|---|---|
| `device_id` | `HelperConfig` JSON, mode 0600 (existing) | until unpair |
| `helper_secret` | `HelperConfig` JSON (existing) | until unpair |
| `access_token` | **not persisted** (in-memory during sign-in only) | seconds |
| `refresh_token` | OS keychain via `keyring` crate | until expiry / sign-out |
| `user_email` | `HelperConfig` JSON (display only) | until unpair |
| Refresh expiry timestamp | `HelperConfig` JSON | until expiry |

Rationale: the helper itself doesn't need user-JWT capability for daily
operation — `helper_sync` is device-scoped via `helper_secret`. We only
need the refresh token if the user later wants to (a) re-mint a helper
secret, (b) call user-scoped RPCs from desktop (e.g. delete account,
manage devices). Keeping the access token out of disk minimizes blast
radius if `HelperConfig` ever leaks.

## 5. Server-side changes

### 5.1 New RPC: `register_desktop_helper`

**Schema cross-check (verified 2026-05-02 against `schema.sql`):**
- Table `public.devices` columns: `id`, `user_id`, `name`, `type` (NOT
  `device_type`), `system`, `helper_version`, `status`, `cpu_usage`,
  `memory_usage`, `helper_secret` (NOT `helper_secret_hash` — this
  column stores the SHA-256 hash but the column name itself is
  `helper_secret`), `push_token`, `push_platform`, `last_seen_at`,
  `created_at`.
- Existing `register_helper` at `helper_rpc.sql:96` writes:
  `'helper_' || encode(gen_random_bytes(32), 'hex')` as the plaintext
  secret returned to the client, and stores
  `encode(digest(v_helper_secret, 'sha256'), 'hex')` in the
  `helper_secret` column.
- Existing `register_helper` ends with `language plpgsql security
  definer set search_path = pg_catalog, public, extensions;` —
  Codex review flagged that omitting `set search_path` opens a
  privilege-escalation footgun (unqualified calls to `digest` /
  `gen_random_bytes` / `auth.uid` could be hijacked by a search-path
  shadowing attack). New RPC matches this hardening.

`backend/supabase/helper_rpc.sql` — append:

```sql
-- Defensive: pgcrypto is already enabled in schema.sql:7 but list it
-- here so this migration is self-contained against fresh Supabase
-- projects.
create extension if not exists pgcrypto;

-- Desktop direct sign-in path (v0.3.0). Mirror of register_helper but
-- skips the pairing-code dance: trusts auth.uid() from the user JWT.
-- Returns the same shape as register_helper so the client code path
-- past this RPC is identical.
--
-- Per-user device cap (20) enforced here. Gemini 3.1 Pro review flagged
-- this as a JWT-replay DoS vector; legacy register_helper currently
-- lacks a cap — backfilling tracked separately. Codex review flagged
-- count-then-insert as raceable; pg_advisory_xact_lock serializes
-- concurrent calls per-user without serializing across users.
create or replace function public.register_desktop_helper(
  p_device_name text,
  p_device_type text default 'desktop',
  p_system text default '',
  p_helper_version text default ''
) returns jsonb language plpgsql security definer
  set search_path = pg_catalog, public, extensions
as $$
declare
  v_user_id uuid := auth.uid();
  v_device_id uuid;
  v_helper_secret text;
  v_existing_count integer;
begin
  if v_user_id is null then
    raise exception 'Not authenticated' using errcode = '42501';
  end if;

  -- Per-user transaction-scoped advisory lock. Concurrent calls for
  -- the same user_id serialize through this lock; different users do
  -- not contend. Prevents the count-then-insert race that would
  -- otherwise let two parallel calls each see 19 devices and both
  -- insert (final count 21). Using hashtext (int4) → implicit cast to
  -- bigint for the single-arg pg_advisory_xact_lock signature; same
  -- pattern as standard Postgres advisory-lock examples.
  perform pg_advisory_xact_lock(hashtext(v_user_id::text)::bigint);

  -- Cap per-user devices at 20. Generous enough for power users
  -- (multiple Macs + Linux servers + Windows boxes) but blocks
  -- abuse / runaway scripts.
  select count(*) into v_existing_count
    from public.devices
    where user_id = v_user_id;
  if v_existing_count >= 20 then
    raise exception 'Device limit reached (20). Remove an existing device first.'
      using errcode = '53000'; -- insufficient_resources
  end if;

  -- Plaintext secret returned to client; column stores SHA-256 hash.
  -- Matches register_helper convention exactly (helper_rpc.sql:93).
  v_helper_secret := 'helper_' || encode(gen_random_bytes(32), 'hex');

  insert into public.devices (
    user_id, name, type, system, helper_version,
    status, helper_secret
  )
  values (
    v_user_id,
    left(p_device_name, 255),
    left(p_device_type, 50),
    left(p_system, 255),
    left(p_helper_version, 20),
    'Online',
    encode(digest(v_helper_secret, 'sha256'), 'hex')
  )
  returning id into v_device_id;

  update public.profiles set paired = true where id = v_user_id;

  return jsonb_build_object(
    'device_id', v_device_id,
    'user_id', v_user_id,
    'helper_secret', v_helper_secret
  );
end;
$$;

-- Authenticated callers only (anon key gets blocked by auth.uid() check).
grant execute on function public.register_desktop_helper(text, text, text, text)
  to authenticated;
revoke execute on function public.register_desktop_helper(text, text, text, text)
  from anon;
```

### 5.2 New RPC: `device_status`

Used by the helper_sync error classifier (Section 6.4) to distinguish
"device or account is gone" from "transient auth blip" after a 401
on helper_sync. Anon-callable (helper has device credentials, not a
user JWT) but verifies the supplied helper_secret hash matches the
stored hash — so this is not a device-id enumeration oracle.

`backend/supabase/helper_rpc.sql` — append:

```sql
create or replace function public.device_status(
  p_device_id uuid,
  p_helper_secret text
) returns jsonb language plpgsql security definer
  set search_path = pg_catalog, public, extensions
as $$
declare
  v_user_id uuid;
  v_stored_hash text;
  v_provided_hash text;
  v_account_active boolean;
begin
  v_provided_hash := encode(digest(p_helper_secret, 'sha256'), 'hex');

  select user_id, helper_secret into v_user_id, v_stored_hash
    from public.devices
    where id = p_device_id;

  -- Device row missing entirely (manual delete, account-cascade, etc.)
  if v_user_id is null then
    return jsonb_build_object('status', 'device_missing');
  end if;

  -- Hash mismatch — secret was rotated server-side, or wrong device_id
  -- supplied. Treat as "device_missing" to the caller (don't leak
  -- existence vs auth-mismatch).
  if v_stored_hash is distinct from v_provided_hash then
    return jsonb_build_object('status', 'device_missing');
  end if;

  -- Account presence check. If profiles row exists, account is live.
  -- (The devices.user_id FK has on delete cascade, so a deleted account
  -- already drops the device row. This branch is defensive.)
  select true into v_account_active
    from public.profiles
    where id = v_user_id;
  if v_account_active is null then
    return jsonb_build_object('status', 'account_missing');
  end if;

  return jsonb_build_object('status', 'healthy');
end;
$$;

grant execute on function public.device_status(uuid, text) to anon, authenticated;
```

**Privacy note**: Returns `device_missing` for both genuinely-missing
devices and hash-mismatches, so this RPC cannot be used to enumerate
which `device_id` UUIDs exist on the server. Callers without a valid
helper_secret get the same response as if the device never existed.

### 5.3 Tests

For `register_desktop_helper`:
- Migration test: anon-key call → 42501.
- Migration test: authenticated call → returns shape `{device_id, user_id, helper_secret}`.
- Integration test against existing `helper_sync` using the new device_id
  + helper_secret → must succeed without code change to helper_sync.
- **Race-condition test**: spawn 5 concurrent calls for the same user
  starting at device count 19; assert exactly one succeeds and four
  return error 53000.
- **Schema-name test**: assert the inserted row has the columns
  `type` and `helper_secret` populated (catches column-name drift
  before deploy).
- **search_path test**: shadow `public.digest` with a malicious
  function in a test schema, run the RPC, assert the original
  pg_catalog.digest is used (validates the search_path lock).

For `device_status`:
- Returns `healthy` for valid (device_id, helper_secret) pair.
- Returns `device_missing` for unknown device_id.
- Returns `device_missing` (not "auth_failed") for valid device_id +
  wrong helper_secret — privacy invariant.
- Returns `account_missing` after profile row deleted (synthetic test).

### 5.4 Migration safety
- Both new RPCs (`register_desktop_helper`, `device_status`) are strictly
  additive. Zero impact on existing `register_helper` callers.
- `devices` table schema unchanged.
- RLS policies unchanged.
- Rollback: drop both functions; existing `register_helper` /
  `helper_sync` flow is untouched.

## 6. Desktop changes

### 6.1 New files

#### `src-tauri/src/auth.rs` (new, ~180 LOC est.)
```rust
//! Supabase email OTP sign-in for the Tauri desktop.
//!
//! Three REST calls:
//!   POST /auth/v1/otp     {email, create_user: false}     → 200 (silent send)
//!   POST /auth/v1/verify  {type: "email", email, token}    → {access_token, refresh_token, expires_in}
//!   POST /auth/v1/token?grant_type=refresh_token  {refresh_token} → new pair
//!
//! All calls go through the existing supabase.rs reqwest client; we just
//! add a JWT-aware variant that injects the access_token in Authorization
//! headers when present.

pub struct AuthSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub user_id: uuid::Uuid,
    pub email: String,
}

pub async fn send_otp(email: &str) -> Result<()>
pub async fn verify_otp(email: &str, token: &str) -> Result<AuthSession>
pub async fn refresh(refresh_token: &str) -> Result<AuthSession>
```

#### `src-tauri/src/keychain.rs` (new, ~80 LOC est.)
```rust
//! OS keychain wrapper using the `keyring` crate.
//!
//! Service: "dev.clipulse.desktop"
//! Account: "supabase-refresh-token"
//!
//! Backends:
//!   - macOS:   Keychain via `keyring`'s `apple-native` feature
//!   - Windows: Credential Manager via `windows-native`
//!   - Linux:   Secret Service (libsecret) via `sync-secret-service`
//!
//! **Fail-closed on Linux without Secret Service.** Codex review
//! correctly flagged that machine-id-derived "encryption" is not
//! security — `/etc/machine-id`, `MachineGuid`, and `IOPlatformUUID`
//! are identifiers, not secrets. An attacker who reads the encrypted
//! file can usually derive the key. Refresh tokens grant ~1 week of
//! account access; storing them with weak crypto is a worse outcome
//! than asking the user to install libsecret.
//!
//! On Linux without Secret Service:
//!   - Sign-in returns a clear error: "Secret Service required.
//!     Install gnome-keyring or kwalletd, or use the Mac pairing path."
//!   - .deb / .rpm metadata declares libsecret-1-0 as a runtime
//!     dependency so default installs already have it; the failure
//!     only fires on minimal headless Linux without a desktop
//!     environment.
//!   - "Mac pairing fallback" is the existing pair_device flow,
//!     which doesn't need keychain.

pub fn store_refresh_token(token: &str) -> Result<(), KeychainError>
pub fn read_refresh_token() -> Result<Option<String>, KeychainError>
pub fn delete_refresh_token() -> Result<(), KeychainError>

pub enum KeychainError {
    NotAvailable, // libsecret missing on Linux — UI shows install hint
    Io(std::io::Error),
    Other(String),
}
```

### 6.2 Modified files

#### `src-tauri/src/lib.rs` — add 6 commands
```rust
#[tauri::command]
async fn auth_send_otp(email: String) -> Result<(), String>
//   POST /auth/v1/otp {email, create_user: true}
//   Surfaces rate-limit errors specifically so UI can display
//   "Too many tries — wait 60s".

#[tauri::command]
async fn auth_verify_otp(
    email: String,
    code: String,
    device_name: String,
    app: tauri::AppHandle,
) -> Result<PairResult, String>
//   1. POST /verify → tokens
//   2. POST register_desktop_helper (with access_token) → device_id + helper_secret
//   3. HelperConfig::save (device_id + helper_secret + email)
//   4. keychain::store_refresh_token(refresh_token)
//      — on KeychainError::NotAvailable, abort with explicit error;
//        UI shows libsecret-install hint
//   5. notify::pair_success
//   6. return PairResult — same shape as existing pair_device

#[tauri::command]
fn auth_status() -> Result<AuthStatus, String>
//   Reads HelperConfig + keychain.read_refresh_token() PRESENCE
//   (does not validate it). Returns {paired, email, has_refresh_token}.
//   Liveness of the refresh_token is checked only when the user
//   triggers a user-scoped action.

#[tauri::command]
fn auth_sign_out() -> Result<(), String>
//   LOCAL-ONLY. Always succeeds regardless of refresh-token state.
//     1. keychain::delete_refresh_token() — best-effort
//     2. HelperConfig::clear() — clears device_id + helper_secret
//     3. (Optional best-effort) POST /auth/v1/logout with whatever
//        refresh_token exists; ignore errors — sign-out completes
//        either way.
//   Codex review: "Sign out must not require a live refresh token.
//   The plan currently routes sign-out through user-JWT refresh, so
//   an expired session turns a local privacy action into an
//   error/banner path." Fixed.

#[tauri::command]
async fn auth_account_check() -> Result<AccountCheckResult, String>
//   New for v0.3.0. Called by the helper_sync error-classifier when
//   it sees a recoverable-vs-fatal device error. Reads device_id +
//   helper_secret from HelperConfig and calls the new server-side
//   `device_status` RPC (Section 5.2). RPC returns `healthy`,
//   `device_missing`, or `account_missing`. Caller clears local
//   HelperConfig + keychain on the latter two, shows banner, stops
//   the sync loop until user signs in again.

#[tauri::command]
fn auth_default_device_name() -> String
//   Synchronous, no errors. Returns whoami::devicename() (e.g.
//   "Jason's Surface" on a properly-named Win machine), falling back
//   to whoami::hostname() if devicename is empty. Frontend uses this
//   to pre-fill the "Device name" input on the verify screen. Trust
//   boundary: this string is shown to the user before being sent to
//   register_desktop_helper, so the user can override anything weird.
```

The existing `pair_device` command is unchanged. UI uses it only when the
user explicitly chooses the advanced "I have a Mac" path.

#### `src-tauri/src/supabase.rs`
Add a `with_user_jwt(&self, access_token: &str)` builder variant that
injects `Authorization: Bearer <access_token>`. Used only by
`register_desktop_helper`. Existing `helper_sync` path with device_secret
is unchanged.

#### `src/App.tsx`
Replace the existing pairing-code input with a 2-step OTP flow:

```
[State 1: enter email]
  Sign in to CLI Pulse
  [____________________]  Email
  [Send code]
  Already have a Mac? Pair from menu bar instead → (toggles old UI)

[State 2: enter code, after Send]
  We sent a code to alice@example.com.
  [______]  6-digit code
  Device name (optional): [_________________________]
    (placeholder = pre-filled default from Rust whoami fallback)
  [Verify]   [Resend]   [Back]

[State 3: signed in]
  ✓ Signed in as alice@example.com
  Device: Jason's Surface
  [Sign out]
```

**Device name source** (Codex review flag): the verify step
includes a "Device name (optional)" input pre-populated with a
sensible default. The default comes from a new Tauri command
`auth_default_device_name()` which calls `whoami::devicename()` (or
falls back to `whoami::hostname()`) on the Rust side. Frontend
displays the resolved value as the placeholder; user can edit. On
verify, frontend sends whatever's in the field to `auth_verify_otp`.
If the user blanks it, Rust uses the same default again before
calling `register_desktop_helper`. We never trust the field to be
non-empty as a precondition — defense against accidentally creating
unnamed device rows.

#### `src/locales/{en,zh-CN,ja}.json`
Add new auth keys (~10 strings each):
```
auth.signin.heading
auth.signin.email_label
auth.signin.send_code
auth.signin.code_label
auth.signin.verify
auth.signin.resend
auth.signin.signed_in_as
auth.signin.sign_out
auth.signin.advanced_pair_from_mac
auth.error.invalid_code
auth.error.rate_limited
auth.error.user_not_found
```

The existing `pair_*` keys keep their meaning for the now-advanced path,
but `pair_hint` is rewritten again to remove the v0.3.0 forward-reference
language (since we now ARE v0.3.0).

### 6.3 Token refresh — **lazy / on-demand only**

**Key insight: helper_sync does NOT need access_token.** It uses
`device_id + helper_secret` (the same path the existing v0.2.x helper
uses today). So the helper background process can ignore the user
session entirely after initial sign-in. There is **no background
refresh loop**.

Original draft had "every 2 min, if expiry soon, refresh". Gemini 3.1
Pro review correctly flagged this as a scaling anti-pattern: it would
wake every desktop daemon 24/7 to refresh user JWTs that 99% of users
never actively use, hammering Supabase Auth in aggregate. Killed it.

Refresh strategy:

```
On user-initiated UI action that needs user JWT
(e.g. "View account", future device-management screen, NOT sign-out):
  1. Read refresh_token from keychain
  2. POST /auth/v1/token?grant_type=refresh_token
  3. On success: use new access_token in-memory for the action only.
     Persist rotated refresh_token to keychain. Do NOT cache the
     access_token across actions.
  4. On 401: refresh token is dead.
       - Clear keychain
       - HelperConfig keeps device_id + helper_secret (data sync unaffected)
       - Show banner: "Your sign-in expired — sign in again to manage
         account settings. Data collection continues normally."
       - User can re-sign-in via the same email + OTP flow

Sign-out is local-only and always succeeds (Section 6.2 auth_sign_out).
Best-effort POST /auth/v1/logout afterward, but failure does not
block sign-out.

helper_sync background loop (unchanged from v0.2.x):
  every 2 min: helper_sync(device_id, helper_secret) → continues forever
  on error → classify (see Section 6.4 below) and decide whether to
             clear local pairing or just retry.
```

**What this buys us:**
- Zero idle Supabase Auth load. Desktops with the app installed but UI
  closed never call `/auth/v1/token`.
- Refresh-token TTL is irrelevant for data sync. A user could sign in
  once and never open the UI again for years; helper_sync keeps working.
- "Sign in expired" is a soft state — only blocks user-scoped actions,
  never blocks data collection. Reduces the urgency of the
  re-authentication prompt.

**What we lose:**
- A user who hasn't opened the UI in >1 week and then opens it WILL hit
  the refresh-token-dead path and need to re-sign-in. This is fine —
  it's exactly how 1Password / Linear / Notion behave.

### 6.4 helper_sync error classification (account/device deleted)

Codex review flagged a gap: the existing `helper_sync` loop runs
forever, but if the user deletes their account from another device, or
manually removes this device from a (future) device-management UI,
`helper_sync` will start failing indefinitely with no recovery path.

New error classifier in the helper_sync loop:

```
on helper_sync error → inspect HTTP status + Postgres error code:
  401 / "device not found" / "helper_secret mismatch":
    1. Pause helper_sync loop (do NOT clear HelperConfig yet —
       could be a transient auth blip)
    2. Call auth_account_check() RPC
    3. If response says "device gone" or "account gone":
         - Clear HelperConfig + keychain
         - Show banner: "This device was removed from your account.
           Sign in again to re-pair."
         - Stop the loop until user signs in again
    4. If "healthy", resume — the original 401 was a transient.

  500 / 502 / network error:
    Backoff (existing retry logic, no change).
```

`auth_account_check()` is the new lib.rs command (Section 6.2). It
calls a new `device_status()` RPC (server-side, also v0.3.0 work) that
returns `{exists: bool, account_active: bool}` looked up from the
helper's own device_id + helper_secret pair.

## 7. Risks & mitigations

| Risk | Mitigation |
|---|---|
| **Email deliverability** — Supabase OTP emails hit spam, enterprise quarantine, or get delayed | (a) Verify SPF/DKIM/DMARC on the sending domain via Supabase project settings before sprint start; (b) UI explicitly says "Check your spam folder"; (c) "Resend" + "Try a different email" both visible from the verify screen; (d) on repeat failures, surface a fallback link to the existing pair-from-Mac flow |
| Supabase OTP rate-limit hit by power users | (a) Confirm exact rate limit + bump if needed in Supabase dashboard before sprint start (closed in Section 11); (b) surface rate-limit error clearly; advise wait time; (c) "Resend" button rate-limited client-side first (30s lockout) |
| **Linux without Secret Service / libsecret** — fail-closed (Section 6.1) | (a) `.deb` / `.rpm` declare `libsecret-1-0` as runtime dep; (b) on missing-keychain error, UI surfaces install hint AND offers the Mac pairing-code path as workaround; (c) Linux auth fallback to encrypted file is **explicitly out of scope** (Codex flagged machine-id-derived crypto as not-real-security) |
| Email delivery latency | "Resend" button disabled for 30s, then re-enabled |
| OTP length / TTL changes server-side | Don't hardcode the "6" digits in copy strings; make it a config; verify endpoint accepts variable lengths |
| User signs in on multiple desktops | Each gets its own device_id; that's correct behavior. 20-device cap (Section 5.1) catches abuse. |
| `register_desktop_helper` deployed but old desktop still calls `pair_helper` | Both RPCs coexist; zero version coupling |
| **Sentry token leakage** — refresh_token / access_token / email / helper_secret captured by Sentry breadcrumbs or error context | Day-1 task: audit `src-tauri/src/sentry_init.rs` `before_send` scrubber; add explicit deny-list for these field names + URL query parameters. Closed in Section 11. |
| **JWT-replay DoS via `register_desktop_helper`** | 20-device cap per user_id + `pg_advisory_xact_lock` to make the cap race-safe (Section 5.1) |
| **`SECURITY DEFINER` privilege escalation** | `set search_path = pg_catalog, public, extensions` on the new RPC matches existing hardening on `register_helper` (Section 5.1, Codex flag) |
| **Schema column-name drift** between spec and `devices` table | Schema cross-check at top of Section 5.1 + dedicated test in Section 5.2 |
| **Account-deleted / device-removed** while helper is sync'ing | helper_sync error classifier (Section 6.4) detects fatal vs transient and clears local pairing on confirmed device-gone |
| **`pgcrypto` extension missing on fresh Supabase project** | Migration script declares `create extension if not exists pgcrypto` even though `schema.sql:7` already does — defense in depth |

## 8. Milestones

Total est: **5–7 working days** for v0.3.0 (revised up from initial 3-day
estimate after Codex flagged optimism bias on Linux keyring testing,
Windows credential store quirks, Supabase email deliverability
verification, migration rollback, cross-platform E2E).

| Day | Work |
|---|---|
| 1 (server) | `register_desktop_helper` + `device_status` RPCs + advisory-lock + race-condition test + schema-name test + Mac smoke check (existing one-click pairing still works) + migration rollback drill |
| 1 (sentry) | Audit `sentry_init.rs` `before_send` scrubber for refresh/access/email/helper_secret leakage. Land before any client work. |
| 2 | Rust: `auth.rs` (3 endpoints, mocked HTTP unit tests) + `keychain.rs` (fail-closed Linux behavior + Win Credential Manager smoke + macOS Keychain smoke) |
| 3 | `lib.rs`: 5 new commands + supabase.rs `with_user_jwt` helper + integration test against staging Supabase project |
| 4 | `App.tsx` 3-state UI + i18n keys (en/zh-CN/ja, with Gemini review pass) + lazy-refresh wiring on user-scoped actions |
| 5 | helper_sync error classifier + account-deleted handling + session-expired banner + sign-out always-local |
| 6 | E2E on Windows VM (the same VM used for v0.2.10 / v0.2.11 / v0.2.12 verifies). E2E on Linux VM (libsecret available + libsecret missing scenarios). E2E sanity on macOS. |
| 7 | Email deliverability live test (real address, check spam, time-to-arrival measurement) + rate-limit live test + CHANGELOG + spec archive into PROJECT_FIX_..._v0.3.0_otp.md after ship |

**Cuts considered if we need to compress:**
- Drop Linux explicitly from v0.3.0 — ship Win + macOS; Linux follows in
  v0.3.1 once libsecret edge cases are settled. ~1.5 day saved.
- Defer the helper_sync error classifier (Section 6.4) to v0.3.1 — at
  worst, account-deleted users see infinite 401s in their sync log
  until they manually unpair. ~0.5 day saved.
Discuss before sprint start.

## 9. Backward compatibility

- Existing v0.2.x users with valid `HelperConfig.device_id` keep working.
  helper_sync is unchanged on the server.
- The Mac one-click pairing flow (`PairingSection.swift`) is untouched.
- Pairing-code input UI lives under "Advanced" disclosure on desktop.
- After v0.3.0 ships, copy in `pair_hint` no longer says "wait for v0.3.0".

## 10. Out of scope (post-v0.3.0)

- **OAuth providers (Apple / Google / GitHub) on desktop** → v0.3.1, via
  loopback PKCE (Codex's pattern). Optional power-user feature; OTP path
  stays as the no-fuss default.
- **iOS "Add Device" UI** → main repo, separate sprint. Exposes
  `generatePairingCode` for phone-first onboarding. Lower priority once
  desktop OTP lands.
- **Device management screen** (list paired devices, remote sign-out) →
  v0.4.0.
- **Password sign-in** — explicitly NOT supported. OTP only. Reduces
  password-reuse blast radius.

## 11. Decisions to close before sprint start

Codex review correctly flagged that several items below were "open
questions" but actually directly affect UI validation, launch
reliability, and secret leakage risk. Resolved here.

1. **OTP rate limit** — DECISION: confirm exact rate limit in the
   Supabase dashboard (`Authentication → Rate Limits → Email OTP`) on
   sprint Day 0. If default (30/hr per IP) is too tight for our usage
   pattern, bump to 60/hr. Surface remaining-quota in UI errors.
2. **OTP length** — DECISION: 6 digits, Supabase default. Verify on
   sprint Day 0 by sending a real OTP and counting digits. Lock UI
   input to exactly 6 digits with auto-submit on 6th keystroke. Length
   string in i18n is parameterized, not hardcoded, so a future change
   to 8 digits requires only a config flip.
3. **Email branding** — DECISION: Punt template customization to
   v0.3.1. v0.3.0 ships with Supabase default plain-text OTP email.
   Set "From: noreply@cli-pulse.com" or similar trustworthy sender via
   Supabase SMTP settings on sprint Day 0 if not already configured.
4. **Sentry token leakage** — DECISION: Day-1 sprint task. Audit
   `src-tauri/src/sentry_init.rs` `before_send` scrubber. Add explicit
   deny-list for field names: `refresh_token`, `access_token`,
   `helper_secret`, `email`, `pairing_code`, `Authorization`.
   Also scrub URL query parameters with the same names (matters for
   breadcrumb URL capture). Test by triggering a deliberate panic in
   a code path that has access_token in scope and verify Sentry event
   does not contain it.
5. **"Sign in" vs "Pair" naming** — DECISION: User-facing copy says
   "Sign in" / "登录" / "サインイン". Code identifiers and Tauri command
   names keep "pair" / "auth_*" mix for git-blame continuity. Locale
   keys: new `auth.*` namespace alongside existing `pair.*`.
6. **Mac one-click flow shape after v0.3.0** — DECISION: Mac users get a
   net-new "Sign in with Email" option in Settings → Connection. The
   existing "Set Up Helper" one-click stays as the default for Mac
   (preserves muscle memory + paid-customer flow), but the OTP path is
   visible. Mac users who prefer email login can switch — and have
   identical helper credentials either way.

## 12. References

- Codex GPT-5.4 review: 2026-05-02 03:57Z (task `task-mont9aj3-vjezps`),
  see chat transcript.
- Gemini 3.1 Pro review: 2026-05-02 04:0xZ, see chat transcript.
- Existing `register_helper` RPC: `backend/supabase/helper_rpc.sql:5+`.
- Existing `upsert_daily_usage` RPC: `backend/supabase/schema.sql:431` —
  this one stays auth.uid()-only because it's the macOS scanner path,
  not the helper path.
- Mac pairing UI: `CLI Pulse Bar/CLI Pulse Bar/PairingSection.swift:46`.
- Pairing-code generator: `CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/AuthManager.swift:322`.
- Swift PKCE / refresh patterns to mirror: `CLI Pulse Bar/CLIPulseCore/Sources/CLIPulseCore/APIClient.swift:72`, `:1075`, `:1088`.
- Supabase GoTrue OTP API: `/auth/v1/otp`, `/auth/v1/verify`, `/auth/v1/token?grant_type=refresh_token`.
- RFC 7636 (PKCE) — kept in mind for v0.3.1 OAuth path.
- RFC 8628 (device flow) — explicitly rejected: Supabase doesn't support natively, custom Edge Function adds maintenance burden, OTP solves the same UX cleaner.
