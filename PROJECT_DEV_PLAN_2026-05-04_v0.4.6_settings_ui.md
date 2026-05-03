# v0.4.6 — Settings UI for provider credentials + CI dynamic matrix + zh-CN polish

**Status:** spec — pending Gemini 3.1 Pro review.
**Author:** Claude Opus 4.7 (Mac session, 2026-05-04).
**Tracks:** v0.4.3 Gemini 3.1 Pro UX review (FAIL #1: env-var-only credentials are an unacceptable UX regression for a paid commercial app); v0.4.5 CI matrix-with-if regression (post-mortem in `feedback_gh_actions_matrix_if.md`); VM polish flag (zh-CN missing thousands separator).
**Parent:** v0.4.5 (Latest).

## 1. Problem

v0.4.3 introduced 5 new providers (Codex / Cursor / Gemini / Copilot / OpenRouter) that
collectively read auth from a mix of file paths and **environment variables**. v0.4.5 shipped
the data + UI layers fully working, but credentials for **Cursor, Copilot, OpenRouter** still
require the user to set environment variables (`CURSOR_COOKIE`, `COPILOT_API_TOKEN`,
`OPENROUTER_API_KEY`) before launching the app. There is no in-app way to enter or update
these values.

Concrete user-impact defects:

1. **Discovery problem.** A non-technical user installing CLI Pulse Desktop on Windows has no way
   to discover that they need to set env vars. The Providers tab silently shows three empty cards
   with the same generic "Quota data unavailable" copy as v0.3.5 had — no actionable guidance.
2. **Persistence problem.** Even a technical user who sets env vars in their PowerShell profile
   loses them across reboots / shells / installer-replacement upgrades. "Set then forget" doesn't
   work — the value lives outside the app's persistence model.
3. **Security visibility problem.** Cookies and tokens in env vars are visible to every process
   the user spawns. A `Get-ChildItem env:` in PowerShell reveals them. A confused user could
   `echo $env:OPENROUTER_API_KEY` into a screen-share. The app should provide a managed-storage
   alternative.

Plus two infra / polish items the team has been tracking:

4. **CI matrix-with-if regression.** v0.4.5 first-tag-push hit `if: matrix.platform == 'windows'`
   at the job level — workflow failed validation in 0 seconds because GitHub Actions doesn't
   expose the matrix context to job-level `if:`. Currently reverted; the `platforms` workflow_dispatch
   input declared in v0.4.4 is a no-op. Need to re-implement Win-only fast iteration via a
   dynamic-matrix setup job.
5. **zh-CN thousands separator.** VM Claude flagged that `formatInt(2782)` renders as `2782` in
   zh-CN locale, while EN renders `2,782`. Cause: probably `Intl.NumberFormat("en")` hard-coded
   somewhere instead of routing through current locale.

## 2. Goal

After v0.4.6:

- A non-technical Windows / Linux user can install CLI Pulse Desktop, click **Settings → Provider
  Credentials**, paste a Cursor cookie / Copilot token / OpenRouter API key, click Save, and see
  tier bars within ~140 seconds (next sync cycle) — without touching env vars or rebooting.
- Credentials persist across launches and survive in-place installer upgrades.
- Credentials are stored at file mode 0600 (Unix) or per-user `%APPDATA%` (Windows ACL default),
  in the same `dev.clipulse.desktop` config dir that already holds `config.json`. NOT in env vars,
  NOT in plaintext anywhere outside this dir.
- Existing v0.4.5 users who set env vars stay working — env var falls back when the new file
  doesn't have the key. New users go through the UI.
- Tag-push CI runs build Windows-only by default (~5 min wall time). Manual workflow_dispatch
  with `platforms=all` (default) runs the full matrix for promote-to-Latest moments.
- zh-CN renders `2,782` (or `2782`-with-locale-correct-grouping) consistently with EN.

### Non-goals

- **OS keychain / `tauri-plugin-stronghold`.** True OS-level secret storage (Win Credential Manager,
  macOS Keychain Access via Security framework, Linux Secret Service / kwallet) is a separate
  cross-platform abstraction layer with its own testing surface. v0.4.6 ships **plaintext
  `provider_creds.json` at file-mode 0600 in app config dir** — same security model as the existing
  `config.json` (which holds `helper_secret`, a non-trivial bearer token). Stronghold migration
  tracked for v0.4.7+.
- **Active Gemini OAuth refresh.** Still requires `gemini` CLI to be run periodically. Tracked
  for v0.4.7+ as a separate sprint with its own Codex review.
- **Settings UI for Gemini token / Claude credentials.** Gemini's `oauth_creds.json` is owned by
  the `gemini` CLI; we only read it. Same for Claude's `.credentials.json`. v0.4.6 only exposes
  UI for the three providers that use env-var-or-flat-string credentials.
- **OpenRouter i32 overflow / bigint migration.** Backend schema change, requires user flag per
  `feedback_cli_pulse_autonomy.md`. Realistic exposure < 0.001% of users. v0.4.7+.
- **Per-provider on/off toggle.** Currently any provider with creds → collected. UI toggle is
  v0.4.7+ scope.

## 3. Decisions

**Storage shape.** New file `provider_creds.json` in the same config dir as `config.json`:
```json
{
  "cursor_cookie": "...",
  "copilot_token": "...",
  "openrouter_api_key": "...",
  "openrouter_base_url": null
}
```
All keys optional. Empty / absent = "not configured" → collector silent-skips at `debug!` level
(unchanged from v0.4.5 env-var-not-set semantics). Same `set_private_mode()` mode-0600 helper as
`config.json` already uses.

**Read priority** in each collector:
1. Env var (existing v0.4.5 behavior — backwards compat for power users)
2. `provider_creds.json` value
3. None → silent skip

This means env-var users keep working identically and the new UI is purely additive.

**Tauri commands.** Two new commands in `lib.rs`:
- `get_provider_creds() -> ProviderCreds` — reads file, returns shape with values **redacted**
  for display (each value replaced with `"<masked>"` or empty + a `has_value: bool` flag per
  field). Frontend never sees the raw secret unless the user clicks "Reveal".
- `set_provider_creds(creds: ProviderCreds) -> Result<()>` — writes the file with mode-0600,
  then fires a `sync_now()` so the next collector cycle picks the value up immediately. Returns
  the masked shape as confirmation.

The "reveal" flow could be a separate `peek_provider_cred(field: String) -> String` that returns
the raw value only when the user clicks the eye-icon. Or just: never re-display, only allow
**replace** (paste new value, save, old value never shown again). Latter is simpler + more secure;
former matches typical password-manager UX. **Decision: simpler — no peek; user replaces or clears.**

**Sync trigger after save.** `set_provider_creds` calls `sync_now` directly (or sends a Tauri
event that `lib.rs::sync_now` is listening for). User clicks Save → tier bars appear within ~3
seconds for Cursor / Copilot / OpenRouter (HTTP roundtrip) versus the default 120s sync cycle.

**Settings UI structure.** Add a new `<IntegrationsSection>` component as a **dedicated section
at the bottom of Settings tab** (separate from Account / Budget / Sync / Updates / Export /
Language) — per Gemini 2026-05-04 review FAIL #6: sandwiching credentials between Account and
Budget breaks the user's mental model of profile-vs-money. Header copy: "Integrations" /
"集成" / "連携サービス". Three rows:

| Row | Input type | Notes |
|---|---|---|
| Cursor cookie | `<input type="password">` single-line, monospace, hor-scroll | Per Gemini #3: textarea is wrong — 1-2 KB cookie as wall of text dominates real-estate. Single-line + paste-once UX. |
| Copilot token | `<input type="password">` (no show/hide toggle — see no-peek §3 decision) | Token is 40-90 chars |
| OpenRouter API key | `<input type="password">` + Base URL field hidden behind "Advanced" disclosure toggle | Per Gemini #9: only self-hosting proxy users need base URL; default-hide reduces 99% confusion. API key is `sk-or-...`. |

Each row: status indicator (✓ "Configured" / ⚠ "Not set"), Save button (per-row),
Clear button (per-row, opens confirmation modal — see §10.7). Save flow shows
single spinner state during save+sync, then row updates to green "Configured" once
done — NOT a 4-state narration (per Gemini #7). Toast for hard failures only.

When server-side last-sync error is available (HTTP 401, network timeout, parse fail), surface
as secondary line below status. Copy uses **friendly mapping** (per Gemini #2):
- HTTP 401 → "Invalid or expired credential"
- Network/timeout → "Couldn't reach <provider>"
- HTTP 403 / quota-style → "Authentication insufficient"
- Other 4xx/5xx → "Last sync failed (see Logs)"

Raw HTTP code stays in `cli-pulse.log` only.

**i18n.** New keys in `providers.creds.*` namespace, all 3 locales. Provider names stay English
("Cursor", "Copilot", "OpenRouter") — they're brand nouns. UI text ("Save", "Clear", "Configured")
gets translated. Help text ("Paste your session cookie from cursor.com here") gets translated.

**Sentry scrubbing.** Already covered by v0.4.3 regex (`OPENROUTER_TOKEN`, `GITHUB_TOKEN_LEGACY`,
`GITHUB_PAT_NEW`, `OPENAI_TOKEN`, `COOKIE_HEADER`, `AUTH_BEARER`). New `provider_creds.json` file
contents would be scrubbed on the way out via existing `scrub_strings_recursive` if they ever leak
into a Sentry event. No new regexes needed.

**CI dynamic matrix.** Replace the broken-and-reverted v0.4.4–v0.4.5 attempt with a setup-matrix
job that emits JSON consumed via `fromJson`:

```yaml
jobs:
  setup-matrix:
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.set.outputs.matrix }}
    steps:
      - id: set
        run: |
          if [[ "${{ github.event_name }}" == "workflow_dispatch" \
                && "${{ inputs.platforms }}" == "all" ]]; then
            echo 'matrix=<full 4-platform JSON>' >> $GITHUB_OUTPUT
          else
            echo 'matrix=<windows x64 + arm64 only>' >> $GITHUB_OUTPUT
          fi
  publish:
    needs: setup-matrix
    strategy:
      matrix: ${{ fromJson(needs.setup-matrix.outputs.matrix) }}
    ...
```

Tag push → Windows-only (~5 min). Manual `gh workflow run release.yml -f tag=vX.Y.Z` →
default `platforms=all` → full matrix (~10 min). Promote-to-Latest workflow checklist note:
**always run with platforms=all** before promoting.

**zh-CN thousands separator.** Trace `formatInt` in `src/lib/format.ts`:
- If hardcoded `Intl.NumberFormat("en")` → fix to `Intl.NumberFormat(currentLocale)`.
- If `Intl.NumberFormat()` (no arg) → JS picks browser default, which Tauri WebView2 reports
  as `en-US` regardless of app locale. Fix to pipe locale from i18n setup.

The fix is a one-line locale parameter pass + 2 unit tests asserting `formatInt(2782, "en")
=== "2,782"` and `formatInt(2782, "zh-CN") === "2,782"` (zh-CN uses comma per CLDR; not `2782`
as VM observed — that observation was likely the bug surface).

## 4. Implementation

### 4.1 New file `src-tauri/src/provider_creds.rs` (~130 LOC)

Mirrors `src-tauri/src/config.rs` shape. Public API:
- `pub struct ProviderCreds { cursor_cookie: Option<String>, copilot_token: Option<String>,
  openrouter_api_key: Option<String>, openrouter_base_url: Option<String> }`
- `pub fn load() -> anyhow::Result<ProviderCreds>` (returns empty default if file absent)
- `pub fn save(creds: &ProviderCreds) -> anyhow::Result<()>` (mode-0600, atomic write via
  temp + rename)
- `pub fn config_path() -> Option<PathBuf>` (uses existing `config::config_dir()`)

Re-uses `config::set_private_mode` helper.

### 4.2 Tauri commands in `lib.rs`

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ProviderCredsView {
    pub cursor_cookie_set: bool,
    pub copilot_token_set: bool,
    pub openrouter_api_key_set: bool,
    pub openrouter_base_url: Option<String>, // not secret, returned plaintext
}

#[tauri::command]
async fn get_provider_creds() -> Result<ProviderCredsView, String> {
    let c = provider_creds::load().map_err(|e| e.to_string())?;
    Ok(ProviderCredsView {
        cursor_cookie_set: c.cursor_cookie.as_deref().is_some_and(|s| !s.is_empty()),
        copilot_token_set: c.copilot_token.as_deref().is_some_and(|s| !s.is_empty()),
        openrouter_api_key_set: c.openrouter_api_key.as_deref().is_some_and(|s| !s.is_empty()),
        openrouter_base_url: c.openrouter_base_url,
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderCredsUpdate {
    pub cursor_cookie: Option<String>,    // None = leave unchanged; Some("") = clear
    pub copilot_token: Option<String>,
    pub openrouter_api_key: Option<String>,
    pub openrouter_base_url: Option<String>,
}

#[tauri::command]
async fn set_provider_creds(
    update: ProviderCredsUpdate,
    app: tauri::AppHandle,
) -> Result<ProviderCredsView, String> {
    let mut current = provider_creds::load().map_err(|e| e.to_string())?;
    if let Some(v) = update.cursor_cookie {
        current.cursor_cookie = if v.is_empty() { None } else { Some(v) };
    }
    // ... same for copilot_token, openrouter_api_key, openrouter_base_url
    provider_creds::save(&current).map_err(|e| e.to_string())?;
    // Trigger a sync so tier bars update within seconds, not 120s.
    let _ = app.emit("provider_creds_changed", ());
    get_provider_creds().await
}
```

The `sync_now` listener for `provider_creds_changed` event lives in the existing background sync
loop — extends 1 line.

### 4.3 Collector updates in `quota/{cursor,copilot,openrouter}.rs`

Each `collect()` gets a 3-line addition at the env-var read site:

```rust
let cookie = std::env::var("CURSOR_COOKIE")
    .ok()
    .filter(|s| !s.is_empty())
    .or_else(|| {
        provider_creds::load().ok().and_then(|c| c.cursor_cookie)
    });
match cookie {
    Some(c) if !c.is_empty() => fetch_with_cookie(&c).await,
    _ => {
        log::debug!("[Cursor] no creds (env or file) — skipping");
        None
    }
}
```

Same shape for `copilot.rs` (`COPILOT_API_TOKEN` env → `copilot_token` file) and `openrouter.rs`
(`OPENROUTER_API_KEY` env → `openrouter_api_key` file; `OPENROUTER_API_URL` env →
`openrouter_base_url` file).

### 4.4 React `<ProviderCredentialsSection>` component

Insert into `Settings.tsx` between Account and Budget sections. Pseudocode:

```tsx
const [creds, setCreds] = useState<ProviderCredsView | null>(null);
const [draft, setDraft] = useState<ProviderCredsUpdate>({});
const [saving, setSaving] = useState(false);

useEffect(() => {
  invoke("get_provider_creds").then(setCreds);
}, []);

async function save() {
  setSaving(true);
  try {
    const next = await invoke<ProviderCredsView>("set_provider_creds", { update: draft });
    setCreds(next);
    setDraft({});  // clear draft after successful save
  } catch (e) {
    toast.error(e.message);
  } finally {
    setSaving(false);
  }
}

return (
  <section>
    <h2>{t("settings.provider_creds.heading")}</h2>
    <p>{t("settings.provider_creds.description")}</p>
    <CursorRow status={creds?.cursor_cookie_set} draft={draft.cursor_cookie}
               onChange={v => setDraft({...draft, cursor_cookie: v})} />
    <CopilotRow ... />
    <OpenRouterRow ... />
    <button onClick={save} disabled={saving || !hasDraftChanges}>
      {t("action.save")}
    </button>
  </section>
);
```

### 4.5 i18n keys (3 locales × ~12 keys each)

**EN** (`en.json`):
```json
"settings": {
  "integrations": {
    "heading": "Integrations",
    "description": "Configure auth tokens for providers that don't use OAuth files. Saved with file-mode 0600 in your config dir.",
    "cursor_cookie_label": "Cursor session cookie",
    "cursor_cookie_help": "Paste from cursor.com browser DevTools → Application → Cookies",
    "copilot_token_label": "GitHub Copilot token",
    "copilot_token_help": "From GitHub OAuth or your Copilot CLI's COPILOT_API_TOKEN env",
    "openrouter_api_key_label": "OpenRouter API key",
    "openrouter_api_key_help": "Generate at openrouter.ai/keys",
    "openrouter_advanced_toggle": "Advanced settings",
    "openrouter_base_url_label": "Custom endpoint (optional)",
    "openrouter_base_url_placeholder": "https://openrouter.ai/api/v1",
    "status_configured": "Configured",
    "status_not_set": "Not set",
    "save_button": "Save",
    "clear_button": "Clear",
    "clear_confirm_title": "Clear {{provider}} credential?",
    "clear_confirm_body": "You'll need to re-paste it from the source. This can't be undone.",
    "clear_confirm_action": "Clear",
    "env_override_banner": "The environment variable {{var}} is currently overriding this saved value. Remove it from your system to use this setting.",
    "error_invalid_credential": "Invalid or expired credential",
    "error_unreachable": "Couldn't reach {{provider}}",
    "error_auth_insufficient": "Authentication insufficient",
    "error_generic_sync": "Last sync failed (see Logs)"
  }
}
```

**zh-CN** (per Gemini #4 review — `已配置` is more professional than alternatives):
```json
"integrations": {
  "heading": "集成",
  "description": "为无 OAuth 文件的服务商配置认证 token。以 0600 文件权限保存在配置目录。",
  "cursor_cookie_label": "Cursor 会话 Cookie",
  "cursor_cookie_help": "从 cursor.com 浏览器开发者工具 → 应用 → Cookies 中复制",
  "copilot_token_label": "GitHub Copilot Token",
  "copilot_token_help": "来自 GitHub OAuth 或 Copilot CLI 的 COPILOT_API_TOKEN 环境变量",
  "openrouter_api_key_label": "OpenRouter API Key",
  "openrouter_api_key_help": "在 openrouter.ai/keys 生成",
  "openrouter_advanced_toggle": "高级设置",
  "openrouter_base_url_label": "自定义接入点(可选)",
  "openrouter_base_url_placeholder": "https://openrouter.ai/api/v1",
  "status_configured": "已配置",
  "status_not_set": "未设置",
  "save_button": "保存",
  "clear_button": "清除",
  "clear_confirm_title": "清除 {{provider}} 凭证?",
  "clear_confirm_body": "你需要从原处重新粘贴。此操作不可撤销。",
  "clear_confirm_action": "清除",
  "env_override_banner": "环境变量 {{var}} 正在覆盖此保存的值。请从系统中移除该变量以生效。",
  "error_invalid_credential": "凭证无效或已过期",
  "error_unreachable": "无法连接到 {{provider}}",
  "error_auth_insufficient": "认证权限不足",
  "error_generic_sync": "最近一次同步失败(详见日志)"
}
```

**ja** (per Gemini #4 — `設定済み` / `未設定`):
```json
"integrations": {
  "heading": "連携サービス",
  "description": "OAuth ファイルを使わないプロバイダーの認証トークンを設定します。設定ディレクトリにファイルモード 0600 で保存されます。",
  "cursor_cookie_label": "Cursor セッション Cookie",
  "cursor_cookie_help": "cursor.com のブラウザ開発者ツール → アプリケーション → Cookie から貼り付け",
  "copilot_token_label": "GitHub Copilot トークン",
  "copilot_token_help": "GitHub OAuth または Copilot CLI の COPILOT_API_TOKEN 環境変数から取得",
  "openrouter_api_key_label": "OpenRouter API キー",
  "openrouter_api_key_help": "openrouter.ai/keys で生成",
  "openrouter_advanced_toggle": "詳細設定",
  "openrouter_base_url_label": "カスタムエンドポイント(任意)",
  "openrouter_base_url_placeholder": "https://openrouter.ai/api/v1",
  "status_configured": "設定済み",
  "status_not_set": "未設定",
  "save_button": "保存",
  "clear_button": "削除",
  "clear_confirm_title": "{{provider}} の認証情報を削除しますか?",
  "clear_confirm_body": "元のソースから再度貼り付ける必要があります。この操作は取り消せません。",
  "clear_confirm_action": "削除",
  "env_override_banner": "環境変数 {{var}} が保存された値を上書きしています。この設定を有効にするにはシステムから変数を削除してください。",
  "error_invalid_credential": "認証情報が無効または期限切れです",
  "error_unreachable": "{{provider}} に接続できません",
  "error_auth_insufficient": "認証権限が不足しています",
  "error_generic_sync": "前回の同期に失敗しました(ログを参照)"
}
```

### 4.6 CI dynamic matrix — DEFERRED to v0.4.7

**Removed from v0.4.6 scope.** v0.4.5 attempt at job-level `if:` with
matrix context broke `release.yml` and required revert. v0.4.6 is already
a substantial frontend + backend + i18n addition; bundling another
release-pipeline rewrite stacks risk we don't need to take. v0.4.7 will
do CI optimization as a focused sprint:
- Build the dynamic-matrix branch separately
- Validate via a throwaway `v0.4.7-rc1` tag push
- Confirm Windows-only path runs in ~5 min before promoting the change
- Add a CI-only test workflow that validates the matrix output JSON itself

For v0.4.6 hotfix iteration: continues to run full 4-platform build
(~10 min). Acceptable cost for one release.

### 4.6b (kept) — original CI dynamic matrix design (for v0.4.7 reference)

Two-job structure (replaces current single `publish` job):

```yaml
jobs:
  setup-matrix:
    name: Resolve build matrix
    runs-on: ubuntu-latest
    outputs:
      matrix: ${{ steps.set.outputs.matrix }}
    steps:
      - id: set
        run: |
          set -euo pipefail
          if [[ "${{ github.event_name }}" == "workflow_dispatch" \
                && "${{ inputs.platforms }}" == "all" ]]; then
            cat <<'JSON' > /tmp/matrix.json
{"include":[
  {"platform":"windows","arch":"x64","os":"windows-latest","bundles":"nsis","rust-target":"x86_64-pc-windows-msvc"},
  {"platform":"windows","arch":"arm64","os":"windows-11-arm","bundles":"nsis","rust-target":"aarch64-pc-windows-msvc"},
  {"platform":"linux","arch":"x64","os":"ubuntu-latest","bundles":"deb,rpm,appimage","rust-target":"x86_64-unknown-linux-gnu"},
  {"platform":"linux","arch":"arm64","os":"ubuntu-24.04-arm","bundles":"deb,rpm,appimage","rust-target":"aarch64-unknown-linux-gnu"}
]}
JSON
          else
            cat <<'JSON' > /tmp/matrix.json
{"include":[
  {"platform":"windows","arch":"x64","os":"windows-latest","bundles":"nsis","rust-target":"x86_64-pc-windows-msvc"},
  {"platform":"windows","arch":"arm64","os":"windows-11-arm","bundles":"nsis","rust-target":"aarch64-pc-windows-msvc"}
]}
JSON
          fi
          # Compact to single line for GITHUB_OUTPUT (multiline values are
          # supported via heredoc syntax but jq -c is more reliable).
          echo "matrix=$(jq -c . /tmp/matrix.json)" >> "$GITHUB_OUTPUT"

  publish:
    needs: setup-matrix
    name: Build & publish (${{ matrix.platform }} / ${{ matrix.arch }})
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix: ${{ fromJson(needs.setup-matrix.outputs.matrix) }}
    steps: ... # unchanged from current workflow
```

Validation strategy: after pushing this change, do a no-op tag push (e.g., `v0.4.6-rc1`) and
confirm only 2 jobs run. Then promote tag to v0.4.6 final via workflow_dispatch with
platforms=all to confirm full matrix path also works. If both pass, the dynamic matrix is verified
under both event types.

### 4.7 zh-CN thousands separator

Inspect `src/lib/format.ts` (`formatInt` definition). If `Intl.NumberFormat("en", ...)` is
hardcoded → swap to `Intl.NumberFormat(i18n.language, ...)`. If no arg is passed → pass
`i18n.language`. Add 3 unit tests:
- `formatInt(2782, "en") === "2,782"`
- `formatInt(2782, "zh-CN") === "2,782"` (CLDR zh uses comma, not no-separator)
- `formatInt(2782, "ja") === "2,782"` (CLDR ja same)

If VM saw `2782` in zh-CN, the cause is likely a missing locale param, not zh-CN actually using
no separator.

## 5. Tests

**Rust unit tests** (~6 new):
- `provider_creds`: round-trip empty, round-trip populated, mode-0600 set on Unix, atomic write
  doesn't corrupt on partial failure, `load()` returns empty on parse failure (don't crash on
  malformed file).
- Each of `cursor.rs` / `copilot.rs` / `openrouter.rs`: env-var path takes priority over file
  path (1 test each, 3 total).

**Frontend Vitest tests** (~4 new):
- `<ProviderCredentialsSection>` renders rows for all 3 providers.
- Save flow: typing in field → click save → invoke called with correct payload.
- Status indicator updates after save returns.
- `formatInt(2782, "zh-CN")` returns `"2,782"`.

**E2E (VM Claude on Win)**:
1. Install v0.4.6 over v0.4.5.
2. Settings → Provider Credentials → paste a fake `OPENROUTER_API_KEY` like `sk-or-test-12345`.
3. Click Save. Confirm UI shows "Configured" status.
4. Wait <5s, check Providers tab — OpenRouter card should show error state (HTTP 401 on /credits)
   not silent skip — proves the file was read and used.
5. Clear the credential. Confirm card returns to "not set" empty state.
6. Set a real OpenRouter key. Confirm tier bars appear within 5s.
7. Verify file at `%APPDATA%\dev.clipulse.desktop\provider_creds.json` has correct content (and
   sensitive value is NOT logged in cli-pulse.log — only `[OpenRouter] reading from creds file`
   metadata).
8. Switch language to zh-CN. Verify all section labels translate. Verify a number like
   `2,782 条消息` shows the comma.

## 6. Risks & mitigations

| Risk | Mitigation |
|---|---|
| User pastes wrong cookie → 401 forever, no UI feedback | "Configured" status with a secondary line: "Last sync: HTTP 401 — cookie may have expired". Wire from server-side log. |
| File-mode 0600 not honored on Windows | %APPDATA%\dev.clipulse.desktop is per-user by NTFS default; Windows ACL hardening is v0.4.7+. Document in changelog as known limitation. |
| Atomic write on Windows fails (rename-over-open-file restriction) | Use `tempfile::NamedTempFile` + persist; falls back to copy + delete on Windows. |
| User's clipboard has cookie + opens 5 windows + types `Get-ClipBoard` | Out of scope. We can't protect against active clipboard exposure. |
| `provider_creds.json` syncs into Dropbox / OneDrive home folder | Config dir is `~/Library/Application Support/...` on Mac and `%APPDATA%` on Win — these are typically NOT cloud-synced by default. Linux `~/.config/...` may be Syncthing'd; we mention in CHANGELOG. |
| Settings UI shows secret in raw form by mistake | "No peek" decision (§3) — UI only shows "Configured" / "Not set" status, never the raw value. Once saved, only replaceable, not viewable. |
| Old env var still set + new file has different value | env var wins by priority. UI should display a warning banner: "Your CURSOR_COOKIE env var is overriding the saved value." `get_provider_creds` returns extra `env_override: bool` per field. |
| File schema drift in v0.4.7+ (e.g., added stronghold migration) | Add `version: 1` field to schema now; v0.4.7+ can branch on it. |
| Multi-user / shared machine | v0.4.6 trusts user-account isolation. Documented as known model. |
| CI dynamic matrix breaks for some unknown reason | Test plan: dummy tag push validates Windows-only path; manual workflow_dispatch validates full path. Both must pass before merging. |

## 7. Milestones (~1.5 days)

| Day | Work |
|---|---|
| 0.5 | `provider_creds.rs` module + 5 unit tests. Tauri commands `get/set_provider_creds`. Collector reads 3-line update each. cargo fmt + clippy + test green. |
| 1 | `<ProviderCredentialsSection>` React component + 4 Vitest tests. i18n keys × 3 locales. format.ts locale fix + 3 tests. |
| 1.25 | CI dynamic matrix rewrite. Push `v0.4.6-rc1` test tag → confirm Windows-only run. Delete rc1 tag. |
| 1.5 | Final cargo + npm sanity. CHANGELOG. Push v0.4.6 tag → Windows-only CI ~5 min → auto-flip Pre-release → hand to VM. After VM PASS, manual workflow_dispatch with platforms=all to add Linux → promote v0.4.6 to Latest. |

## 8. Backward compatibility

- v0.4.5 desktops on auto-update: unaffected. New `provider_creds.json` is additive — absence
  is identical to "no credentials" which is the existing v0.4.5 state for users without env vars.
- Existing env-var users: unaffected. Env vars take priority over file values.
- iOS / Android / Mac: unaffected. No backend / Mac collector changes.
- v0.4.x desktops on auto-update pick up v0.4.6 automatically.

## 9. Out of scope (deferred)

- **OS keychain / stronghold migration** (v0.4.7+, separate sprint with cross-platform testing).
- **Active Gemini OAuth refresh** (v0.4.7+).
- **Per-provider on/off toggle UI** (v0.4.7+).
- **OpenRouter i32 overflow → bigint** (v0.4.7+, **needs user flag** for backend schema).
- **tauri-action cleanup 404 quirk** investigation (cosmetic; non-blocking).
- **China provider collectors** (Kimi / GLM / Zai / MiniMax / Alibaba / VolcanoEngine).

## 10. Decisions to close before sprint start

1. **Save semantics on partial fields.** When user updates only one field and clicks Save, we
   merge into existing file (other fields preserved). When user clicks Clear, we set that field
   to empty. **Decision: confirmed merge on save, explicit clear button per row.**
2. **Reveal flow.** No peek. User can replace or clear, never view raw saved value. **Confirmed.**
3. **Sync-after-save UX.** Trigger sync_now via Tauri event after `set_provider_creds`. UI shows
   a "Saving..." → "Saved" → "Syncing..." → "Synced" status flow within ~5s. **Confirmed.**
4. **CI rc1 tag for dynamic matrix validation.** Push `v0.4.6-rc1` (intentionally non-final) to
   trigger Windows-only run, then `git tag -d v0.4.6-rc1; git push origin :v0.4.6-rc1` cleanup.
   **Confirmed minor friction acceptable for one-time validation.**
5. **zh-CN actual expected output.** `Intl.NumberFormat("zh-CN").format(2782)` returns `"2,782"`
   per CLDR (not `2782` and not `2 782`). Source: ICU CLDR root data. **Confirmed: bug is
   missing locale param, not actual CLDR difference.**
6. **`env_override: bool` flag in `ProviderCredsView`.** When env var set + file value also set,
   show banner. **Confirmed worth adding for UX clarity.**
7. **Clear-credential confirmation modal** (Gemini #8 FAIL resolution). One-click clear is a
   UX trap — Cursor cookies require browser DevTools re-export which is non-trivial. **Confirmed:
   per-row Clear button opens a modal with provider-specific title + body + Clear/Cancel
   actions.** i18n strings `clear_confirm_title` / `clear_confirm_body` /
   `clear_confirm_action` already in §4.5.

## 11. Review history

- **Gemini 3.1 Pro (2026-05-04)** — UX / product / i18n review. 10 findings:
  - **#1 No-peek decision: SHIP-IT.** Confirmed correct for first-iter indie commercial app —
    drastically reduces security surface. Spec unchanged.
  - **#2 HTTP 401 raw status: NIT.** Map to friendly copy ("Invalid or expired credential"
    etc.). Spec §3 + §4.5 updated with full mapping table.
  - **#3 Cursor cookie textarea: FAIL.** Use single-line password input + horizontal scroll.
    Spec §3 table updated.
  - **#4 i18n quality: NIT.** zh-CN `已配置` confirmed; ja `設定済み` / `未設定` confirmed.
    Spec §4.5 i18n keys re-written with Gemini's specific translations.
  - **#5 env_override banner copy: NIT.** Use Gemini's drafted EN/zh-CN/ja copy verbatim.
    Spec §4.5 includes them.
  - **#6 Settings placement: FAIL.** Don't sandwich between Account and Budget. Move to
    dedicated "Integrations" / "集成" / "連携サービス" section at bottom of Settings tab.
    Spec §3 + §4.4 updated.
  - **#7 Save flow 4-state: FAIL.** Collapse to 2 states (spinner → green Configured).
    Spec §3 + §10.3 updated.
  - **#8 Clear-credential confirm: FAIL.** Must require confirmation modal — Cursor cookies
    are hard to re-acquire. Spec §10.7 added.
  - **#9 OpenRouter base URL visibility: NIT.** Hide behind "Advanced settings" toggle —
    99% of users don't need it. Spec §3 table updated.
  - **#10 zh-CN comma format: SHIP-IT.** VM observation incorrect — modern zh-CN UIs use
    Western thousands separator per CLDR. `Intl.NumberFormat("zh-CN").format(2782)` returns
    `"2,782"`. Spec §10.5 confirmed.
- **Codex review:** Pending separate spawn for technical / security review of
  `provider_creds.rs` mode-0600 + atomic write semantics + collector env-var-vs-file priority.

## 12. References

- v0.4.3 dev plan + Gemini 3.1 Pro review:
  `PROJECT_DEV_PLAN_2026-05-02_v0.4.3_multi_provider_quota.md` (FAIL #1 — env-var-only UX
  unacceptable for paid commercial app)
- v0.4.5 commit + CHANGELOG: tier bar direction fix + pluralization (this is the parent)
- v0.4.5 CI matrix-with-if regression post-mortem:
  `~/.claude/projects/.../memory/feedback_gh_actions_matrix_if.md`
- VM-as-real-E2E pattern memory:
  `~/.claude/projects/.../memory/feedback_vm_as_real_e2e.md`
- Existing `config.rs` persistence pattern:
  `cli-pulse-desktop/src-tauri/src/config.rs:1-90`
- Existing Settings React component (location to extend):
  `cli-pulse-desktop/src/App.tsx:284-298` + the `Settings` component definition
- Existing collectors that read env vars:
  - `quota/cursor.rs:64` (CURSOR_COOKIE)
  - `quota/copilot.rs:56` (COPILOT_API_TOKEN)
  - `quota/openrouter.rs:67-68` (OPENROUTER_API_KEY, OPENROUTER_API_URL)
- CodexBar upstream (Mac reference for any patterns we want to mirror):
  `github.com/steipete/CodexBar` (commit 82bbcde verified 2026-05-02)
