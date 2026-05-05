# Handoff prompt — v0.4.20 implementation (for the next local Mac Claude session)

Copy-paste the section below into a new Claude session on the Mac. Keep this file as the canonical handoff; don't paraphrase when copy-pasting.

---

## ▼ START COPY-PASTE BELOW THIS LINE ▼

You are taking over development of `cli-pulse-desktop` (Tauri 2 + Rust + React, Win+Linux desktop app at `/Users/jason/Documents/cli-pulse-desktop/`). The previous session just shipped v0.4.13 → v0.4.19 in two days (7 patches, all VM-verified). v0.4.20 is the next ship — plan + Gemini 3.1 Pro review are already written and waiting for you to implement.

The user (Jason) operates you autonomously. He's already approved this work batch. Don't ask permission to act unless you hit one of the 3 explicit exception categories below (backend schema, account-level, key rotation). Ship the code, then write a VM verification prompt at the end and hand it to him to forward.

### 1) Ground yourself — read these in order

These are required before you write code. Skim what you need; don't grind through every line.

**Auto-memory** (already in your context as `MEMORY.md` references):
- `feedback_desktop_autonomy.md` — your scope of authority on this repo, plus the 3 exception categories
- `feedback_vm_as_real_e2e.md` — Mac is host-managed Claude; real-world OAuth/file-shape testing must run on the Win VM
- `feedback_gemini_review_patterns.md` — the recurring catches Gemini surfaces; your plan has already been reviewed but this primes you for next time
- `feedback_github_secret_scanner.md` — `concat!()` workaround for OAuth literals
- `feedback_vm_indicator_testing.md` — time-based UI indicators can't be triggered by passive idle on a healthy VM
- `reference_gemini_oauth_refresh.md` — the v0.4.7-v0.4.12 Gemini refresh chain context
- `reference_desktop_repo.md` — repo location, stack, sprint history through v0.2.x (older but the architectural invariants near the top are still load-bearing)

**The plan + review** (created by previous session):
- `/Users/jason/Documents/cli-pulse-desktop/PROJECT_DEV_PLAN_2026-05-05_v0.4.20_health_visibility.md` — your spec. Read top to bottom. Items 1+2+3 with implementation sketches + Gemini's review fixes already merged in. The `Per Gemini 3.1 Pro v0.4.20 review:` annotations are not optional — they're correctness fixes Gemini caught.

**The recent ship history** (skim, don't read in full):
- `/Users/jason/Documents/cli-pulse-desktop/CHANGELOG.md` — top 6-7 entries are today's work; the patterns are clear from there.

**Files you'll touch** (read before editing each):
- `src-tauri/src/lib.rs` — Tauri commands, background sync loop (Item 1 lives here)
- `src-tauri/src/quota/mod.rs` — `collect_all` orchestrator, `PRE_EXPIRY_BUFFER_MS` constant (Item 2's `CollectorError` enum lives here)
- `src-tauri/src/quota/{claude,codex,cursor,gemini,copilot,openrouter}.rs` — six collector modules (Item 2 refactor: `collect()` returns `Result<Option<QuotaSnapshot>, CollectorError>` instead of `Option<QuotaSnapshot>`)
- `src-tauri/src/provider_creds.rs` — `current_backend()` getter (Item 3 needs to expose it via `ProviderCredsView`)
- `src/App.tsx` — `Providers` component (Item 2 UI badge), `IntegrationsSection` (Item 3 storage line), `ProviderCredsView` type
- `src/locales/{en,zh-CN,ja}.json` — i18n keys for new UI

### 2) Working pattern — match what was just shipped

Each release has the same shape:
1. Implement code on the items in the plan, in the order the plan specifies (Item 3 → Item 1 → Item 2 in this case — smallest first, refactor last).
2. Add unit tests for each item — the plan tells you what to assert. Aim to keep the test count growing release-to-release; v0.4.19 was at 151 backend + 44 frontend.
3. `cd src-tauri && cargo fmt && cargo clippy --lib -- -D warnings && cargo test --lib` — must all pass.
4. `cd .. && npm run build && npm run test` — must all pass.
5. Bump version in 4 places: `tauri.conf.json`, `package.json`, `src-tauri/Cargo.toml`. Then `npm install --package-lock-only --silent` to update `package-lock.json`, and `cd src-tauri && cargo build --quiet` to update `Cargo.lock`.
6. Add a CHANGELOG entry at the TOP of `CHANGELOG.md` matching the style of the v0.4.19 entry.
7. Commit with a HEREDOC message following the recent style (look at `git log --oneline -10`). Co-author line is `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.
8. Tag `v0.4.20` and push: `git tag v0.4.20 && git push origin main && git push origin v0.4.20`.
9. The pre-push hook runs CI gates locally. If it fails, fix and re-push.
10. Set up a Monitor watcher to be notified when the Windows-x64 build finishes (~12 min). The recipe is in the recent shell history — it polls `gh run view <run_id> --json jobs` for the windows-x64 conclusion. Don't watch Linux; user only cares about Windows for this VM.
11. When Windows finishes successfully, promote: `gh release edit v0.4.20 --draft=false --prerelease=false --latest`. Verify with `curl -sLI -A "Mozilla/5.0" "https://github.com/JasonYeYuhe/cli-pulse-desktop/releases/download/v0.4.20/CLI.Pulse_0.4.20_x64-setup.exe"` returning 200.

### 3) Pitfalls / things you MUST not get wrong

- **Pre-push hook runs rustfmt + clippy + tests + frontend build/test.** If you push with unformatted code, it fails locally before the push happens. Always `cargo fmt` before commit.
- **GitHub Push Protection blocks literal Google OAuth `<digits>-<word>.apps.googleusercontent.com` patterns.** v0.4.20 doesn't add new OAuth client IDs but if you DO touch any test fixtures with that shape, use `concat!()` to split the literal. See `feedback_github_secret_scanner.md`.
- **Backend schema changes need user approval.** v0.4.20 is local-only — this isn't an issue. But if you discover during implementation that you need a Supabase column or RPC change, STOP and flag to user. (You won't.)
- **The diagnostic backend visibility (Item 3) is purely client-side data already exposed via `provider_creds::current_backend()`. No backend RPC change.** The naive read of "expose this in UI" might tempt you toward a new RPC; don't.
- **Item 2 collector signature refactor:** when you change `pub async fn collect() -> Option<QuotaSnapshot>` to `Result<Option<QuotaSnapshot>, CollectorError>`, the `tokio::spawn` calls in `quota::mod.rs::collect_all` need to handle the new return type. The orchestrator should keep panicking-isolation semantics (`task.await.is_panic()` already handles that) but now also surface `Err` cases as `CollectorOutcome { error: Some(...) }`.
- **Don't run `cd <project> && git ...`** in shell commands — `git` already operates on the current working tree. Just use `git ...` from the project root or pass `-C <path>`.
- **Don't pre-empt the user.** Ship the code, do the VM-prompt step, then stop. Don't start guessing v0.4.21.

### 4) Tools at your disposal

- **Supabase MCP** is available (`mcp__9d150514-...` tools). Project ID `gkjwsxotmwrgqsvfijzs`, Tokyo region. v0.4.20 is local-only so you shouldn't need it.
- **Gemini 3.1 Pro CLI** at `/opt/homebrew/bin/gemini`. The plan has ALREADY been reviewed; you don't need to re-review. But if you make a non-trivial deviation from the plan during implementation (e.g. you find the proposed `mpsc::channel(1)` actually doesn't compose well with the existing loop and you need a different pattern), do a re-review. See `reference_gemini_cli.md` for invocation.
- **Background process control:** `Bash` with `run_in_background: true` for one-shot waits. `Monitor` tool for streaming events. Use the recipe pattern in the recent monitor invocations.
- **Claude in Chrome MCP** for browser-side things — not needed for v0.4.20.

### 5) When you're done

After v0.4.20 promoted to Latest, write a VM verification prompt and present it as a code block for the user to forward. Use the same shape as the v0.4.19 verification prompt the previous session sent (you can find it in the conversation history; the structure is: ⚠️ prep → 3 lettered blocks (A/B/C) for the 3 items → "report verbatim" instructions → PID + binary mtime + version confirmation).

The 3 blocks for v0.4.20:
- **A — MPSC tick-reset (Item 1)**: have VM click "Refresh now" then immediately `Get-Content` the log; verify the next background tick fires ~120s after the manual click time, NOT 120s after the previous tick. Specifically check for the new `[INFO] background tick reset by manual refresh during idle window` log line.
- **B — Per-provider error badge (Item 2)**: have VM corrupt `~/.gemini/oauth_creds.json` (rename to `.bak`, then `echo "{ broken json" > oauth_creds.json`), wait for next sync (or click Refresh now), confirm a red error badge appears on the Gemini card with parse-error reason. Then restore: `del oauth_creds.json && rename oauth_creds.json.bak oauth_creds.json`.
- **C — Settings storage line (Item 3)**: open Settings → Integrations, confirm there's a small "Storage: OS keychain" emerald label at the top. Take a screenshot.

Plus PID, binary mtime, version confirmation.

After you write the VM prompt, your turn ends. The user forwards it. The VM Claude reports back to the user, the user pastes the report into a NEW local Claude session, and that session does the next ship cycle.

### 6) Memory updates after ship

If anything novel comes up during implementation (a Gemini catch you hadn't seen before, a tokio gotcha, etc.), save a feedback memory under `/Users/jason/.claude/projects/-Users-jason-Documents-cli-pulse/memory/` and add a one-line entry to `MEMORY.md`. See the existing memory files for the format. Don't save things that are already in the code or commit history.

### 7) If something goes wrong

- **CI fails on the tag push:** `gh run view <id> --log-failed` to see the error. Fix locally, push amended commit + retag (delete old tag with `git tag -d` first).
- **Push blocked by Push Protection:** the secret scanner is firing on a literal you added. Use `concat!()` to split.
- **Build hits a Rust compile error you don't immediately understand:** `cargo build` (not check) sometimes surfaces clearer errors. If still stuck, ask the user — don't go off plan.
- **VM reports a PARTIAL or FAIL after promotion:** ship a v0.4.21 patch — same loop. Don't try to "fix in place" the released binary.

Good ship.

## ▲ END COPY-PASTE ABOVE THIS LINE ▲
