/**
 * v1.30 F2a — warning-threshold reference markers for quota bars.
 *
 * Port of the Mac `QuotaBarMarkers` (CLIPulseCore) so a paired account sees
 * the same marker positions on both platforms. All fractions are **as-used**
 * (0 = empty, 1 = fully used); CLI Pulse quota bars render *remaining*, so a
 * renderer converts via {@link placeOnRemainingBar} — an as-used fraction `f`
 * sits at `1 − f` on a remaining bar (so a "95% used" critical marker sits
 * near the empty end of a countdown bar).
 *
 * NOTE: the *expected-pace* marker + pace text live in {@link ./pace} (v1.38
 * F1/F2b), which derives the per-tier window length from the tier name rather
 * than a wire-format `windowMinutes` (the desktop's `TierEntry` still carries
 * only a reset timestamp). This module keeps just the warning-threshold ticks.
 */

/** Mac default warning thresholds (percent used). */
export const DEFAULT_WARN_THRESHOLDS = [80, 95];

/**
 * Warning-threshold fractions (0..1, as-used) from configured percents
 * (e.g. `[80, 95]` → `[0.8, 0.95]`). Out-of-range values (≤0, ≥100) are
 * dropped; the result is de-duplicated and sorted ascending.
 */
export function warningFractions(thresholdsPercent: number[]): number[] {
  const out = new Set<number>();
  for (const p of thresholdsPercent) {
    const f = p / 100;
    if (f > 0 && f < 1) out.add(f);
  }
  return [...out].sort((a, b) => a - b);
}

/**
 * Place an as-used fraction onto a **remaining** bar: a used fraction `f`
 * fills to `1 − f` (headroom). Clamped to 0..1; non-finite input (NaN/±∞) is
 * treated as 0 so callers can safely feed the result straight into a CSS
 * `left: N%` without ever emitting `NaN%`.
 */
export function placeOnRemainingBar(usedFraction: number): number {
  const f = Number.isFinite(usedFraction)
    ? Math.max(0, Math.min(1, usedFraction))
    : 0;
  return 1 - f;
}
