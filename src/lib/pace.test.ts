import { describe, it, expect } from "vitest";
import {
  windowMinutesForTier,
  computePace,
  parseResetMs,
} from "./pace";

describe("windowMinutesForTier", () => {
  it("parses an explicit hour count anywhere in the name", () => {
    expect(windowMinutesForTier("5h Window")).toBe(300);
    expect(windowMinutesForTier("4-hour")).toBe(240);
    expect(windowMinutesForTier("12 hrs")).toBe(720);
    expect(windowMinutesForTier("1 hour")).toBe(60);
  });

  it("recognizes standalone window words", () => {
    expect(windowMinutesForTier("Hourly")).toBe(60);
    expect(windowMinutesForTier("Daily")).toBe(1440);
    expect(windowMinutesForTier("Weekly")).toBe(10080);
    expect(windowMinutesForTier("weekly")).toBe(10080);
  });

  it("returns null for variable / unnamed windows and tier nicknames", () => {
    expect(windowMinutesForTier("Monthly")).toBeNull();
    expect(windowMinutesForTier("Sonnet only")).toBeNull();
    expect(windowMinutesForTier("Designs")).toBeNull();
    // Contains the word "Daily" but is not a daily window — must NOT match.
    expect(windowMinutesForTier("Daily Routines")).toBeNull();
    expect(windowMinutesForTier("")).toBeNull();
  });

  it("does not misfire on a digit followed by an h-word that isn't 'hour'", () => {
    expect(windowMinutesForTier("Top 5 heavy")).toBeNull();
  });
});

describe("computePace", () => {
  // A 5-hour window ending at t=5h; "now" is 1h in → 20% elapsed.
  const H = 60 * 60_000;
  const resetTimeMs = 5 * H; // window end
  const nowMs = 1 * H; // 1h after window start (start = resetTimeMs - 5h = 0)

  it("returns null when the window length is unknown", () => {
    expect(
      computePace({ usedFraction: 0.5, windowMinutes: null, resetTimeMs, nowMs }),
    ).toBeNull();
  });

  it("returns null when the reset timestamp is missing", () => {
    expect(
      computePace({ usedFraction: 0.5, windowMinutes: 300, resetTimeMs: null, nowMs }),
    ).toBeNull();
  });

  it("computes the elapsed fraction of the window", () => {
    const p = computePace({ usedFraction: 0.2, windowMinutes: 300, resetTimeMs, nowMs });
    expect(p).not.toBeNull();
    expect(p!.expectedFraction).toBeCloseTo(0.2); // 1h of 5h elapsed
  });

  it("flags 'ahead' when usage outruns elapsed time beyond tolerance", () => {
    // 60% used but only 20% of the window elapsed → ahead.
    const p = computePace({ usedFraction: 0.6, windowMinutes: 300, resetTimeMs, nowMs });
    expect(p!.status).toBe("ahead");
  });

  it("flags 'under' when usage lags elapsed time beyond tolerance", () => {
    // 2% used, 20% elapsed → comfortably under.
    const p = computePace({ usedFraction: 0.02, windowMinutes: 300, resetTimeMs, nowMs });
    expect(p!.status).toBe("under");
  });

  it("flags 'on_track' within the tolerance band", () => {
    // 22% used vs 20% elapsed → within the default 5pp band.
    const p = computePace({ usedFraction: 0.22, windowMinutes: 300, resetTimeMs, nowMs });
    expect(p!.status).toBe("on_track");
  });

  it("clamps used + expected fractions into 0..1", () => {
    // now past the window end → expected clamps to 1; used over 1 clamps to 1.
    const p = computePace({
      usedFraction: 1.5,
      windowMinutes: 300,
      resetTimeMs,
      nowMs: 10 * H,
    });
    expect(p!.expectedFraction).toBe(1);
    expect(p!.usedFraction).toBe(1);
    expect(p!.status).toBe("on_track"); // 1 vs 1
  });

  it("respects a custom tolerance", () => {
    const args = { usedFraction: 0.28, windowMinutes: 300, resetTimeMs, nowMs };
    expect(computePace({ ...args, toleranceFraction: 0.05 })!.status).toBe("ahead");
    expect(computePace({ ...args, toleranceFraction: 0.2 })!.status).toBe("on_track");
  });
});

describe("parseResetMs", () => {
  it("parses an RFC3339 timestamp to epoch ms", () => {
    // Assert against an independent construction (Date.UTC), not Date.parse of
    // the same input, so the test can't pass a subtly-wrong implementation.
    expect(parseResetMs("2026-07-08T00:00:00Z")).toBe(Date.UTC(2026, 6, 8, 0, 0, 0));
  });
  it("returns null for absent or invalid input", () => {
    expect(parseResetMs(null)).toBeNull();
    expect(parseResetMs(undefined)).toBeNull();
    expect(parseResetMs("")).toBeNull();
    expect(parseResetMs("not a date")).toBeNull();
  });
});
