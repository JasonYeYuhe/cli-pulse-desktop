# Handoff prompt — v0.5.4 + v0.5.5 + v0.5.6 next-session implementation

Copy the section between the markers below into a fresh Mac Claude Code session at `/Users/jason/Documents/cli-pulse-desktop/`. The previous session's context window was getting full after 9 ships in one day; this prompt re-seeds a clean session with all the on-disk artifacts + reviewer findings the implementation needs.

---

## ▼ START COPY-PASTE BELOW THIS LINE ▼

You are taking over `cli-pulse-desktop` (Tauri 2 + Rust + React, Win+Linux only) at `/Users/jason/Documents/cli-pulse-desktop/`. The previous session shipped 8 versions in one calendar day (v0.4.21 → v0.5.3), all VM-verified clean. **Current Latest = v0.5.3.** The next sprint of three small ships is *fully planned + reviewed by Codex and Gemini 3.1 Pro*, but **NOT YET IMPLEMENTED**. Your job: implement v0.5.4, v0.5.5, v0.5.6 in that order, applying every reviewer fix. Then write a VM verify prompt for Jason to forward.

The user (Jason) operates you autonomously per `feedback_desktop_autonomy.md`. Don't ask permission for code changes / commits / version bumps / releases / hotfixes / refactors — just do them. Each ship has its own pre-push gates + Gemini review pass; don't skip those.

### 1) Ground yourself — required reading in order

**Auto-memory (already in context):**
- `feedback_desktop_autonomy.md` — your scope of authority, 3 explicit exception categories
- `feedback_vm_as_real_e2e.md` — Mac is host-managed (no creds files); VM is the real-world test target
- `feedback_gemini_review_patterns.md` — recurring catches Gemini surfaces
- `feedback_github_secret_scanner.md` — `concat!()` workaround for OAuth literals
- `reference_desktop_repo.md` — repo location, stack, **complete sprint history through v0.5.3** (was just updated with the day's 8-ship summary)
- `reference_sentry.md` — Sentry org/project layout, sentry-cli usage, **issue-vs-event release-filter gotcha** (now documented as of 2026-05-05)
- `reference_supabase_creds.md` + `reference_supabase_access_token.md` — Supabase project ID + Mgmt API token
- `reference_gemini_cli.md` — `/opt/homebrew/bin/gemini` invocation pattern

**The plan + reviews on disk (READ ALL THREE):**
- `/Users/jason/Documents/cli-pulse-desktop/PROJECT_DEV_PLAN_2026-05-05_v0.5.4_v0.5.5_v0.5.6_post_parity_trio.md` — v1 plan, **has fatal flaws** flagged by Codex; do NOT implement as-written
- The previous session's chat surfaced the convergent reviewer findings — you have to apply them. They're summarized in §3 below.

### 2) The three ships, in order

**v0.5.4 — Settings → Danger Zone** (1.5 h, smallest, ship FIRST)
- New Settings section: **Clear local caches** + **Delete cloud account**
- Delete-account flow uses type-to-confirm gate (user types `DELETE` / `删除` / `削除`)
- Clear-caches scope: `cache_invalidate()` (in-mem dashboard) + `cache::wipe_all(None)` (disk scan cache) + provider_summary cache + collector_status cache
- Delete-account scope: clear-caches + keychain (refresh_token + provider_creds) + config

**v0.5.5 — Activity Timeline chart on Sessions tab** (4–5 h, REVISED scope vs v1 plan)
- v1 plan was wrong: `list_sessions` is a **current-process snapshot capped at 12** (`sessions.rs:294`,`L304-L312`), NOT a 24h history
- **Correct data source:** new PostgREST GET against the `sessions` table (mirrors v0.5.2 `get_sessions_since`). `sessions` table has real `started_at` + `last_active_at`
- New Tauri command `get_sessions_history(hours: u32)` returning rows from last N hours
- SVG chart: 6 lanes (one per provider), 24px each, 24h x-axis, color by provider
- Memo key: full session-id-and-timestamp join, NOT `sessions.length + sessions[0].last_active_at`

**v0.5.6 — Tray menu mini-metrics** (3–4 h, REDESIGNED vs v1 plan)
- v1 plan said `tray.set_menu(Some(rebuild()))` every 30s — **WRONG**: Tauri docs say Linux AppIndicator menus can't be removed/replaced after set; Win/Linux both dismiss the open context menu when replaced
- **Correct primitive:** keep `MenuItem` handles on the tray state; call `MenuItem::set_text()` to mutate in-place
- Refresh cadence: 120s (NOT 30s — see §3 P2 cache race below)
- Show `Month so far: $X.XX / Forecast: $Y.YY` (NOT today's cost — too small for daily-developer audience)
- React frontend's language-change handler must invoke a Tauri command to force-refresh tray text immediately (don't wait for next tick)

### 3) Reviewer findings — apply every one

The v0.5.4–v0.5.6 plan got Codex + Gemini 3.1 Pro reviews. Both found multiple P1s. Synthesized list:

**P1 (both reviewers caught):**
1. **Delete-account ordering**: RPC FIRST, then best-effort local clear. NEVER clear keychain before calling RPC — `with_user_jwt` reads `refresh_token` from keychain to mint the JWT (`lib.rs:688, L693-L698`). Clear-keychain-first → RPC has no auth → user thinks account deleted but server still has their data (Gemini: "massive trust/privacy violation").
2. **Tray live-rebuild wrong primitive**: see v0.5.6 spec above. Use `set_text()`, not `set_menu()` rebuild.
3. **Activity Timeline data source**: see v0.5.5 spec above. `list_sessions` ≠ 24h history.

**P2:**
4. **Tray refresh + main-app cache race** at 30s: my v1 plan claimed Overview also polls dashboard every 30s — that's WRONG (Codex confirmed: Overview only fetches on mount/paired-change; Providers tab is the 30s poller for `provider_summary`). Tray should read the existing 30s-TTL `DASHBOARD_CACHE` (`lib.rs:563, L573-L580`) without forcing a fresh fetch. Or: tray refresh is 120s, naturally avoids the race.
5. **Clear-caches scope underspecified**: split helpers explicitly. `clear_local_caches` = `cache_invalidate()` + `cache::wipe_all(None)`. `delete_account_and_unpair` = clear-caches + `keychain::delete_refresh_token()` + `provider_creds` wipe + `config::clear()`.
6. **SVG memo key for Activity Timeline**: `sessions.length + sessions[0]?.last_active_at` is leaky — non-first session updates miss. Use `sessions.map(s => `${s.id}-${s.last_active_at}`).join(',')` for ≤200 sessions.
7. **Tray menu i18n desync**: when user changes app language in Settings, frontend updates immediately but tray waits up to 120s. Fix: language-change handler calls `invoke("force_tray_menu_refresh")` to re-render tray immediately.

**Decisions on the 4 explicit Gemini review questions:**
- Type-to-confirm: KEEP literal `DELETE` typing requirement (dev-tool audience — friction is feature)
- Activity Timeline lane height: 24px, not 40px (240px chart total too chunky for desktop; 24px × 6 lanes ≈ 144–160px is right)
- Tray cost field: `Month so far: $X.XX / Forecast: $Y.YY`, NOT today's cost (today is fractions of a cent for individual devs — no signal)
- Confirmation pattern: see "type DELETE" above

**Decisions on the 3 explicit Codex review questions:**
- Delete-account ordering: RPC-first, then best-effort local clear. Refresh JWT before RPC, hold it through the call.
- Tray-state cache reuse: read existing `DASHBOARD_CACHE` directly. Do NOT add a second cache. Single-flight protect if tray's 120s tick coincides with cache miss.
- SVG vs Canvas for v0.5.5: SVG is fine. The bigger issue was the data-source mismatch — fix that first; perf isn't the threat.

### 4) Working pattern (matches the day's cadence)

For each of v0.5.4 / v0.5.5 / v0.5.6:

1. **Implement** code per spec + reviewer-findings list
2. **Pre-flight RPC / data shape if doing v0.5.5**: dump `sessions` table columns via Supabase MCP first; verify the PostgREST GET shape works for paired demo user JWT
3. **Bump versions** in 4 places: `tauri.conf.json`, `package.json`, `src-tauri/Cargo.toml` → then `npm install --package-lock-only --silent` + `cargo build --quiet` to refresh the 2 lock files
4. **Write CHANGELOG entry** at TOP of `CHANGELOG.md` matching v0.5.3's style. Cite reviewer-caught fixes inline.
5. **Run gates**: `cd src-tauri && cargo fmt --check && cargo clippy --lib -- -D warnings && cargo test --lib && cd .. && npm run build && npm run test`. All must pass.
6. **Gemini review** the per-ship diff via `git diff <files> | /opt/homebrew/bin/gemini -p "..."` per `reference_gemini_cli.md`. Apply any P1+P2 fixes before commit.
7. **Commit + tag + push**: `git tag vX.Y.Z && git push origin main && git push origin vX.Y.Z`. Pre-push hook re-runs gates locally.
8. **Watch CI** via Monitor — wait for Windows job conclusion. Don't poll manually.
9. **Promote** to Latest: `gh release edit vX.Y.Z --draft=false --prerelease=false --latest`. Verify NSIS URL returns 200.
10. **Move to next ship.** Don't skip ahead.

After v0.5.6 promotes: write a CONSOLIDATED VM verify prompt (Phase 1 update + 4 blocks: Danger Zone delete + clear-caches paths / Activity Timeline real-data render / Tray mini-metrics live-update / regression spot-checks). Format matches the v0.5.0+0.5.1+0.5.2 consolidated prompt the previous session sent (see git log).

### 5) Things you MUST NOT get wrong

- **Don't run `cargo fmt` (without `--check`) before commit unless `--check` complained.** The previous session's verified state assumes the formatter ran already.
- **Don't push back-to-back faster than CI completes.** `concurrency.cancel-in-progress: true` on Release workflow means a v0.5.5 push could cancel v0.5.4's CI. Wait for Monitor to fire success on each version before pushing the next tag.
- **Don't add backend RPCs / schema changes.** The user's autonomy contract requires explicit approval for those (per `feedback_desktop_autonomy.md`). All three ships above are local-Rust + frontend — no DDL.
- **Don't skip Gemini review per ship.** It caught P1+P2s on every single ship in the v0.5.x sprint.
- **Don't trust comments from prior code that say "polls every 30s"** — Codex caught the v0.5.4-6 plan asserting Overview polls every 30s, but the actual code only fetches on mount. **Always grep the actual code path, don't infer.**

### 6) When you're done

After v0.5.6 promoted to Latest:
1. Append a consolidated v0.5.4+0.5.5+0.5.6 entry to memory (`reference_desktop_repo.md` sprint history table, same format as the 2026-05-05 v0.4.x / v0.5.x summaries)
2. Write the VM verify prompt as a Chinese-prefixed code block, ready for Jason to copy-paste to a fresh VM Claude session
3. Stop. Don't pre-empt v0.5.7.

If at any point you find a v0.5.4-6 plan assumption is wrong (like the previous session found `list_sessions` ≠ 24h history mid-planning), STOP and surface it before continuing. Real failure mode of post-9-ship sessions is shipping plausible UI on top of wrong data assumptions.

### 7) Memory updates after ship

If anything novel comes up during implementation (a new tooling gotcha, a Tauri-2-specific quirk, a Supabase RLS edge case), save a feedback memory under `/Users/jason/.claude/projects/-Users-jason-Documents-cli-pulse/memory/` and add a one-line entry to `MEMORY.md`. See existing files for format. Don't save things already in code or commit history.

Good ship.

## ▲ END COPY-PASTE ABOVE THIS LINE ▲
