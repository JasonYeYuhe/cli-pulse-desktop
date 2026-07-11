// Compare mode — period-over-period window math (v0.10.2). Pure and
// timezone-safe: every date is a "YYYY-MM-DD" *local* day-key string (the
// scanner's bucket key, `DailyEntry.date`), and all boundary arithmetic runs on
// those strings via UTC-noon math so it never depends on the host clock or
// timezone. Tests inject `todayKey`, so no clock is needed. Kept in a standalone
// module (mirrors dateRange.ts / pace.ts / money.ts) so the split-window math is
// unit-tested under Vitest without mounting the App tree.
//
// The feature: given the wide baseline scan's per-day `entries`, split them into
// the *current* N-day window (ending today, inclusive) and the *previous* equal-
// length N-day window immediately before it, then diff each metric. The baseline
// scan spans a fixed 180 days (see `scan_usage_baseline` in Rust), which is
// exactly `2 × COMPARE_MAX_WINDOW_DAYS`, so any comparable range fits.

/** The subset of `DailyEntry` this module needs — one per (date, provider,
 *  model) bucket. Cached tokens are intentionally excluded from `tokens`
 *  (the cross-platform "ioTokens" convention: input + output, excluding cached). */
export type CompareEntry = {
  date: string; // "YYYY-MM-DD", local TZ
  provider: string;
  model: string;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number | null;
  message_count: number;
};

/** Totals for one window. `msgs` counts only the Claude message bucket; `cost`
 *  and `tokens` come from the non-bucket entries (matching the app-wide split). */
export type PeriodTotals = {
  cost: number;
  tokens: number; // input + output, excluding cached
  msgs: number;
};

/** Current + previous totals for a comparison. */
export type PeriodPair = { current: PeriodTotals; previous: PeriodTotals };

/** The largest current-window length for which an equal previous window still
 *  fits inside the scanner's 180-day cap (2 × 90). Compare mode is unavailable
 *  for wider custom ranges — you can't get an apples-to-apples prior window. */
export const COMPARE_MAX_WINDOW_DAYS = 90;

/** Options for the compare functions. `msgBucket` is the message-count sentinel
 *  model (the App's `CLAUDE_MSG_BUCKET`); passed in so this module stays free of
 *  App imports and is trivially testable. */
export type CompareOptions = {
  todayKey: string;
  windowDays: number;
  msgBucket: string;
};

/** Whether compare mode can produce an equal previous window for this range. */
export function compareAvailable(windowDays: number): boolean {
  return Number.isFinite(windowDays) && windowDays >= 1 && windowDays <= COMPARE_MAX_WINDOW_DAYS;
}

/** Add `delta` days to a "YYYY-MM-DD" key, returning a new "YYYY-MM-DD" key.
 *  Timezone-safe: the date is built at *UTC noon* so neither DST transitions nor
 *  the host timezone can shift the calendar day. Malformed input is returned
 *  unchanged (defensive — real keys come from the scanner and are always valid). */
export function shiftDayKey(dayKey: string, delta: number): string {
  const parts = dayKey.split("-");
  if (parts.length !== 3) return dayKey;
  const y = Number(parts[0]);
  const m = Number(parts[1]);
  const d = Number(parts[2]);
  if (!Number.isFinite(y) || !Number.isFinite(m) || !Number.isFinite(d)) return dayKey;
  const t = Date.UTC(y, m - 1, d, 12, 0, 0) + delta * 86_400_000;
  const dt = new Date(t);
  const yy = dt.getUTCFullYear();
  const mm = String(dt.getUTCMonth() + 1).padStart(2, "0");
  const dd = String(dt.getUTCDate()).padStart(2, "0");
  return `${yy}-${mm}-${dd}`;
}

/** Inclusive [from, to] window boundaries as day-key strings.
 *  - current  = the N days ending today, inclusive:      [today-(N-1) .. today]
 *  - previous = the N days immediately before that:      [today-(2N-1) .. today-N] */
export type Windows = {
  curFrom: string;
  curTo: string;
  prevFrom: string;
  prevTo: string;
};

export function computeWindows(todayKey: string, windowDays: number): Windows {
  const n = Math.max(1, Math.floor(windowDays));
  return {
    curTo: todayKey,
    curFrom: shiftDayKey(todayKey, -(n - 1)),
    prevTo: shiftDayKey(todayKey, -n),
    prevFrom: shiftDayKey(todayKey, -(2 * n - 1)),
  };
}

/** Lexicographic inclusive range test — valid for zero-padded "YYYY-MM-DD". */
function inWindow(day: string, from: string, to: string): boolean {
  return day >= from && day <= to;
}

function addEntry(t: PeriodTotals, e: CompareEntry, msgBucket: string): void {
  if (e.model === msgBucket) {
    t.msgs += e.message_count;
    return;
  }
  t.tokens += e.input_tokens + e.output_tokens;
  t.cost += e.cost_usd ?? 0;
}

function emptyTotals(): PeriodTotals {
  return { cost: 0, tokens: 0, msgs: 0 };
}

/** Whole-account current-vs-previous totals over the two windows. */
export function comparePeriods(entries: CompareEntry[], opts: CompareOptions): PeriodPair {
  const w = computeWindows(opts.todayKey, opts.windowDays);
  const current = emptyTotals();
  const previous = emptyTotals();
  for (const e of entries) {
    if (inWindow(e.date, w.curFrom, w.curTo)) addEntry(current, e, opts.msgBucket);
    else if (inWindow(e.date, w.prevFrom, w.prevTo)) addEntry(previous, e, opts.msgBucket);
  }
  return { current, previous };
}

/** Per-provider current-vs-previous totals. Keyed by `provider`. */
export function comparePeriodsByProvider(
  entries: CompareEntry[],
  opts: CompareOptions
): Map<string, PeriodPair> {
  const w = computeWindows(opts.todayKey, opts.windowDays);
  const out = new Map<string, PeriodPair>();
  const rec = (provider: string): PeriodPair => {
    let r = out.get(provider);
    if (!r) {
      r = { current: emptyTotals(), previous: emptyTotals() };
      out.set(provider, r);
    }
    return r;
  };
  for (const e of entries) {
    if (inWindow(e.date, w.curFrom, w.curTo)) addEntry(rec(e.provider).current, e, opts.msgBucket);
    else if (inWindow(e.date, w.prevFrom, w.prevTo))
      addEntry(rec(e.provider).previous, e, opts.msgBucket);
  }
  return out;
}

/** A directional percentage change. `pct` is signed; it is `null` in the "new"
 *  case (previous window was zero but the current is not — an infinite/undefined
 *  ratio) so the UI shows "new" instead of a nonsense number. */
export type Delta = {
  pct: number | null;
  direction: "up" | "down" | "flat";
  isNew: boolean;
};

/** Percent change from `previous` to `current`, with safe divide-by-zero:
 *  - previous > 0            → signed percentage
 *  - previous == 0, current > 0 → "new" (pct null, direction up)
 *  - both zero               → flat 0% */
export function computeDelta(current: number, previous: number): Delta {
  const cur = Number.isFinite(current) ? current : 0;
  const prev = Number.isFinite(previous) ? previous : 0;
  if (prev === 0) {
    if (cur === 0) return { pct: 0, direction: "flat", isNew: false };
    return { pct: null, direction: "up", isNew: true };
  }
  const pct = ((cur - prev) / prev) * 100;
  const direction = pct > 0 ? "up" : pct < 0 ? "down" : "flat";
  return { pct, direction, isNew: false };
}

/** Rounded percent magnitude for a badge, e.g. `12.4 → "12%"`. Returns `null`
 *  for the "new" case (no finite baseline). Caps absurd ratios at "999%+" so a
 *  first-time spike doesn't render a ten-digit badge. */
export function formatDeltaPercent(delta: Delta): string | null {
  if (delta.isNew || delta.pct === null) return null;
  const p = Math.abs(Math.round(delta.pct));
  // A non-zero change that rounds to 0% would contradict the ▲/▼ arrow the badge
  // renders from `direction` (e.g. "▲ 0%" for +0.4%). Show "<1%" instead.
  if (p === 0 && delta.direction !== "flat") return "<1%";
  return p >= 1000 ? "999%+" : `${p}%`;
}
