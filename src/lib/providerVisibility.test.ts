import { describe, it, expect, beforeEach } from "vitest";
import {
  HIDDEN_PROVIDERS_KEY,
  loadHiddenProviders,
  saveHiddenProviders,
  toggleHiddenProvider,
} from "./providerVisibility";

beforeEach(() => {
  localStorage.clear();
});

describe("loadHiddenProviders", () => {
  it("returns an empty set when nothing is stored", () => {
    expect(loadHiddenProviders().size).toBe(0);
  });

  it("reads back a previously saved set", () => {
    localStorage.setItem(
      HIDDEN_PROVIDERS_KEY,
      JSON.stringify(["Claude", "Codex"]),
    );
    const got = loadHiddenProviders();
    expect(got.has("Claude")).toBe(true);
    expect(got.has("Codex")).toBe(true);
    expect(got.size).toBe(2);
  });

  it("returns empty (fails open) on malformed JSON", () => {
    localStorage.setItem(HIDDEN_PROVIDERS_KEY, "{not json");
    expect(loadHiddenProviders().size).toBe(0);
  });

  it("returns empty when the payload is not an array", () => {
    localStorage.setItem(
      HIDDEN_PROVIDERS_KEY,
      JSON.stringify({ Claude: true }),
    );
    expect(loadHiddenProviders().size).toBe(0);
  });

  it("drops non-string entries but keeps valid ones", () => {
    localStorage.setItem(
      HIDDEN_PROVIDERS_KEY,
      JSON.stringify(["Claude", 42, null, "Gemini"]),
    );
    const got = loadHiddenProviders();
    expect([...got].sort()).toEqual(["Claude", "Gemini"]);
  });
});

describe("saveHiddenProviders", () => {
  it("round-trips through load", () => {
    saveHiddenProviders(new Set(["Cursor"]));
    expect([...loadHiddenProviders()]).toEqual(["Cursor"]);
  });

  it("an empty set persists as an empty array", () => {
    saveHiddenProviders(new Set());
    expect(localStorage.getItem(HIDDEN_PROVIDERS_KEY)).toBe("[]");
    expect(loadHiddenProviders().size).toBe(0);
  });
});

describe("toggleHiddenProvider", () => {
  it("adds a provider that wasn't hidden", () => {
    const next = toggleHiddenProvider(new Set(), "Claude");
    expect(next.has("Claude")).toBe(true);
  });

  it("removes a provider that was hidden", () => {
    const next = toggleHiddenProvider(new Set(["Claude"]), "Claude");
    expect(next.has("Claude")).toBe(false);
  });

  it("does not mutate the input set", () => {
    const input = new Set(["Claude"]);
    toggleHiddenProvider(input, "Codex");
    expect([...input]).toEqual(["Claude"]);
  });
});
