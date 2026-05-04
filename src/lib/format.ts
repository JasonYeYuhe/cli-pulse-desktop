// Pure presentation helpers. Kept in a standalone module so they can
// be unit-tested under Vitest without spinning up the full App tree.

/**
 * USD currency formatter for amounts ranging from sub-cent to multi-thousand.
 * - $0 stays "$0.00" (not "$0.0000")
 * - amounts under $0.01 use 4-decimal precision so they don't read as "$0.00"
 * - everything else uses 2-decimal precision
 *
 * Intentionally locale-agnostic: we want consistent "$1.23" output across
 * en/zh-CN/ja UIs because the column is data, not prose.
 */
export function formatUSD(n: number): string {
  if (!Number.isFinite(n)) return "$0.00";
  if (n === 0) return "$0.00";
  if (n > 0 && n < 0.01) return `$${n.toFixed(4)}`;
  return `$${n.toFixed(2)}`;
}

/**
 * Thousand-separated integer formatter. Always renders in en-US locale so
 * tests can assert exact strings — currency-style output should match
 * across locales. Use the runtime `Intl.NumberFormat` for the actual
 * Number.toLocaleString call (jsdom provides this).
 */
export function formatInt(n: number): string {
  if (!Number.isFinite(n)) return "0";
  return Math.trunc(n).toLocaleString("en-US");
}

/**
 * RFC 4180 CSV cell escaper. Handles:
 * - null / undefined → empty cell
 * - cells containing a comma, quote, CR, or LF are wrapped in quotes
 * - inner double-quotes are doubled per RFC 4180
 *
 * Caller is responsible for joining cells with "," and rows with "\n".
 */
export function csvEscape(value: string | number | null | undefined): string {
  if (value === null || value === undefined) return "";
  const s = String(value);
  if (/[",\n\r]/.test(s)) return '"' + s.replace(/"/g, '""') + '"';
  return s;
}

/**
 * Render an array of rows into a single CSV blob. First row is the header.
 * Trailing newline appended so editors that treat the file as a list of
 * lines don't lose the final row.
 */
export function rowsToCsv(rows: ReadonlyArray<ReadonlyArray<string | number | null | undefined>>): string {
  return rows.map((row) => row.map(csvEscape).join(",")).join("\n") + "\n";
}

/**
 * v0.4.15 — provider-card stale indicator.
 *
 * 6 minutes — slightly above the 2-minute background sync cycle so the
 * badge doesn't flap right before each refresh. Per Gemini 3.1 Pro
 * review of the v0.4.14-v0.4.16 dev plan: a 5-min threshold matched
 * the cycle exactly and would cause visible flicker on every sync.
 */
export const STALE_THRESHOLD_MS = 6 * 60_000;

/**
 * True when the server-side provider_summary row is older than
 * STALE_THRESHOLD_MS. Returns false on null/undefined/garbage input —
 * "no timestamp" is not the same as "stale" (synthetic rows from
 * usage_agg in the SQL FULL OUTER JOIN have no quota row + thus no
 * updated_at; we don't want to flag them as stale).
 */
export function isStaleProviderRow(updated_at: string | null | undefined): boolean {
  if (!updated_at) return false;
  const t = Date.parse(updated_at);
  if (Number.isNaN(t)) return false;
  return Date.now() - t > STALE_THRESHOLD_MS;
}

/**
 * Render a relative-time string like "5 min" / "2 hr" / "3 d" suitable
 * for a tooltip. Always en-US-style ("min" not "minutes") because the
 * tooltip text is interpolated into a localized string via
 * `t("providers.stale_tooltip", { age })` — the unit names in the
 * surrounding sentence carry the localization weight, this helper
 * emits a compact value.
 *
 * Returns the raw input verbatim for unparseable timestamps (defensive:
 * we'd rather surface the bad value than crash the render).
 */
export function formatRelativeMinutes(updated_at: string): string {
  const t = Date.parse(updated_at);
  if (Number.isNaN(t)) return updated_at;
  const minutes = Math.floor((Date.now() - t) / 60_000);
  if (minutes < 1) return "<1 min";
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} hr`;
  const days = Math.floor(hours / 24);
  return `${days} d`;
}
