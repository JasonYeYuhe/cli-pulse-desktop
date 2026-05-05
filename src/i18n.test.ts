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
    // v0.4.20 — per-provider error badge on Providers tab.
    "providers.error_badge",
    "providers.error_tooltip",
    // v0.4.22 — Sentry diagnostic emit button in Settings → About.
    "settings.about_sentry_test_button",
    "settings.about_sentry_test_sending",
    "settings.about_sentry_test_sent",
    "settings.about_sentry_test_tooltip",
    // v0.4.22 — per-provider "synced X ago" line on Providers tab.
    "providers.synced_ago",
    "providers.synced_ago_tooltip",
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
