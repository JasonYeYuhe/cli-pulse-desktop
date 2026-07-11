import { describe, it, expect, beforeEach } from "vitest";
import { COMPARE_MODE_KEY, loadCompareMode, saveCompareMode } from "./comparePrefs";

beforeEach(() => {
  localStorage.clear();
});

describe("loadCompareMode", () => {
  it("defaults to false when nothing is stored", () => {
    expect(loadCompareMode()).toBe(false);
  });
  it("reads back a saved true", () => {
    saveCompareMode(true);
    expect(loadCompareMode()).toBe(true);
  });
  it("reads back a saved false", () => {
    saveCompareMode(false);
    expect(loadCompareMode()).toBe(false);
  });
  it('treats any non-"1" payload as false', () => {
    localStorage.setItem(COMPARE_MODE_KEY, "true");
    expect(loadCompareMode()).toBe(false);
    localStorage.setItem(COMPARE_MODE_KEY, "yes");
    expect(loadCompareMode()).toBe(false);
  });
});

describe("saveCompareMode", () => {
  it("persists as 1/0", () => {
    saveCompareMode(true);
    expect(localStorage.getItem(COMPARE_MODE_KEY)).toBe("1");
    saveCompareMode(false);
    expect(localStorage.getItem(COMPARE_MODE_KEY)).toBe("0");
  });
});
