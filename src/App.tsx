import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { check as checkUpdate } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useTranslation } from "react-i18next";
import { SUPPORTED_LANGS, setLang, type LangCode } from "./i18n";
import {
  formatInt,
  formatUSD,
  formatRelativeMinutes,
  isStaleProviderRow,
  rowsToCsv,
} from "./lib/format";
import appIcon from "./assets/app-icon.png";
import "./App.css";

type DailyEntry = {
  date: string;
  provider: string;
  model: string;
  input_tokens: number;
  cached_tokens: number;
  output_tokens: number;
  cost_usd: number | null;
  message_count: number;
};

type ScanResult = {
  entries: DailyEntry[];
  total_cost_usd: number;
  total_tokens: number;
  today_key: string;
  days_scanned: number;
  files_scanned: number;
};

type ConfigView = {
  paired: boolean;
  device_id: string | null;
  device_name: string | null;
  device_type: string;
  helper_version: string;
  user_id: string | null;
};

type SyncReport = {
  sessions_synced: number;
  alerts_synced: number;
  metrics_synced: number;
  metrics_errored: number;
  total_cost_usd: number;
  total_tokens: number;
  files_scanned: number;
  live_sessions_sent: number;
  live_processes_seen: number;
};

type LiveSession = {
  id: string;
  name: string;
  provider: string;
  project: string;
  status: string;
  total_usage: number;
  exact_cost: number | null;
  requests: number;
  error_count: number;
  collection_confidence: "high" | "medium" | "low";
  started_at: string;
  last_active_at: string;
  cpu_usage: number;
  memory_mb: number;
  pids: number[];
  command: string;
};

type SessionsSnapshot = {
  sessions: LiveSession[];
  total_processes_seen: number;
  matched_before_dedup: number;
  collected_at: string;
};

type Alert = {
  id: string;
  type: string;
  severity: "Info" | "Warning" | "Critical";
  title: string;
  message: string;
  created_at: string;
  related_project_id?: string | null;
  related_project_name?: string | null;
  related_session_id?: string | null;
  related_session_name?: string | null;
  related_provider?: string | null;
  related_device_name?: string | null;
  source_kind?: string | null;
  source_id?: string | null;
  grouping_key?: string | null;
  suppression_key?: string | null;
};

type AlertThresholds = {
  daily_budget_usd: number | null;
  weekly_budget_usd: number | null;
  cpu_spike_pct: number;
};

type TabKey = "overview" | "providers" | "sessions" | "alerts" | "settings";

const CLAUDE_MSG_BUCKET = "__claude_msg__";

export default function App() {
  const { t } = useTranslation();
  const tabs: { key: TabKey; label: string }[] = [
    { key: "overview", label: t("tab.overview") },
    { key: "providers", label: t("tab.providers") },
    { key: "sessions", label: t("tab.sessions") },
    { key: "alerts", label: t("tab.alerts") },
    { key: "settings", label: t("tab.settings") },
  ];
  const [tab, setTab] = useState<TabKey>("overview");
  const [scan, setScan] = useState<ScanResult | null>(null);
  const [config, setConfig] = useState<ConfigView | null>(null);
  const [sessions, setSessions] = useState<SessionsSnapshot | null>(null);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [alerts, setAlerts] = useState<Alert[] | null>(null);
  const [alertsLoading, setAlertsLoading] = useState(false);
  const [updateAvailable, setUpdateAvailable] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastSync, setLastSync] = useState<{ at: Date; report: SyncReport } | null>(null);

  const refreshConfig = useCallback(async () => {
    try {
      const c = await invoke<ConfigView>("get_config");
      setConfig(c);
    } catch (e: any) {
      // Config load failures shouldn't block the UI — fall back to unpaired
      console.warn("get_config failed", e);
      setConfig({
        paired: false,
        device_id: null,
        device_name: null,
        device_type: "Desktop",
        helper_version: "?",
        user_id: null,
      });
    }
  }, []);

  const runScan = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const result = await invoke<ScanResult>("scan_usage", { days: 30 });
      setScan(result);
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshSessions = useCallback(async () => {
    setSessionsLoading(true);
    try {
      const snap = await invoke<SessionsSnapshot>("list_sessions");
      setSessions(snap);
    } catch (e: any) {
      console.warn("list_sessions failed", e);
    } finally {
      setSessionsLoading(false);
    }
  }, []);

  const refreshAlerts = useCallback(async () => {
    setAlertsLoading(true);
    try {
      const list = await invoke<Alert[]>("preview_alerts");
      setAlerts(list);
    } catch (e: any) {
      console.warn("preview_alerts failed", e);
    } finally {
      setAlertsLoading(false);
    }
  }, []);

  useEffect(() => {
    refreshConfig();
    runScan();
    refreshSessions();
    refreshAlerts();

    // Silent update check on first mount. We DON'T auto-download —
    // user still controls the install via Settings → Updates.
    // Failure is non-fatal (offline, GitHub down, etc.) — stay quiet
    // so the user isn't pestered.
    (async () => {
      try {
        const upd = await checkUpdate();
        if (upd) setUpdateAvailable(upd.version);
      } catch (e) {
        console.warn("update check failed:", e);
      }
    })();
  }, [refreshConfig, runScan, refreshSessions, refreshAlerts]);

  // Sessions tab refreshes every 10s while visible
  useEffect(() => {
    if (tab !== "sessions") return;
    const id = setInterval(refreshSessions, 10_000);
    return () => clearInterval(id);
  }, [tab, refreshSessions]);

  // Alerts tab refreshes every 30s while visible
  useEffect(() => {
    if (tab !== "alerts") return;
    const id = setInterval(refreshAlerts, 30_000);
    return () => clearInterval(id);
  }, [tab, refreshAlerts]);

  return (
    <div className="min-h-screen flex flex-col bg-neutral-950 text-neutral-100">
      <header className="border-b border-neutral-800 px-6 py-3 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <img
            src={appIcon}
            alt="CLI Pulse"
            className="w-7 h-7 rounded"
            draggable={false}
          />
          <div>
            <div className="font-semibold text-sm">{t("app.name")}</div>
            <div className="text-xs text-neutral-500">
              {t("app.subtitle_desktop")} · {config?.device_type ?? "…"}
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          {updateAvailable && (
            <button
              onClick={() => setTab("settings")}
              className="px-2.5 py-1 text-xs rounded-md bg-emerald-950/60 border border-emerald-700 text-emerald-200 hover:bg-emerald-900/60"
              title={t("updater.banner_available", { version: updateAvailable })}
            >
              ⬆ {t("updater.banner_available", { version: updateAvailable })} ·{" "}
              <span className="font-semibold">{t("updater.banner_action")}</span>
            </button>
          )}
          <PairBadge paired={!!config?.paired} />
          <button
            onClick={runScan}
            disabled={loading}
            className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50"
          >
            {loading ? t("action.scanning") : t("action.rescan")}
          </button>
        </div>
      </header>

      <nav className="border-b border-neutral-800 px-6 flex gap-1">
        {tabs.map((tabItem) => (
          <button
            key={tabItem.key}
            onClick={() => setTab(tabItem.key)}
            className={`px-3 py-2.5 text-sm border-b-2 transition-colors ${
              tab === tabItem.key
                ? "border-emerald-500 text-white"
                : "border-transparent text-neutral-400 hover:text-neutral-200"
            }`}
          >
            {tabItem.label}
          </button>
        ))}
      </nav>

      <main className="flex-1 overflow-auto p-6">
        {error && (
          <div className="mb-4 px-4 py-3 rounded-md bg-red-950/60 border border-red-900 text-sm text-red-200">
            {error}
          </div>
        )}
        {tab === "overview" && <Overview scan={scan} loading={loading} paired={!!config?.paired} />}
        {tab === "providers" && <Providers scan={scan} paired={!!config?.paired} />}
        {tab === "sessions" && (
          <Sessions
            snapshot={sessions}
            loading={sessionsLoading}
            onRefresh={refreshSessions}
          />
        )}
        {tab === "alerts" && (
          <Alerts alerts={alerts} loading={alertsLoading} onRefresh={refreshAlerts} />
        )}
        {tab === "settings" && (
          <Settings
            config={config}
            scan={scan}
            lastSync={lastSync}
            onPaired={async () => {
              await refreshConfig();
            }}
            onUnpaired={async () => {
              setLastSync(null);
              await refreshConfig();
            }}
            onSynced={(report) => setLastSync({ at: new Date(), report })}
          />
        )}
      </main>
    </div>
  );
}

function PairBadge({ paired }: { paired: boolean }) {
  const { t } = useTranslation();
  return paired ? (
    <span className="px-2 py-0.5 text-xs rounded-md bg-emerald-950/60 border border-emerald-900 text-emerald-300">
      {t("badge.paired")}
    </span>
  ) : (
    <span className="px-2 py-0.5 text-xs rounded-md bg-neutral-800 border border-neutral-700 text-neutral-400">
      {t("badge.not_paired")}
    </span>
  );
}

// v0.3.4 — server-side dashboard summary, mirrors `dashboard_summary`
// RPC shape from app_rpc.sql:11.
type DashboardSummaryRow = {
  today_usage: number;
  today_cost: number;
  active_sessions: number;
  online_devices: number;
  unresolved_alerts: number;
  today_sessions: number;
};

function Overview({
  scan,
  loading,
  paired,
}: {
  scan: ScanResult | null;
  loading: boolean;
  paired: boolean;
}) {
  const { t } = useTranslation();
  const today = useMemo(() => {
    if (!scan) return null;
    const todays = scan.entries.filter((e) => e.date === scan.today_key);
    const cost = todays
      .filter((e) => e.model !== CLAUDE_MSG_BUCKET)
      .reduce((s, e) => s + (e.cost_usd ?? 0), 0);
    const tokens = todays.reduce((s, e) => s + e.input_tokens + e.output_tokens, 0);
    const msgs = todays
      .filter((e) => e.model === CLAUDE_MSG_BUCKET)
      .reduce((s, e) => s + e.message_count, 0);
    return { cost, tokens, msgs };
  }, [scan]);

  // v0.3.4 — fetch server-aggregated dashboard summary when paired.
  // This is the cross-device "today" view (Mac + Win + Linux + iOS all
  // contributing to the same account). Failures are soft — the
  // local-scan tiles below stay useful.
  const [serverDash, setServerDash] = useState<DashboardSummaryRow | null>(null);
  useEffect(() => {
    if (!paired) {
      setServerDash(null);
      return;
    }
    let cancelled = false;
    invoke<DashboardSummaryRow>("get_dashboard_summary")
      .then((d) => {
        if (!cancelled) setServerDash(d);
      })
      .catch(() => {
        if (!cancelled) setServerDash(null);
      });
    return () => {
      cancelled = true;
    };
  }, [paired]);

  if (!scan && loading) return <Skeleton />;
  if (!scan) return null;

  return (
    <div className="space-y-6">
      {/* v0.3.4 — server-aggregated tiles. Visible only when paired and
          the dashboard_summary RPC returned data. Six tiles match the
          Mac/iOS metrics grid. */}
      {paired && serverDash && (
        <section>
          <h2 className="text-sm font-semibold text-neutral-400 mb-2">
            {t("overview.account_today")}
          </h2>
          <div className="grid grid-cols-2 md:grid-cols-6 gap-3">
            <StatCard
              label={t("overview.tile_today_cost")}
              value={formatUSD(serverDash.today_cost)}
            />
            <StatCard
              label={t("overview.tile_today_usage")}
              value={formatInt(serverDash.today_usage)}
              hint={t("overview.tokens_hint")}
            />
            <StatCard
              label={t("overview.tile_today_sessions")}
              value={formatInt(serverDash.today_sessions)}
            />
            <StatCard
              label={t("overview.tile_active_sessions")}
              value={formatInt(serverDash.active_sessions)}
            />
            <StatCard
              label={t("overview.tile_online_devices")}
              value={formatInt(serverDash.online_devices)}
            />
            <StatCard
              label={t("overview.tile_unresolved_alerts")}
              value={formatInt(serverDash.unresolved_alerts)}
            />
          </div>
        </section>
      )}

      <section>
        {paired && serverDash && (
          <h2 className="text-sm font-semibold text-neutral-400 mb-2">
            {t("overview.this_device")}
          </h2>
        )}
        <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
          <StatCard label={t("overview.today_cost")} value={today ? formatUSD(today.cost) : "—"} hint={scan.today_key} />
          <StatCard label={t("overview.today_tokens")} value={today ? formatInt(today.tokens) : "—"} hint={t("overview.tokens_hint")} />
          <StatCard label={t("overview.today_messages")} value={today ? formatInt(today.msgs) : "—"} hint={t("overview.claude_only_hint")} />
          <StatCard
            label={t("overview.last_n_days_cost", { days: scan.days_scanned })}
            value={formatUSD(scan.total_cost_usd)}
            hint={t("overview.files_scanned_hint", { n: scan.files_scanned })}
          />
        </div>
      </section>

      <section>
        <h2 className="text-sm font-semibold text-neutral-400 mb-2">{t("overview.trend_title")}</h2>
        <CostTrendChart scan={scan} />
      </section>

      <section>
        <h2 className="text-sm font-semibold text-neutral-400 mb-2">{t("overview.today_breakdown")}</h2>
        <EntriesTable
          entries={scan.entries.filter((e) => e.date === scan.today_key && e.model !== CLAUDE_MSG_BUCKET)}
        />
      </section>
    </div>
  );
}

function CostTrendChart({ scan }: { scan: ScanResult }) {
  const { t } = useTranslation();
  const days = useMemo(() => {
    // Build the last 7 ISO dates relative to today_key
    const baseDate = new Date(scan.today_key + "T00:00:00Z");
    const out: {
      key: string;
      label: string;
      claudeCost: number;
      codexCost: number;
      otherCost: number;
      totalCost: number;
    }[] = [];
    for (let i = 6; i >= 0; i--) {
      const d = new Date(baseDate);
      d.setUTCDate(baseDate.getUTCDate() - i);
      const key = d.toISOString().slice(0, 10);
      const label =
        i === 0
          ? t("overview.label_today")
          : d.toLocaleDateString(undefined, { weekday: "short" });
      const entries = scan.entries.filter(
        (e) => e.date === key && e.model !== CLAUDE_MSG_BUCKET
      );
      const claudeCost = entries
        .filter((e) => e.provider === "Claude")
        .reduce((s, e) => s + (e.cost_usd ?? 0), 0);
      const codexCost = entries
        .filter((e) => e.provider === "Codex")
        .reduce((s, e) => s + (e.cost_usd ?? 0), 0);
      const otherCost = entries
        .filter((e) => e.provider !== "Claude" && e.provider !== "Codex")
        .reduce((s, e) => s + (e.cost_usd ?? 0), 0);
      const totalCost = claudeCost + codexCost + otherCost;
      out.push({ key, label, claudeCost, codexCost, otherCost, totalCost });
    }
    return out;
  }, [scan]);

  const maxCost = Math.max(...days.map((d) => d.totalCost), 1);
  const chartWidth = 720;
  const chartHeight = 200;
  const barPadding = 16;
  const barWidth = (chartWidth - barPadding * 8) / 7;
  const barGap = barPadding;

  return (
    <div className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <svg
        viewBox={`0 0 ${chartWidth} ${chartHeight + 40}`}
        className="w-full h-auto"
        role="img"
        aria-label="7-day cost trend"
      >
        {/* Y-axis grid */}
        {[0.25, 0.5, 0.75, 1].map((frac) => (
          <line
            key={frac}
            x1={0}
            x2={chartWidth}
            y1={chartHeight - chartHeight * frac}
            y2={chartHeight - chartHeight * frac}
            stroke="#262626"
            strokeWidth={1}
            strokeDasharray="2 3"
          />
        ))}

        {days.map((d, i) => {
          const x = barPadding + i * (barWidth + barGap);
          const claudeH = (d.claudeCost / maxCost) * chartHeight;
          const codexH = (d.codexCost / maxCost) * chartHeight;
          const otherH = (d.otherCost / maxCost) * chartHeight;
          let yCursor = chartHeight;
          return (
            <g key={d.key}>
              <title>
                {d.key}
                {"\n"}Claude ${d.claudeCost.toFixed(2)} · Codex ${d.codexCost.toFixed(2)}
                {d.otherCost > 0 ? ` · Other $${d.otherCost.toFixed(2)}` : ""}
                {"\n"}Total ${d.totalCost.toFixed(2)}
              </title>
              {claudeH > 0 && (
                <rect
                  x={x}
                  y={(yCursor -= claudeH) || 0}
                  width={barWidth}
                  height={claudeH}
                  fill="#10b981"
                  rx={2}
                />
              )}
              {codexH > 0 && (
                <rect
                  x={x}
                  y={(yCursor -= codexH) || 0}
                  width={barWidth}
                  height={codexH}
                  fill="#06b6d4"
                  rx={2}
                />
              )}
              {otherH > 0 && (
                <rect
                  x={x}
                  y={(yCursor -= otherH) || 0}
                  width={barWidth}
                  height={otherH}
                  fill="#a855f7"
                  rx={2}
                />
              )}
              {d.totalCost > 0 && (
                <text
                  x={x + barWidth / 2}
                  y={chartHeight - (d.totalCost / maxCost) * chartHeight - 6}
                  textAnchor="middle"
                  fontSize="10"
                  fill="#a3a3a3"
                  fontFamily="ui-monospace,monospace"
                >
                  ${d.totalCost.toFixed(d.totalCost < 10 ? 2 : 0)}
                </text>
              )}
              <text
                x={x + barWidth / 2}
                y={chartHeight + 18}
                textAnchor="middle"
                fontSize="11"
                fill={i === 6 ? "#e5e5e5" : "#737373"}
              >
                {d.label}
              </text>
            </g>
          );
        })}
      </svg>
      <div className="flex items-center gap-4 mt-2 text-xs text-neutral-500">
        <LegendDot color="#10b981" label="Claude" />
        <LegendDot color="#06b6d4" label="Codex" />
        <LegendDot color="#a855f7" label="Other" />
        <span className="ml-auto font-mono">{t("overview.max_per_day", { value: formatUSD(maxCost) })}</span>
      </div>
    </div>
  );
}

function LegendDot({ color, label }: { color: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <span className="w-2 h-2 rounded-full" style={{ background: color }} />
      {label}
    </span>
  );
}

type ProviderAgg = {
  provider: string;
  cost: number;
  input: number;
  output: number;
  cached: number;
  msgs: number;
  days: Set<string>;
  models: Map<string, { cost: number; input: number; output: number; cached: number }>;
};

// v0.3.4 — server-side provider summary row, mirrors the JSON shape
// returned by the `provider_summary` RPC in app_rpc.sql:62. iOS / Android
// already consume this; the desktop now joins them.
type ProviderTier = {
  name: string;
  quota: number;
  remaining: number;
  reset_time: string | null;
};

type ProviderSummaryRow = {
  provider: string;
  today_usage: number;
  total_usage: number;
  estimated_cost: number;        // 7-day rolling
  estimated_cost_today: number;
  estimated_cost_30_day: number;
  remaining: number | null;
  quota: number | null;
  plan_type: string | null;
  reset_time: string | null;
  tiers: ProviderTier[];
  /** v0.4.15 — RFC3339 timestamp from server. null for usage-only rows. */
  updated_at: string | null;
};

function Providers({ scan, paired }: { scan: ScanResult | null; paired: boolean }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  // v0.3.4 — fetch server-side provider quota / plan / tiers when paired.
  // Keyed by `paired` so a sign-in/sign-out cycle re-fetches. Errors are
  // swallowed to a soft-empty state — the local-scan card below stays
  // useful regardless.
  const [serverRows, setServerRows] = useState<ProviderSummaryRow[] | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  useEffect(() => {
    if (!paired) {
      setServerRows(null);
      setServerError(null);
      return;
    }
    let cancelled = false;
    invoke<ProviderSummaryRow[]>("get_provider_summary")
      .then((rows) => {
        if (!cancelled) {
          setServerRows(rows);
          setServerError(null);
        }
      })
      .catch((e: any) => {
        if (!cancelled) {
          setServerRows(null);
          setServerError(String(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [paired]);
  // Index server rows by provider name for O(1) lookup during render.
  const serverByProvider = useMemo(() => {
    const m = new Map<string, ProviderSummaryRow>();
    if (serverRows) {
      for (const row of serverRows) m.set(row.provider, row);
    }
    return m;
  }, [serverRows]);

  const grouped = useMemo<ProviderAgg[] | null>(() => {
    // v0.4.8 — also render cards when only server data is available
    // (paired user with valid Gemini/Codex creds but no local scan
    // history yet). v0.4.7 and earlier gated card visibility on
    // local scan cache existence, so a freshly-paired user with a
    // populated provider_quotas row but no recent local activity saw
    // an empty Providers tab. VM verification of v0.4.7 confirmed the
    // gap (Gemini card missing despite server having a row).
    if (!scan && !serverRows) return null;
    const map = new Map<string, ProviderAgg>();
    if (scan) {
      for (const e of scan.entries) {
        const cur =
          map.get(e.provider) ??
          ({
            provider: e.provider,
            cost: 0,
            input: 0,
            output: 0,
            cached: 0,
            msgs: 0,
            days: new Set<string>(),
            models: new Map(),
          } satisfies ProviderAgg);
        if (e.model === CLAUDE_MSG_BUCKET) {
          cur.msgs += e.message_count;
          map.set(e.provider, cur);
          continue;
        }
        cur.input += e.input_tokens;
        cur.output += e.output_tokens;
        cur.cached += e.cached_tokens;
        cur.cost += e.cost_usd ?? 0;
        cur.days.add(e.date);
        const m = cur.models.get(e.model) ?? { cost: 0, input: 0, output: 0, cached: 0 };
        m.cost += e.cost_usd ?? 0;
        m.input += e.input_tokens;
        m.output += e.output_tokens;
        m.cached += e.cached_tokens;
        cur.models.set(e.model, m);
        map.set(e.provider, cur);
      }
    }
    // Backfill any server-known providers absent from local scan with
    // empty aggregates. The card still renders the plan badge +
    // tier bars from server data even though local-scan numbers are
    // zero. Subtitle below distinguishes "no local activity yet" copy.
    if (serverRows) {
      for (const row of serverRows) {
        if (!map.has(row.provider)) {
          map.set(row.provider, {
            provider: row.provider,
            cost: 0,
            input: 0,
            output: 0,
            cached: 0,
            msgs: 0,
            days: new Set<string>(),
            models: new Map(),
          });
        }
      }
    }
    // Sort by cost desc; secondary key on provider name so server-only
    // entries (cost=0) have a stable order instead of insertion order.
    return Array.from(map.values()).sort((a, b) => {
      if (b.cost !== a.cost) return b.cost - a.cost;
      return a.provider.localeCompare(b.provider);
    });
  }, [scan, serverRows]);

  if (!grouped) return null;

  function toggle(provider: string) {
    const next = new Set(expanded);
    if (next.has(provider)) next.delete(provider);
    else next.add(provider);
    setExpanded(next);
  }

  const maxCost = Math.max(...grouped.map((g) => g.cost), 1);

  return (
    <div className="space-y-3">
      {paired && serverError && (
        <div className="text-xs text-neutral-500 px-3 py-2 rounded-md border border-neutral-800/60 bg-neutral-900/30">
          {t("providers.server_unavailable")}
        </div>
      )}
      {grouped.map((v) => {
        const isOpen = expanded.has(v.provider);
        const barPct = (v.cost / maxCost) * 100;
        const sortedModels = Array.from(v.models.entries())
          .sort((a, b) => b[1].cost - a[1].cost)
          .slice(0, 10);
        const srv = serverByProvider.get(v.provider);
        return (
          <div
            key={v.provider}
            className="rounded-lg border border-neutral-800 bg-neutral-900/40 overflow-hidden"
          >
            <button
              type="button"
              onClick={() => toggle(v.provider)}
              className="w-full p-4 flex items-center justify-between hover:bg-neutral-900/60 text-left"
            >
              <div className="flex items-center gap-3 flex-1 min-w-0">
                <span className="text-neutral-500 text-xs w-4">{isOpen ? "▼" : "▶"}</span>
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 flex-wrap">
                    <span className="font-semibold">{v.provider}</span>
                    {srv?.plan_type && <PlanBadge plan={srv.plan_type} />}
                    {srv && isStaleProviderRow(srv.updated_at) && (
                      <span
                        className="px-1.5 py-0.5 text-xs rounded bg-amber-950/60 border border-amber-800 text-amber-300"
                        title={t("providers.stale_tooltip", {
                          age: formatRelativeMinutes(srv.updated_at!),
                        })}
                      >
                        {t("providers.stale_badge")}
                      </span>
                    )}
                  </div>
                  <div className="text-xs text-neutral-500">
                    {/* v0.4.8 — when server-only provider has no local
                        scan history (zero days/msgs/models), show a
                        single "no local activity yet" line instead of
                        "0 active days · 0 msgs". Tier bars below come
                        from server data. */}
                    {v.days.size === 0 && v.msgs === 0 && v.models.size === 0 ? (
                      <span>{t("providers.no_local_scan_yet")}</span>
                    ) : (
                      <>
                        {t("providers.active_days", { count: v.days.size })}
                        {" · "}
                        {t("providers.messages", { count: v.msgs })}
                        {v.models.size > 0 && (
                          <>
                            {" · "}
                            {t("providers.models", { count: v.models.size })}
                          </>
                        )}
                      </>
                    )}
                  </div>
                  <div className="mt-2 h-1 bg-neutral-800 rounded overflow-hidden max-w-xs">
                    <div
                      className="h-full bg-gradient-to-r from-emerald-500 to-cyan-500"
                      style={{ width: `${barPct}%` }}
                    />
                  </div>
                </div>
              </div>
              <div className="text-right shrink-0">
                <div className="font-mono text-lg">{formatUSD(v.cost)}</div>
                <div className="text-xs text-neutral-500">
                  {t("providers.io_tokens", { value: formatInt(v.input + v.output) })}
                </div>
              </div>
            </button>

            {/* v0.3.4 — server-side quota: tier bars per the iOS/macOS Mac
                app's UI pattern. Falls back to a single overall bar when
                tiers is empty AND quota > 0 AND provider != "Claude"
                (Claude's empty-tiers state means data unavailable, not
                "100% remaining" — matches the Mac heuristic). */}
            {paired && srv && (
              <div className="px-4 pb-3 pt-2 border-t border-neutral-800/50 space-y-2">
                {srv.tiers.length > 0 ? (
                  <div className="space-y-1.5">
                    {srv.tiers.map((tier) => {
                      // v0.4.5 — bar visualizes REMAINING (matches the
                      // "X/Y left" text label). Color heat by remaining:
                      // - ≤10% left → red (critical)
                      // - ≤40% left → amber (warning)
                      // - >40% left → green (safe)
                      const remaining = Math.max(0, Math.min(tier.quota, tier.remaining));
                      const remainingPct = tier.quota > 0 ? (remaining / tier.quota) * 100 : 0;
                      const color =
                        remainingPct <= 10
                          ? "from-rose-500 to-red-500"
                          : remainingPct <= 40
                            ? "from-amber-400 to-orange-500"
                            : "from-emerald-500 to-cyan-500";
                      return (
                        <div key={tier.name} className="text-xs">
                          <div className="flex justify-between text-neutral-400 mb-0.5">
                            <span>{tier.name}</span>
                            <span className="font-mono">
                              {t("providers.tier_left", {
                                remaining: formatInt(tier.remaining),
                                quota: formatInt(tier.quota),
                              })}
                            </span>
                          </div>
                          <div className="h-1.5 bg-neutral-800 rounded overflow-hidden">
                            <div
                              className={`h-full bg-gradient-to-r ${color}`}
                              style={{ width: `${Math.min(100, remainingPct)}%` }}
                            />
                          </div>
                        </div>
                      );
                    })}
                  </div>
                ) : srv.quota && srv.quota > 0 && srv.provider !== "Claude" ? (
                  // Single overall bar for non-Claude providers with a flat quota.
                  // v0.4.5 — same direction flip as the per-tier bars above.
                  (() => {
                    const remaining = Math.max(0, Math.min(srv.quota, srv.remaining ?? 0));
                    const remainingPct = srv.quota > 0 ? (remaining / srv.quota) * 100 : 0;
                    const color =
                      remainingPct <= 10
                        ? "from-rose-500 to-red-500"
                        : remainingPct <= 40
                          ? "from-amber-400 to-orange-500"
                          : "from-emerald-500 to-cyan-500";
                    return (
                      <div className="text-xs">
                        <div className="flex justify-between text-neutral-400 mb-0.5">
                          <span>{t("providers.quota_label")}</span>
                          <span className="font-mono">
                            {t("providers.tier_left", {
                              remaining: formatInt(srv.remaining ?? 0),
                              quota: formatInt(srv.quota),
                            })}
                          </span>
                        </div>
                        <div className="h-1.5 bg-neutral-800 rounded overflow-hidden">
                          <div
                            className={`h-full bg-gradient-to-r ${color}`}
                            style={{ width: `${Math.min(100, remainingPct)}%` }}
                          />
                        </div>
                      </div>
                    );
                  })()
                ) : (
                  // Claude with empty tiers, or any provider where quota
                  // data isn't available — be honest, don't fake a bar.
                  <div className="text-xs text-neutral-500">
                    {t("providers.quota_unavailable")}
                  </div>
                )}
              </div>
            )}

            {isOpen && sortedModels.length > 0 && (
              <div className="px-4 pb-4 pt-1 border-t border-neutral-800/50">
                <table className="w-full text-xs">
                  <thead className="text-neutral-500">
                    <tr>
                      <th className="text-left font-normal py-1.5">{t("providers.col_model")}</th>
                      <th className="text-right font-normal py-1.5">{t("providers.col_input")}</th>
                      <th className="text-right font-normal py-1.5">{t("providers.col_output")}</th>
                      <th className="text-right font-normal py-1.5">{t("providers.col_cost")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {sortedModels.map(([model, m]) => (
                      <tr key={model} className="border-t border-neutral-800/40">
                        <td className="py-1.5 font-mono truncate max-w-[22ch]">{model}</td>
                        <td className="py-1.5 text-right font-mono">{formatInt(m.input)}</td>
                        <td className="py-1.5 text-right font-mono">{formatInt(m.output)}</td>
                        <td className="py-1.5 text-right font-mono">{formatUSD(m.cost)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </div>
        );
      })}
      {grouped.length === 0 && (
        <div className="text-sm text-neutral-500">{t("providers.no_usage")}</div>
      )}
    </div>
  );
}

// v0.3.4 — Plan-type badge. Matches the Mac convention: Free/free →
// orange (signals "upgrade available"); anything else → emerald.
function PlanBadge({ plan }: { plan: string }) {
  const isFree = plan.toLowerCase() === "free";
  const cls = isFree
    ? "border border-orange-700/60 text-orange-300 bg-orange-950/40"
    : "border border-emerald-700/60 text-emerald-300 bg-emerald-950/40";
  return (
    <span className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${cls}`}>{plan}</span>
  );
}

function Settings({
  config,
  scan,
  lastSync,
  onPaired,
  onUnpaired,
  onSynced,
}: {
  config: ConfigView | null;
  scan: ScanResult | null;
  lastSync: { at: Date; report: SyncReport } | null;
  onPaired: () => Promise<void>;
  onUnpaired: () => Promise<void>;
  onSynced: (r: SyncReport) => void;
}) {
  const { t } = useTranslation();
  const [code, setCode] = useState("");
  const [deviceName, setDeviceName] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  // v0.3.0 — OTP sign-in state. When `paired` is true, the form is
  // never shown; we keep the state hooks at the top level so React
  // doesn't unmount/remount on every transition.
  const [otpStage, setOtpStage] = useState<"email" | "code" | "signed-in">("email");
  const [otpEmail, setOtpEmail] = useState("");
  const [otpCode, setOtpCode] = useState("");
  const [otpDeviceName, setOtpDeviceName] = useState("");
  const [resendCooldown, setResendCooldown] = useState(0);
  const [showLegacyPair, setShowLegacyPair] = useState(false);

  // Pre-fill the device-name input with whatever Rust whoami suggests.
  // Synchronous-feeling on first paint because the Tauri command is
  // local-only.
  useEffect(() => {
    if (otpDeviceName === "") {
      invoke<string>("auth_default_device_name")
        .then((name) => setOtpDeviceName(name))
        .catch(() => {});
    }
    // We only want this on initial mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Resend cooldown ticker.
  useEffect(() => {
    if (resendCooldown <= 0) return;
    const id = setTimeout(() => setResendCooldown((s) => s - 1), 1000);
    return () => clearTimeout(id);
  }, [resendCooldown]);
  const [updater, setUpdater] = useState<
    | { state: "idle" }
    | { state: "checking" }
    | { state: "up-to-date" }
    | { state: "available"; version: string; body?: string }
    | { state: "downloading"; pct: number }
    | { state: "ready" }
    | { state: "error"; text: string }
  >({ state: "idle" });

  async function doCheckUpdate() {
    setUpdater({ state: "checking" });
    try {
      const upd = await checkUpdate();
      if (!upd) {
        setUpdater({ state: "up-to-date" });
        return;
      }
      setUpdater({ state: "available", version: upd.version, body: upd.body });
      let total = 0;
      let downloaded = 0;
      await upd.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
          setUpdater({ state: "downloading", pct: 0 });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          const pct = total > 0 ? Math.round((downloaded / total) * 100) : 0;
          setUpdater({ state: "downloading", pct });
        } else if (event.event === "Finished") {
          setUpdater({ state: "ready" });
        }
      });
    } catch (e: any) {
      setUpdater({ state: "error", text: String(e) });
    }
  }

  async function doSendOtp(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setMsg(null);
    try {
      await invoke("auth_send_otp", { email: otpEmail.trim() });
      setOtpStage("code");
      setResendCooldown(30);
    } catch (err: any) {
      setMsg({ kind: "err", text: String(err) });
    } finally {
      setBusy(false);
    }
  }

  async function doVerifyOtp(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setMsg(null);
    try {
      const result = await invoke<{ device_id: string; user_id: string; device_name: string }>(
        "auth_verify_otp",
        {
          email: otpEmail.trim(),
          code: otpCode.trim(),
          deviceName: otpDeviceName.trim() || null,
        },
      );
      setMsg({
        kind: "ok",
        text: t("messages.signed_in_as", {
          email: otpEmail.trim(),
          name: result.device_name,
        }),
      });
      setOtpCode("");
      setOtpStage("signed-in");
      await onPaired();
    } catch (err: any) {
      setMsg({ kind: "err", text: String(err) });
    } finally {
      setBusy(false);
    }
  }

  async function doResendOtp() {
    if (resendCooldown > 0 || busy) return;
    setBusy(true);
    setMsg(null);
    try {
      await invoke("auth_send_otp", { email: otpEmail.trim() });
      setResendCooldown(30);
    } catch (err: any) {
      setMsg({ kind: "err", text: String(err) });
    } finally {
      setBusy(false);
    }
  }

  function doResetOtp() {
    setOtpStage("email");
    setOtpCode("");
    setMsg(null);
  }

  async function doPair(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setMsg(null);
    try {
      const result = await invoke<{ device_id: string; user_id: string; device_name: string }>("pair_device", {
        pairingCode: code.trim(),
        deviceName: deviceName.trim() || null,
      });
      setMsg({
        kind: "ok",
        text: t("messages.paired_as", { name: result.device_name, id: result.device_id.slice(0, 8) }),
      });
      setCode("");
      await onPaired();
    } catch (e: any) {
      setMsg({ kind: "err", text: String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function doUnpair() {
    if (!confirm(t("settings.unpair_confirm"))) return;
    setBusy(true);
    setMsg(null);
    try {
      await invoke("unpair_device");
      setMsg({ kind: "ok", text: t("messages.device_unpaired") });
      // v0.3.3: reset the OTP flow state so the email-input form
      // renders again on the next paint. doVerifyOtp leaves
      // otpStage="signed-in"; without this, the unpaired view rendered
      // the heading + hint + legacy disclosure but no email input
      // (neither the "email" nor "code" stage block matched). VM E2E
      // had to switch tabs and back to recover.
      setOtpStage("email");
      setOtpCode("");
      await onUnpaired();
    } catch (e: any) {
      setMsg({ kind: "err", text: String(e) });
    } finally {
      setBusy(false);
    }
  }

  async function doSyncNow() {
    setBusy(true);
    setMsg(null);
    try {
      const report = await invoke<SyncReport>("sync_now");
      onSynced(report);
      setMsg({
        kind: "ok",
        text: t("messages.sync_ok", {
          sessions: report.sessions_synced,
          alerts: report.alerts_synced,
          metrics: report.metrics_synced,
        }),
      });
    } catch (e: any) {
      setMsg({ kind: "err", text: String(e) });
    } finally {
      setBusy(false);
    }
  }

  const paired = !!config?.paired;

  return (
    <div className="max-w-2xl space-y-6">
      <AboutSection paired={paired} />

      <LanguageSection />

      {paired && <BudgetSection />}

      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
        <h2 className="text-sm font-semibold text-neutral-300 mb-2">{t("settings.account_heading")}</h2>
        <dl className="grid grid-cols-[140px_1fr] gap-y-1 text-sm">
          <dt className="text-neutral-500">{t("settings.status")}</dt>
          <dd>
            <PairBadge paired={paired} />
          </dd>
          <dt className="text-neutral-500">{t("settings.device_name")}</dt>
          <dd className="font-mono text-xs">{config?.device_name ?? t("misc.none")}</dd>
          <dt className="text-neutral-500">{t("settings.device_id")}</dt>
          <dd className="font-mono text-xs truncate">{config?.device_id ?? t("misc.none")}</dd>
          <dt className="text-neutral-500">{t("settings.user_id")}</dt>
          <dd className="font-mono text-xs truncate">{config?.user_id ?? t("misc.none")}</dd>
          <dt className="text-neutral-500">{t("settings.helper_version")}</dt>
          <dd className="font-mono text-xs">{config?.helper_version ?? t("misc.none")}</dd>
        </dl>
      </section>

      {!paired && (
        <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
          <h2 className="text-sm font-semibold text-neutral-300 mb-1">
            {t("auth.signin.heading")}
          </h2>
          <p className="text-xs text-neutral-500 mb-3">{t("auth.signin.hint")}</p>

          {otpStage === "email" && (
            <form onSubmit={doSendOtp} className="space-y-3">
              <div>
                <label className="block text-xs text-neutral-400 mb-1">
                  {t("auth.signin.email_label")}
                </label>
                <input
                  type="email"
                  required
                  value={otpEmail}
                  onChange={(e) => setOtpEmail(e.target.value)}
                  placeholder="you@example.com"
                  className="w-full max-w-sm px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
                  autoFocus
                />
              </div>
              <button
                type="submit"
                disabled={busy || !otpEmail.trim()}
                className="px-4 py-2 rounded-md bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-medium disabled:opacity-50"
              >
                {busy ? t("auth.signin.sending") : t("auth.signin.send_code")}
              </button>
            </form>
          )}

          {otpStage === "code" && (
            <form onSubmit={doVerifyOtp} className="space-y-3">
              <p className="text-xs text-neutral-400">
                {t("auth.signin.code_sent", { email: otpEmail.trim() })}
              </p>
              <div>
                <label className="block text-xs text-neutral-400 mb-1">
                  {t("auth.signin.code_label")}
                </label>
                <input
                  type="text"
                  inputMode="numeric"
                  pattern="\d+"
                  value={otpCode}
                  onChange={(e) => setOtpCode(e.target.value.replace(/\D/g, ""))}
                  placeholder=""
                  className="w-44 px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 text-center font-mono tracking-widest text-lg focus:outline-none focus:border-emerald-500"
                  autoFocus
                />
              </div>
              <div>
                <label className="block text-xs text-neutral-400 mb-1">
                  {t("auth.signin.device_name_optional")}
                </label>
                <input
                  type="text"
                  value={otpDeviceName}
                  onChange={(e) => setOtpDeviceName(e.target.value)}
                  placeholder={t("auth.signin.device_name_placeholder")}
                  className="w-full max-w-sm px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
                />
              </div>
              <div className="flex gap-2">
                <button
                  type="submit"
                  disabled={busy || otpCode.length < 4}
                  className="px-4 py-2 rounded-md bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-medium disabled:opacity-50"
                >
                  {busy ? t("auth.signin.verifying") : t("auth.signin.verify")}
                </button>
                <button
                  type="button"
                  onClick={doResendOtp}
                  disabled={busy || resendCooldown > 0}
                  className="px-3 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700 disabled:opacity-50"
                >
                  {resendCooldown > 0
                    ? t("auth.signin.resend_in", { seconds: resendCooldown })
                    : t("auth.signin.resend")}
                </button>
                <button
                  type="button"
                  onClick={doResetOtp}
                  disabled={busy}
                  className="px-3 py-2 rounded-md text-sm text-neutral-400 hover:text-neutral-200 disabled:opacity-50"
                >
                  {t("auth.signin.back")}
                </button>
              </div>
              <p className="text-xs text-neutral-600">{t("auth.signin.spam_hint")}</p>
            </form>
          )}

          <div className="mt-4 pt-3 border-t border-neutral-800">
            <button
              type="button"
              onClick={() => setShowLegacyPair((v) => !v)}
              className="text-xs text-neutral-500 hover:text-neutral-300"
            >
              {showLegacyPair
                ? t("auth.signin.hide_legacy")
                : t("auth.signin.show_legacy")}
            </button>
            {showLegacyPair && (
              <div className="mt-3">
                <p className="text-xs text-neutral-500 mb-2">{t("settings.pair_hint")}</p>
                <form onSubmit={doPair} className="space-y-3">
                  <div>
                    <label className="block text-xs text-neutral-400 mb-1">
                      {t("settings.pairing_code")}
                    </label>
                    <input
                      type="text"
                      inputMode="numeric"
                      pattern="\d{6}"
                      maxLength={6}
                      value={code}
                      onChange={(e) => setCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
                      placeholder="123456"
                      className="w-32 px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 text-center font-mono tracking-widest text-lg focus:outline-none focus:border-emerald-500"
                    />
                  </div>
                  <div>
                    <label className="block text-xs text-neutral-400 mb-1">
                      {t("settings.device_name_optional")}
                    </label>
                    <input
                      type="text"
                      value={deviceName}
                      onChange={(e) => setDeviceName(e.target.value)}
                      placeholder={t("settings.device_name_placeholder")}
                      className="w-full max-w-sm px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
                    />
                  </div>
                  <button
                    type="submit"
                    disabled={busy || code.length !== 6}
                    className="px-4 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-white text-sm border border-neutral-700 disabled:opacity-50"
                  >
                    {busy ? t("action.pairing") : t("action.pair_device")}
                  </button>
                </form>
              </div>
            )}
          </div>
        </section>
      )}

      {paired && (
        <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
          <h2 className="text-sm font-semibold text-neutral-300">{t("settings.sync_heading")}</h2>
          {lastSync && (
            <div className="text-xs text-neutral-500">
              {t("settings.last_sync", {
                time: lastSync.at.toLocaleTimeString(),
                sessions: lastSync.report.live_sessions_sent,
                cost: formatUSD(lastSync.report.total_cost_usd),
                files: lastSync.report.files_scanned,
              })}
            </div>
          )}
          <div className="flex gap-2">
            <button
              onClick={doSyncNow}
              disabled={busy}
              className="px-4 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700 disabled:opacity-50"
            >
              {busy ? t("action.syncing") : t("action.sync_now")}
            </button>
            <button
              onClick={doUnpair}
              disabled={busy}
              className="px-4 py-2 rounded-md bg-red-950/60 hover:bg-red-900/60 text-sm border border-red-900 text-red-200 disabled:opacity-50"
            >
              {t("action.unpair_device")}
            </button>
          </div>
          <p className="text-xs text-neutral-600">
            {t("settings.auto_sync_hint")}
          </p>
        </section>
      )}

      <ExportSection scan={scan} />

      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
        <h2 className="text-sm font-semibold text-neutral-300">{t("settings.updates_heading")}</h2>
        <UpdaterPanel
          state={updater}
          onCheck={doCheckUpdate}
          onRelaunch={async () => {
            try {
              await relaunch();
            } catch (e: any) {
              setUpdater({ state: "error", text: String(e) });
            }
          }}
        />
        <p className="text-xs text-neutral-600" dangerouslySetInnerHTML={{ __html: t("settings.updates_hint") }} />
      </section>

      {/* v0.4.7 — Integrations placed at true bottom of Settings tab,
          after Updates, per v0.4.6 dev plan §3 (was incorrectly between
          Export and Updates in v0.4.6 first ship; VM verification flagged
          the discrepancy). Kept this position so the section visually
          reads as "advanced / opt-in" tail of the Settings tab. */}
      <IntegrationsSection />

      {msg && (
        <div
          className={`px-4 py-3 rounded-md text-sm border ${
            msg.kind === "ok"
              ? "bg-emerald-950/50 border-emerald-900 text-emerald-200"
              : "bg-red-950/60 border-red-900 text-red-200"
          }`}
        >
          {msg.text}
        </div>
      )}
    </div>
  );
}

// v0.4.6 — Settings UI for provider credentials (Cursor / Copilot /
// OpenRouter). Calls Tauri commands `get_provider_creds` /
// `set_provider_creds`. Backend lives in `provider_creds.rs`.
//
// Design per Gemini 3.1 Pro 2026-05-04 review:
// - No-peek (#1): UI never re-displays raw saved value, only "Configured"
//   / "Not set" status.
// - Friendly error copy (#2): map HTTP statuses to localized strings.
// - Single-line password input (#3): Cursor cookie too, not textarea.
// - "Integrations" placement (#6): bottom of Settings tab, dedicated section.
// - 2-state save flow (#7): spinner during save+sync, then green "Configured".
// - Clear confirmation modal (#8): one-click is a UX trap.
// - OpenRouter base URL behind Advanced toggle (#9): default-hidden.

type ProviderCredsView = {
  cursor_cookie_set: boolean;
  copilot_token_set: boolean;
  openrouter_api_key_set: boolean;
  openrouter_base_url: string | null;
  env_override_cursor: boolean;
  env_override_copilot: boolean;
  env_override_openrouter_key: boolean;
  env_override_openrouter_url: boolean;
};

type ProviderCredsUpdateKey =
  | "cursor_cookie"
  | "copilot_token"
  | "openrouter_api_key"
  | "openrouter_base_url";

function IntegrationsSection() {
  const { t } = useTranslation();
  const [view, setView] = useState<ProviderCredsView | null>(null);
  const [drafts, setDrafts] = useState<Partial<Record<ProviderCredsUpdateKey, string>>>({});
  const [savingField, setSavingField] = useState<ProviderCredsUpdateKey | null>(null);
  const [confirmClear, setConfirmClear] = useState<ProviderCredsUpdateKey | null>(null);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    try {
      const v = await invoke<ProviderCredsView>("get_provider_creds");
      setView(v);
    } catch (e: any) {
      setError(String(e));
    }
  }

  useEffect(() => {
    refresh();
  }, []);

  async function saveField(key: ProviderCredsUpdateKey, value: string) {
    setSavingField(key);
    setError(null);
    try {
      const v = await invoke<ProviderCredsView>("set_provider_creds", {
        update: { [key]: value },
      });
      setView(v);
      setDrafts((d) => {
        const next = { ...d };
        delete next[key];
        return next;
      });
    } catch (e: any) {
      setError(String(e));
    } finally {
      setSavingField(null);
    }
  }

  if (!view) {
    return (
      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
        <h2 className="text-sm font-semibold text-neutral-300">
          {t("settings.integrations.heading")}
        </h2>
      </section>
    );
  }

  const rows: {
    provider: string;
    key: ProviderCredsUpdateKey;
    labelKey: string;
    helpKey: string;
    isSet: boolean;
    envOverride: boolean;
    envVar: string;
  }[] = [
    {
      provider: "Cursor",
      key: "cursor_cookie",
      labelKey: "settings.integrations.cursor_cookie_label",
      helpKey: "settings.integrations.cursor_cookie_help",
      isSet: view.cursor_cookie_set,
      envOverride: view.env_override_cursor,
      envVar: "CURSOR_COOKIE",
    },
    {
      provider: "Copilot",
      key: "copilot_token",
      labelKey: "settings.integrations.copilot_token_label",
      helpKey: "settings.integrations.copilot_token_help",
      isSet: view.copilot_token_set,
      envOverride: view.env_override_copilot,
      envVar: "COPILOT_API_TOKEN",
    },
    {
      provider: "OpenRouter",
      key: "openrouter_api_key",
      labelKey: "settings.integrations.openrouter_api_key_label",
      helpKey: "settings.integrations.openrouter_api_key_help",
      isSet: view.openrouter_api_key_set,
      envOverride: view.env_override_openrouter_key,
      envVar: "OPENROUTER_API_KEY",
    },
  ];

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-4">
      <h2 className="text-sm font-semibold text-neutral-300">
        {t("settings.integrations.heading")}
      </h2>
      <p className="text-xs text-neutral-500">{t("settings.integrations.description")}</p>

      {rows.map((row) => {
        const draftVal = drafts[row.key];
        const isSaving = savingField === row.key;
        const canSave = draftVal != null && draftVal !== "" && !isSaving;
        return (
          <div key={row.key} className="space-y-1">
            <div className="flex items-baseline justify-between gap-2">
              <label className="text-xs font-medium text-neutral-300">{t(row.labelKey)}</label>
              <span
                className={`text-xs ${row.isSet ? "text-emerald-300" : "text-neutral-500"}`}
              >
                {t(
                  row.isSet
                    ? "settings.integrations.status_configured"
                    : "settings.integrations.status_not_set"
                )}
              </span>
            </div>
            <p className="text-xs text-neutral-500">{t(row.helpKey)}</p>
            {row.envOverride && (
              <p className="text-xs text-amber-300 bg-amber-950/30 border border-amber-900 rounded px-2 py-1">
                {t("settings.integrations.env_override_banner", { var: row.envVar })}
              </p>
            )}
            <div className="flex gap-2">
              <input
                type="password"
                value={draftVal ?? ""}
                onChange={(e) => setDrafts({ ...drafts, [row.key]: e.target.value })}
                placeholder={row.isSet ? "••••••••" : ""}
                spellCheck={false}
                autoComplete="off"
                className="flex-1 px-2 py-1 text-xs font-mono bg-neutral-950 border border-neutral-800 rounded text-neutral-200"
                disabled={isSaving}
              />
              <button
                type="button"
                onClick={() => canSave && saveField(row.key, draftVal!)}
                disabled={!canSave}
                className="px-3 py-1 text-xs rounded bg-emerald-950/60 hover:bg-emerald-900/60 border border-emerald-900 text-emerald-200 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                {isSaving ? "…" : t("settings.integrations.save_button")}
              </button>
              {row.isSet && (
                <button
                  type="button"
                  onClick={() => setConfirmClear(row.key)}
                  disabled={isSaving}
                  className="px-3 py-1 text-xs rounded bg-red-950/40 hover:bg-red-900/60 border border-red-900 text-red-300 disabled:opacity-40"
                >
                  {t("settings.integrations.clear_button")}
                </button>
              )}
            </div>
          </div>
        );
      })}

      {/* OpenRouter base URL — Advanced toggle (Gemini #9) */}
      <div className="border-t border-neutral-800 pt-3">
        <button
          type="button"
          onClick={() => setShowAdvanced(!showAdvanced)}
          className="text-xs text-neutral-400 hover:text-neutral-200"
        >
          {showAdvanced ? "▼ " : "▶ "}
          {t("settings.integrations.openrouter_advanced_toggle")}
        </button>
        {showAdvanced && (
          <div className="mt-2 space-y-1">
            <label className="text-xs text-neutral-300">
              {t("settings.integrations.openrouter_base_url_label")}
            </label>
            <div className="flex gap-2">
              <input
                type="text"
                value={drafts.openrouter_base_url ?? view.openrouter_base_url ?? ""}
                onChange={(e) =>
                  setDrafts({ ...drafts, openrouter_base_url: e.target.value })
                }
                placeholder={t("settings.integrations.openrouter_base_url_placeholder") || ""}
                className="flex-1 px-2 py-1 text-xs font-mono bg-neutral-950 border border-neutral-800 rounded text-neutral-200"
              />
              <button
                type="button"
                onClick={() =>
                  saveField("openrouter_base_url", drafts.openrouter_base_url ?? "")
                }
                disabled={
                  drafts.openrouter_base_url == null || savingField === "openrouter_base_url"
                }
                className="px-3 py-1 text-xs rounded bg-emerald-950/60 hover:bg-emerald-900/60 border border-emerald-900 text-emerald-200 disabled:opacity-40"
              >
                {savingField === "openrouter_base_url"
                  ? "…"
                  : t("settings.integrations.save_button")}
              </button>
            </div>
          </div>
        )}
      </div>

      {error && (
        <div className="px-3 py-2 text-xs rounded bg-red-950/60 border border-red-900 text-red-200">
          {error}
        </div>
      )}

      {/* Confirm modal for clear (Gemini #8) */}
      {confirmClear && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 px-4">
          <div className="bg-neutral-900 border border-neutral-800 rounded-lg p-5 max-w-sm w-full space-y-3 shadow-xl">
            <h3 className="text-sm font-semibold text-neutral-100">
              {t("settings.integrations.clear_confirm_title", {
                provider: rows.find((r) => r.key === confirmClear)?.provider ?? confirmClear,
              })}
            </h3>
            <p className="text-xs text-neutral-400">
              {t("settings.integrations.clear_confirm_body")}
            </p>
            <div className="flex justify-end gap-2 pt-1">
              <button
                type="button"
                onClick={() => setConfirmClear(null)}
                className="px-3 py-1 text-xs rounded border border-neutral-700 text-neutral-300 hover:bg-neutral-800"
              >
                {t("action.cancel")}
              </button>
              <button
                type="button"
                onClick={async () => {
                  const k = confirmClear;
                  setConfirmClear(null);
                  await saveField(k, "");
                }}
                className="px-3 py-1 text-xs rounded bg-red-950/60 border border-red-900 text-red-200 hover:bg-red-900/60"
              >
                {t("settings.integrations.clear_confirm_action")}
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}

function ExportSection({ scan }: { scan: ScanResult | null }) {
  const { t } = useTranslation();

  function triggerDownload(content: string, filename: string, mime: string) {
    const blob = new Blob([content], { type: mime });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }

  function exportCsv() {
    if (!scan) return;
    const rows: (string | number | null)[][] = [
      ["date", "provider", "model", "input_tokens", "cached_tokens", "output_tokens", "cost_usd", "message_count"],
      ...scan.entries
        .filter((e) => e.model !== CLAUDE_MSG_BUCKET)
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
    const stamp = new Date().toISOString().slice(0, 10);
    triggerDownload(rowsToCsv(rows), `cli-pulse-usage-${stamp}.csv`, "text/csv");
  }

  function exportJson() {
    if (!scan) return;
    const stamp = new Date().toISOString().slice(0, 10);
    triggerDownload(JSON.stringify(scan, null, 2), `cli-pulse-usage-${stamp}.json`, "application/json");
  }

  const days = scan?.days_scanned ?? 30;
  const disabled = !scan;

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
      <h2 className="text-sm font-semibold text-neutral-300">{t("settings.export_heading")}</h2>
      <p className="text-xs text-neutral-500">{t("settings.export_hint", { days })}</p>
      <div className="flex gap-2">
        <button
          onClick={exportCsv}
          disabled={disabled}
          className="px-4 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700 disabled:opacity-50"
        >
          {t("settings.export_csv")}
        </button>
        <button
          onClick={exportJson}
          disabled={disabled}
          className="px-4 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700 disabled:opacity-50"
        >
          {t("settings.export_json")}
        </button>
      </div>
    </section>
  );
}

type DiagnosticSnapshot = {
  app_version: string;
  os: string;
  arch: string;
  family: string;
  paired: boolean;
  device_id_short: string | null;
  cache_dir: string | null;
  log_dir: string | null;
  /** v0.4.16 — "os_keychain" or "file"; surfaces fallback on Linux without libsecret. */
  provider_creds_backend: "os_keychain" | "file";
};

function AboutSection({ paired }: { paired: boolean }) {
  const { t } = useTranslation();
  const [diag, setDiag] = useState<DiagnosticSnapshot | null>(null);
  const [copied, setCopied] = useState(false);

  // Re-fetch diagnostics whenever the paired state flips. v0.3.2 E2E
  // surfaced that a fresh OTP sign-in left the About panel showing the
  // pre-pair "Not paired: -" — the dependency array was [] so the
  // diagnostic_snapshot was a one-shot. The Account section above
  // updates correctly via the parent's config refetch; About now
  // tracks the same signal so the diagnostics-copy block doesn't leak
  // stale state into support tickets.
  useEffect(() => {
    invoke<DiagnosticSnapshot>("diagnostic_snapshot")
      .then(setDiag)
      .catch((e) => console.warn("diagnostic_snapshot failed", e));
  }, [paired]);

  function diagText(d: DiagnosticSnapshot): string {
    return [
      `CLI Pulse Desktop ${d.app_version}`,
      `Platform: ${d.family} (${d.arch})`,
      `OS label: ${d.os}`,
      `Paired: ${d.paired ? `yes (device ${d.device_id_short ?? "?"}…)` : "no"}`,
      `Cache dir: ${d.cache_dir ?? "(none)"}`,
      `Logs: ${d.log_dir ?? "(unavailable)"}`,
      // v0.4.17 — surface the provider-creds backend so security-conscious
      // users can verify the OS keychain (vs. the v0.4.6 plaintext file
      // fallback used on Linux without libsecret). v0.4.16 wired the
      // backend into DiagnosticSnapshot but missed adding the formatter
      // line — VM verification of v0.4.16 caught this gap.
      `Creds backend: ${d.provider_creds_backend === "os_keychain" ? "OS keychain" : "file (keyring unavailable)"}`,
      `User agent: ${navigator.userAgent}`,
    ].join("\n");
  }

  async function copyDiag() {
    if (!diag) return;
    try {
      await navigator.clipboard.writeText(diagText(diag));
      setCopied(true);
      setTimeout(() => setCopied(false), 1600);
    } catch (e) {
      console.warn("clipboard write failed", e);
    }
  }

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
      <h2 className="text-sm font-semibold text-neutral-300">{t("settings.about_heading")}</h2>
      {diag ? (
        <>
          <dl className="grid grid-cols-[140px_1fr] gap-y-1 text-sm">
            <dt className="text-neutral-500">{t("settings.about_version")}</dt>
            <dd className="font-mono text-xs">{diag.app_version}</dd>
            <dt className="text-neutral-500">{t("settings.about_platform")}</dt>
            <dd className="font-mono text-xs">{diag.family} · {diag.arch}</dd>
            <dt className="text-neutral-500">
              {diag.paired ? t("settings.about_paired_for") : t("settings.about_not_paired")}
            </dt>
            <dd className="font-mono text-xs truncate">
              {diag.paired ? `${diag.device_id_short}…` : t("misc.none")}
            </dd>
          </dl>
          <p className="text-xs text-neutral-500">{t("settings.about_diagnostics_hint")}</p>
          <div className="flex items-center gap-2">
            <button
              onClick={copyDiag}
              className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800"
            >
              {copied ? `✓ ${t("settings.about_copied")}` : t("settings.about_copy_diagnostics")}
            </button>
            <a
              href="https://github.com/JasonYeYuhe/cli-pulse-desktop"
              target="_blank"
              rel="noreferrer"
              className="text-xs text-emerald-400 hover:underline"
            >
              {t("settings.about_repo_link")} ↗
            </a>
          </div>
        </>
      ) : (
        <div className="text-sm text-neutral-500">{t("misc.loading")}</div>
      )}
    </section>
  );
}

function LanguageSection() {
  const { t, i18n } = useTranslation();
  const current = (i18n.language || "en") as LangCode;
  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <h2 className="text-sm font-semibold text-neutral-300 mb-2">{t("settings.language_heading")}</h2>
      <select
        value={current}
        onChange={(e) => setLang(e.target.value as LangCode)}
        className="px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 text-sm focus:outline-none focus:border-emerald-500"
      >
        {SUPPORTED_LANGS.map((l) => (
          <option key={l.code} value={l.code}>
            {l.label}
          </option>
        ))}
      </select>
    </section>
  );
}

function BudgetSection() {
  const { t } = useTranslation();
  const [thresholds, setThresholds] = useState<AlertThresholds | null>(null);
  const [daily, setDaily] = useState<string>("");
  const [weekly, setWeekly] = useState<string>("");
  const [cpu, setCpu] = useState<string>("80");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const t = await invoke<AlertThresholds>("get_thresholds");
        setThresholds(t);
        setDaily(t.daily_budget_usd != null ? String(t.daily_budget_usd) : "");
        setWeekly(t.weekly_budget_usd != null ? String(t.weekly_budget_usd) : "");
        setCpu(String(t.cpu_spike_pct ?? 80));
      } catch (e: any) {
        console.warn("get_thresholds failed", e);
      }
    })();
  }, []);

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setMsg(null);
    try {
      const dailyNum = daily.trim() === "" ? null : Number(daily);
      const weeklyNum = weekly.trim() === "" ? null : Number(weekly);
      const cpuNum = cpu.trim() === "" ? 80 : Number(cpu);
      if (dailyNum != null && (isNaN(dailyNum) || dailyNum < 0)) {
        throw new Error(t("messages.err_budget_nonneg"));
      }
      if (weeklyNum != null && (isNaN(weeklyNum) || weeklyNum < 0)) {
        throw new Error(t("messages.err_weekly_nonneg"));
      }
      if (isNaN(cpuNum) || cpuNum < 0 || cpuNum > 100) {
        throw new Error(t("messages.err_cpu_range"));
      }
      const next: AlertThresholds = {
        daily_budget_usd: dailyNum,
        weekly_budget_usd: weeklyNum,
        cpu_spike_pct: cpuNum,
      };
      await invoke("set_thresholds", { thresholds: next });
      setThresholds(next);
      setMsg({ kind: "ok", text: t("messages.budget_saved") });
    } catch (e: any) {
      setMsg({ kind: "err", text: String(e?.message ?? e) });
    } finally {
      setBusy(false);
    }
  }

  if (!thresholds) {
    return (
      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
        <h2 className="text-sm font-semibold text-neutral-300 mb-2">{t("settings.budget_heading")}</h2>
        <div className="text-sm text-neutral-500">{t("misc.loading")}</div>
      </section>
    );
  }

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <h2 className="text-sm font-semibold text-neutral-300 mb-1">{t("settings.budget_heading")}</h2>
      <p className="text-xs text-neutral-500 mb-3">{t("settings.budget_hint")}</p>
      <form onSubmit={save} className="space-y-3 max-w-md">
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="block text-xs text-neutral-400 mb-1">{t("settings.daily_budget_usd")}</span>
            <input
              type="number"
              step="0.01"
              min="0"
              value={daily}
              onChange={(e) => setDaily(e.target.value)}
              placeholder="25"
              className="w-full px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
            />
          </label>
          <label className="block">
            <span className="block text-xs text-neutral-400 mb-1">{t("settings.weekly_budget_usd")}</span>
            <input
              type="number"
              step="0.01"
              min="0"
              value={weekly}
              onChange={(e) => setWeekly(e.target.value)}
              placeholder="150"
              className="w-full px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
            />
          </label>
        </div>
        <label className="block">
          <span className="block text-xs text-neutral-400 mb-1">{t("settings.cpu_threshold_label")}</span>
          <input
            type="number"
            min="0"
            max="100"
            step="1"
            value={cpu}
            onChange={(e) => setCpu(e.target.value)}
            className="w-24 px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
          />
          <span className="text-xs text-neutral-600 ml-2">{t("settings.cpu_threshold_help")}</span>
        </label>
        <button
          type="submit"
          disabled={busy}
          className="px-4 py-2 rounded-md bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-medium disabled:opacity-50"
        >
          {busy ? t("action.saving") : t("action.save")}
        </button>
      </form>
      {msg && (
        <div
          className={`mt-3 px-3 py-2 rounded-md text-xs border ${
            msg.kind === "ok"
              ? "bg-emerald-950/50 border-emerald-900 text-emerald-200"
              : "bg-red-950/60 border-red-900 text-red-200"
          }`}
        >
          {msg.text}
        </div>
      )}
    </section>
  );
}

function UpdaterPanel({
  state,
  onCheck,
  onRelaunch,
}: {
  state:
    | { state: "idle" }
    | { state: "checking" }
    | { state: "up-to-date" }
    | { state: "available"; version: string; body?: string }
    | { state: "downloading"; pct: number }
    | { state: "ready" }
    | { state: "error"; text: string };
  onCheck: () => void;
  onRelaunch: () => void;
}) {
  const { t } = useTranslation();
  switch (state.state) {
    case "idle":
      return (
        <button
          onClick={onCheck}
          className="px-4 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700"
        >
          {t("action.check_updates")}
        </button>
      );
    case "checking":
      return <div className="text-sm text-neutral-400">{t("action.checking")}</div>;
    case "up-to-date":
      return (
        <div className="text-sm text-emerald-300">{t("updater.up_to_date")}</div>
      );
    case "available":
      return (
        <div className="text-sm text-neutral-300">
          {t("updater.available", { version: state.version })}
        </div>
      );
    case "downloading":
      return (
        <div className="space-y-1">
          <div className="text-xs text-neutral-400">{t("updater.downloading", { pct: state.pct })}</div>
          <div className="h-1.5 bg-neutral-800 rounded overflow-hidden">
            <div
              className="h-full bg-emerald-500 transition-all"
              style={{ width: `${state.pct}%` }}
            />
          </div>
        </div>
      );
    case "ready":
      return (
        <div className="flex items-center gap-3">
          <span className="text-sm text-emerald-300">{t("updater.ready")}</span>
          <button
            onClick={onRelaunch}
            className="px-3 py-1.5 text-xs rounded-md bg-emerald-600 hover:bg-emerald-500 text-white"
          >
            {t("action.restart_now")}
          </button>
        </div>
      );
    case "error":
      return (
        <div className="text-sm text-red-300">
          {t("updater.error", {
            error: state.text.length > 160 ? state.text.slice(0, 160) + "…" : state.text,
          })}
        </div>
      );
  }
}

function Sessions({
  snapshot,
  loading,
  onRefresh,
}: {
  snapshot: SessionsSnapshot | null;
  loading: boolean;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  if (!snapshot && loading) {
    return <Skeleton />;
  }
  if (!snapshot) return null;

  const sessions = snapshot.sessions;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="text-xs text-neutral-500">
          {t("sessions.header", {
            active: sessions.length,
            total: snapshot.total_processes_seen,
            time: new Date(snapshot.collected_at).toLocaleTimeString(),
          })}
        </div>
        <button
          onClick={onRefresh}
          disabled={loading}
          className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50"
        >
          {loading ? t("action.refreshing") : t("action.refresh_now")}
        </button>
      </div>

      {sessions.length === 0 ? (
        <div className="text-sm text-neutral-500 italic py-10 text-center">
          {t("sessions.empty")}
        </div>
      ) : (
        <div className="overflow-hidden rounded-lg border border-neutral-800">
          <table className="w-full text-sm">
            <thead className="bg-neutral-900/60 text-left text-xs text-neutral-400">
              <tr>
                <th className="px-3 py-2">{t("sessions.col_provider")}</th>
                <th className="px-3 py-2">{t("sessions.col_project")}</th>
                <th className="px-3 py-2">{t("sessions.col_name")}</th>
                <th className="px-3 py-2 text-right">{t("sessions.col_cpu")}</th>
                <th className="px-3 py-2 text-right">{t("sessions.col_memory")}</th>
                <th className="px-3 py-2 text-right">{t("sessions.col_confidence")}</th>
              </tr>
            </thead>
            <tbody>
              {sessions.map((s) => (
                <tr key={s.id} className="border-t border-neutral-800">
                  <td className="px-3 py-2 font-medium">{s.provider}</td>
                  <td className="px-3 py-2 font-mono text-xs">{s.project}</td>
                  <td
                    className="px-3 py-2 font-mono text-xs max-w-sm truncate"
                    title={s.command}
                  >
                    {s.name}
                  </td>
                  <td className="px-3 py-2 text-right font-mono">
                    {(s.cpu_usage ?? 0).toFixed(1)}%
                  </td>
                  <td className="px-3 py-2 text-right font-mono">{s.memory_mb ?? 0} MB</td>
                  <td className="px-3 py-2 text-right">
                    <ConfidenceDot c={s.collection_confidence} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function ConfidenceDot({ c }: { c: "high" | "medium" | "low" }) {
  const { t } = useTranslation();
  const color =
    c === "high" ? "bg-emerald-400" : c === "medium" ? "bg-amber-400" : "bg-neutral-500";
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-neutral-400">
      <span className={`w-1.5 h-1.5 rounded-full ${color}`} />
      {t(`sessions.confidence_${c}`)}
    </span>
  );
}

function Alerts({
  alerts,
  loading,
  onRefresh,
}: {
  alerts: Alert[] | null;
  loading: boolean;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  if (!alerts && loading) return <Skeleton />;
  if (!alerts) return null;

  const bySeverity = (s: string): number => (s === "Critical" ? 0 : s === "Warning" ? 1 : 2);
  const sorted = [...alerts].sort(
    (a, b) => bySeverity(a.severity) - bySeverity(b.severity) || b.created_at.localeCompare(a.created_at)
  );

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="text-xs text-neutral-500">
          {alerts.length === 0
            ? t("alerts.nothing")
            : t("alerts.active", { count: alerts.length })}
        </div>
        <button
          onClick={onRefresh}
          disabled={loading}
          className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50"
        >
          {loading ? t("action.refreshing") : t("action.refresh")}
        </button>
      </div>

      {sorted.length === 0 ? (
        <EmptyAlertsHint />
      ) : (
        <div className="space-y-2">
          {sorted.map((a) => (
            <AlertCard key={a.id} alert={a} />
          ))}
        </div>
      )}
    </div>
  );
}

function AlertCard({ alert }: { alert: Alert }) {
  const { t } = useTranslation();
  const accent =
    alert.severity === "Critical"
      ? "border-red-800 bg-red-950/40"
      : alert.severity === "Warning"
      ? "border-amber-800 bg-amber-950/30"
      : "border-neutral-800 bg-neutral-900/40";
  const icon =
    alert.severity === "Critical" ? "🛑" : alert.severity === "Warning" ? "⚠️" : "ℹ️";
  return (
    <div className={`p-4 rounded-lg border ${accent}`}>
      <div className="flex items-start gap-3">
        <div className="text-lg leading-none mt-0.5">{icon}</div>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-semibold">{alert.title}</span>
            <span className="text-xs text-neutral-500 font-mono">{alert.type}</span>
          </div>
          <div className="text-sm text-neutral-300 mt-1">{alert.message}</div>
          <div className="text-xs text-neutral-500 mt-2 flex flex-wrap gap-x-3 gap-y-0.5">
            {alert.related_provider && <span>{t("misc.provider_label", { name: alert.related_provider })}</span>}
            {alert.related_project_name && (
              <span>{t("misc.project_label", { name: alert.related_project_name })}</span>
            )}
            <span>{new Date(alert.created_at).toLocaleString()}</span>
          </div>
        </div>
      </div>
    </div>
  );
}

function EmptyAlertsHint() {
  const { t } = useTranslation();
  return (
    <div className="p-6 rounded-lg border border-neutral-800 bg-neutral-900/30 text-sm text-neutral-400">
      <div className="font-semibold text-neutral-300 mb-1">{t("alerts.empty_title")}</div>
      <p dangerouslySetInnerHTML={{ __html: t("alerts.empty_body") }} />
    </div>
  );
}

function EntriesTable({ entries }: { entries: DailyEntry[] }) {
  const { t } = useTranslation();
  if (entries.length === 0) {
    return <div className="text-sm text-neutral-500 italic">{t("overview.no_usage_today")}</div>;
  }
  const sorted = [...entries].sort((a, b) => (b.cost_usd ?? 0) - (a.cost_usd ?? 0));
  return (
    <div className="overflow-hidden rounded-lg border border-neutral-800">
      <table className="w-full text-sm">
        <thead className="bg-neutral-900/60 text-left text-xs text-neutral-400">
          <tr>
            <th className="px-3 py-2">Provider</th>
            <th className="px-3 py-2">Model</th>
            <th className="px-3 py-2 text-right">Input</th>
            <th className="px-3 py-2 text-right">Output</th>
            <th className="px-3 py-2 text-right">Cost</th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((e, i) => (
            <tr key={i} className="border-t border-neutral-800">
              <td className="px-3 py-2">{e.provider}</td>
              <td className="px-3 py-2 font-mono text-xs">{e.model}</td>
              <td className="px-3 py-2 text-right font-mono">{formatInt(e.input_tokens)}</td>
              <td className="px-3 py-2 text-right font-mono">{formatInt(e.output_tokens)}</td>
              <td className="px-3 py-2 text-right font-mono">
                {e.cost_usd != null ? formatUSD(e.cost_usd) : "—"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function StatCard({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <div className="text-xs text-neutral-500 mb-1">{label}</div>
      <div className="text-2xl font-mono">{value}</div>
      {hint && <div className="text-xs text-neutral-600 mt-1">{hint}</div>}
    </div>
  );
}

function Skeleton() {
  const { t } = useTranslation();
  return (
    <div className="space-y-4">
      <div className="text-xs text-neutral-500">{t("misc.scanning_hint")}</div>
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        {[0, 1, 2, 3].map((i) => (
          <div
            key={i}
            className="h-24 rounded-lg border border-neutral-800 bg-neutral-900/40 animate-pulse"
          />
        ))}
      </div>
    </div>
  );
}
