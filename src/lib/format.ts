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
