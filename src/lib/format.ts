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
 * Human-readable byte size (1024-based). Used by the Machine tab for
 * memory. Defensive: non-finite or negative → "—" (never renders NaN).
 * Examples: 512 → "512 B", 1536 → "1.5 KB", 3_400_000_000 → "3.2 GB".
 */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  const digits = i === 0 ? 0 : v < 10 ? 1 : 0;
  return `${v.toFixed(digits)} ${units[i]}`;
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

/**
 * Sub-minute relative-time short unit returned by
 * `formatRelativeShortParts`. Callers translate via i18n keys
 * `time.unit_s` / `time.unit_min` / `time.unit_hr` / `time.unit_d`
 * so zh-CN renders 秒 / 分钟 / 小时 / 天 (per v0.4.23 VM L10n
 * finding) and ja renders 秒 / 分 / 時間 / 日.
 */
export type RelativeUnit = "s" | "min" | "hr" | "d";

/**
 * Decompose `updated_at` into a `{value, unit}` pair for i18n
 * composition. Sub-minute resolution (the "synced X ago" line on
 * the Providers tab updates as recently as ~2 s after a manual
 * click; collapsing to "<1 min" reads as unhelpfully stale-looking
 * right after a fresh sync — that's why the v0.4.22 line uses a
 * separate helper from `formatRelativeMinutes`).
 *
 * Returns `null` for unparseable timestamps. Callers should hide
 * the relative-time line entirely on null (vs. rendering a
 * "synced [bad-string] ago" leaked-data look).
 *
 * Negative deltas (server clock skew or a future-dated row) clamp
 * to `value: 0, unit: "s"` so users see "synced 0 s ago" rather
 * than "-3 s".
 */
export function formatRelativeShortParts(
  updated_at: string,
): { value: number; unit: RelativeUnit } | null {
  const t = Date.parse(updated_at);
  if (Number.isNaN(t)) return null;
  const seconds = Math.max(0, Math.floor((Date.now() - t) / 1000));
  if (seconds < 60) return { value: seconds, unit: "s" };
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return { value: minutes, unit: "min" };
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return { value: hours, unit: "hr" };
  return { value: Math.floor(hours / 24), unit: "d" };
}

/**
 * English-only short relative-time string. Kept for the
 * unparseable-passthrough behavior that pre-existed
 * `formatRelativeShortParts`, plus tests / debug helpers that
 * don't have an i18n context. UI code should use
 * `formatRelativeShortParts` + `t("time.unit_<unit>", {count})`
 * instead.
 *
 * Output shape: "12 s" / "45 s" / "3 min" / "2 hr" / "5 d".
 * Returns the raw input verbatim for unparseable timestamps.
 */
export function formatRelativeShort(updated_at: string): string {
  const parts = formatRelativeShortParts(updated_at);
  if (!parts) return updated_at;
  return `${parts.value} ${parts.unit}`;
}

/**
 * v0.10.1 — decompose a raw *seconds* duration into a `{value, unit}`
 * pair for i18n composition via the `time.unit_<unit>` keys. Sibling of
 * `formatRelativeShortParts` but takes a number of seconds (as the Swarm
 * RPC emits `age_s` / `oldest_blocked_age_s` / `last_seen_s_ago`) rather
 * than a timestamp string. Negative / NaN clamp to `{0, "s"}`.
 */
export function secondsToShortParts(seconds: number): {
  value: number;
  unit: RelativeUnit;
} {
  const s = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
  if (s < 60) return { value: s, unit: "s" };
  const minutes = Math.floor(s / 60);
  if (minutes < 60) return { value: minutes, unit: "min" };
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return { value: hours, unit: "hr" };
  return { value: Math.floor(hours / 24), unit: "d" };
}
