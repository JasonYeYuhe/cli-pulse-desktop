// Scan-window ("date range") selector for the Overview + Providers tabs
// (v0.10.1 — the deferred v0.10.0 date-range item). The Rust scanner already
// parametrizes its window (`scan_usage(days)`, clamped 1..=180 server-side);
// this module only persists the user's chosen window length and mirrors that
// clamp on the client so the two never disagree.
//
// Kept in a standalone module (mirrors format.ts / money.ts / providerVisibility.ts)
// so the load / save / clamp math is unit-tested under Vitest without mounting
// the App tree.

/** localStorage key. Namespaced like the `cli-pulse.display-currency` key. */
export const DATE_RANGE_KEY = "cli-pulse.date-range-days";

/** Smallest window the backend accepts (`scan_usage` clamps `days.clamp(1, 180)`). */
export const MIN_DAYS = 1;
/** Largest window the backend accepts. Keep in lock-step with `scan_usage`. */
export const MAX_DAYS = 180;
/** Default window — unchanged from the historical hard-coded `days: 30`. */
export const DEFAULT_DAYS = 30;

/**
 * The one-tap presets offered in the selector. "Custom" is any value that
 * isn't one of these (the user types an arbitrary 1..180 day count). 30 stays
 * the middle/default so the common case is a single click.
 */
export const RANGE_PRESETS: readonly number[] = [7, 30, 90];

/**
 * Coerce an arbitrary number into a valid window length: floor to an integer
 * and clamp into [MIN_DAYS, MAX_DAYS]. Non-finite / NaN input (e.g. a blank
 * custom field) collapses to DEFAULT_DAYS so a scan is never issued with a
 * garbage window. Pure — the single source of window-math truth.
 */
export function clampDays(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_DAYS;
  const floored = Math.floor(n);
  if (floored < MIN_DAYS) return MIN_DAYS;
  if (floored > MAX_DAYS) return MAX_DAYS;
  return floored;
}

/** True when `days` is one of the one-tap presets (vs. a custom value). */
export function isPreset(days: number): boolean {
  return RANGE_PRESETS.includes(days);
}

/**
 * Read the persisted window length. Tolerant of every failure mode — missing
 * key, non-numeric payload, or an out-of-range value all resolve to a valid
 * clamped window (defaulting to DEFAULT_DAYS). Never throws.
 */
export function loadRangeDays(): number {
  try {
    const raw = localStorage.getItem(DATE_RANGE_KEY);
    if (!raw) return DEFAULT_DAYS;
    const parsed = Number.parseInt(raw, 10);
    if (Number.isNaN(parsed)) return DEFAULT_DAYS;
    return clampDays(parsed);
  } catch {
    return DEFAULT_DAYS;
  }
}

/**
 * Persist the chosen window length (clamped first, so storage never holds an
 * out-of-range value). Best-effort: a storage-quota error or private-mode
 * denial is swallowed — the in-memory choice still drives this session.
 */
export function saveRangeDays(days: number): void {
  try {
    localStorage.setItem(DATE_RANGE_KEY, String(clampDays(days)));
  } catch {
    // ignore — a non-persistent window choice still beats crashing.
  }
}
