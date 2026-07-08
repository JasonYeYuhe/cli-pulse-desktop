import { describe, it, expect } from "vitest";
import {
  DEFAULT_WARN_THRESHOLDS,
  warningFractions,
  placeOnRemainingBar,
} from "./quotaMarkers";

describe("warningFractions", () => {
  it("maps percents to fractions, dropping out-of-range", () => {
    expect(warningFractions([80, 95])).toEqual([0.8, 0.95]);
    expect(warningFractions([0, 100, 50])).toEqual([0.5]); // 0 and 100 dropped
    expect(warningFractions([-5, 120])).toEqual([]);
  });

  it("de-duplicates and sorts ascending", () => {
    expect(warningFractions([95, 80, 80])).toEqual([0.8, 0.95]);
  });

  it("the default thresholds are 80/95", () => {
    expect(warningFractions(DEFAULT_WARN_THRESHOLDS)).toEqual([0.8, 0.95]);
  });
});

describe("placeOnRemainingBar", () => {
  it("puts an as-used fraction at 1 − f (remaining orientation)", () => {
    // 80% used → the tick sits at 20% (matches the remaining fill).
    expect(placeOnRemainingBar(0.8)).toBeCloseTo(0.2, 10);
    expect(placeOnRemainingBar(0.95)).toBeCloseTo(0.05, 10);
    expect(placeOnRemainingBar(0)).toBe(1);
    expect(placeOnRemainingBar(1)).toBe(0);
  });

  it("clamps out-of-range fractions", () => {
    expect(placeOnRemainingBar(-0.5)).toBe(1);
    expect(placeOnRemainingBar(1.5)).toBe(0);
  });

  it("treats non-finite input as 0 (never returns NaN for a CSS position)", () => {
    expect(placeOnRemainingBar(NaN)).toBe(1);
    expect(placeOnRemainingBar(Infinity)).toBe(1);
    expect(placeOnRemainingBar(-Infinity)).toBe(1);
  });
});
