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

export function setLang(code: LangCode) {
  i18n.changeLanguage(code);
  try {
    (globalThis as any).localStorage?.setItem(STORAGE_KEY, code);
  } catch {
    /* localStorage can be unavailable in weird contexts — ignore */
  }
}

export default i18n;
