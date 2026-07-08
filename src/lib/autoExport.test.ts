import { describe, it, expect, beforeEach } from "vitest";
import {
  AUTO_EXPORT_KEY,
  DEFAULT_AUTO_EXPORT,
  MIN_INTERVAL_MIN,
  MAX_INTERVAL_MIN,
  DEFAULT_INTERVAL_MIN,
  clampInterval,
  loadAutoExport,
  saveAutoExport,
  buildUsageCsv,
  type ExportEntry,
} from "./autoExport";

beforeEach(() => {
  localStorage.clear();
});

describe("clampInterval", () => {
  it("passes in-range integers through", () => {
    expect(clampInterval(30)).toBe(30);
    expect(clampInterval(5)).toBe(5);
  });
  it("clamps to the bounds", () => {
    expect(clampInterval(1)).toBe(MIN_INTERVAL_MIN);
    expect(clampInterval(99999)).toBe(MAX_INTERVAL_MIN);
  });
  it("floors fractions and defaults on non-finite", () => {
    expect(clampInterval(30.9)).toBe(30);
    expect(clampInterval(NaN)).toBe(DEFAULT_INTERVAL_MIN);
  });
});

describe("loadAutoExport", () => {
  it("defaults to disabled when nothing is stored", () => {
    expect(loadAutoExport()).toEqual(DEFAULT_AUTO_EXPORT);
  });
  it("round-trips through save", () => {
    saveAutoExport({ enabled: true, format: "both", intervalMin: 60 });
    expect(loadAutoExport()).toEqual({ enabled: true, format: "both", intervalMin: 60 });
  });
  it("fails safe (disabled) on malformed JSON", () => {
    localStorage.setItem(AUTO_EXPORT_KEY, "{not json");
    expect(loadAutoExport()).toEqual(DEFAULT_AUTO_EXPORT);
  });
  it("sanitizes an unknown format and out-of-range interval", () => {
    localStorage.setItem(
      AUTO_EXPORT_KEY,
      JSON.stringify({ enabled: true, format: "xml", intervalMin: 100000 }),
    );
    const s = loadAutoExport();
    expect(s.enabled).toBe(true);
    expect(s.format).toBe(DEFAULT_AUTO_EXPORT.format); // "xml" rejected
    expect(s.intervalMin).toBe(MAX_INTERVAL_MIN);
  });
  it("treats a non-boolean enabled as disabled", () => {
    localStorage.setItem(AUTO_EXPORT_KEY, JSON.stringify({ enabled: "yes" }));
    expect(loadAutoExport().enabled).toBe(false);
  });
});

describe("saveAutoExport", () => {
  it("clamps the interval before persisting", () => {
    saveAutoExport({ enabled: true, format: "csv", intervalMin: 1 });
    expect(loadAutoExport().intervalMin).toBe(MIN_INTERVAL_MIN);
  });
});

describe("buildUsageCsv", () => {
  const entry = (over: Partial<ExportEntry>): ExportEntry => ({
    date: "2026-07-08",
    provider: "Claude",
    model: "sonnet",
    input_tokens: 100,
    cached_tokens: 20,
    output_tokens: 50,
    cost_usd: 0.123456,
    message_count: 3,
    ...over,
  });

  it("emits a header + one row per entry", () => {
    const csv = buildUsageCsv([entry({})], "__claude_msg__");
    const lines = csv.trimEnd().split("\n");
    expect(lines[0]).toBe(
      "date,provider,model,input_tokens,cached_tokens,output_tokens,cost_usd,message_count",
    );
    expect(lines[1]).toBe("2026-07-08,Claude,sonnet,100,20,50,0.123456,3");
  });

  it("skips the synthetic message-bucket rows", () => {
    const csv = buildUsageCsv(
      [entry({}), entry({ model: "__claude_msg__", message_count: 9 })],
      "__claude_msg__",
    );
    expect(csv.trimEnd().split("\n")).toHaveLength(2); // header + 1 real row
  });

  it("renders a null cost as an empty cell", () => {
    const csv = buildUsageCsv([entry({ cost_usd: null })], "__claude_msg__");
    expect(csv.trimEnd().split("\n")[1]).toBe("2026-07-08,Claude,sonnet,100,20,50,,3");
  });
});
