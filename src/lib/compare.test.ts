import { describe, it, expect } from "vitest";
import {
  compareAvailable,
  comparePeriods,
  comparePeriodsByProvider,
  computeDelta,
  computeWindows,
  formatDeltaPercent,
  shiftDayKey,
  COMPARE_MAX_WINDOW_DAYS,
  type CompareEntry,
} from "./compare";

const MSG_BUCKET = "__claude_msg__";

// Helper to build a per-day entry with sensible defaults.
function entry(p: Partial<CompareEntry> & { date: string }): CompareEntry {
  return {
    provider: "Claude",
    model: "claude-sonnet",
    input_tokens: 0,
    output_tokens: 0,
    cost_usd: 0,
    message_count: 0,
    ...p,
  };
}

describe("shiftDayKey", () => {
  it("adds and subtracts days within a month", () => {
    expect(shiftDayKey("2026-07-11", -1)).toBe("2026-07-10");
    expect(shiftDayKey("2026-07-11", 0)).toBe("2026-07-11");
    expect(shiftDayKey("2026-07-11", 3)).toBe("2026-07-14");
  });

  it("crosses month and year boundaries", () => {
    expect(shiftDayKey("2026-07-01", -1)).toBe("2026-06-30");
    expect(shiftDayKey("2026-01-01", -1)).toBe("2025-12-31");
    expect(shiftDayKey("2026-12-31", 1)).toBe("2027-01-01");
  });

  it("handles a leap day (2028 is a leap year)", () => {
    expect(shiftDayKey("2028-02-28", 1)).toBe("2028-02-29");
    expect(shiftDayKey("2028-03-01", -1)).toBe("2028-02-29");
    // 2026 is not a leap year.
    expect(shiftDayKey("2026-02-28", 1)).toBe("2026-03-01");
  });

  it("is timezone-safe: a 30-day shift never lands off-by-one regardless of host TZ", () => {
    // UTC-noon arithmetic means no DST/host-offset can shift the calendar day.
    expect(shiftDayKey("2026-03-08", 1)).toBe("2026-03-09"); // US DST spring-forward date
    expect(shiftDayKey("2026-11-01", 1)).toBe("2026-11-02"); // US DST fall-back date
    expect(shiftDayKey("2026-07-11", -30)).toBe("2026-06-11");
  });

  it("returns malformed input unchanged (defensive)", () => {
    expect(shiftDayKey("not-a-date", 5)).toBe("not-a-date");
    expect(shiftDayKey("2026-07", 1)).toBe("2026-07");
  });
});

describe("computeWindows", () => {
  it("produces equal-length, adjacent, non-overlapping current/previous windows", () => {
    const w = computeWindows("2026-07-11", 30);
    // current: the 30 days ending today inclusive
    expect(w.curTo).toBe("2026-07-11");
    expect(w.curFrom).toBe("2026-06-12");
    // previous: the 30 days immediately before
    expect(w.prevTo).toBe("2026-06-11");
    expect(w.prevFrom).toBe("2026-05-13");
    // adjacency: prevTo is exactly the day before curFrom
    expect(shiftDayKey(w.prevTo, 1)).toBe(w.curFrom);
  });

  it("N=7 windows span 7 days each", () => {
    const w = computeWindows("2026-07-11", 7);
    expect(w.curFrom).toBe("2026-07-05"); // 7 days: Jul 5..11
    expect(w.prevTo).toBe("2026-07-04");
    expect(w.prevFrom).toBe("2026-06-28"); // 7 days: Jun 28..Jul 4
  });

  it("N=90 (max) previous window bottoms out within the 180-day baseline", () => {
    const w = computeWindows("2026-07-11", 90);
    // 2*90 - 1 = 179 days back — inside a 180-day scan (today included).
    expect(w.prevFrom).toBe(shiftDayKey("2026-07-11", -179));
  });
});

describe("comparePeriods", () => {
  const opts = { todayKey: "2026-07-11", windowDays: 30, msgBucket: MSG_BUCKET };

  it("buckets entries into current vs previous and sums cost + tokens", () => {
    const entries: CompareEntry[] = [
      entry({ date: "2026-07-10", input_tokens: 100, output_tokens: 50, cost_usd: 2 }), // current
      entry({ date: "2026-06-12", input_tokens: 10, output_tokens: 5, cost_usd: 0.5 }), // current (edge)
      entry({ date: "2026-06-11", input_tokens: 200, output_tokens: 100, cost_usd: 4 }), // previous (edge)
      entry({ date: "2026-05-13", input_tokens: 1, output_tokens: 1, cost_usd: 0.1 }), // previous (edge)
      entry({ date: "2026-05-12", input_tokens: 999, output_tokens: 999, cost_usd: 99 }), // out of range
    ];
    const { current, previous } = comparePeriods(entries, opts);
    expect(current.tokens).toBe(165); // 150 + 15
    expect(current.cost).toBeCloseTo(2.5);
    expect(previous.tokens).toBe(302); // 300 + 2
    expect(previous.cost).toBeCloseTo(4.1);
  });

  it("excludes cached tokens (only input+output count) and treats the msg bucket separately", () => {
    const entries: CompareEntry[] = [
      entry({ date: "2026-07-05", input_tokens: 100, output_tokens: 20, cost_usd: 1 }),
      // message-count bucket: contributes msgs only, never tokens/cost
      entry({ date: "2026-07-05", model: MSG_BUCKET, message_count: 7, cost_usd: 999 }),
    ];
    const { current } = comparePeriods(entries, opts);
    expect(current.tokens).toBe(120);
    expect(current.cost).toBeCloseTo(1); // the 999 in the msg bucket is ignored
    expect(current.msgs).toBe(7);
  });

  it("treats a null cost as zero", () => {
    const entries: CompareEntry[] = [
      entry({ date: "2026-07-05", input_tokens: 5, output_tokens: 5, cost_usd: null }),
    ];
    const { current } = comparePeriods(entries, opts);
    expect(current.cost).toBe(0);
    expect(current.tokens).toBe(10);
  });
});

describe("comparePeriodsByProvider", () => {
  it("keeps per-provider current/previous totals independent", () => {
    const opts = { todayKey: "2026-07-11", windowDays: 7, msgBucket: MSG_BUCKET };
    const entries: CompareEntry[] = [
      entry({ date: "2026-07-10", provider: "Claude", input_tokens: 100, output_tokens: 0, cost_usd: 2 }),
      entry({ date: "2026-07-01", provider: "Claude", input_tokens: 40, output_tokens: 0, cost_usd: 1 }), // prev window
      entry({ date: "2026-07-10", provider: "Codex", input_tokens: 10, output_tokens: 10, cost_usd: 0 }),
    ];
    const byProv = comparePeriodsByProvider(entries, opts);
    expect(byProv.get("Claude")!.current.cost).toBeCloseTo(2);
    expect(byProv.get("Claude")!.current.tokens).toBe(100);
    expect(byProv.get("Claude")!.previous.cost).toBeCloseTo(1);
    expect(byProv.get("Codex")!.current.tokens).toBe(20);
    expect(byProv.get("Codex")!.previous.tokens).toBe(0);
  });

  it("excludes the msg bucket from a provider's cost/tokens (msgs only)", () => {
    const opts = { todayKey: "2026-07-11", windowDays: 7, msgBucket: MSG_BUCKET };
    const byProv = comparePeriodsByProvider(
      [
        entry({ date: "2026-07-10", provider: "Claude", input_tokens: 100, output_tokens: 20, cost_usd: 1 }),
        entry({ date: "2026-07-10", provider: "Claude", model: MSG_BUCKET, message_count: 7, cost_usd: 999 }),
      ],
      opts
    );
    const claude = byProv.get("Claude")!;
    expect(claude.current.tokens).toBe(120); // msg bucket contributes no tokens
    expect(claude.current.cost).toBeCloseTo(1); // the 999 in the msg bucket is ignored
    expect(claude.current.msgs).toBe(7);
  });
});

describe("computeDelta", () => {
  it("computes a signed percentage when the previous window is non-zero", () => {
    expect(computeDelta(120, 100)).toMatchObject({ direction: "up", isNew: false });
    expect(computeDelta(120, 100).pct).toBeCloseTo(20);
    expect(computeDelta(80, 100)).toMatchObject({ direction: "down", isNew: false });
    expect(computeDelta(80, 100).pct).toBeCloseTo(-20);
    expect(computeDelta(100, 100)).toMatchObject({ direction: "flat", pct: 0, isNew: false });
  });

  it("flags 'new' when previous is zero but current is not (avoids +Infinity%)", () => {
    const d = computeDelta(50, 0);
    expect(d.isNew).toBe(true);
    expect(d.pct).toBeNull();
    expect(d.direction).toBe("up");
  });

  it("is flat when both windows are zero", () => {
    expect(computeDelta(0, 0)).toMatchObject({ direction: "flat", pct: 0, isNew: false });
  });

  it("current=0, previous>0 is a full -100% drop", () => {
    const d = computeDelta(0, 40);
    expect(d.direction).toBe("down");
    expect(d.pct).toBeCloseTo(-100);
  });

  it("coerces non-finite inputs to zero", () => {
    expect(computeDelta(Number.NaN, 100).direction).toBe("down"); // treated as 0 vs 100
    expect(computeDelta(50, Number.NaN)).toMatchObject({ isNew: true }); // 50 vs 0
  });
});

describe("formatDeltaPercent", () => {
  it("rounds the magnitude and appends a percent sign", () => {
    expect(formatDeltaPercent(computeDelta(112, 100))).toBe("12%");
    expect(formatDeltaPercent(computeDelta(88, 100))).toBe("12%"); // magnitude, unsigned
    expect(formatDeltaPercent(computeDelta(100, 100))).toBe("0%");
  });

  it("returns null for the 'new' case", () => {
    expect(formatDeltaPercent(computeDelta(10, 0))).toBeNull();
  });

  it("caps absurd ratios at 999%+", () => {
    expect(formatDeltaPercent(computeDelta(20000, 100))).toBe("999%+");
  });

  it("shows <1% for a nonzero change that rounds to 0 (never a direction-contradicting 0%)", () => {
    // +0.4% is "up" but Math.round → 0; must not read "0%" beside an up-arrow.
    expect(formatDeltaPercent(computeDelta(1004, 1000))).toBe("<1%");
    // -0.4% likewise.
    expect(formatDeltaPercent(computeDelta(996, 1000))).toBe("<1%");
    // exactly flat still reads 0%.
    expect(formatDeltaPercent(computeDelta(1000, 1000))).toBe("0%");
  });
});

describe("compareAvailable", () => {
  it("is true for 1..90 and false beyond", () => {
    expect(compareAvailable(1)).toBe(true);
    expect(compareAvailable(30)).toBe(true);
    expect(compareAvailable(COMPARE_MAX_WINDOW_DAYS)).toBe(true);
    expect(compareAvailable(91)).toBe(false);
    expect(compareAvailable(180)).toBe(false);
    expect(compareAvailable(0)).toBe(false);
    expect(compareAvailable(Number.NaN)).toBe(false);
  });
});
