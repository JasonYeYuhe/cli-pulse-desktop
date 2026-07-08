// Activity strip + usage streaks for the Overview tab (learned from
// javis603/token-monitor's home-screen heatmap + streaks). Pure, timezone-safe
// logic over the LOCAL scan's daily entries, so it works unpaired and is
// unit-testable with an injected `from` date.

import { lastNLocalDates } from "./format";

/** One day in the activity window. `tokens` drives the heat intensity. */
export type DayActivity = {
  date: string;
  tokens: number;
  active: boolean;
};

/** Minimal shape we read off a scan entry (`ScanResult.entries[i]`). */
export type ActivityEntry = {
  date: string;
  input_tokens: number;
  cached_tokens: number;
  output_tokens: number;
  message_count: number;
};

/** Sum tokens + message counts per local date. */
export function aggregateByDate(
  entries: ReadonlyArray<ActivityEntry>,
): Map<string, { tokens: number; msgs: number }> {
  const map = new Map<string, { tokens: number; msgs: number }>();
  for (const e of entries) {
    const cur = map.get(e.date) ?? { tokens: 0, msgs: 0 };
    cur.tokens += (e.input_tokens || 0) + (e.cached_tokens || 0) + (e.output_tokens || 0);
    cur.msgs += e.message_count || 0;
    map.set(e.date, cur);
  }
  return map;
}

/**
 * Dense day-by-day activity for the last `days` local days ending at `from`
 * (inclusive), zero-filling days with no entries. A day counts as `active` when
 * it has any tokens OR any messages (so a Claude-only day, whose tokens live in
 * a separate message bucket, still counts).
 */
export function buildActivity(
  entries: ReadonlyArray<ActivityEntry>,
  days: number,
  from: Date = new Date(),
): DayActivity[] {
  const agg = aggregateByDate(entries);
  return lastNLocalDates(days, from).map((date) => {
    const a = agg.get(date);
    return {
      date,
      tokens: a?.tokens ?? 0,
      active: !!a && (a.tokens > 0 || a.msgs > 0),
    };
  });
}

/**
 * Streaks over the window (which is ascending, oldest→newest):
 * - `longest`: the longest run of consecutive active days anywhere.
 * - `current`: consecutive active days ending at the most recent day, with a
 *   one-day grace for an inactive "today" (an unfinished today shouldn't zero a
 *   live streak — it falls back to ending at yesterday).
 */
export function computeStreaks(
  activity: ReadonlyArray<DayActivity>,
): { current: number; longest: number } {
  let longest = 0;
  let run = 0;
  for (const d of activity) {
    if (d.active) {
      run += 1;
      if (run > longest) longest = run;
    } else {
      run = 0;
    }
  }

  let i = activity.length - 1;
  // Grace: if the most recent day (today) is inactive, start from yesterday.
  if (i >= 0 && !activity[i].active) i -= 1;
  let current = 0;
  for (; i >= 0; i -= 1) {
    if (activity[i].active) current += 1;
    else break;
  }

  return { current, longest };
}

/**
 * Prompt-cache hit rate from raw token sums: `cached / (input + cached)` as a
 * percentage, or `null` when there are no input+cached tokens (avoids 0/0). The
 * single source of the cache-rate formula — used account-wide (`cacheHitRate`)
 * and per-provider (the Overview breakdown). Tolerates NaN/undefined sums.
 */
export function cacheHitRateOf(input: number, cached: number): number | null {
  const i = input || 0;
  const c = cached || 0;
  const denom = i + c;
  return denom > 0 ? (c / denom) * 100 : null;
}

/**
 * Prompt-cache hit rate over the window: `cached / (input + cached)` as a
 * percentage. `null` when there are no input+cached tokens (avoids 0/0) — e.g.
 * a window whose only rows are Claude message buckets with no token detail.
 */
export function cacheHitRate(entries: ReadonlyArray<ActivityEntry>): number | null {
  let cached = 0;
  let input = 0;
  for (const e of entries) {
    cached += e.cached_tokens || 0;
    input += e.input_tokens || 0;
  }
  return cacheHitRateOf(input, cached);
}

/**
 * Heat level 0–4 for a day, relative to the window's busiest day. 0 = no
 * activity; 1–4 are quartile-ish buckets of `tokens / max`. An active day with
 * 0 tokens (message-only) still shows as level 1 so it's visible.
 */
export function activityLevel(tokens: number, max: number, active: boolean): 0 | 1 | 2 | 3 | 4 {
  if (!active) return 0;
  if (max <= 0 || tokens <= 0) return 1;
  const frac = tokens / max;
  if (frac >= 0.75) return 4;
  if (frac >= 0.5) return 3;
  if (frac >= 0.25) return 2;
  return 1;
}
