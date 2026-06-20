import { describe, it, expect } from "vitest";
import {
  PROVIDER_COLORS,
  DEFAULT_PROVIDER_COLOR,
  providerColor,
  providerMonogram,
} from "./providerTheme";

describe("providerColor", () => {
  it("returns the ported Mac accent for known providers", () => {
    // Spot-check the values converted from PulseTheme.providerColor.
    expect(providerColor("Claude")).toBe("#E68C33");
    expect(providerColor("Codex")).toBe("#5C82FF");
    expect(providerColor("Gemini")).toBe("#9463FA");
    expect(providerColor("Cursor")).toBe("#66CC66");
    expect(providerColor("Copilot")).toBe("#4DB3E6");
    expect(providerColor("OpenRouter")).toBe("#33A6E6");
  });

  it("is case-insensitive and whitespace-tolerant", () => {
    expect(providerColor("claude")).toBe("#E68C33");
    expect(providerColor("CLAUDE")).toBe("#E68C33");
    expect(providerColor("  Claude ")).toBe("#E68C33");
  });

  it("falls back to the default gray for unknown providers", () => {
    expect(providerColor("Nonesuch")).toBe(DEFAULT_PROVIDER_COLOR);
  });

  it("falls back for null / undefined / empty input", () => {
    expect(providerColor(null)).toBe(DEFAULT_PROVIDER_COLOR);
    expect(providerColor(undefined)).toBe(DEFAULT_PROVIDER_COLOR);
    expect(providerColor("")).toBe(DEFAULT_PROVIDER_COLOR);
    expect(providerColor("   ")).toBe(DEFAULT_PROVIDER_COLOR);
  });

  it("every palette entry is a valid 6-digit hex color", () => {
    for (const [name, hex] of Object.entries(PROVIDER_COLORS)) {
      expect(hex, name).toMatch(/^#[0-9A-F]{6}$/);
    }
    expect(DEFAULT_PROVIDER_COLOR).toMatch(/^#[0-9A-F]{6}$/);
  });
});

describe("providerMonogram", () => {
  it("returns the uppercased first character", () => {
    expect(providerMonogram("claude")).toBe("C");
    expect(providerMonogram("OpenRouter")).toBe("O");
    expect(providerMonogram("  gemini")).toBe("G");
  });

  it("returns empty string for blank / null input", () => {
    expect(providerMonogram("")).toBe("");
    expect(providerMonogram("   ")).toBe("");
    expect(providerMonogram(null)).toBe("");
    expect(providerMonogram(undefined)).toBe("");
  });
});
