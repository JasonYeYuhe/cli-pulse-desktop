import { describe, it, expect, beforeEach } from "vitest";
import { ALWAYS_ON_TOP_KEY, loadAlwaysOnTop, saveAlwaysOnTop } from "./windowPrefs";

beforeEach(() => {
  localStorage.clear();
});

describe("loadAlwaysOnTop", () => {
  it("defaults to false when nothing is stored", () => {
    expect(loadAlwaysOnTop()).toBe(false);
  });
  it("reads back a saved true", () => {
    saveAlwaysOnTop(true);
    expect(loadAlwaysOnTop()).toBe(true);
  });
  it("reads back a saved false", () => {
    saveAlwaysOnTop(false);
    expect(loadAlwaysOnTop()).toBe(false);
  });
  it("treats any non-\"1\" payload as false", () => {
    localStorage.setItem(ALWAYS_ON_TOP_KEY, "true");
    expect(loadAlwaysOnTop()).toBe(false);
    localStorage.setItem(ALWAYS_ON_TOP_KEY, "yes");
    expect(loadAlwaysOnTop()).toBe(false);
  });
});

describe("saveAlwaysOnTop", () => {
  it("persists as 1/0", () => {
    saveAlwaysOnTop(true);
    expect(localStorage.getItem(ALWAYS_ON_TOP_KEY)).toBe("1");
    saveAlwaysOnTop(false);
    expect(localStorage.getItem(ALWAYS_ON_TOP_KEY)).toBe("0");
  });
});
