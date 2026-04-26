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
