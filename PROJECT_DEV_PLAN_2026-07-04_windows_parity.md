# CLI Pulse Desktop — Windows/Linux Parity Dev Plan (2026-07-04)

**Goal:** bring `cli-pulse-desktop` (Tauri 2 · Rust + React) from **v0.10.0** to feature parity
and wire-format compatibility with the macOS/iOS app **`cli-pulse-private` v1.38.0**, from first
principles, shipped incrementally one version per slice.

**Method:** this plan is the output of a 10-reader cross-repo gap analysis (each Mac feature area
v1.30→v1.38 cross-referenced against the desktop's current modules), a synthesis pass, and an
adversarial completeness critic — then hand-corrected against **live prod state** (Supabase
`list_migrations`) and a **current GitHub Actions billing check**. Findings marked ⚠️ below are
corrections to the raw synthesis / to the handoff's own assumptions.

---

## 0. Where the desktop actually stands (v0.10.0, verified 2026-07-04)

The stale memory (`reference_desktop_repo`, 56 days old) undersold the current state. Verified today:

- **5 tabs:** Overview / Providers / Sessions / Alerts / Settings. **41 Tauri commands.**
- **282 backend tests** (Rust) + **54 frontend** (Vitest). *(CHANGELOG claims 285/57 — a ~3-test
  doc drift; source count is authoritative. Fix the CHANGELOG numbers opportunistically.)*
- **Scanner/pricing is already bit-exact** with Mac for the whole cost algorithm (per-message
  Claude cost, 200K tiered threshold, `__claude_msg__` bucket, token dedup by
  `(message.id,requestId)`, Codex cumulative delta, `cost_nanos/1e9` fallback). **One drift only**
  (family-fallback pricing, see v0.11.1).
- **6 quota collectors** live (Claude, Codex, Cursor, Gemini, Copilot, OpenRouter) with OAuth
  refresh for Claude+Gemini; concurrent with panic isolation.
- **ConPTY managed-session host is LIVE** (re-landed v0.9.2 after the v0.8.0 BEX64 crash + v0.8.1
  revert). `ConPtyTransport` (portable-pty) + `RemoteAgentManager` 1s tick loop + per-row
  Send/Stop/Interrupt. **But:** `read_stdout()` intentionally returns empty (lifecycle-events only —
  no output stream yet); managed sessions accept **`provider=claude` only** (Codex/shell rejected).
- **Remote Approvals** (view+decide, 3-layer high-risk fail-closed, adaptive polling) + **Windows
  permission-hook emission** (`bin/remote_hook.rs`: risk classifier + secret redaction + cwd HMAC).
- Overview parity cards (cost forecast, risk signals, top projects, 7-day trend, activity timeline),
  tray mini-metrics, crash-recovery circuit breaker, Sentry sync-flush, diagnostic bundle, OTP
  sign-in, keyboard shortcuts.
- **RC health / recovery diagnostics are already at/ahead of Mac** — `AboutSection` polls
  `diagnostic_snapshot`+`agent_diagnostic` every 5s, has the diagnostic bundle, Sentry test-event,
  hook installer. **Not a gap.**

**What the desktop is missing vs Mac v1.38** falls into three shapes:
1. **Cheap client-only ports** where the data already flows on desktop (pace text, threshold ticks,
   30-day per-provider chart, service-status badges, provider search, family-fallback pricing).
2. **Additive device-authed reads/writes** against a shared Supabase whose target migrations are
   **already applied or additive-inert** — no schema change needed.
3. **Two XL epics** that are the real weight — the in-app xterm.js terminal and the R0 realtime
   terminal.

---

## 1. ⚠️ Two corrections to the handoff's premises (verified today)

### 1a. The ARM "cost leak" is NOT a billing issue — the handoff's claim is stale.
`ci.yml` lines 33 & 95 run `ubuntu-24.04-arm` + `windows-11-arm` on every push/PR. The handoff (and
the raw synthesis) call this a billed "larger-runner" leak. **That is no longer true:** as of
GitHub's **GA on 2025-08-07**, `windows-11-arm` and `ubuntu-24.04-arm` are **standard 4-vCPU
runners that are FREE on public repos**. Only *larger*/custom-label ARM runners are billed. So there
is **$0 leak**. ([GA changelog](https://github.blog/changelog/2025-08-07-arm64-hosted-runners-for-public-repositories-are-now-generally-available/))

**What's actually true:** we ship **x64 only** (release matrix dropped ARM 2026-05-04; **0 ARM
downloads across 31 releases**), so building+testing ARM on *every* push is **redundant CI
wall-clock**, not money. **Recommendation:** keep `ubuntu-24.04-arm` for cheap cross-arch coverage
(catches endianness/`usize`/alignment bugs); optionally drop `windows-11-arm` from the *build* leg to
save CI minutes. **Low priority, do NOT bill it as a cost fix, and keep ARM out of the release
matrix regardless.** *(This correction matters: presenting a false billing claim to the owner or in
review would be a trust error.)*
- **Repo disambiguation (a reviewer tripped on this):** the `ci.yml` in question is in
  **`cli-pulse-desktop`, which is confirmed PUBLIC** (`gh repo view … --json visibility` →
  `PUBLIC`, 2026-07-04). The `REPO_VISIBILITY_STRATEGY.md` doc lives only in the **Mac** repo
  (`cli-pulse-private`) and is about that product's IP posture — it does **not** govern the desktop
  repo's CI. Both plan reviewers (Codex, Gemini 3.1 Pro) independently re-confirmed §1a; the ARM
  runners are free here.

### 1b. R0 realtime server side is live *infra* but the private path is **owner-gated OFF**.
`list_migrations` (prod, 2026-07-04) confirmed `v0.56` + `v0.61` realtime-terminal-authz **applied**,
but **`v0.65` r0_token_least_privilege NOT yet applied** (matches `feedback_f1_f8_hardening`:
"migrate_v0.65 NOT applied; custom-role E2E gate before cutover") and `realtime_private_enabled` is
**OFF in prod** with `mint-realtime-token` issuing **no live tokens**, pending the owner-run S7
cutover. So a desktop R0 host would broadcast to a channel **no viewer can privately join yet**.
⚠️ **This is a moving target (Codex note):** re-verify the live migration + `realtime_private_enabled`
state at the moment you start R0 implementation rather than depending on "v0.65 not applied" staying
true — the Mac R0 cutover is actively in progress (July 3/4 handoffs). Regardless of when it flips,
R0 work must be **phased behind the Mac cutover** and ship **default-OFF with proven byte-identical
no-broadcast behavior** (see v0.25.0).

---

## 2. Server capability ledger (prod-verified, no schema change needed)

Every migration the parity features depend on is **already applied and additive-inert** (ADD COLUMN
nullable / jsonb param `default null`), so the desktop can adopt them as a **device** without a
schema change (avoids ask-first gate #1):

| Capability | Migration | Prod applied? | Desktop use |
|---|---|---|---|
| `helper_heartbeat` (cpu/mem/plan_status/metrics) | v0.63/prior | ✅ | v0.17.0 (currently dead wrapper) |
| `device_sensors` columns (19) | v0.63 (2026-07-04) | ✅ | v0.18.0 system-monitor sink |
| `p_metrics` size guard (≤8192 B) | v0.64 (2026-07-04) | ✅ | v0.18.0 guardrail |
| `devices.provider_plan_status` | v0.60 (2026-07-01) | ✅ | v0.17.0 read / v0.24.0 write |
| realtime terminal authz + private start payload | v0.56 / v0.61 | ✅ | v0.25.0 host |
| **r0 token least-privilege** | **v0.65** | ❌ **pending** | **blocks R0 cutover** |
| remote swarms / swarm alerts | v0.48 / v0.49 | ✅ | v0.19.0 viewer read |
| remote input raw / tail snapshot | v0.50 / v0.51 | ✅ | terminal epics |
| desktop OTP / daily_usage device_id | v0.36 / v0.37 | ✅ | already used |

---

## 3. Prioritized roadmap (value ÷ effort)

One theme per version, starting from v0.11.0. Effort: S<M<L<XL. Every wire-touching slice is pinned
to a byte-identical invariant so a paired account reads identically on both platforms.

### Phase 0 — Infra (enabling; do first)

**v0.11.0 — Automated Windows GUI launch-smoke CI job** · effort **M** · the #1 enabling task.
- On `windows-latest` (WebView2 preinstalled): build the app, launch it in a **`--smoke` env mode**
  where the React app calls a Tauri command on mount that writes a `frontend-ready` marker file, then
  the job **asserts**: (1) process still alive after N s (catches v0.8.0 `STATUS_STACK_BUFFER_OVERRUN`
  crash-on-launch); (2) the exe is the **GUI** binary not a sidecar (catches the v0.2.10 `default-run`
  wrong-binary class — a `scan_cli` exits instantly); (3) a top-level "CLI Pulse" window exists
  (P/Invoke `EnumWindows`); (4) frontend mounted (the marker — catches the v0.2.11 white-screen
  class). Capture a `CopyFromScreen` screenshot → upload artifact.
- **Why the marker-file approach, not tauri-driver/WebDriver:** WebDriver-for-WebView2 is flaky in CI;
  the handoff's marker pattern is deterministic and matches how `scripts/vm-smoke-full.ps1` already
  proves launch on the VM. This job **is** how we satisfy the mandatory pre-publish smoke gate without
  a human most of the time.
- Also add a **Linux** launch-smoke on `ubuntu-latest` under `xvfb-run` (WebKitGTK) for the
  `.AppImage`.
- ⚠️ **CI ARM:** optional wall-clock trim only (§1a) — **not** scheduled as a cost fix.
- **Wire invariant:** none (CI/tooling). Gate merges on the process-survival exit code; keep the
  screenshot-render assertion advisory at first if headless GPU render is flaky.

### Phase 1 — Trust / correctness (cheap, high value)

**v0.11.1 — Claude family-fallback pricing** · effort **S** · highest value÷effort.
- `pricing.rs::normalize_claude_model` has **no** family-fallback step. When a new Claude minor
  (e.g. `claude-opus-4-8`) isn't in the table it prices to **$0**, collapsing Today/Week cost + every
  chart. Mac added this at `CostUsageScanner.swift:587-632`.
- Port `fn family_fallback(model)`: regex `^claude-(opus|sonnet|haiku)-(\d+)-(\d+)$`, build family
  stem, iterate `CLAUDE_MODELS` keys with that prefix, keep only **minor < 100** (so a legacy
  date-as-minor `claude-sonnet-4-20250514` can't win), return the max-minor sibling; **exact-match
  check MUST precede fallback**; carry the sibling's 200K-tiered fields through.
- **Wire invariant:** for any `M` not in the table, desktop `claude_cost_usd(M,…)` == Mac
  `claudeCostUSD(M,…)`. Add a fixture test with an unknown future minor.

### Phase 2 — v1.30 pace + charts (client-only, data already in hand)

**v0.12.0 — Provider pace text + warning-threshold tick markers** · effort **M**.
- Port the pure-Foundation pace engine: pace arrow/percent (`▲/▼/≈` + integer %), stage thresholds
  `±2/±6/±12`, `compactCountdown` (`d/h/m`) — output byte-identical to Mac. Threshold ticks are
  **REMAINING-oriented**: an 80%-used line sits at `left:20%` (`1−f`), matching Mac
  `onRemainingBar:true`. Lenient ISO-8601 `reset_time` parse (not strict chrono); gate pace to
  Codex/Claude with a future `reset_time` (`None` → absent).
- ⚠️ The **expected-pace tick** (F2b) that needs `window_minutes` plumbing is **ask-first** (§5) —
  ship the warning-threshold ticks (F2a) here; F2b follows under whichever path the owner picks.

**v0.13.0 — Per-provider 30-day I/O token history chart** · effort **M**.
- `get_daily_usage` is already wired+cached and SVG bar charts already exist — this is purely a new
  per-provider view over data in hand. 30-day input+output bars, gap-filled, empty state.
- **Wire invariant:** `ioTokens = input + output` **EXCLUDING cached_tokens** (add cache reads and
  magnitudes diverge from Mac); bucket on **LOCAL `yyyy-MM-dd`** via `today_key`/`localTodayKey`,
  **never raw UTC** (the recurring dashboard TZ trap); align gap-fill to `scan.today_key`.

**v0.14.0 — Provider service-status badges + provider search box** · effort **M**.
- Atlassian Statuspage v2 catalog (12 providers), lazy per-badge fetch with client TTL cache
  (no burst of 12 GETs, no global timer), `Option/None` graceful-fail so a moved status page → no
  badge, not an error. Search = trivial client filter on the display provider string.
- ⚠️ **Sequencing (critic LOW):** a badge only renders for a *configured* provider, so 8 of the 12
  catalog entries stay dark until their collectors land in v0.15–v0.20. **Trim the desktop catalog to
  currently-supported providers** and grow it with each collector batch — don't ship 12 dark badges.

### Phase 3 — Provider/quota expansion (one-per-PR, Mac Phase-C discipline)

Each new provider's `provider` string **MUST equal the Mac `ProviderKind` rawValue byte-for-byte**
(casing/dots/spaces) or the dual-writer forks the `(user_id, provider)` PK. Enforce via the
`MAC_PROVIDER_KIND_SNAPSHOT` contract test (`quota/mod.rs:264`). New `ProviderKind` ⇒ exhaustive-arm
updates (icon/enum switches). `#[serde(default)] Option<>` on all response fields.

**v0.15.0-prep — refresh the provider-name contract snapshot** · effort **S** (folded into v0.15.0).
- ⚠️ **Codex P2:** `MAC_PROVIDER_KIND_SNAPSHOT` (`quota/mod.rs:257`) still lists only the 6 live
  providers; the Mac v1.38 `ProviderKind` enum (`Models.swift:52`) has the full set. **Refresh the
  snapshot to the full Mac enum before the first provider batch** so every new collector's literal is
  validated against the real contract, not a stale 6-entry list.

**v0.15.0 — API-key batch 1** · effort **M** · `z.ai`, `GLM` (智谱), `DeepSeek`, `Crof` — high
CN/global popularity, pure api-key HTTPS on the `openrouter.rs` pattern. *(DeepSeek balances are
STRING-encoded → parse defensively; Crof reset is America/Chicago-midnight, DST-safe truncate.)*

**v0.16.0 — API-key/credit + status batch 2** · effort **M** · `Moonshot`, `Groq` (status-only),
`ElevenLabs` (`xi-api-key` header, not Bearer), `MiniMax`, `Alibaba`, `Warp`, `Kilo`, `Amp`,
`Ollama` (localhost). Status-only providers still round-trip a `QuotaSnapshot`.

**v0.16.1 — ⚠️ API-key batch 3 (critic HIGH — 7 collectors the synthesis dropped)** · effort **M**.
- The synthesis silently omitted 7 real, portable, pure-api-key Mac collectors from **both** the
  schedule and the deferred list: **`Venice`** (Bearer), **`Azure OpenAI`** (api-key header),
  **`Codebuff`** (Bearer `CODEBUFF_API_KEY`), **`Deepgram`** (`Token DEEPGRAM_API_KEY`),
  **`LLM Proxy`** (Bearer), **`OpenAI Admin`** (Bearer `OPENAI_ADMIN_KEY`). Ship these here on the
  same pattern.
- ⚠️ **+ `Volcano Engine` (火山引擎) — Codex P1:** the synthesis wrongly *deferred* this as "needs a
  helper backend"; the Mac collector (`VolcanoEngineCollector.swift:31`) is **pure api-key**
  (`config.apiKey` or `ARK_API_KEY` env), fully portable — schedule it in this batch, not deferred.
- **`AWS Bedrock`** (SigV4 env creds) gets **its own slice v0.16.2** — SigV4 signing is heavier than
  a Bearer header.

**v0.20.0 — Cookie/manual-paste collectors** · effort **L** · `Mistral`, `Perplexity`, `Kimi`,
`Kimi K2`, `Manus`, `Abacus AI`, `Command Code`, `StepFun`, `T3 Chat`, `MiMo`, `Alibaba Token Plan`,
`Windsurf`, `OpenCode Go`, `Grok`, **`Augment`** (⚠️ Codex P1 — `AUGMENT_COOKIE` env or manual cookie
for `augmentcode.com`; portable, was wrongly deferred). HTTP ports cross-platform via
**manually-pasted session cookies/env** (only macOS `SweetCookieKit` auto-import is unavailable) —
ship manual-cookie with clear help text + expiry surfaced. *(Reuse `openrouter.rs` `$1=100_000` credit SCALE for
Perplexity; Windsurf/Grok/OpenCodeGo need protobuf/gRPC-web/HTML parsing — heavier.)*

**v0.21.0 — Vertex AI** · effort **M** · gcloud ADC + `oauth2` refresh + Cloud Monitoring, modeled on
the existing `gemini_refresh.rs` token machinery. `once_cell`/`Mutex` token cache (don't hammer
`oauth2.googleapis.com` per 120s tick); reject service-account ADC with a clear error; honor
`CLOUDSDK_CONFIG` (`%APPDATA%/gcloud` vs `~/.config/gcloud`).

*(⚠️ Desktop-deferred provider group (Codex P1 — these are NOT Mac-deferred; Mac v1.38 ships them):
JetBrains AI, Kiro, OpenCode, Droid/Factory, Antigravity, Synthetic — deferred **on desktop** for
per-collector portability reasons (OS-specific credential-store / config-dir scraping, subprocess
CLIs), NOT because Mac lacks them. Lowest ROI; do the ~35 api-key/cookie providers first. See §6 for
the honest per-collector rationale. `Augment` + `Volcano Engine` were wrongly in this bucket — now
scheduled in v0.20.0 / v0.16.1.)*

### Phase 4 — Backend adopt (device-authed, no schema change)

**v0.17.0 — Adopt `helper_heartbeat` + whole-device CPU%/mem% + cross-device read-back** · effort **M**.
- Wire the currently-dead `helper_heartbeat` wrapper (`supabase.rs:1163`, 0 callers) with real
  whole-device load (`sysinfo` global CPU + used/total mem, no privileges); add a read-only
  cross-device health/plan-status card off a `devices` read (user JWT).
- ⚠️ **Coordination (critic LOW):** `helper_sync` *already* writes `status=Online,last_seen_at=now()`
  every 120s tick; `helper_heartbeat` *also* writes status/cpu/mem/last_seen. Two RPCs touch the row
  per tick — benign (both `now()`) but **intentional**: ride heartbeat on the same 120s tick, do NOT
  drop the existing `helper_sync` Online-write.
- **Wire invariant:** trailing `p_provider_plan_status`/`p_metrics` MUST be
  `Option + skip_serializing_if=None` so server per-field coalesce preserves last-known (NULL ≠ {});
  CPU/mem stay `i32 0..100`; every read-back sensor column `Option + #[serde(default)]` (all
  nullable); parse `provider_plan_status` defensively (unknown → no warning).

**v0.18.0 — System Monitor: local "Machine" tab (the user-facing cockpit)** · effort **L**.
- ⚠️ **Corrected scope (Gemini P1):** the Mac System Monitor (`DEV_PLAN_2026-07-04_system_monitor.md`
  Phase 1, lines 90–108) is **fundamentally a LOCAL UI feature** — an Activity-Monitor-style
  "Machine" tab with **CPU/mem/temp/fan/power gauges, a battery-health card, and a live top-N
  process table** — with per-process rows kept **LOCAL, deliberately NOT synced** to Supabase
  (lines 150–151: "do NOT sync hundreds of rows"). The original v0.18.0 scoped only the backend
  sync half and **missed the entire user-facing cockpit** — the higher-value half.
- **Desktop is well-positioned:** `sessions.rs` already enumerates processes (CPU/mem) and `sysinfo`
  already reads global CPU/mem — a top-N **all-process** table + gauges are a natural extension.
- **Build:** add a **6th tab "Machine"** (`TabKey` + one exhaustive-switch arm, `Ctrl/Cmd+6`) — on
  desktop this is trivial vs the Mac's `AppState.Tab` exhaustive-switch tax. Gauges for CPU/mem +
  temps (`sysinfo` `Components`: hwmon on Linux, WMI/LibreHardwareMonitor on Windows) + battery card
  (`starship-battery`) + a live top-N process list (name/pid/%CPU/RSS, ranked, refresh ~2s).
- **Wire invariant:** **NONE — this slice is LOCAL-only** (no Supabase write); per-process rows never
  leave the device (privacy + volume). Ranked %CPU must divide by core count so `total ≤ 100%`
  (the Mac plan's per-process share note). Capability map truthful: show "—"/"not available" when
  `sysinfo` `Components` is empty (Windows VM/headless) rather than fabricating a temp.

**v0.18.1 — System Monitor: sensor metrics sync into `p_metrics`** · effort **M**.
- The cross-device half: mirror the enriched snapshot (temps/battery/thermal-ish) to the phone via
  the **already-applied** `helper_heartbeat → device_sensors` path (v0.63/v0.64) — capability-gated,
  ship only the subset the platform can read. Rides the v0.17.0 heartbeat tick.
- **Wire invariant:** send ONLY v0.63 schema keys; `battery_state` MUST be exactly
  `charging|discharging|charged|none|unknown` (map `starship-battery::State` explicitly — a raw
  `Full` variant is silently dropped; the #1 trap); temps are °C reals dropped outside `-40..150`;
  **OMIT `thermal_state`** (no desktop analog — don't invent); capability map must be **truthful**
  (never fabricate a temp when `Components` is empty on a Windows VM); blob ≤ 8192 B or v0.64
  ignores the whole block. Per-process rows are **NOT** synced (v0.18.0 keeps them local).

### Phase 5 — Trust / first-run

**v0.19.0 — Onboarding wizard + account card + swarm viewer** · effort **M**.
- First-launch wizard (Welcome + privacy disclosure → hand off to existing OTP), account card
  name/email + hide-personal-info toggle (also redacts the diagnostic-bundle path), and an additive
  read-only **swarm rollup viewer** (`remote_app_list_swarms()`).
- ⚠️ **Swarm asymmetry (critic MEDIUM):** the desktop can **consume** swarm rows but cannot
  **produce** them — the producer is the Mac's unsandboxed helper doing git-worktree grouping +
  blocked-approval rollup (`remote_helper_swarm_heartbeat`, v0.48), which the desktop's `sessions.rs`
  has no equivalent of. **Document this as a known asymmetry:** agents a user runs *on the desktop*
  won't appear in their Swarm view until a desktop-side worktree rollup producer is built (a possible
  later slice, not scheduled now).
- **Wire invariant:** every onboarding step needs an escape; privacy copy must match the desktop's
  ACTUAL data flow (local scan + Supabase sync); new `ConfigView` fields `Option<String>` + serde
  default; swarm payload stays opaque `serde_json::Value`; RPC returns `[]` (not error) when
  remote_control disabled; capture `RemoteSession.realtime_private` as `Option<bool>` serde default
  (absent → public `term:`).

### Phase 6 — Managed on-plan (shared track — align to Mac's contract)

**v0.24.0 — Managed Codex on-plan: env-scrub spawn + `auth_mode` + plan-status heartbeat** · effort **L**.
- ⚠️ **Depends on v0.22.0 (Codex P1):** the Mac on-plan plan assumes managed sessions already spawn +
  stream; the desktop today gates managed sessions to **Claude-only** (`agent.rs:300`) with an
  empty-output transport. So v0.22.0 (output stream + un-gate Codex spawn) is a **hard prerequisite** —
  v0.24.0 adds only the env-scrub/auth/plan-status layer on top. (Alternatively, the non-user-facing
  env/auth groundwork here could land early, but the *user-facing* managed Codex needs v0.22.)
- Make a desktop-hosted managed Codex run on the user's **ChatGPT plan** (not billed API) and
  honestly report `on_plan`/`off_plan` to phones. Compute `auth_mode` (chatgpt vs apikey);
  `env_remove OPENAI_API_KEY` **post parent-merge** + `CODEX_HOME` pin, **gated on verified chatgpt**;
  report `devices.provider_plan_status`.
- ⚠️ **Also lift the "provider=claude only" restriction** on managed sessions (from §0) so Codex can
  actually be hosted — this is the desktop precondition for on-plan Codex.
- ⚠️ **Managed-Claude auth check (critic LOW):** Mac needed `ClaudeOAuthInjector`
  (`CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR` fd injection) because macOS keychain/ACL read fails in a
  spawned GUI session. This is **probably N/A** on Win/Linux (Claude Code reads file-based
  `~/.claude/.credentials.json` directly) — but **add a verification checkpoint** in the terminal
  epics (v0.22/v0.23): confirm a managed Claude session spawns authenticated; if not, port the fd seam.
- **Wire invariant:** values MUST be exact `on_plan`/`off_plan` (server whitelists these; `unknown`
  = ABSENCE of the key); lowercase provider keys `codex`/`gemini` matching the mobile picker;
  `p_provider_plan_status` **before** `p_metrics`, both defaulted; scrub ONLY under verified chatgpt
  (else you break intentional apikey billing); **report `on_plan` ONLY if the desktop actually
  scrubs** (no lying signal).
- ⚠️ **Gemini-on-plan is ask-first** (§5) — `agy` is macOS/homebrew-only; the Win/Linux mechanism is
  an owner decision. Codex-on-plan ships without it.

### Phase 7 — In-app terminal epic (the headline v1.31–v1.34 feature)

**v0.22.0 — Terminal foundations: output stream + raw stdin + resize (no UI yet)** · effort **L**.
- The load-bearing plumbing: a real output path (the reader thread currently drains-and-discards),
  verbatim keystroke stdin, live `TIOCSWINSZ` resize — de-risked and independently testable before
  the UI lands.
- **Wire invariant (local-only):** output sender **non-blocking/bounded drop-oldest** so a slow UI
  never stalls the child (no PTY backpressure); apply `redaction.rs` at the write seam
  (defense-in-depth); do NOT route local output through `EventPoster` (that's the DB/remote 4 KB
  path); raw stdin verbatim (no `\n`→`\r` mangle), ~1 MB decoded cap, per-session serial write
  ordering; resize clamp `1..32767` failure-soft; restructure the master handle into
  `Arc<Mutex<>>` so reader+resize share it.

**v0.23.0 — In-app terminal: xterm.js UI + local spawn/detach lifecycle + reattach snapshot** · effort **XL**.
- Drop xterm.js into the WebView over the Phase-1 plumbing; a synchronous local spawn/detach path that
  bypasses the Supabase command round-trip (`LocalSessionController` + Settings toggle); reattach
  paint via a ~64 KB per-session output ring; Ctrl-C rides raw stdin `0x03` for free.
- ⚠️ **N/A vs Mac:** no DEVID-unsandbox/entitlement/container-migration work — Tauri Win/Linux builds
  are unsandboxed and spawn PTYs freely; the desktop "gate" is just a Settings toggle. SmartScreen
  reputation is handled by the existing signing pipeline.
- **Wire invariant:** local + DB spawn paths MUST share **ONE** session map (else double-count/orphan)
  + a max-age/idle cap (unbounded API-spend guard); reattach **subscribes to the output channel FIRST
  then fetches the snapshot** (race-safe); ring guarded by a `Mutex`, redacted; if a local session
  registers cross-device, reuse `remote_helper_register_session` so `cwd_basename`/`cwd_hmac` privacy
  posture stays wire-compatible; `provider` stays a `String`.

### Phase 8 — R0 realtime terminal (largest/riskiest; phased, default-OFF, cutover-gated)

**v0.25.0 — R0 HOST producer over Supabase broadcast (default-OFF)** · effort **XL**.
- Ship the HOST half FIRST (pure `reqwest` HTTP POST to `/broadcast` — no websocket) so phones can
  live-tail desktop sessions. Per-session ES256 mint-realtime-token; cloud `tail_snapshot`
  dispatcher for viewer warm-resume.
- ⚠️ **Acceptance criterion, not just an invariant (critic MEDIUM + §1b):** server RLS/RPC/edge-fn
  infra is live (v0.56/v0.61) **but the private path is owner-gated OFF and `mint-realtime-token` is
  inert until the S7 cutover (v0.65 pending)**. So the desktop producer MUST be **default-OFF** and
  parse the start-payload `realtime_private` as **`Option<bool>`, broadcasting ONLY on `Some(true)`**
  (Codex P1: a plain `#[serde(default)] bool` conflates absent/unknown with explicit-public and
  loses the fail-closed distinction — Mac's `remote_agent.py:1708` broadcasts only when
  `realtime_private is True`), with **proven byte-identical no-broadcast behavior** when `None`/
  `Some(false)`. Do not build against a moving token contract — stay **agnostic to token internals**
  so a v0.65 custom-role change can't break the producer. **Sequence this after the Mac R0 cutover
  lands.**
- **Wire invariant:** redact BEFORE bytes reach the sink; `data_b64` = **std** base64 (not url-safe);
  topic lowercase `pterm:`+`session_id` (matches `r0_session_id` binding); event ∈
  `{stdout,stderr,tail_snapshot_result}`; `"private":true` per message; proactive token refresh
  ~45 min + requeue-once on 401 (never reactive-only, never log the token); **output-only** channel.

**v0.26.0 — R0 VIEWER (Phoenix websocket) — SCOPE-DECISION GATED** · effort **XL**.
- The optional second half: a net-new Rust Supabase Realtime (Phoenix websocket) client + a
  desktop-as-viewer live-tail panel. **No desktop websocket precedent.** ⚠️ **Only build if product
  wants desktop-as-viewer** — host-only (v0.25.0) may fully satisfy v1.37 parity (a dev's *viewers*
  are their phone/Mac). **Flagged as a scope question (§5), not auto-scheduled.**
- **Wire invariant (if built):** `access_token` is a **sibling of `config` INSIDE** the `phx_join`
  payload (misplacement silently blackholes); re-send `access_token` on ~1 h JWT refresh;
  `private/public` topic must match `realtime_private` (`pterm:` vs `term:`) and rejoin on change;
  join-rejection is FATAL with ONE retry (not a reconnect storm); decoder hardening (event allowlist
  + byte cap + `session_id` cross-check); the viewer authenticates with its **own** GoTrue login JWT,
  not the host's minted token.

---

## 4. Suggested sequence (dependency + value order)

```
v0.11.0  Launch-smoke CI ...................... INFRA (unblocks safe shipping)
v0.11.1  Family-fallback pricing .............. TRUST (S, ship immediately)
v0.12.0  Pace text + threshold ticks .......... v1.30 (M)
v0.13.0  30-day per-provider chart ............ v1.30 (M)
v0.15.0  API-key batch 1 + refresh MAC_PROVIDER_KIND_SNAPSHOT to full Mac enum FIRST (Codex P2)
v0.16.x  API-key batches 2+3 (+7 critic, + Volcano Engine ARK_API_KEY) + Bedrock (SigV4)
v0.14.0  Service-status badges + search ....... ⚠️ AFTER collectors (badge needs a configured provider)
v0.17.0  helper_heartbeat + device read-back ... backend adopt (M)
v0.18.0  System Monitor: LOCAL "Machine" tab ... v1.38 (L)  ⚠️ local UI + top-N process table
v0.18.1  System Monitor: sensor sync .......... v1.38 (M)  rides v0.17 heartbeat
v0.19.0  Onboarding + account + swarm viewer ... first-run (M)
v0.20.0  Cookie/manual-paste collectors (+ Augment)
v0.21.0  Vertex AI ............................. (M)
v0.22.0  Terminal foundations ................. v1.31-34 (L)  ⚠️ output stream + un-gate Codex spawn
v0.24.0  Managed Codex on-plan ................ v1.35-36 (L)  ⚠️ DEPENDS on v0.22 (spawn+stream)
v0.23.0  Terminal xterm.js UI ................. v1.31-34 (XL)
v0.25.0  R0 host producer (default-OFF) ....... v1.37 (XL)  [AFTER Mac R0 cutover]
v0.26.0  R0 viewer websocket .................. v1.37 (XL)  [SCOPE-GATED]
```
⚠️ **Reorder from reviews:** (1) terminal foundations **v0.22.0 now precedes v0.24.0** — managed
Codex on-plan needs the desktop to actually *spawn Codex as a managed session* (today `agent.rs:300`
gates to Claude-only) and *stream output* (today the transport drains-and-discards), which v0.22.0
delivers (Codex P1). (2) Service-status **v0.14.0 moves after the collector batches** — a badge only
lights up for a configured provider (Gemini P2). Numbers are indicative; batches may split further
under review. Every ship: implement → `cargo fmt`/`clippy`/tests green → **CI + launch-smoke green**
→ Codex/Gemini diff review → tag → prerelease → smoke-verify launch+mount → promote `--latest`.
`PROJECT_FIX_*.md` per real bug.

---

## 5. ⚠️ ASK-FIRST / BLOCKED (do NOT build without an explicit owner decision)

Per the autonomy contract's three gates (shared-schema change / paid+account action / updater-pubkey
rotation) plus product-scope calls:

1. **F2b expected-pace tick, server path** — plumbing `window_minutes`/`role` through
   `provider_quotas.tiers` is a **cross-repo shared-Supabase schema change** (touches the Mac upload
   path, `provider_summary`, every collector). **GATE = schema change (yes needed).** A client-side
   static window-minutes lookup (PATH A) needs no server change but hidden-couples to Mac collector
   constants — recommend shipping F2a ticks first, treat F2b as a follow-up under the owner's chosen
   path.
2. **Gemini-on-plan managed spawn on Win/Linux** — `agy` is macOS/homebrew-only; no cross-platform
   equivalent. **GATE = owner design decision** on the Win/Linux mechanism (bare `gemini` + user
   OAuth, or defer). Do NOT invent an `agy` dependency. *(Codex-on-plan in v0.24.0 is unblocked.)*
3. **Team management (create/invite/roles)** — server RPCs exist, but the visibility gate is a
   StoreKit/mobile-store **Pro entitlement** (`isProOrAbove`) the Tauri desktop has no equivalent of;
   would need the server to expose an account-tier flag + a "how does a desktop user become Pro"
   answer. **GATE = owner/product + likely a new entitlement surface.** Building now risks dead
   paid-feature UI.
4. **R0 VIEWER scope (v0.26.0)** — is desktop-as-viewer even wanted, or does host-only (v0.25.0)
   satisfy v1.37 parity? **GATE = product scope decision** (host-only avoids the entire net-new
   Phoenix-websocket effort).
5. **R0 host timing** — do not flip R0 host to non-default until the **Mac R0 S7 cutover** (v0.65
   apply + `realtime_private_enabled` on) is done. Not a build blocker (build default-OFF), but a
   promote blocker.

---

## 6. Deferred / Not-applicable (with reasons — kept on the ledger, not silently dropped)

- **Watch "—" micro-copy** — N/A (no watch surface; desktop already renders honest empty/zero).
- **DEVID-unsandbox / entitlement / container-migration** — N/A (Tauri Win/Linux is unsandboxed).
- **RC health / recovery diagnostics** — ALREADY DONE, at/ahead of Mac (§0).
- **Scanner model tables + cost algorithm** — ALREADY BIT-EXACT; family-fallback (v0.11.1) was the
  sole drift.
- **Provider group JetBrains AI / Kiro / OpenCode / Droid(Factory) / Antigravity / Synthetic** —
  ⚠️ **desktop-deferred, NOT Mac-deferred (Codex P1 corrected a false claim):** Mac v1.38 ships real
  collectors for all of these. They are deferred **on desktop** for honest per-collector portability
  reasons: JetBrains AI = JetBrains IDE config-dir read (portable path logic exists, medium effort);
  Kiro = `kiro-cli` subprocess; OpenCode = config/subprocess; Droid/Factory + Antigravity + Synthetic
  = credential-store / API-shape work with lower ROI. Revisit per-collector; each is schedulable, none
  is "Mac deferred." **`Augment` (v0.20.0) and `Volcano Engine` (v0.16.1) were moved OUT of this
  bucket** — both are portable (manual cookie / `ARK_API_KEY`).
- **Swarm PRODUCER on desktop** — known asymmetry (§ v0.19.0); needs a git-worktree rollup watcher;
  possible later slice.
- **Full remote-terminal command consumption as a REMOTE viewer** — same XL epic as R0 Phase B;
  scope-gated (§5.4).
- **Pre-existing desktop backlog** (boot-time orphan reconciliation, long-poll pull_commands, POSIX
  reader-fd leak, OAuth refresh for Cursor/Copilot/OpenRouter, Windows push notifications, React
  component tests, date-range picker, per-provider visibility toggle, CSV/JSON export, light theme,
  custom alert-rules engine) — out of Mac-parity scope; schedule independently.

---

## 7. Cross-platform invariant watchlist (the "don't break wire compat" checklist)

- **Provider-name literals** = Mac `ProviderKind` rawValue exactly (`z.ai`, `GLM`, `Vertex AI`,
  `Abacus AI`, `Kimi K2`, `OpenAI Admin`, `AWS Bedrock`, `LLM Proxy`, `Alibaba Token Plan`) or the
  dual-writer forks the `(user_id,provider)` PK. Enforce via `MAC_PROVIDER_KIND_SNAPSHOT`
  (`quota/mod.rs:264`).
- **Pace/threshold rendering** — remaining-bar orientation (`1−f`), byte-identical arrow/percent +
  `±2/±6/±12` buckets + `compactCountdown` units.
- **Token/day bucketing** — `ioTokens` exclude `cached_tokens`; bucket on LOCAL `yyyy-MM-dd`, never
  raw UTC.
- **`helper_heartbeat` trailing params** — `Option + skip_serializing_if=None` (per-field coalesce;
  NULL ≠ {}); `p_provider_plan_status` **before** `p_metrics`; both plan-status
  (`on_plan`/`off_plan`; `unknown`=absent) and sensor keys are server-whitelisted (anything else
  silently dropped).
- **`battery_state`** strict enum `charging|discharging|charged|none|unknown`; `p_metrics` ≤ 8192 B;
  capability map truthful (no fabricated temps).
- **Codex `on_plan` truthfulness** — report `on_plan` ONLY if the desktop actually env-scrubs under
  verified chatgpt auth; else `off_plan`/omit.
- **R0 broadcast** — redact before sink, std-base64, lowercase `pterm:`, event allowlist,
  `private:true` **per message in the HTTP POST body** (verified against Mac `realtime_broadcast.py:235`
  + its test assertion — this is NOT viewer-only), inbound `realtime_private` parsed as `Option<bool>`
  broadcasting only on `Some(true)`, proactive token refresh, output-only, default-OFF byte-identical.
- **R0 viewer `phx_join`** — `access_token` sibling of `config` inside payload, re-sent on refresh,
  fatal-with-one-retry, `pterm:`/`term:` matched to `realtime_private`.
- **Local terminal spawn** — one shared session map + idle/max-age cap; subscribe-then-snapshot;
  non-blocking bounded output; `provider` stays a `String`.
- **Serde forward-compat** — every added/nullable field on any new backend read (`devices`,
  `swarms`, `RemoteSession.realtime_private`, `ConfigView`) is `Option + #[serde(default)]`.
- **Rust toolchain** — do not bump without checking `sysinfo`/`tauri` MSRV (transitive `time` ≥ 1.88).
- **Tests never depend on host timezone** (a UTC CI runner has bitten this repo twice).

---

## 8. Review + next step

This plan goes to **Codex (`codex exec`)** + **Gemini 3.1 Pro (`agy`)** review before any feature
code (per the handoff + owner's standard flow). After incorporating review, first code is **v0.11.0
(launch-smoke CI)** — the enabling infra that makes shipping without a Windows PC safe — followed by
**v0.11.1 (family-fallback pricing)** as the first user-facing correctness ship.

---

## 9. Review adjudication (2026-07-04)

Both reviewers returned **APPROVE-WITH-CHANGES** with **zero surviving P0**. They cross-validated each
other (each independently confirmed the other's disputed calls). Adjudication:

**Accepted → folded into this revision:**
| # | Source | Finding | Change |
|---|---|---|---|
| 1 | Gemini P1 | System Monitor is a local UI feature; only the sync half was scoped | Split into **v0.18.0 local "Machine" tab** (gauges + top-N process table, LOCAL) + **v0.18.1 sensor sync** |
| 2 | Codex P1 | Deferred list falsely claimed Mac deferred JetBrains AI/Augment/Volcano Engine | Reclassified §6 as **desktop-deferred with per-collector rationale**; **Volcano Engine → v0.16.1** (pure `ARK_API_KEY`), **Augment → v0.20.0** (manual cookie) |
| 3 | Codex P1 | Managed Codex on-plan (v0.24) mis-sequenced before terminal foundations | **Reordered v0.22.0 before v0.24.0**; added hard-dependency note |
| 4 | Codex P1 | R0 `realtime_private` should be tri-state, not `default bool` | Changed to **`Option<bool>`, broadcast only on `Some(true)`** (v0.25.0 + §7) |
| 5 | Codex P2 | `MAC_PROVIDER_KIND_SNAPSHOT` stale (6 vs full enum) | Added **v0.15.0-prep** to refresh the snapshot before the first batch |
| 6 | Gemini P2 | Service-status badges before their collectors | **Resequenced v0.14.0 after the collector batches** |
| 7 | Codex note | "v0.65 not applied" is a moving target | §1b/v0.25.0 reworded to **verify live at implementation time** |

**Rejected → verified false against ground truth (documented so they aren't re-litigated):**
| Source | Finding | Why rejected |
|---|---|---|
| **Gemini P0** | "Repo is private → ARM CI is a billing leak; revert §1a" | ❌ `gh repo view` → `cli-pulse-desktop` is **PUBLIC**; Gemini read the **Mac** repo's `REPO_VISIBILITY_STRATEGY.md` (supplied via `--add-dir`) and **conflated the two repos**. **Codex independently confirmed "§1a is correct."** ARM standard runners are free on this public repo. |
| **Gemini P1** | "`private:true` per message is wrong; belongs only in `phx_join`" | ❌ Mac's own code sends it per-message in the HTTP POST body: `helper/realtime_broadcast.py:235` (`"private": True`) + `test_realtime_broadcast.py:144` assertion ("else blackhole"). **Codex independently confirmed the broadcast shape matches Mac.** Invariant kept. |

**Meta-lesson:** giving a reviewer both repos via `--add-dir` let it cross a repo boundary and raise a
confident-but-wrong P0. Ground-truth verification (`gh`, prod `list_migrations`, the actual Mac source)
plus a second independent reviewer caught it. Keep both practices.
