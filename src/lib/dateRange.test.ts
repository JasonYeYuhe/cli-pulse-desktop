import { describe, it, expect, beforeEach } from "vitest";
import {
  DATE_RANGE_KEY,
  DEFAULT_DAYS,
  MIN_DAYS,
  MAX_DAYS,
  RANGE_PRESETS,
  clampDays,
  isPreset,
  loadRangeDays,
  saveRangeDays,
} from "./dateRange";

beforeEach(() => {
  localStorage.clear();
});

describe("clampDays", () => {
  it("passes valid in-range integers through unchanged", () => {
    expect(clampDays(7)).toBe(7);
    expect(clampDays(30)).toBe(30);
    expect(clampDays(90)).toBe(90);
  });

  it("clamps below MIN_DAYS up to MIN_DAYS", () => {
    expect(clampDays(0)).toBe(MIN_DAYS);
    expect(clampDays(-5)).toBe(MIN_DAYS);
  });

  it("clamps above MAX_DAYS down to MAX_DAYS", () => {
    expect(clampDays(181)).toBe(MAX_DAYS);
    expect(clampDays(100000)).toBe(MAX_DAYS);
  });

  it("floors fractional values", () => {
    expect(clampDays(7.9)).toBe(7);
    expect(clampDays(30.001)).toBe(30);
  });

  it("collapses non-finite input to DEFAULT_DAYS", () => {
    expect(clampDays(NaN)).toBe(DEFAULT_DAYS);
    expect(clampDays(Infinity)).toBe(DEFAULT_DAYS);
    expect(clampDays(-Infinity)).toBe(DEFAULT_DAYS);
  });
});

describe("isPreset", () => {
  it("recognises the one-tap presets", () => {
    for (const p of RANGE_PRESETS) expect(isPreset(p)).toBe(true);
  });

  it("treats any other value as custom", () => {
    expect(isPreset(14)).toBe(false);
    expect(isPreset(60)).toBe(false);
    expect(isPreset(DEFAULT_DAYS)).toBe(true); // 30 is a preset
  });
});

describe("loadRangeDays", () => {
  it("defaults to DEFAULT_DAYS when nothing is stored", () => {
    expect(loadRangeDays()).toBe(DEFAULT_DAYS);
  });

  it("reads back a previously saved window", () => {
    localStorage.setItem(DATE_RANGE_KEY, "90");
    expect(loadRangeDays()).toBe(90);
  });

  it("defaults on a non-numeric payload", () => {
    localStorage.setItem(DATE_RANGE_KEY, "not-a-number");
    expect(loadRangeDays()).toBe(DEFAULT_DAYS);
  });

  it("clamps an out-of-range stored value", () => {
    localStorage.setItem(DATE_RANGE_KEY, "9999");
    expect(loadRangeDays()).toBe(MAX_DAYS);
    localStorage.setItem(DATE_RANGE_KEY, "0");
    expect(loadRangeDays()).toBe(MIN_DAYS);
  });
});

describe("saveRangeDays", () => {
  it("round-trips through load", () => {
    saveRangeDays(7);
    expect(loadRangeDays()).toBe(7);
  });

  it("clamps before persisting so storage never holds an out-of-range value", () => {
    saveRangeDays(500);
    expect(localStorage.getItem(DATE_RANGE_KEY)).toBe(String(MAX_DAYS));
    saveRangeDays(-3);
    expect(localStorage.getItem(DATE_RANGE_KEY)).toBe(String(MIN_DAYS));
  });

  it("floors a custom fractional value", () => {
    saveRangeDays(45.7);
    expect(loadRangeDays()).toBe(45);
  });
});
