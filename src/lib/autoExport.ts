// Auto-export settings + the shared CSV builder (v0.10.1 — the deferred v0.10.0
// export item). Users who want a spreadsheet of their usage kept up to date
// (for their own dashboards / billing records) can enable a periodic write to a
// folder while the app runs, instead of clicking Export every time.
//
// The window-math + settings live here (mirrors dateRange.ts / money.ts) so they
// unit-test without the App tree; the actual file write is a Rust command
// (`write_export_file`) that needs no fs/dialog plugin.

import { rowsToCsv } from "./format";

/** localStorage key. Namespaced like the other `cli-pulse.*` settings. */
export const AUTO_EXPORT_KEY = "cli-pulse.auto-export";

export type ExportFormat = "csv" | "json" | "both";
export const EXPORT_FORMATS: readonly ExportFormat[] = ["csv", "json", "both"];

export type AutoExportSettings = {
  enabled: boolean;
  format: ExportFormat;
  intervalMin: number;
};

/** Bounds for the auto-export cadence (minutes). */
export const MIN_INTERVAL_MIN = 5;
export const MAX_INTERVAL_MIN = 24 * 60; // a day
export const DEFAULT_INTERVAL_MIN = 30;

export const DEFAULT_AUTO_EXPORT: AutoExportSettings = {
  enabled: false,
  format: "csv",
  intervalMin: DEFAULT_INTERVAL_MIN,
};

/** Floor + clamp the cadence into [MIN, MAX]; non-finite → the default. */
export function clampInterval(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_INTERVAL_MIN;
  const f = Math.floor(n);
  if (f < MIN_INTERVAL_MIN) return MIN_INTERVAL_MIN;
  if (f > MAX_INTERVAL_MIN) return MAX_INTERVAL_MIN;
  return f;
}

/**
 * Read persisted settings, tolerating every failure mode (missing key, bad
 * JSON, wrong shape, out-of-range interval, unknown format) by falling back to
 * the safe default — which is **disabled**, so garbage state never silently
 * starts writing files. Never throws.
 */
export function loadAutoExport(): AutoExportSettings {
  try {
    const raw = localStorage.getItem(AUTO_EXPORT_KEY);
    if (!raw) return { ...DEFAULT_AUTO_EXPORT };
    const parsed = JSON.parse(raw) as Partial<AutoExportSettings>;
    const format = EXPORT_FORMATS.includes(parsed.format as ExportFormat)
      ? (parsed.format as ExportFormat)
      : DEFAULT_AUTO_EXPORT.format;
    return {
      enabled: parsed.enabled === true,
      format,
      intervalMin: clampInterval(Number(parsed.intervalMin)),
    };
  } catch {
    return { ...DEFAULT_AUTO_EXPORT };
  }
}

/** Persist settings (interval clamped first). Best-effort — swallows failures. */
export function saveAutoExport(s: AutoExportSettings): void {
  try {
    localStorage.setItem(
      AUTO_EXPORT_KEY,
      JSON.stringify({ ...s, intervalMin: clampInterval(s.intervalMin) }),
    );
  } catch {
    // ignore — a non-persistent choice still drives this session.
  }
}

/** The one-and-same daily entry shape the exporter reads (subset of ScanResult). */
export type ExportEntry = {
  date: string;
  provider: string;
  model: string;
  input_tokens: number;
  cached_tokens: number;
  output_tokens: number;
  cost_usd: number | null;
  message_count: number;
};

/** CSV column order — stable across the download button + auto-export. */
export const CSV_HEADER: readonly string[] = [
  "date",
  "provider",
  "model",
  "input_tokens",
  "cached_tokens",
  "output_tokens",
  "cost_usd",
  "message_count",
];

/**
 * Render usage entries to CSV (the single source of the export shape — used by
 * both the Export button and auto-export). Skips the synthetic Claude
 * message-bucket rows (`msgBucketModel`), which carry no token detail. Costs
 * keep 6-decimal precision; a null cost is an empty cell.
 */
export function buildUsageCsv(
  entries: ReadonlyArray<ExportEntry>,
  msgBucketModel: string,
): string {
  const rows: (string | number | null)[][] = [
    [...CSV_HEADER],
    ...entries
      .filter((e) => e.model !== msgBucketModel)
      .map((e): (string | number | null)[] => [
        e.date,
        e.provider,
        e.model,
        e.input_tokens,
        e.cached_tokens,
        e.output_tokens,
        e.cost_usd == null ? "" : e.cost_usd.toFixed(6),
        e.message_count,
      ]),
  ];
  return rowsToCsv(rows);
}
