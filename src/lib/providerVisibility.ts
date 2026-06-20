// Per-provider visibility filter for the Providers tab (v0.10.1).
//
// Users with several paired providers want to mute the ones they don't
// track. We persist the *hidden* set (not the shown set) so a provider
// that only starts reporting usage later shows up by default rather
// than being silently filtered out — the opt-out only ever applies to
// providers the user explicitly hid.
//
// Kept in a standalone module (mirrors format.ts) so the load / save /
// toggle logic is unit-tested under Vitest without mounting the App
// tree.

/** localStorage key. Namespaced like the i18n `cli-pulse.lang` key. */
export const HIDDEN_PROVIDERS_KEY = "cli-pulse.hidden-providers";

/**
 * Read the hidden-provider set from localStorage. Tolerant of every
 * failure mode — missing key, malformed JSON, a non-array payload, or
 * non-string entries all collapse to an empty set (fail open: show
 * everything rather than hide based on garbage state). Never throws.
 */
export function loadHiddenProviders(): Set<string> {
  try {
    const raw = localStorage.getItem(HIDDEN_PROVIDERS_KEY);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((p): p is string => typeof p === "string"));
  } catch {
    return new Set();
  }
}

/**
 * Persist the hidden-provider set as a JSON string array. Best-effort:
 * a storage-quota error or a privacy-mode denial is swallowed (the
 * in-memory state still drives the current session's UI).
 */
export function saveHiddenProviders(hidden: Set<string>): void {
  try {
    localStorage.setItem(HIDDEN_PROVIDERS_KEY, JSON.stringify([...hidden]));
  } catch {
    // ignore — non-persistent visibility still beats crashing the tab.
  }
}

/**
 * Pure toggle: return a NEW set with `provider` flipped in/out. Does
 * not mutate the input (React state must never be mutated in place) and
 * does not persist — the caller decides when to save.
 */
export function toggleHiddenProvider(
  hidden: Set<string>,
  provider: string,
): Set<string> {
  const next = new Set(hidden);
  if (next.has(provider)) next.delete(provider);
  else next.add(provider);
  return next;
}
