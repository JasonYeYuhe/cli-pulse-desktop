// Usage-pace ("on track / ahead of pace") for the Providers-tab quota bars
// (v1.30 F1/F2b parity). The Mac app draws an expected-pace marker + pace text
// per tier from a per-collector `windowMinutes`. The desktop's tier data
// (server `provider_summary`) only carries the reset *end* timestamp
// (`reset_time`) and the fill percentage — the window LENGTH is encoded only in
// the human tier name ("5h Window", "4-hour", "Weekly"). Rather than a
// shared-schema change to ship `windowMinutes` on the wire, we derive it
// client-side from that name, EXACTLY for recognizable fixed windows and
// `null` for everything else (calendar-month, unnamed windows, tier nicknames).
//
// This deliberately never falls back to a default window: the Mac's C3 fix
// established that an assumed window produces a wrong pace, so we show no pace
// at all rather than a misleading one. Pure + timezone-safe (an injected `now`
// in ms), so it's fully unit-tested without a clock or the App tree.

export type PaceStatus = "ahead" | "on_track" | "under";

export type PaceResult = {
  /** Fraction of the tier's quota consumed, clamped to 0..1. */
  usedFraction: number;
  /** Fraction of the reset window elapsed at `now`, clamped to 0..1. */
  expectedFraction: number;
  /**
   * "ahead"    — using faster than time is passing (will exhaust early),
   * "under"    — using slower than time (comfortable headroom),
   * "on_track" — within the tolerance band of the expected pace.
   */
  status: PaceStatus;
};

function clamp01(n: number): number {
  if (!Number.isFinite(n)) return 0;
  if (n < 0) return 0;
  if (n > 1) return 1;
  return n;
}

/**
 * Derive a tier's reset-window length in MINUTES from its display name, or
 * `null` when the name doesn't unambiguously encode a fixed duration.
 *
 * Recognized:
 *   - an explicit hour count anywhere in the name — "5h Window", "4-hour",
 *     "12 hrs" → N × 60 (the `\b` guard stops it matching "…heavy" etc.);
 *   - the standalone window words "hourly" / "daily" / "weekly".
 * NOT recognized (→ null): "Monthly" (calendar length varies — no reliable
 * linear pace), tier nicknames that merely contain a word ("Daily Routines"
 * ≠ "Daily"), and anything else ("Sonnet only", "Designs").
 */
export function windowMinutesForTier(name: string): number | null {
  if (!name) return null;
  const hours = name.match(/(\d+)\s*-?\s*h(?:ours?|rs?|r)?\b/i);
  if (hours) {
    const n = Number.parseInt(hours[1], 10);
    if (Number.isFinite(n) && n > 0) return n * 60;
  }
  switch (name.trim().toLowerCase()) {
    case "hourly":
      return 60;
    case "daily":
      return 24 * 60;
    case "weekly":
      return 7 * 24 * 60;
    default:
      return null;
  }
}

/**
 * Compare usage against a linear expected pace over the reset window. Returns
 * `null` (no pace shown) when the window length is unknown, the reset timestamp
 * is missing/unparseable, or `now` is not finite — i.e. whenever a pace would
 * be a guess. `windowStart = resetTimeMs − windowMinutes`, and the expected
 * fraction is the share of that window elapsed at `now`.
 */
export function computePace(params: {
  usedFraction: number;
  windowMinutes: number | null;
  resetTimeMs: number | null;
  nowMs: number;
  /** Half-width of the "on track" band, as a fraction (default 0.05 = 5pp). */
  toleranceFraction?: number;
}): PaceResult | null {
  const { usedFraction, windowMinutes, resetTimeMs, nowMs } = params;
  const tol = params.toleranceFraction ?? 0.05;
  if (windowMinutes == null || !(windowMinutes > 0)) return null;
  if (resetTimeMs == null || !Number.isFinite(resetTimeMs)) return null;
  if (!Number.isFinite(nowMs)) return null;

  const windowMs = windowMinutes * 60_000;
  const startMs = resetTimeMs - windowMs;
  const expectedFraction = clamp01((nowMs - startMs) / windowMs);
  const used = clamp01(usedFraction);

  const diff = used - expectedFraction;
  const status: PaceStatus =
    diff > tol ? "ahead" : diff < -tol ? "under" : "on_track";
  return { usedFraction: used, expectedFraction, status };
}

/** Parse an RFC3339 reset timestamp to epoch ms, or `null` if absent/invalid. */
export function parseResetMs(resetTime: string | null | undefined): number | null {
  if (!resetTime) return null;
  const ms = Date.parse(resetTime);
  return Number.isNaN(ms) ? null : ms;
}
