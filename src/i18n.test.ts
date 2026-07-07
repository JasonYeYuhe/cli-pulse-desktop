import { describe, it, expect, beforeEach, vi } from "vitest";

// We test the public surface of `./i18n` — language detection on first
// load, persistence across calls, and `setLang`.
//
// i18next is initialized at module-load time, so to test "first-launch
// detection" we have to force a fresh import. Vitest's `vi.resetModules()`
// + dynamic import does this cleanly.

async function freshI18n() {
  vi.resetModules();
  return await import("./i18n");
}

beforeEach(() => {
  localStorage.clear();
  vi.resetModules();
});

describe("i18n bootstrap", () => {
  it("respects a stored localStorage choice over navigator", async () => {
    localStorage.setItem("cli-pulse.lang", "ja");
    const { default: i18n } = await freshI18n();
    expect(i18n.language).toBe("ja");
    expect(i18n.t("tab.overview")).toBe("概要");
  });

  it("falls back when localStorage stores an unsupported code", async () => {
    localStorage.setItem("cli-pulse.lang", "xx-bogus");
    const { default: i18n } = await freshI18n();
    // xx-bogus is rejected; we fall back to navigator-derived choice
    // (jsdom default "en-US" → "en") or "en" final fallback.
    expect(["en", "en-US", "en-GB"].some((l) => i18n.language.startsWith(l))).toBe(true);
  });

  it("falls back to a string for keys that don't exist", async () => {
    const { default: i18n } = await freshI18n();
    const result = i18n.t("nonexistent.key.path");
    expect(typeof result).toBe("string");
  });

  it("setLang updates active language and persists to localStorage", async () => {
    const mod = await freshI18n();
    mod.setLang("zh-CN");
    expect(mod.default.language).toBe("zh-CN");
    expect(localStorage.getItem("cli-pulse.lang")).toBe("zh-CN");
    expect(mod.default.t("tab.overview")).toBe("概览");
  });

  it("setLang to ja flips translations to Japanese", async () => {
    const mod = await freshI18n();
    mod.setLang("ja");
    expect(mod.default.t("tab.settings")).toBe("設定");
  });
});

describe("i18n covers all critical labels in all 3 languages", () => {
  // Every required key must resolve to a non-empty string in every
  // supported language. Catches accidentally-deleted keys before they
  // ship.
  const REQUIRED_KEYS = [
    "tab.overview",
    "tab.providers",
    "tab.sessions",
    "tab.alerts",
    "tab.settings",
    "action.rescan",
    "action.pair_device",
    "action.sync_now",
    "action.unpair_device",
    "action.check_updates",
    "badge.paired",
    "badge.not_paired",
    "settings.account_heading",
    "settings.budget_heading",
    "settings.sync_heading",
    "settings.updates_heading",
    "settings.export_heading",
    "settings.language_heading",
    // v0.4.20 — storage backend visibility line in Settings → Integrations.
    "settings.integrations.storage_label",
    "settings.integrations.storage_os_keychain",
    "settings.integrations.storage_file",
    "settings.integrations.storage_file_tooltip",
    // DeepSeek collector (v0.15 provider batch) — Settings input row.
    "settings.integrations.deepseek_api_key_label",
    "settings.integrations.deepseek_api_key_help",
    // z.ai collector (v0.15 provider batch) — Settings input row.
    "settings.integrations.zai_api_key_label",
    "settings.integrations.zai_api_key_help",
    // Crof collector (v0.15 provider batch) — Settings input row.
    "settings.integrations.crof_api_key_label",
    "settings.integrations.crof_api_key_help",
    // MiniMax collector (v0.15 provider batch) — Settings input row.
    "settings.integrations.minimax_api_key_label",
    "settings.integrations.minimax_api_key_help",
    // Moonshot collector (v0.15 provider batch) — Settings input row.
    "settings.integrations.moonshot_api_key_label",
    "settings.integrations.moonshot_api_key_help",
    // Venice collector (v0.15 provider batch) — Settings input row.
    "settings.integrations.venice_api_key_label",
    "settings.integrations.venice_api_key_help",
    // Kimi K2 collector (v0.15 provider batch) — Settings input row.
    "settings.integrations.kimi_k2_api_key_label",
    "settings.integrations.kimi_k2_api_key_help",
    // Augment collector (v0.16 cookie batch) — Settings cookie-paste row.
    "settings.integrations.augment_cookie_label",
    "settings.integrations.augment_cookie_help",
    // Perplexity collector (v0.16 cookie batch) — Settings cookie-paste row.
    "settings.integrations.perplexity_cookie_label",
    "settings.integrations.perplexity_cookie_help",
    // T3 Chat collector (v0.16 cookie batch) — Settings cookie-paste row.
    "settings.integrations.t3chat_cookie_label",
    "settings.integrations.t3chat_cookie_help",
    // StepFun collector (v0.16 cookie batch) — Settings cookie-paste row.
    "settings.integrations.stepfun_cookie_label",
    "settings.integrations.stepfun_cookie_help",
    // Warp collector (v0.16 — GraphQL api-key) — Settings input row.
    "settings.integrations.warp_api_key_label",
    "settings.integrations.warp_api_key_help",
    // Kimi collector (v0.16 — connect-JSON token) — Settings input row.
    "settings.integrations.kimi_auth_token_label",
    "settings.integrations.kimi_auth_token_help",
    // Grok collector (v0.16 — gRPC-web cookie) — Settings cookie-paste row.
    "settings.integrations.grok_cookie_label",
    "settings.integrations.grok_cookie_help",
    // GLM collector (v0.17 — status-only api-key) — Settings input row.
    "settings.integrations.glm_api_key_label",
    "settings.integrations.glm_api_key_help",
    // Volcano Engine collector (v0.17 — dual-mode api-key) — Settings input row.
    "settings.integrations.volcano_api_key_label",
    "settings.integrations.volcano_api_key_help",
    // Groq collector (v0.17 — status-only prometheus) — Settings input row.
    "settings.integrations.groq_api_key_label",
    "settings.integrations.groq_api_key_help",
    // Mistral collector (v0.17 — status-only cookie spend) — Settings cookie-paste row.
    "settings.integrations.mistral_cookie_label",
    "settings.integrations.mistral_cookie_help",
    // Deepgram collector (v0.17 — status-only usage counts) — Settings input row.
    "settings.integrations.deepgram_api_key_label",
    "settings.integrations.deepgram_api_key_help",
    // ElevenLabs collector — real character quota (used / limit) + voice slots.
    "settings.integrations.elevenlabs_api_key_label",
    "settings.integrations.elevenlabs_api_key_help",
    // Kilo collector — credit balance + Kilo Pass subscription ($ figure).
    "settings.integrations.kilo_api_key_label",
    "settings.integrations.kilo_api_key_help",
    // Alibaba coding-plan collector — 5h/weekly/monthly request quotas.
    "settings.integrations.alibaba_api_key_label",
    "settings.integrations.alibaba_api_key_help",
    // v0.4.20 — per-provider error badge on Providers tab.
    "providers.error_badge",
    "providers.error_tooltip",
    // v0.10.1 — per-provider visibility filter on the Providers tab.
    // A missing label would render a blank filter chip / hint, which
    // silently miscommunicates which provider a chip controls. Pin
    // every key in all 3 languages.
    "providers.visibility_label",
    "providers.visibility_show_all",
    "providers.visibility_hide_tooltip",
    "providers.visibility_show_tooltip",
    "providers.all_hidden",
    // v1.30 F2a — quota-bar warning-threshold tick tooltip.
    "providers.warn_threshold",
    // v0.13.0 — per-provider 30-day usage chart.
    "providers.chart_title",
    "providers.chart_no_history",
    "providers.chart_io_total",
    // v0.14 — provider service-status badge severity labels.
    "providers.status_operational",
    "providers.status_maintenance",
    "providers.status_minor",
    "providers.status_major",
    "providers.status_critical",
    // v0.10.1 — alert related-entity chips (session + device), macOS parity.
    "misc.session_label",
    "misc.device_label",
    // v0.10.1 — Overview provider-usage breakdown section title.
    "overview.provider_usage_title",
    // v0.10.1 — Swarm tab (macOS/iOS parity). Pin tab label, state
    // hints, and card chrome. Plural keys (agents/blocked_count) are
    // exercised separately and intentionally omitted here.
    "tab.swarm",
    "shortcuts.tab_swarm",
    "swarm.title",
    "swarm.summary",
    "swarm.not_paired_hint",
    "swarm.disabled_hint",
    "swarm.load_failed",
    "swarm.no_swarms",
    "swarm.empty_hint",
    "swarm.blocked_badge",
    "swarm.oldest_blocked",
    "swarm.stale",
    "swarm.last_seen",
    "swarm.worktree",
    // System Monitor "Machine" tab (v1.38 parity). A missing label
    // would render blank gauge/column chrome; pin all 3 languages.
    "tab.machine",
    "shortcuts.tab_machine",
    "machine.title",
    "machine.load_failed",
    "machine.loading",
    "machine.process_count",
    "machine.cpu",
    "machine.cpu_cores",
    "machine.memory",
    "machine.top_processes",
    "machine.no_processes",
    "machine.col_process",
    "machine.col_pid",
    "machine.col_cpu",
    "machine.col_mem",
    "machine.local_note",
    // Capability-gated sensors (temps + battery).
    "machine.battery",
    "machine.batt_charging",
    "machine.batt_discharging",
    "machine.batt_full",
    "machine.batt_empty",
    "machine.batt_unknown",
    "machine.temperatures",
    // Cross-device fleet health read-back on the Machine tab.
    "machine.fleet_title",
    "machine.fleet_loading",
    "machine.fleet_none",
    "machine.fleet_load_failed",
    "machine.fleet_online",
    "machine.fleet_offline",
    "machine.fleet_unnamed",
    "machine.fleet_this",
    // v0.10.1 — Alert lifecycle (macOS parity): filter, actions, states.
    "alerts.filter_open",
    "alerts.filter_resolved",
    "alerts.filter_all",
    "alerts.resolve_all",
    "alerts.all_clear",
    "alerts.all_clear_hint",
    "alerts.no_matching",
    "alerts.load_failed",
    "alerts.action_ack",
    "alerts.action_resolve",
    "alerts.action_snooze",
    "alerts.resolved_label",
    "alerts.severity_critical",
    "alerts.severity_warning",
    // v0.4.22 — Sentry diagnostic emit button in Settings → About.
    "settings.about_sentry_test_button",
    "settings.about_sentry_test_sending",
    "settings.about_sentry_test_sent",
    "settings.about_sentry_test_tooltip",
    // v0.4.22 — per-provider "synced X ago" line on Providers tab.
    "providers.synced_ago",
    "providers.synced_ago_tooltip",
    // v0.5.0 — localized time-unit short forms used by the
    // synced-ago line. Replaces the v0.4.22 hardcoded English
    // "s/min/hr/d" that VM caught reading as visually-empty in
    // zh-CN before CJK characters. Top-level so other features
    // (cost forecast last-updated, sessions list etc.) can reuse.
    "time.unit_s",
    "time.unit_min",
    "time.unit_hr",
    "time.unit_d",
    // v0.5.1 — Overview Insights row: Forecast + Risk Signals cards.
    "overview.forecast_title",
    "overview.forecast_bounds",
    "overview.forecast_based_on",
    "overview.forecast_unreliable",
    "overview.forecast_failed",
    "overview.forecast_no_data",
    "overview.risk_signals_title",
    "overview.risk_no_signals",
    "overview.risk_more_count",
    "overview.risk_signals_offline",
    "overview.risk_signals_stale",
    // v0.5.2 — TopProjectsCard. 3rd Insights-row card.
    "overview.top_projects_title",
    "overview.top_projects_unknown",
    "overview.top_projects_empty",
    "overview.top_projects_failed",
    // v0.5.4 — Settings → Danger Zone. The full list of new keys is
    // pinned because mistranslating any one of them on a destructive
    // action (e.g. dropping the Japanese 削除 phrase) would silently
    // soft-disable the type-to-confirm gate for that language.
    "settings.danger.heading",
    "settings.danger.clear_caches_title",
    "settings.danger.clear_caches_body",
    "settings.danger.clear_caches_button",
    "settings.danger.clear_caches_confirm_button",
    "settings.danger.clear_caches_processing",
    "settings.danger.clear_caches_done",
    "settings.danger.delete_account_title",
    "settings.danger.delete_account_body",
    "settings.danger.delete_account_button",
    "settings.danger.delete_account_confirm_prompt",
    "settings.danger.delete_account_confirm_button",
    "settings.danger.delete_account_processing",
    "settings.danger.delete_phrase",
    "settings.danger.action_failed",
    // v0.5.5 — Activity Timeline chart on Sessions tab. Provider
    // labels are pinned (an empty string would render as a blank
    // lane label that looks like a layout bug); empty / failed /
    // stale states share the v0.5.3 RiskSignalsCard pattern of
    // "every distinct state must have its own copy" so the user
    // never wonders whether a blank chart means "no activity" or
    // "fetch failed".
    "providers.claude_label",
    "providers.codex_label",
    "providers.cursor_label",
    "providers.copilot_label",
    "providers.gemini_label",
    "providers.openrouter_label",
    "sessions.timeline_title",
    "sessions.timeline_loading",
    "sessions.timeline_empty",
    "sessions.timeline_failed",
    "sessions.timeline_stale",
    "sessions.timeline_other_lane",
    "sessions.timeline_x_now",
    "sessions.timeline_x_now_minus",
    // v0.5.6 — Tray mini-metrics. The frontend pushes these to Rust
    // via `force_tray_menu_refresh` so the tray menu re-renders in
    // the user's app language. A missing key would silently send an
    // empty string to Rust → blank tray menu items at the next
    // language change. Pin every key.
    "tray.header_label",
    "tray.month_so_far_template",
    "tray.forecast_template",
    "tray.synced_ago_template",
    "tray.synced_never",
    "tray.not_paired",
    "tray.no_data",
    "tray.open_label",
    "tray.quit_label",
    // v0.6.0 — Remote Approvals (privacy-critical feature). The
    // consent dialog body bullets and the high-risk-blocked tooltip
    // are pinned because mistranslating them would either (a)
    // mislead users about the privacy posture they're enabling, or
    // (b) leave them confused why Approve is disabled on a row.
    "remote.title",
    "remote.empty_pending",
    "remote.disabled_hint",
    "remote.risk_low",
    "remote.risk_medium",
    "remote.risk_high",
    "remote.high_risk_blocked_tooltip",
    "remote.approve_button",
    "remote.deny_button",
    "remote.action_failed",
    "remote.error_already_decided",
    "remote.sessions_heading",
    "remote.sessions_readonly_badge",
    "remote.sessions_empty",
    // v0.6.2 — managed-session control buttons. Pinning the
    // tooltip especially because it carries security-relevant
    // copy ("Ctrl+C-equivalent — interrupt vs stop").
    "remote.session_send_button",
    "remote.session_stop_button",
    "remote.session_interrupt_button",
    "remote.session_interrupt_tooltip",
    "remote.session_prompt_placeholder",
    "remote.session_prompt_submit",
    // v0.7.0 — Claude hook installer (Settings → Privacy). Each
    // status string and button copy is pinned because the user
    // depends on accurate state to know if their hook is registered.
    "settings.hook_install_heading",
    "settings.hook_install_body",
    "settings.hook_install_status_ok",
    "settings.hook_install_status_stale",
    "settings.hook_install_status_missing",
    "settings.hook_install_install_button",
    "settings.hook_install_reinstall_button",
    "settings.hook_install_update_button",
    "settings.hook_install_done_installed",
    "settings.hook_install_done_updated",
    "settings.privacy_heading",
    "settings.privacy_body",
    "settings.privacy_toggle_label",
    "settings.privacy_status_on",
    "settings.privacy_status_off",
    "settings.privacy_consent_title",
    "settings.privacy_consent_body_b1",
    "settings.privacy_consent_body_b2",
    "settings.privacy_consent_body_b3",
    "settings.privacy_consent_enable_button",
    // v0.8.0 introduced spawn-dialog + agent-diagnostic keys; v0.8.1
    // reverted them; v0.9.2 brings them back as part of the ConPTY
    // managed-session host redo. Mistranslating the cwd help text
    // would mislead users about what gets uploaded (only the HMAC
    // fingerprint + basename — full path stays local).
    "remote.session_start_button",
    "remote.session_start_dialog_title",
    "remote.session_start_cwd_label",
    "remote.session_start_cwd_help",
    "remote.session_start_provider_label",
    "remote.session_start_submit",
    "remote.session_start_processing",
    "remote.session_start_failed",
    "remote.agent_status_heading",
    "remote.agent_status_lifetime",
    "remote.agent_status_last_tick",
    "remote.agent_status_never_ticked",
    "remote.agent_status_not_running",
    // v0.10.0 — keyboard shortcut help overlay. The shortcut bindings
    // themselves live in App.tsx's keydown handler; the overlay
    // reads these labels. A missing label would render a blank row
    // in the help dialog, which silently miscommunicates the
    // shortcut. Pin every label.
    "shortcuts.title",
    "shortcuts.rescan",
    "shortcuts.settings",
    "shortcuts.tab_overview",
    "shortcuts.tab_providers",
    "shortcuts.tab_sessions",
    "shortcuts.tab_alerts",
    "shortcuts.tab_settings",
    "shortcuts.toggle_help",
    "shortcuts.close_modal",
    "action.close",
    // v0.9.3 — Save diagnostic bundle button (Settings → About).
    // The tooltip carries the privacy posture explanation; missing
    // translation would silently leave users unsure what's in the
    // zip they're about to share. Pin every key.
    "settings.about_save_bundle_button",
    "settings.about_save_bundle_saving",
    "settings.about_save_bundle_done",
    "settings.about_save_bundle_failed",
    "settings.about_save_bundle_tooltip",
    // v0.9.0 — categorized update error messages. Each maps to a
    // specific user-actionable instruction; missing translations
    // would silently fall back to the generic message and lose the
    // category-specific guidance. Pin every key in every language.
    "updater.error_network",
    "updater.error_permissions",
    "updater.error_disk_full",
    "updater.error_path_not_found",
    "updater.error_signature",
    "updater.error_unknown",
    "updater.error_manual_download",
  ] as const;

  it.each(["en", "zh-CN", "ja"] as const)(
    "language %s has every required key non-empty",
    async (lang) => {
      const mod = await freshI18n();
      mod.setLang(lang);
      for (const key of REQUIRED_KEYS) {
        const v = mod.default.t(key);
        expect(typeof v).toBe("string");
        expect(v.length).toBeGreaterThan(0);
        // Sanity: must not return the key path verbatim (= missing translation)
        expect(v).not.toBe(key);
      }
    }
  );
});

describe("i18n plural forms (v0.4.5)", () => {
  // v0.4.5 — Providers tab strings now route through i18next plural rules
  // ("1 active day" vs "2 active days", etc.). zh-CN / ja have a single
  // form per CLDR; en has _one + _other. These tests catch regressions
  // where a plural variant gets accidentally deleted.

  it("en active_days uses singular for count=1, plural for count!=1", async () => {
    const { default: i18n } = await freshI18n();
    i18n.changeLanguage("en");
    expect(i18n.t("providers.active_days", { count: 1 })).toBe("1 active day");
    expect(i18n.t("providers.active_days", { count: 0 })).toBe("0 active days");
    expect(i18n.t("providers.active_days", { count: 4 })).toBe("4 active days");
  });

  it("en models pluralizes correctly", async () => {
    const { default: i18n } = await freshI18n();
    i18n.changeLanguage("en");
    expect(i18n.t("providers.models", { count: 1 })).toBe("1 model");
    expect(i18n.t("providers.models", { count: 3 })).toBe("3 models");
  });

  it("en messages pluralizes correctly", async () => {
    const { default: i18n } = await freshI18n();
    i18n.changeLanguage("en");
    expect(i18n.t("providers.messages", { count: 1 })).toBe("1 msg");
    expect(i18n.t("providers.messages", { count: 2 })).toBe("2 msgs");
  });

  it("zh-CN active_days uses single form for any count (CLDR: zh has only `other`)", async () => {
    const mod = await freshI18n();
    mod.setLang("zh-CN");
    expect(mod.default.t("providers.active_days", { count: 1 })).toBe("1 天活跃");
    expect(mod.default.t("providers.active_days", { count: 5 })).toBe("5 天活跃");
  });

  it("ja models uses single form for any count (CLDR: ja has only `other`)", async () => {
    const mod = await freshI18n();
    mod.setLang("ja");
    expect(mod.default.t("providers.models", { count: 1 })).toBe("1 モデル");
    expect(mod.default.t("providers.models", { count: 3 })).toBe("3 モデル");
  });
});

describe("i18n delete-phrase per-language (v0.5.4)", () => {
  // The Settings → Danger Zone delete-account flow requires the user to
  // type a localized literal phrase to enable the destructive button.
  // The frontend compares `state.typed === t("settings.danger.delete_phrase")`
  // — string equality, no fuzzy matching. These tests pin the exact
  // phrase per language so a translation drift can't silently disable
  // the gate (e.g. zh-CN dropping back to "DELETE" would still resolve,
  // but a Chinese user typing "删除" would no longer enable the button).

  it("en delete phrase resolves to literal DELETE", async () => {
    const { default: i18n } = await freshI18n();
    i18n.changeLanguage("en");
    expect(i18n.t("settings.danger.delete_phrase")).toBe("DELETE");
  });

  it("zh-CN delete phrase resolves to 删除", async () => {
    const mod = await freshI18n();
    mod.setLang("zh-CN");
    expect(mod.default.t("settings.danger.delete_phrase")).toBe("删除");
  });

  it("ja delete phrase resolves to 削除", async () => {
    const mod = await freshI18n();
    mod.setLang("ja");
    expect(mod.default.t("settings.danger.delete_phrase")).toBe("削除");
  });
});

describe("i18n number formatter (v0.4.6)", () => {
  // v0.4.6 — `{{count, number}}` runs the integer through Intl.NumberFormat
  // with the active language so 2782 renders as "2,782" instead of "2782".
  // VM 2026-05-04 flagged that v0.4.5 left numbers unformatted in the
  // plural-routed messages key. Fix: i18n.ts adds a `format` callback for
  // the `number` formatter; locale strings opt in via `{{count, number}}`.

  it("en messages key applies thousands separator for large counts", async () => {
    const { default: i18n } = await freshI18n();
    i18n.changeLanguage("en");
    expect(i18n.t("providers.messages", { count: 2782 })).toBe("2,782 msgs");
    expect(i18n.t("providers.messages", { count: 1234567 })).toBe("1,234,567 msgs");
  });

  it("zh-CN messages key applies thousands separator (CLDR: zh-CN uses comma)", async () => {
    const mod = await freshI18n();
    mod.setLang("zh-CN");
    expect(mod.default.t("providers.messages", { count: 2782 })).toBe("2,782 条消息");
  });

  it("ja messages key applies thousands separator (CLDR: ja uses comma)", async () => {
    const mod = await freshI18n();
    mod.setLang("ja");
    expect(mod.default.t("providers.messages", { count: 2782 })).toBe("2,782 メッセージ");
  });
});
