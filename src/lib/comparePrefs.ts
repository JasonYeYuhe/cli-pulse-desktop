// Compare-mode preference (v0.10.2). Persists whether the "compare to previous
// period" toggle is on. Mirrors windowPrefs.ts / dateRange.ts: a tiny standalone
// module reading/writing a namespaced `cli-pulse.*` localStorage key, so load/
// save is unit-tested without the App tree. Never throws — a storage denial just
// means the choice doesn't survive the next launch, and compare defaults OFF.

/** localStorage key. Namespaced like the other `cli-pulse.*` settings. */
export const COMPARE_MODE_KEY = "cli-pulse.compare-mode";

/**
 * Read the compare-mode preference. Stored as "1"/"0"; anything else (missing,
 * malformed) reads as `false` — compare is an opt-in overlay, so a bad value
 * silently falls back to the plain view.
 */
export function loadCompareMode(): boolean {
  try {
    return localStorage.getItem(COMPARE_MODE_KEY) === "1";
  } catch {
    return false;
  }
}

/** Persist the preference. Best-effort. */
export function saveCompareMode(value: boolean): void {
  try {
    localStorage.setItem(COMPARE_MODE_KEY, value ? "1" : "0");
  } catch {
    // ignore — non-persistent still drives the current session.
  }
}
