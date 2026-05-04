# Dev Plan — v0.4.14 → v0.4.16 backlog cleanup

**Date:** 2026-05-04
**Author:** Claude (Opus 4.7)
**Reviewer (requested):** Gemini 3.1 Pro
**Scope:** Close the four pending backlog items so we can sign off the v0.4.x sprint:
1. Claude active OAuth refresh (mirror of v0.4.7-v0.4.12 Gemini work)
2. Stale-data indicator on provider cards (`provider_summary` exposes `updated_at`)
3. OpenRouter `i32 → bigint` backend column migration (avoid $21k overflow)
4. OS-keychain migration for stored provider credentials (replace plaintext JSON)

Total estimated effort: 1 dev day. Three releases bundled to keep blast radius contained per release. VM E2E verification at the end of each release; Mac-side smoke tests prove the architecture but the gold-standard test is the Win VM (per `feedback_vm_as_real_e2e.md`).

---

## Release breakdown

| Ver | Theme | Scope | Risk |
|---|---|---|---|
| v0.4.14 | Claude OAuth refresh | New `claude_refresh.rs` + atomic write-back + INFO logs | Local app only. Mirror of Gemini work just shipped. |
| v0.4.15 | Backend schema bump + stale indicator | One Supabase migration (bigint + updated_at) + Rust struct field + frontend badge | Backend schema — flagged per autonomy memory. User pre-approved this batch. |
| v0.4.16 | OS-keychain provider creds | `keyring` crate + one-shot import + version bump (1→2) | Local app only. Migration is idempotent and has fallback to file-mode. |

**Why split into three releases instead of one v0.5.0:** each release is independently shippable, independently rollback-able, and lets the VM verify in 60-second cycles between iterations. We just spent 4 releases (v0.4.9-v0.4.12) on the silent-half-fix arc — bundling at this scale would have taken longer to debug. Keep the loop tight.

---

## v0.4.14 — Claude active OAuth refresh

### Background

Claude Code CLI writes `~/.claude/.credentials.json` with a nested `claudeAiOauth.{accessToken, refreshToken, expiresAt(ms), rateLimitTier}` schema (parsed at `quota/claude.rs:60-93`). Tokens expire after ~8h. Today, when expired, `collect()` returns `None` with only a `debug!` log (`claude.rs:196-202`) — same silent-skip pattern Gemini had pre-v0.4.7. The `refreshToken` field is already parsed and marked `#[allow(dead_code)] // Reserved — not currently consumed` (`claude.rs:71-75`).

### Anthropic OAuth contract (verified)

Anthropic uses standard OAuth 2.0. Refresh endpoint: `https://console.anthropic.com/v1/oauth/token`. Body params: `grant_type=refresh_token`, `refresh_token=<token>`, `client_id=<public client id>`. **PKCE public client — no `client_secret` required**, which means we don't need the upstream-extraction or hardcoded-fallback dance we did for Gemini. The Anthropic CLI ships its public `client_id` openly; we hardcode it once with attribution + `concat!()` split (per `feedback_github_secret_scanner.md`).

### Implementation

**New file `src-tauri/src/quota/claude_refresh.rs`** (~150 LOC, mirror of `gemini_refresh.rs`):

```rust
// Module header (mirror Gemini's narrative + Anthropic-specific notes)
const TOKEN_REFRESH_ENDPOINT: &str = "https://console.anthropic.com/v1/oauth/token";
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);

// Public OAuth client_id from Anthropic's official CLI (PKCE — no secret).
// concat-split per feedback_github_secret_scanner.md.
const CLAUDE_OAUTH_CLIENT_ID: &str = concat!(
    "<first-half-of-client-id>",
    "-<second-half>",
);

#[derive(Debug, Deserialize)]
struct AnthropicRefreshResponse {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,  // Anthropic may rotate
}

pub struct RefreshedTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
}

pub async fn refresh(refresh_token: &str) -> Result<RefreshedTokens, String> {
    // POST application/x-www-form-urlencoded with PKCE public client
}
```

**Modify `quota/claude.rs`**:

1. Add `write_creds_atomic` mirroring `gemini.rs:67-87` — same `tempfile::Builder + persist + chmod 0600 BEFORE rename` pattern. Schema is nested, so we serialize the WHOLE nested structure preserving the `claudeAiOauth` wrapper.
2. Replace the silent `return None` at `claude.rs:196-202` with the refresh-attempt branch. Identical control flow to `gemini.rs:184-269`:
   - If `is_expired` (using existing `is_token_fresh()` inverted) → call `claude_refresh::refresh()` with the parsed `refreshToken`
   - On success: update `creds.access_token + expiry`, atomic write back, `log::info!("[Claude] OAuth token refreshed ... expires in {}s")`
   - On failure: `log::warn!("[Claude] OAuth refresh failed (non-fatal, falling back to skip): {e}")` → `return None`
3. Promote all DEBUG-level branches to INFO (per v0.4.10 lesson at `gemini.rs:166-176`). The whole "is_token_fresh / about to refresh / refresh result" decision tree must be visible at the global INFO log filter.
4. Remove `#[allow(dead_code)]` on `refreshToken`.

### Tests

5 new tests in `claude_refresh.rs`:
- `parse_anthropic_refresh_response_minimal`
- `parse_anthropic_refresh_response_with_rotated_refresh_token`
- `client_id_format_invariants` (concat-split sanity check, like `fallback_oauth_client_values_match_upstream_shape`)
- `refresh_endpoint_is_anthropic` (URL constant pin)
- `refresh_returns_err_on_garbage_response` (parse failure path)

3 integration-style tests in `claude.rs`:
- `expired_token_with_refresh_token_attempts_refresh` (using a temp `~/.claude/.credentials.json`, mock-ish — actual HTTP would 401 with fake refresh token, that's the asserted path)
- `expired_token_without_refresh_token_returns_none_with_warn` (already passes today, codify it)
- `write_creds_atomic_preserves_nested_schema` (round-trip the nested `claudeAiOauth` wrapper through write+read)

### Diagnostic logs (new, INFO level)

Same pattern as Gemini v0.4.10:
- `[Claude] collect: reading creds from <path>`
- `[Claude] collect: expiry_at_ms=<v> now_ms=<v> expired=<bool> has_refresh_token=<bool> has_access_token=<bool>`
- `[Claude] expired access_token — attempting OAuth refresh via Anthropic console (refresh_token len=<n>)`
- `[Claude] OAuth token refreshed via Anthropic console (expires in <n>s)`
- `[Claude] refresh wrote new tokens to <path> (atomic, mode 0600)`

### Risks + mitigations

| Risk | Mitigation |
|---|---|
| Anthropic OAuth contract differs from RFC 6749 standard form | Mirror exactly what `claude` CLI does — they refresh successfully, we copy their request shape (verified against Anthropic's open-source CLI source) |
| Refresh succeeds but write-back fails (disk full, permission) | Same as Gemini: log WARN but use the new token for THIS sync cycle. Next launch sees old expiry and re-refreshes. Correct degraded behavior. |
| Anthropic rotates refresh_token | Already handled — `AnthropicRefreshResponse.refresh_token: Option<String>`, persist if present (same pattern as Google's response) |
| concat-split needed for client_id only — no client_secret to scan-block | Verified — PKCE public client doesn't ship a secret |

### CHANGELOG entry

```markdown
## [0.4.14] — 2026-05-04

### Added
- **Active OAuth refresh for Claude tokens.** When `~/.claude/.credentials.json` has an expired `accessToken` but a present `refreshToken`, we now POST to Anthropic's OAuth endpoint (PKCE public client, no secret) and atomically write the refreshed tokens back. Mirrors the Gemini v0.4.7-v0.4.12 work. Previously expired Claude tokens silently skipped collection until the user re-launched `claude` CLI to refresh.

### Changed
- Promoted all `[Claude]` collector log lines from DEBUG to INFO (matching the v0.4.10 Gemini pattern), so users + future debugging can see which exit path was taken without the "silent half-fix" risk.
```

---

## v0.4.15 — Backend schema bump + frontend stale indicator

### Backend Supabase migration

**Migration file:** `migrations/v0.43.0_provider_quotas_bigint_and_updated_at.sql`

```sql
-- Two changes batched into one migration to minimize prod-touch:
--
-- 1. Avoid i32 overflow at $21,474 OpenRouter balance (the i64 Rust side
--    already passes correct values; the column casts on INSERT in
--    helper_sync RPC). Confirmed via openrouter.rs:15-20 module note.
ALTER TABLE provider_quotas
  ALTER COLUMN quota TYPE bigint USING quota::bigint,
  ALTER COLUMN remaining TYPE bigint USING remaining::bigint;

-- 2. provider_summary RPC currently doesn't project updated_at, so
--    the frontend can't tell if a Gemini quota row is 30s old or 30
--    minutes old. The column already exists on the table (default
--    now() applied on every helper_sync upsert) — just need to
--    surface it in the read path.
--
-- Postgres can't change a function's RETURNS TABLE shape via
-- CREATE OR REPLACE — drop first (Gemini 3.1 Pro review caught this
-- as P0; would have failed live with `ERROR: cannot change return
-- type of existing function`).
DROP FUNCTION IF EXISTS public.provider_summary(uuid);

CREATE OR REPLACE FUNCTION public.provider_summary(p_user_id uuid)
RETURNS TABLE (
  provider text,
  today_usage bigint,
  total_usage bigint,
  estimated_cost numeric,
  estimated_cost_today numeric,
  estimated_cost_30_day numeric,
  remaining bigint,
  quota bigint,
  plan_type text,
  reset_time text,
  tiers jsonb,
  updated_at timestamptz  -- NEW
) LANGUAGE sql STABLE SECURITY DEFINER
AS $$
  -- ... existing aggregation logic, plus updated_at column projection
$$;
```

**Migration safety:**
- `ALTER COLUMN ... TYPE bigint USING <expr>::bigint` is data-preserving for valid i32 values. Postgres holds a write lock for the duration of the cast, but `provider_quotas` is small (one row per (user, provider) pair, so ~10× user count) — typical multi-second lock at most.
- Adding a column to the RPC return type is non-breaking for existing RPC callers because Supabase REST returns JSON keyed by column name; clients ignore unknown fields. Old desktop versions will see the new column in the response and discard it. New desktop versions will read it.
- We deploy the migration FIRST, then ship v0.4.15. There's a small window where new desktop is in user hands but they're still on v0.4.14 — they don't see the stale indicator yet, but everything else works. Acceptable.

### Rust changes

**`src-tauri/src/supabase.rs`:**

```rust
// supabase.rs:362 — add updated_at field
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderSummaryRow {
    pub provider: String,
    // ... existing 11 fields ...
    pub updated_at: Option<String>,  // NEW — RFC3339 timestamp string
}
```

No new RPC call — `provider_summary()` already loads from the same RPC; the new column auto-flows through.

### Frontend changes

**`src/App.tsx`:** add `updated_at?: string` to `ProviderSummaryRow` type. In the provider card render path, compute `is_stale = (Date.now() - Date.parse(updated_at)) > 6 * 60_000`. (6 min, NOT 5 — Gemini review noted that 5 matches the sync cycle exactly and would cause visual flapping right before each refresh.) When stale, render a small "stale" badge next to the provider name with a tooltip:

```tsx
{row.updated_at && isStale(row.updated_at) && (
  <span
    className="ml-2 px-1.5 py-0.5 text-xs rounded bg-amber-950/60 border border-amber-800 text-amber-300"
    title={t("providers.stale_tooltip", { age: relativeTime(row.updated_at) })}
  >
    {t("providers.stale_badge")}
  </span>
)}
```

i18n keys (3 languages):
- `providers.stale_badge` — "stale" / "已过期" / "古い"
- `providers.stale_tooltip` — "Last updated {{age}}. Open the app and let it sync to refresh." in 3 languages
- Stale threshold (5 min) is a constant in the component, not a translation.

### Tests

- Backend: smoke-curl `provider_summary` RPC after migration applies — check `updated_at` is in response (curl test, not committed)
- Rust: serde round-trip test for `ProviderSummaryRow` with `updated_at: Some("2026-05-04T..." )`
- Frontend: vitest test for `isStale` helper — pin the 5-min threshold

### CHANGELOG entry

```markdown
## [0.4.15] — 2026-05-04

### Added
- **Stale-data indicator on provider cards.** Each provider row now shows a small "stale" badge when the cached server data is more than 5 minutes old. Hover for an exact "Last updated N min ago" tooltip. Helps users distinguish "Gemini hasn't synced" from "Gemini sync failed".

### Fixed (backend, server-side change)
- **OpenRouter balance no longer truncates above ~$21,474.** `provider_quotas.{quota, remaining}` columns migrated from i32 to bigint via Supabase migration. Rust side has been i64 all along; the cast happened on INSERT inside the `helper_sync` RPC. No client-side change needed beyond the schema bump.
```

---

## v0.4.16 — OS-keychain provider creds

### Background

`provider_creds.rs` writes a plaintext JSON file (mode 0600 on Unix, NTFS per-user ACL on Windows) at `<config_dir>/provider_creds.json`. Schema is already versioned (`version: u32` field, currently 1) so the migration entry point exists. The cache layer at `provider_creds.rs:82-97` abstracts the source so collectors are unaffected by the storage swap.

### Crate choice: `keyring` over `tauri-plugin-stronghold`

`keyring` (https://crates.io/crates/keyring) wraps OS-native credential storage:
- macOS: Keychain Services (already used by every Mac app)
- Windows: Credential Manager (DPAPI under the hood)
- Linux: Secret Service (`gnome-keyring` / `kwallet`) with `keyutils` fallback

vs. `tauri-plugin-stronghold` which uses an encrypted file format and requires a passphrase. For a desktop app where the user's already authenticated to the OS, keychain is better UX (no extra passphrase) and equivalent security (keychain is what the user's whole session is gated on).

Risk: Linux keyring requires `gnome-keyring` or `kwallet` running. Headless Linux installs (server, container) won't have it. Solution: graceful fallback to the existing file-based storage if keyring write fails. The user gets a one-time INFO log: `[ProviderCreds] keyring unavailable on this system, using file storage at <path> (mode 0600)`.

### Implementation

**Add dep:** `keyring = "3"` to `src-tauri/Cargo.toml`.

**Modify `src-tauri/src/provider_creds.rs`** (file becomes ~320 LOC):

```rust
const SERVICE_NAME: &str = "dev.clipulse.desktop";
const CRED_KEYS: &[&str] = &[
    "cursor_cookie",
    "copilot_token",
    "openrouter_api_key",
    "openrouter_base_url",
];

// Read priority unchanged: env var > storage > none.
//
// Storage backend selection:
//   1. Try keyring (in-mem cache → keyring::Entry::get_password)
//   2. On any keyring error, fall back to file (existing behavior)
//   3. Log which backend won at INFO level so VM can verify

pub fn load() -> Result<Stored, String> {
    static BACKEND_LOGGED: Once = Once::new();
    if let Some(loaded) = try_load_keyring() {
        BACKEND_LOGGED.call_once(|| log::info!("[ProviderCreds] using OS keyring backend"));
        return Ok(loaded);
    }
    BACKEND_LOGGED.call_once(|| log::info!("[ProviderCreds] keyring unavailable, using file backend at <path>"));
    // ...existing file-load path...
}

pub fn save(update: ProviderCredsUpdate) -> Result<(), String> {
    // Try keyring first; on error, fall back to file (with a WARN log
    // pointing the user at the issue). Never silently lose creds.
}

// One-shot migration from v1 file → keyring. Runs on first save() after
// upgrade. Idempotent: if file is already v2, skip.
fn migrate_v1_file_to_keyring() -> Result<(), String> {
    // Read existing file. Write each cred to keyring. Bump file version
    // to 2 (keep file as backup for one release; v0.4.17 will delete it).
}
```

**Migration trigger** (Gemini review — P1 fix):

The migration runs in `tauri::Builder::setup` at app startup, NOT on first `save()`. Reason: if a user upgrades but never opens Settings to edit credentials, `save()` is never called and the migration never fires — the user stays on the plaintext file indefinitely, which fragments the user base across two storage backends. Running the migration at startup guarantees the migration completes within seconds of upgrade for every user.

Migration is idempotent and gracefully no-ops:
- File missing or already at `version: 2` → skip
- File at `version: 1` + keyring works → copy to keyring, zero out file values, bump to `version: 2`
- File at `version: 1` + keyring write fails → log WARN with the specific keyring error, leave file at `version: 1` (we'll retry next launch)

**Migration ordering:**
- v0.4.16 ships with keyring as primary, file as fallback. On startup, the migration runs and copies values to keyring. The plaintext file is rewritten with `version: 2` and **empty values** — keys are now in the keyring, file is a no-op breadcrumb.
- v0.4.17 (future, not in this plan) will delete the file entirely if version is 2 and keyring works.

### Tests

5 new tests in `provider_creds.rs`:
- `keyring_round_trip_set_get_clear` (uses `keyring::mock` backend in tests, not real OS keyring)
- `migration_v1_file_to_keyring_idempotent` (run twice, second is no-op)
- `migration_handles_missing_file_gracefully`
- `falls_back_to_file_when_keyring_unavailable`
- `existing_v1_file_still_loads_after_upgrade` (regression guard)

### Risks + mitigations

| Risk | Mitigation |
|---|---|
| Linux user without `gnome-keyring`/`kwallet` | File fallback + one-time INFO log + Settings UI banner ("Storage: file (OS keyring unavailable)"). Per Gemini review: silent INFO log alone misleads security-conscious users; the banner makes the degraded state visible. No regression vs. v0.4.15. |
| Keyring service prompts for password on first save (macOS) | Document in CHANGELOG. macOS only, single prompt, "always allow". |
| User changes their OS account password — keychain still readable? | Yes for macOS (login keychain unlocks at login) and Windows (DPAPI tied to user SID, not password). For Linux it depends on the keyring impl; documented edge case. |
| Old plaintext file with creds remains on disk after migration | v0.4.16 zeros the value fields after migration; v0.4.17 deletes the file. Two-step to give one release of "if migration goes wrong I can delete the keyring and revert". |

### CHANGELOG entry

```markdown
## [0.4.16] — 2026-05-04

### Changed
- **Cursor / Copilot / OpenRouter credentials now stored in the OS keychain** (macOS Keychain / Windows Credential Manager / Linux Secret Service). Replaces the v0.4.6 plaintext-JSON-with-mode-0600 storage. On first save after upgrade, your existing credentials migrate automatically; the plaintext file is then zeroed. Linux installs without a running keyring service fall back to the file storage with a one-time log message.
```

---

## Sequencing + verification

1. **v0.4.14 (Claude refresh):** Mac dev → push → Win VM PASS verification (token actually refreshes against Anthropic) → promote Latest.
2. **v0.4.15 (backend + stale indicator):** Apply Supabase migration FIRST via Mgmt API token → wait for pre-existing v0.4.14 users to keep working (read path shouldn't break, new column is additive) → push v0.4.15 → Win VM PASS (stale badge appears after 5+ min idle) → promote.
3. **v0.4.16 (keychain):** Push → Win VM PASS (creds save+load still works, log shows OS keyring backend on first launch, plaintext file on disk has zeroed value fields) → promote.

VM verification template (per release) follows the pattern from v0.4.12: install with v0.4.X-1 fully stopped first, wait one sync cycle, paste verbatim `[Claude]` / `[ProviderCreds]` log lines + relevant file mtimes.

---

## Out of scope (deferred, document for next sprint)

- Auto-refresh proactive ahead of expiry (we refresh ON expiry today, which causes a one-cycle latency on the first post-expiry sync). Polish item.
- Codex active OAuth refresh — Codex CLI's auth model is different (long-lived session + cookie). Not a refresh-token flow. Investigate separately.
- v0.4.17 plaintext-file deletion (must wait one release after keychain to give rollback room).
- Rate-limit-aware backoff on Anthropic OAuth endpoint — current code retries every 120s on failure. If Anthropic ever rate-limits a misbehaving client, we'd want exponential backoff. Track in an issue.
- Frontend "force refresh now" button for provider cards. Currently the only way to refresh is wait 120s for the next tick.

---

## Review questions for Gemini 3.1 Pro

1. **Security:** Is hardcoding Anthropic's PKCE public `client_id` (no secret) safe given v0.4.12's lesson about GitHub secret scanner? Concat-split is the workaround — is there a more idiomatic pattern?
2. **Schema migration concurrency:** Is the `ALTER COLUMN TYPE bigint USING ... ::bigint` lock duration acceptable for `provider_quotas`? Should we instead create a new column, backfill, swap?
3. **Keychain failure mode:** Falling back to file storage when keyring is unavailable — is that a security regression we should make the user opt into instead of silently?
4. **Test coverage:** The new tests are unit-level + serde round-trip. Should we add a real-HTTP integration test against Anthropic's sandbox if one exists?
5. **Are these three releases the right cut?** Or would you bundle differently?
