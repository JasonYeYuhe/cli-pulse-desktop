// Per-provider brand palette — ported 1:1 from the macOS/iOS app's
// `PulseTheme.providerColor` (CLIPulseCore/Components.swift) so the
// Windows/Linux desktop shows the SAME provider accent colors as the
// Mac / iPhone / Watch apps. Added v0.10.1 to close the parity gap the
// audit flagged repeatedly: "Claude / Codex / Gemini are visually
// indistinguishable" on desktop.
//
// Swift stores colors as RGB in 0..1; these are the same values
// converted to hex (round(component * 255)). Keep this table in sync
// with the Swift source if the Mac palette changes.
//
// Kept standalone (like format.ts) so the lookup is unit-tested without
// the App tree — and so session rows, alerts, swarm, and the tray can
// reuse the exact same palette in later ships.

/** Canonical provider name (lowercased) → accent hex. Mirrors the
 * Swift `switch provider` in PulseTheme.providerColor. */
export const PROVIDER_COLORS: Record<string, string> = {
  claude: "#E68C33",
  codex: "#5C82FF",
  gemini: "#9463FA",
  cursor: "#66CC66",
  opencode: "#8080CC",
  droid: "#B366B3",
  antigravity: "#D9598C",
  copilot: "#4DB3E6",
  "z.ai": "#F2991A",
  minimax: "#994DE6",
  augment: "#40BF8C",
  "jetbrains ai": "#F24D80",
  "kimi k2": "#6699F2",
  amp: "#E6BF33",
  synthetic: "#8C73D9",
  warp: "#33CCCC",
  kilo: "#BF8C59",
  openrouter: "#33A6E6",
  ollama: "#4DCCA6",
  alibaba: "#F28026",
  crof: "#73B3F2",
  deepseek: "#4D73D9",
  elevenlabs: "#268C8C",
  venice: "#A659D9",
  kimi: "#598CE6",
  kiro: "#33B380",
  "vertex ai": "#4285F5",
  perplexity: "#1ABFBF",
  "volcano engine": "#268CF2",
};

/** Fallback for providers absent from the palette (Swift: `.gray`). */
export const DEFAULT_PROVIDER_COLOR = "#9CA3AF";

/**
 * Accent hex for a provider name. Case-insensitive + whitespace-
 * trimmed, mirroring `ProviderDisplay.normalize` on the Swift side, so
 * "Claude" / "claude" / "  CLAUDE " all resolve to the same color.
 * Unknown / blank providers get DEFAULT_PROVIDER_COLOR.
 */
export function providerColor(provider: string | null | undefined): string {
  if (!provider) return DEFAULT_PROVIDER_COLOR;
  const key = provider.trim().toLowerCase();
  return PROVIDER_COLORS[key] ?? DEFAULT_PROVIDER_COLOR;
}

/**
 * Single-character monogram for the provider avatar. First non-space
 * character, uppercased; empty string for blank input (the caller then
 * hides the avatar). SF Symbols don't exist on the web, so the colored
 * monogram is the desktop's stand-in glyph — the accent color carries
 * the identity, matching the Mac palette exactly.
 */
export function providerMonogram(provider: string | null | undefined): string {
  if (!provider) return "";
  const trimmed = provider.trim();
  return trimmed ? trimmed.charAt(0).toUpperCase() : "";
}
