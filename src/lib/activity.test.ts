import { describe, expect, it } from "vitest";
import {
  activityLevel,
  aggregateByDate,
  buildActivity,
  cacheHitRate,
  computeStreaks,
  type ActivityEntry,
  type DayActivity,
} from "./activity";

function entry(date: string, tokens: number, msgs = 0, cached = 0): ActivityEntry {
  return {
    date,
    input_tokens: tokens,
    cached_tokens: cached,
    output_tokens: 0,
    message_count: msgs,
  };
}

// A fixed local "today" so the window is deterministic regardless of test TZ.
const FROM = new Date(2026, 6, 7); // 2026-07-07 local

describe("aggregateByDate", () => {
  it("sums tokens and messages per date", () => {
    const agg = aggregateByDate([
      entry("2026-07-06", 100),
      entry("2026-07-06", 50, 2),
      entry("2026-07-07", 0, 5),
    ]);
    expect(agg.get("2026-07-06")).toEqual({ tokens: 150, msgs: 2 });
    expect(agg.get("2026-07-07")).toEqual({ tokens: 0, msgs: 5 });
  });
});

describe("buildActivity", () => {
  it("zero-fills the window and marks active days (tokens or messages)", () => {
    const act = buildActivity([entry("2026-07-05", 200), entry("2026-07-07", 0, 3)], 3, FROM);
    expect(act.map((d) => d.date)).toEqual(["2026-07-05", "2026-07-06", "2026-07-07"]);
    expect(act.map((d) => d.active)).toEqual([true, false, true]); // msg-only day is active
    expect(act[1]).toEqual({ date: "2026-07-06", tokens: 0, active: false });
  });
});

describe("computeStreaks", () => {
  const a = (active: boolean): DayActivity => ({ date: "x", tokens: active ? 1 : 0, active });

  it("counts current streak ending today", () => {
    expect(computeStreaks([a(true), a(true), a(true)])).toEqual({ current: 3, longest: 3 });
  });

  it("gives a one-day grace when today is inactive", () => {
    // today inactive, but the two prior days form a streak ending yesterday.
    expect(computeStreaks([a(true), a(true), a(false)])).toEqual({ current: 2, longest: 2 });
  });

  it("zeroes current when today and yesterday are both inactive", () => {
    expect(computeStreaks([a(true), a(false), a(false)])).toEqual({ current: 0, longest: 1 });
  });

  it("longest is the max run anywhere in the window", () => {
    expect(computeStreaks([a(true), a(true), a(false), a(true)])).toEqual({ current: 1, longest: 2 });
  });

  it("empty window is zero/zero", () => {
    expect(computeStreaks([])).toEqual({ current: 0, longest: 0 });
  });
});

describe("cacheHitRate", () => {
  it("computes cached / (input + cached) as a percentage", () => {
    // input 300 + cached 700 → 70% hit.
    expect(cacheHitRate([entry("2026-07-07", 300, 0, 700)])).toBeCloseTo(70);
  });
  it("sums across entries", () => {
    const rate = cacheHitRate([entry("d1", 100, 0, 100), entry("d2", 300, 0, 500)]);
    expect(rate).toBeCloseTo((600 / 1000) * 100); // 60%
  });
  it("is null when there are no input+cached tokens", () => {
    expect(cacheHitRate([])).toBeNull();
    expect(cacheHitRate([entry("d1", 0, 5, 0)])).toBeNull(); // message-only day
  });
});

describe("activityLevel", () => {
  it("returns 0 for inactive days", () => {
    expect(activityLevel(0, 1000, false)).toBe(0);
  });
  it("returns 1 for an active but token-less (message-only) day", () => {
    expect(activityLevel(0, 1000, true)).toBe(1);
  });
  it("buckets by fraction of the busiest day", () => {
    expect(activityLevel(1000, 1000, true)).toBe(4);
    expect(activityLevel(600, 1000, true)).toBe(3);
    expect(activityLevel(300, 1000, true)).toBe(2);
    expect(activityLevel(100, 1000, true)).toBe(1);
  });
});
