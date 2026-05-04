# Dev Plan — v0.4.19 polish (single release)

**Date:** 2026-05-05
**Author:** Claude (Opus 4.7)
**Reviewer (requested):** Gemini 3.1 Pro
**Scope:** Three small items bundled into one release. All three are local-only (no backend schema changes, no new external endpoints, no new deps). Total estimated effort: half a dev day.

## Context

Today's earlier sprint (v0.4.13 → v0.4.18) closed the v0.4.x backlog: Claude OAuth refresh, provider stale indicator, OpenRouter `i32→bigint` Supabase migration, OS-keychain provider creds, plus two VM-driven follow-ups (diagnostic snapshot field, OpenRouter URL Clear button parity). v0.4.18 is currently building.

Audit of `src-tauri/src/`, `src/`, and the v0.4.14-v0.4.16 dev plan's deferred section surfaced three remaining items worth doing now. Bundling because each is < 50 LOC and the surface areas don't overlap.

## Surprise finding (worth flagging early)

**Codex active OAuth refresh ALREADY ships in v0.4.x.** The deferred section of the v0.4.14-v0.4.16 plan claimed "Codex CLI's auth model is different (long-lived session + cookie). Not a refresh-token flow." That was wrong. `quota/codex.rs:226-269` has a complete OAuth refresh against `https://auth.openai.com/oauth/token` with PKCE public client_id, atomic write-back, and rotation handling. The only difference from Claude/Gemini is staleness detection: Codex tracks `last_refresh` (RFC3339 string) and treats > 8 days as needing refresh, vs. Claude/Gemini's `expiresAt` (epoch ms) timestamp comparison.

No Codex work needed. Delete that deferral from the prior plan's notes, save a "Codex parity already shipped" note to memory.

---

## Item 1 — Plaintext `provider_creds.json` deletion

### Background

v0.4.16 migrated provider creds from a plaintext-mode-0600 JSON file to the OS keychain. After migration, the file is rewritten with `version: 2` and zeroed values — kept on disk as a "rollback breadcrumb" in case migration goes wrong. The plan stated "v0.4.17 will delete the file entirely after one release of rollback room." We've now had v0.4.16 + v0.4.17 + v0.4.18 in production; rollback room is exercised. Time to delete.

### Implementation

In `provider_creds::init_backend()` after the migration completes, when the file is at `version: 2` AND keychain is `OsKeychain` AND we're on this app session past v0.4.19, delete the file with a one-time INFO log.

```rust
// At end of migrate_v1_file_to_keychain_if_needed() success path,
// AND in init_backend() when we detect a v2 file already present:
fn cleanup_v2_file_if_present(path: &Path) {
    if !path.exists() { return; }
    match fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<ProviderCreds>(&t).ok()) {
        Some(creds) if creds.version >= 2 => {
            // v2 means values are in keychain. The file is no-op breadcrumb.
            // v0.4.19 deletes it now that rollback window has passed.
            if let Err(e) = fs::remove_file(path) {
                log::warn!("[ProviderCreds] could not delete v2 breadcrumb file at {} (non-fatal): {e}", path.display());
            } else {
                log::info!("[ProviderCreds] deleted v2 breadcrumb file at {} — values live in OS keychain only", path.display());
            }
        }
        _ => {} // v1 file or non-existent: leave alone (keychain unavailable, fall back path)
    }
}
```

The cleanup runs ONLY when the active backend is `OsKeychain` — if keychain is unavailable, we're still using the file as primary storage and must not delete it.

### Tests

- `cleanup_keeps_v1_file` (defensive — must not delete pre-migration file)
- `cleanup_removes_v2_file_when_keychain_active`
- `cleanup_skips_when_file_missing`

### Risk

**P0 (Gemini's likely catch):** if `init_backend()` hits a race where keychain is initially up but flickers down between the migration write and the file delete, we'd delete a file whose values aren't actually in the keychain → user loses creds.

Mitigation: only delete after `save_to_keychain()` returns Ok in the migration path AND a re-read of all 4 keychain accounts succeeds with the same values. For pre-existing v2 files (no migration needed this run), trust the file's `version: 2` marker — it was written by us in a prior session after a successful keychain write.

---

## Item 2 — "Force refresh now" button on Providers tab

### Background

Background sync runs every 120s. Users who paste a fresh OpenRouter API key in Settings → Integrations have no way to see their balance until the next tick. Today the only way to force a sync is to restart the app. Closing this gap is one Tauri command (already exists: `sync_now`) + one frontend button.

### Implementation

**Frontend:** `<button>` in the Providers tab header, calls `invoke('sync_now')`, updates the `serverRows` state on success. While running, button shows a spinner AND is `disabled` (Gemini review P1 — without `disabled`, a user spam-clicking fires concurrent `sync_now` invocations against provider rate limits). Disabled when `paired === false`.

i18n keys:
- `providers.force_refresh_button` — "Refresh now" / "立即刷新" / "今すぐ更新"
- `providers.force_refresh_loading` — "Refreshing…" / "刷新中…" / "更新中…"

**Backend:** no new code. `sync_now` Tauri command already exists in `lib.rs` (used by background tick + manual `runScan` via the header). Just wire the frontend button.

**Gemini's "what you'd do differently" — defer the MPSC tick reset.** Gemini suggested routing the manual refresh through a tokio::sync::Notify channel that resets the background `tokio::time::sleep(SYNC_INTERVAL)` so a click at second 118 doesn't fire a second tick at second 120. That IS a real correctness improvement, but: (a) `sync_now` is idempotent at the helper_sync level (upsert by `(user_id, provider)`), so a redundant sync is bounded extra cost; (b) the refactor needs `tokio::select!` + a Notify wired into the loop, which is meaningful complexity for a polish ship. Documented as a follow-up — track for a future sprint where we touch `spawn_background_sync` for other reasons.

### Tests

Frontend only — vitest:
- Verify button is disabled when not paired
- Verify spinner shows during invoke

### Risk

Low. The Tauri command is idempotent — if a background tick is mid-flight, the manual sync just runs sequentially. No locking needed.

---

## Item 3 — Proactive pre-expiry refresh

### Background

Today, Claude (`is_token_fresh()` at `claude.rs:243`) and Gemini (`is_expired()` at `gemini.rs:55`) refresh **on** expiry. The first sync cycle after the token expires has to do a refresh round-trip BEFORE the `/usage` fetch — adds 1-2 seconds latency to that one cycle. Better: refresh when `expiresAt - now < 5 minutes`, so the refresh happens at most one cycle before expiry and the post-expiry cycle finds a fresh token already in place.

### Implementation

Bump the safety margin in `claude.rs::EXPIRY_SAFETY_MARGIN_SECS` from 60 to 300 (5 min). Same idea in `gemini.rs::is_expired()`: change the comparison from `exp_ms < now_ms` to `exp_ms < now_ms + 5*60*1000.0`. Add a constant for clarity.

**Why 5 min, not 60s:** background sync runs every 120s. If the expiry window is < 60s, a single missed cycle (e.g., user closes laptop lid for a sync interval) means the token expires before the next refresh. 5 min gives 2-3 sync cycles of buffer.

**Why not 30 min:** longer windows mean we refresh more often than necessary, eating Anthropic/Google rate limits unnecessarily. 5 min is the sweet spot per the v0.4.7 review notes (Codex review of v0.4.3 spec).

Codex doesn't need a change — its 8-day staleness window is already proactive (`REFRESH_STALENESS_DAYS = 8` at `codex.rs:48` triggers a refresh days before the actual token would fail).

### Tests

- `claude::is_token_fresh` — token expiring in 4 min returns false (pre-expiry refresh fires)
- `claude::is_token_fresh` — token expiring in 6 min returns true (still cached)
- `gemini::is_expired` — symmetric pair

### Risk

Low. Worst case: we refresh more often. But the proactive window is small (5 min ≈ 0.06% of an 8h token life), so real refresh frequency is bounded by token life regardless.

---

## Sequencing

All three items in one v0.4.19 release. Order of implementation:

1. Item 3 (proactive refresh) — smallest, most contained.
2. Item 2 (Force refresh button) — frontend-only.
3. Item 1 (file deletion) — most safety-critical; do last so the migration can be tested against the v0.4.18 binary's file-zeroing path before deletion lands.

Single VM verification at the end covering all three.

---

## Out of scope (explicitly deferred)

- **Rate-limit-aware backoff on OAuth refresh endpoints.** No incident yet; revisit if Anthropic / Google ever 429.
- **Codex staleness window tuning.** Hardcoded 8 days at `codex.rs:48`. If OpenAI shortens token life this'll silently fail (`/wham/usage` would 401). Defer until that happens.
- **`_json_ref_placeholder` dead code at `lib.rs:1244-1245`.** Trivial removal but I want to verify it doesn't break the build first; defer to a separate cleanup commit.
- **`wipe_all()` cache helper.** Either expose as a Settings → "Clear scan cache" button OR delete entirely. Wants a UX decision, not a code one. Defer.
- **OpenRouter URL clearing UX refactor.** v0.4.18 shipped a working button; refactoring to a unified `clearKind: "secret" | "plain"` config is polish, not a fix.

---

## Review questions for Gemini 3.1 Pro

1. **Item 1 race:** is the "re-read keychain values match what we just wrote" sanity check before deletion sufficient? Or should I add a checksum / signature on the file itself before deleting? (Concern: keychain write returns Ok but actual underlying credential storage is corrupt — file deletion would lock us out of recovery.)
2. **Item 3 window size:** 5 min vs alternatives (1 min: too tight, 30 min: too aggressive). Is there a reason to pick something else for the Anthropic/Google specific case?
3. **Bundling:** three items in one release vs. three separate patches. Bundled saves CI and one VM cycle. Any reason to split?
4. **Codex deferral correction:** the prior plan's deferral was wrong (Codex refresh ships). Worth a CHANGELOG note saying "deferral was incorrect; Codex parity has been live since v0.4.3"? Or just delete the deferral silently?
