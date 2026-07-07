import { describe, expect, it } from "vitest";
import { CURRENCIES, formatMoney } from "./money";

describe("formatMoney", () => {
  const rates = { CNY: 7.21, EUR: 0.92, GBP: 0.78, JPY: 161.3 };

  it("passes USD through the exact USD formatter (keeps sub-cent precision)", () => {
    expect(formatMoney(1.23, "USD", rates)).toBe("$1.23");
    expect(formatMoney(0.004, "USD", null)).toBe("$0.0040");
    expect(formatMoney(0, "USD", rates)).toBe("$0.00");
  });

  it("converts to CNY with 2 decimals + grouping", () => {
    expect(formatMoney(1000, "CNY", rates)).toBe("¥7,210.00");
  });

  it("uses 0 decimals + a disambiguated symbol for JPY", () => {
    expect(formatMoney(10, "JPY", rates)).toBe("JP¥1,613");
  });

  it("formats EUR/GBP with their symbols", () => {
    expect(formatMoney(100, "EUR", rates)).toBe("€92.00");
    expect(formatMoney(100, "GBP", rates)).toBe("£78.00");
  });

  it("falls back to USD when the rate is missing or the table is null", () => {
    expect(formatMoney(2, "CNY", { EUR: 0.9 })).toBe("$2.00");
    expect(formatMoney(2, "CNY", null)).toBe("$2.00");
    expect(formatMoney(2, "XYZ", rates)).toBe("$2.00"); // unknown currency
  });

  it("treats a non-finite USD input as 0 before converting", () => {
    expect(formatMoney(Number.NaN, "CNY", rates)).toBe("¥0.00");
  });

  it("offers USD first in the currency list", () => {
    expect(CURRENCIES[0].code).toBe("USD");
  });
});
