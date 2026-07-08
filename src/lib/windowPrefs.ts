// Window/appearance preferences (v0.10.1 P2). Currently just "always on top" —
// handy for a usage monitor you want kept visible over other windows. Persisted
// in localStorage and applied via the Tauri window API; kept in a standalone
// module (mirrors money.ts / dateRange.ts) so load/save is unit-tested without
// the App tree or a live Tauri window.

/** localStorage key. Namespaced like the other `cli-pulse.*` settings. */
export const ALWAYS_ON_TOP_KEY = "cli-pulse.always-on-top";

/**
 * Read the always-on-top preference. Stored as "1"/"0"; anything else (missing,
 * malformed) reads as `false` — the OS default, so a monitor never silently
 * pins itself over everything on a bad value. Never throws.
 */
export function loadAlwaysOnTop(): boolean {
  try {
    return localStorage.getItem(ALWAYS_ON_TOP_KEY) === "1";
  } catch {
    return false;
  }
}

/** Persist the preference. Best-effort — a storage denial just means it doesn't
 * survive the next launch. */
export function saveAlwaysOnTop(value: boolean): void {
  try {
    localStorage.setItem(ALWAYS_ON_TOP_KEY, value ? "1" : "0");
  } catch {
    // ignore — non-persistent still drives the current session.
  }
}
