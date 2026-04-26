import i18n from "i18next";
import { initReactI18next } from "react-i18next";

import en from "./locales/en.json";
import zhCN from "./locales/zh-CN.json";
import ja from "./locales/ja.json";

/**
 * Supported UI languages. Keep in sync with the `locales/` directory
 * and the `<select>` in Settings → Language.
 */
export const SUPPORTED_LANGS = [
  { code: "en", label: "English" },
  { code: "zh-CN", label: "简体中文" },
  { code: "ja", label: "日本語" },
] as const;

export type LangCode = (typeof SUPPORTED_LANGS)[number]["code"];

const STORAGE_KEY = "cli-pulse.lang";

function detectInitialLang(): LangCode {
  // 1. User choice stashed in localStorage (set via Settings)
  const stored = (globalThis as any).localStorage?.getItem(STORAGE_KEY);
  if (stored && SUPPORTED_LANGS.some((l) => l.code === stored)) {
    return stored as LangCode;
  }
  // 2. Browser/OS language — exact match first, then prefix
  const nav = (globalThis as any).navigator?.language as string | undefined;
  if (nav) {
    const exact = SUPPORTED_LANGS.find((l) => l.code === nav);
    if (exact) return exact.code;
    const prefix = nav.split("-")[0];
    const pre = SUPPORTED_LANGS.find((l) => l.code.split("-")[0] === prefix);
    if (pre) return pre.code;
  }
  return "en";
}

i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    "zh-CN": { translation: zhCN },
    ja: { translation: ja },
  },
  lng: detectInitialLang(),
  fallbackLng: "en",
  interpolation: { escapeValue: false },
  returnNull: false,
});

/**
 * Switch the active UI language. `i18next.changeLanguage` returns a
 * Promise that resolves once resources for `code` are loaded, but
 * because all three locales are bundled at build time (statically
 * imported above), resolution is effectively synchronous in practice.
 * We still track the Promise so any future resource-loading error
 * surfaces as a console warning instead of an unhandled rejection.
 *
 * Caller can `await` if it cares about completion (Settings panel
 * doesn't — it triggers a re-render via React state change anyway).
 */
export function setLang(code: LangCode): Promise<void> {
  // Persist BEFORE switching — if changeLanguage somehow throws, we
  // still want the choice remembered for the next launch.
  try {
    (globalThis as any).localStorage?.setItem(STORAGE_KEY, code);
  } catch {
    /* localStorage can be unavailable in weird contexts — ignore */
  }
  return Promise.resolve(i18n.changeLanguage(code))
    .then(() => undefined)
    .catch((err) => {
      // Don't propagate — language switch failures shouldn't crash the
      // app. Log so they're not silently swallowed.
      console.warn("setLang(", code, ") failed:", err);
    });
}

export default i18n;
