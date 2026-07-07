// Multi-currency cost display (learned from javis603/token-monitor's
// USD/TWD/HKD/CNY support). Costs are computed + stored in USD everywhere; this
// converts for DISPLAY only, using daily FX rates fetched by the Rust `fx`
// module. Kept deterministic (no `Intl` currency formatting, whose exact output
// varies by ICU version) so it's unit-testable.

import { formatUSD } from "./format";

/** USD-based rate table from `get_fx_rates` (`rates[c]` = units of `c` per 1 USD). */
export type FxRates = { base: string; rates: Record<string, number>; as_of: string };

type CurrencyMeta = { code: string; symbol: string; decimals: number };

// The currencies offered in Settings. USD is always first (the base/default);
// the rest need a live rate to be selectable in practice.
export const CURRENCIES: readonly CurrencyMeta[] = [
  { code: "USD", symbol: "$", decimals: 2 },
  { code: "CNY", symbol: "¥", decimals: 2 },
  { code: "EUR", symbol: "€", decimals: 2 },
  { code: "GBP", symbol: "£", decimals: 2 },
  { code: "JPY", symbol: "JP¥", decimals: 0 }, // "JP¥" disambiguates from CNY "¥"
];

const META = new Map(CURRENCIES.map((c) => [c.code, c]));
const STORAGE_KEY = "cli-pulse.display-currency";

export function loadCurrency(): string {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    return v && META.has(v) ? v : "USD";
  } catch {
    return "USD";
  }
}

export function saveCurrency(code: string): void {
  try {
    localStorage.setItem(STORAGE_KEY, code);
  } catch {
    // Ignore — private-mode / disabled storage just means the choice
    // doesn't persist across launches.
  }
}

/**
 * Format a USD amount in `currency`. Falls back to the exact USD formatter
 * (which keeps sub-cent precision) for USD, an unknown currency, or a missing/
 * non-finite rate — so a slow/failed FX fetch never blanks a cost.
 */
export function formatMoney(
  usd: number,
  currency: string,
  rates: Record<string, number> | null | undefined,
): string {
  const meta = META.get(currency);
  const rate = rates?.[currency];
  if (!meta || currency === "USD" || rate == null || !Number.isFinite(rate)) {
    return formatUSD(usd);
  }
  const converted = (Number.isFinite(usd) ? usd : 0) * rate;
  const num = converted.toLocaleString("en-US", {
    minimumFractionDigits: meta.decimals,
    maximumFractionDigits: meta.decimals,
  });
  return `${meta.symbol}${num}`;
}
