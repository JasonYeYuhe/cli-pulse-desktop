import { describe, it, expect } from "vitest";
import { csvEscape, formatInt, formatUSD, rowsToCsv } from "./format";

describe("formatUSD", () => {
  it("renders zero as $0.00", () => {
    expect(formatUSD(0)).toBe("$0.00");
  });

  it("uses 4 decimal places below 1 cent", () => {
    expect(formatUSD(0.0001)).toBe("$0.0001");
    expect(formatUSD(0.0099)).toBe("$0.0099");
  });

  it("uses 2 decimal places at and above 1 cent", () => {
    expect(formatUSD(0.01)).toBe("$0.01");
    expect(formatUSD(0.5)).toBe("$0.50");
    expect(formatUSD(123.456)).toBe("$123.46");
    expect(formatUSD(1_234_567.89)).toBe("$1234567.89");
  });

  it("clamps non-finite values to $0.00 (no NaN leaks)", () => {
    expect(formatUSD(NaN)).toBe("$0.00");
    expect(formatUSD(Infinity)).toBe("$0.00");
    expect(formatUSD(-Infinity)).toBe("$0.00");
  });
});

describe("formatInt", () => {
  it("uses thousand separators", () => {
    expect(formatInt(1000)).toBe("1,000");
    expect(formatInt(1_234_567)).toBe("1,234,567");
  });

  it("zero stays zero", () => {
    expect(formatInt(0)).toBe("0");
  });

  it("clamps non-finite to zero", () => {
    expect(formatInt(NaN)).toBe("0");
    expect(formatInt(Infinity)).toBe("0");
  });

  it("truncates fractional inputs", () => {
    expect(formatInt(1234.9)).toBe("1,234");
  });
});

describe("csvEscape", () => {
  it("returns empty string for null/undefined", () => {
    expect(csvEscape(null)).toBe("");
    expect(csvEscape(undefined)).toBe("");
  });

  it("passes plain values through unchanged", () => {
    expect(csvEscape("hello")).toBe("hello");
    expect(csvEscape(42)).toBe("42");
  });

  it("wraps cells containing comma in quotes", () => {
    expect(csvEscape("a,b")).toBe('"a,b"');
  });

  it("wraps cells containing newline or CR in quotes", () => {
    expect(csvEscape("line1\nline2")).toBe('"line1\nline2"');
    expect(csvEscape("with\rCR")).toBe('"with\rCR"');
  });

  it("doubles internal double-quotes per RFC 4180", () => {
    expect(csvEscape('say "hi"')).toBe('"say ""hi"""');
  });

  it("handles a value that's just a single quote", () => {
    expect(csvEscape('"')).toBe('""""');
  });
});

describe("rowsToCsv", () => {
  it("renders a 2x2 table without escaping", () => {
    const rows = [
      ["a", "b"],
      ["c", "d"],
    ];
    expect(rowsToCsv(rows)).toBe("a,b\nc,d\n");
  });

  it("escapes embedded special chars", () => {
    const rows = [
      ["model", "cost"],
      ["claude-sonnet-4-6", "$0.91"],
      ['model with "quotes"', "1.5"],
      ["a,b,c", "0"],
    ];
    const got = rowsToCsv(rows);
    expect(got).toContain('"model with ""quotes"""');
    expect(got).toContain('"a,b,c"');
    expect(got.endsWith("\n")).toBe(true);
  });

  it("renders the schema we ship in Settings → Export", () => {
    const rows = [
      [
        "date",
        "provider",
        "model",
        "input_tokens",
        "cached_tokens",
        "output_tokens",
        "cost_usd",
        "message_count",
      ],
      ["2026-04-26", "Claude", "claude-sonnet-4-6", 1000, 0, 500, "0.005000", 1],
    ];
    const out = rowsToCsv(rows);
    expect(out).toBe(
      "date,provider,model,input_tokens,cached_tokens,output_tokens,cost_usd,message_count\n" +
        "2026-04-26,Claude,claude-sonnet-4-6,1000,0,500,0.005000,1\n"
    );
  });
});
