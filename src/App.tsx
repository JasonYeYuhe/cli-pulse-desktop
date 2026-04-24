import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { check as checkUpdate } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
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
  metrics_uploaded: number;
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

const TABS: { key: TabKey; label: string }[] = [
  { key: "overview", label: "Overview" },
  { key: "providers", label: "Providers" },
  { key: "sessions", label: "Sessions" },
  { key: "alerts", label: "Alerts" },
  { key: "settings", label: "Settings" },
];

const CLAUDE_MSG_BUCKET = "__claude_msg__";

function formatUSD(n: number): string {
  if (n === 0) return "$0.00";
  if (n < 0.01) return `$${n.toFixed(4)}`;
  return `$${n.toFixed(2)}`;
}

function formatInt(n: number): string {
  return n.toLocaleString("en-US");
}

export default function App() {
  const [tab, setTab] = useState<TabKey>("overview");
  const [scan, setScan] = useState<ScanResult | null>(null);
  const [config, setConfig] = useState<ConfigView | null>(null);
  const [sessions, setSessions] = useState<SessionsSnapshot | null>(null);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [alerts, setAlerts] = useState<Alert[] | null>(null);
  const [alertsLoading, setAlertsLoading] = useState(false);
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
          <div className="w-7 h-7 rounded bg-gradient-to-br from-emerald-400 to-cyan-500" />
          <div>
            <div className="font-semibold text-sm">CLI Pulse</div>
            <div className="text-xs text-neutral-500">
              Desktop · Sprint 1 · {config?.device_type ?? "…"}
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <PairBadge paired={!!config?.paired} />
          <button
            onClick={runScan}
            disabled={loading}
            className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50"
          >
            {loading ? "Scanning…" : "Rescan"}
          </button>
        </div>
      </header>

      <nav className="border-b border-neutral-800 px-6 flex gap-1">
        {TABS.map((t) => (
          <button
            key={t.key}
            onClick={() => setTab(t.key)}
            className={`px-3 py-2.5 text-sm border-b-2 transition-colors ${
              tab === t.key
                ? "border-emerald-500 text-white"
                : "border-transparent text-neutral-400 hover:text-neutral-200"
            }`}
          >
            {t.label}
          </button>
        ))}
      </nav>

      <main className="flex-1 overflow-auto p-6">
        {error && (
          <div className="mb-4 px-4 py-3 rounded-md bg-red-950/60 border border-red-900 text-sm text-red-200">
            {error}
          </div>
        )}
        {tab === "overview" && <Overview scan={scan} loading={loading} />}
        {tab === "providers" && <Providers scan={scan} />}
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
  return paired ? (
    <span className="px-2 py-0.5 text-xs rounded-md bg-emerald-950/60 border border-emerald-900 text-emerald-300">
      Paired
    </span>
  ) : (
    <span className="px-2 py-0.5 text-xs rounded-md bg-neutral-800 border border-neutral-700 text-neutral-400">
      Not paired
    </span>
  );
}

function Overview({ scan, loading }: { scan: ScanResult | null; loading: boolean }) {
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

  if (!scan && loading) return <Skeleton />;
  if (!scan) return null;

  return (
    <div className="space-y-6">
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <StatCard label="Today — Cost" value={today ? formatUSD(today.cost) : "—"} hint={scan.today_key} />
        <StatCard label="Today — Tokens" value={today ? formatInt(today.tokens) : "—"} hint="input + output" />
        <StatCard label="Today — Messages" value={today ? formatInt(today.msgs) : "—"} hint="Claude only" />
        <StatCard
          label={`Last ${scan.days_scanned}d — Cost`}
          value={formatUSD(scan.total_cost_usd)}
          hint={`${scan.files_scanned} files scanned`}
        />
      </div>

      <section>
        <h2 className="text-sm font-semibold text-neutral-400 mb-2">Last 7 days — cost</h2>
        <CostTrendChart scan={scan} />
      </section>

      <section>
        <h2 className="text-sm font-semibold text-neutral-400 mb-2">Today's breakdown</h2>
        <EntriesTable
          entries={scan.entries.filter((e) => e.date === scan.today_key && e.model !== CLAUDE_MSG_BUCKET)}
        />
      </section>
    </div>
  );
}

function CostTrendChart({ scan }: { scan: ScanResult }) {
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
          ? "Today"
          : d.toLocaleDateString("en-US", { weekday: "short" });
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
        <span className="ml-auto font-mono">Max: {formatUSD(maxCost)}/day</span>
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

function Providers({ scan }: { scan: ScanResult | null }) {
  const grouped = useMemo(() => {
    if (!scan) return null;
    const map = new Map<
      string,
      { input: number; output: number; cached: number; cost: number; msgs: number; days: Set<string> }
    >();
    for (const e of scan.entries) {
      if (e.model === CLAUDE_MSG_BUCKET) {
        const agg = map.get(e.provider);
        if (agg) agg.msgs += e.message_count;
        continue;
      }
      const cur =
        map.get(e.provider) ??
        { input: 0, output: 0, cached: 0, cost: 0, msgs: 0, days: new Set<string>() };
      cur.input += e.input_tokens;
      cur.output += e.output_tokens;
      cur.cached += e.cached_tokens;
      cur.cost += e.cost_usd ?? 0;
      cur.days.add(e.date);
      map.set(e.provider, cur);
    }
    return Array.from(map.entries());
  }, [scan]);

  if (!grouped) return null;

  return (
    <div className="space-y-3">
      {grouped.map(([provider, v]) => (
        <div
          key={provider}
          className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 flex items-center justify-between"
        >
          <div>
            <div className="font-semibold">{provider}</div>
            <div className="text-xs text-neutral-500">
              {v.days.size} active days · {formatInt(v.msgs)} msgs
            </div>
          </div>
          <div className="text-right">
            <div className="font-mono text-lg">{formatUSD(v.cost)}</div>
            <div className="text-xs text-neutral-500">{formatInt(v.input + v.output)} I/O tokens</div>
          </div>
        </div>
      ))}
      {grouped.length === 0 && (
        <div className="text-sm text-neutral-500">No usage found in last 30 days.</div>
      )}
    </div>
  );
}

function Settings({
  config,
  lastSync,
  onPaired,
  onUnpaired,
  onSynced,
}: {
  config: ConfigView | null;
  lastSync: { at: Date; report: SyncReport } | null;
  onPaired: () => Promise<void>;
  onUnpaired: () => Promise<void>;
  onSynced: (r: SyncReport) => void;
}) {
  const [code, setCode] = useState("");
  const [deviceName, setDeviceName] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
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
        text: `Paired as "${result.device_name}" (${result.device_id.slice(0, 8)}…).`,
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
    if (!confirm("Unpair this device? You can pair again later using a new 6-digit code.")) return;
    setBusy(true);
    setMsg(null);
    try {
      await invoke("unpair_device");
      setMsg({ kind: "ok", text: "Device unpaired." });
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
        text: `Synced ${report.metrics_uploaded} metrics, ${report.sessions_synced} sessions, ${report.alerts_synced} alerts.`,
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
      {paired && <BudgetSection />}

      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
        <h2 className="text-sm font-semibold text-neutral-300 mb-2">Account</h2>
        <dl className="grid grid-cols-[140px_1fr] gap-y-1 text-sm">
          <dt className="text-neutral-500">Status</dt>
          <dd>
            <PairBadge paired={paired} />
          </dd>
          <dt className="text-neutral-500">Device name</dt>
          <dd className="font-mono text-xs">{config?.device_name ?? "—"}</dd>
          <dt className="text-neutral-500">Device ID</dt>
          <dd className="font-mono text-xs truncate">{config?.device_id ?? "—"}</dd>
          <dt className="text-neutral-500">User ID</dt>
          <dd className="font-mono text-xs truncate">{config?.user_id ?? "—"}</dd>
          <dt className="text-neutral-500">Helper version</dt>
          <dd className="font-mono text-xs">{config?.helper_version ?? "—"}</dd>
        </dl>
      </section>

      {!paired && (
        <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
          <h2 className="text-sm font-semibold text-neutral-300 mb-1">Pair with iPhone</h2>
          <p className="text-xs text-neutral-500 mb-3">
            Open CLI Pulse on iOS → Settings → Add device. Enter the 6-digit code shown on your phone.
          </p>
          <form onSubmit={doPair} className="space-y-3">
            <div>
              <label className="block text-xs text-neutral-400 mb-1">Pairing code</label>
              <input
                type="text"
                inputMode="numeric"
                pattern="\d{6}"
                maxLength={6}
                value={code}
                onChange={(e) => setCode(e.target.value.replace(/\D/g, "").slice(0, 6))}
                placeholder="123456"
                className="w-32 px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 text-center font-mono tracking-widest text-lg focus:outline-none focus:border-emerald-500"
                autoFocus
              />
            </div>
            <div>
              <label className="block text-xs text-neutral-400 mb-1">Device name (optional)</label>
              <input
                type="text"
                value={deviceName}
                onChange={(e) => setDeviceName(e.target.value)}
                placeholder="e.g. Jason's Surface"
                className="w-full max-w-sm px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
              />
            </div>
            <button
              type="submit"
              disabled={busy || code.length !== 6}
              className="px-4 py-2 rounded-md bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-medium disabled:opacity-50"
            >
              {busy ? "Pairing…" : "Pair device"}
            </button>
          </form>
        </section>
      )}

      {paired && (
        <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
          <h2 className="text-sm font-semibold text-neutral-300">Sync</h2>
          {lastSync && (
            <div className="text-xs text-neutral-500">
              Last sync {lastSync.at.toLocaleTimeString()} — uploaded{" "}
              {lastSync.report.metrics_uploaded} metrics · {lastSync.report.live_sessions_sent}{" "}
              live sessions · {formatUSD(lastSync.report.total_cost_usd)} over{" "}
              {lastSync.report.files_scanned} files
            </div>
          )}
          <div className="flex gap-2">
            <button
              onClick={doSyncNow}
              disabled={busy}
              className="px-4 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700 disabled:opacity-50"
            >
              {busy ? "Syncing…" : "Sync now"}
            </button>
            <button
              onClick={doUnpair}
              disabled={busy}
              className="px-4 py-2 rounded-md bg-red-950/60 hover:bg-red-900/60 text-sm border border-red-900 text-red-200 disabled:opacity-50"
            >
              Unpair device
            </button>
          </div>
          <p className="text-xs text-neutral-600">
            Auto-sync runs every 2 minutes. Local scan + helper_sync + upsert_daily_usage.
          </p>
        </section>
      )}

      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
        <h2 className="text-sm font-semibold text-neutral-300">Updates</h2>
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
        <p className="text-xs text-neutral-600">
          Updates are signed. Releases publish at{" "}
          <span className="font-mono">github.com/JasonYeYuhe/cli-pulse-desktop/releases</span>.
        </p>
      </section>

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

function BudgetSection() {
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
        throw new Error("Daily budget must be a non-negative number.");
      }
      if (weeklyNum != null && (isNaN(weeklyNum) || weeklyNum < 0)) {
        throw new Error("Weekly budget must be a non-negative number.");
      }
      if (isNaN(cpuNum) || cpuNum < 0 || cpuNum > 100) {
        throw new Error("CPU threshold must be between 0 and 100.");
      }
      const next: AlertThresholds = {
        daily_budget_usd: dailyNum,
        weekly_budget_usd: weeklyNum,
        cpu_spike_pct: cpuNum,
      };
      await invoke("set_thresholds", { thresholds: next });
      setThresholds(next);
      setMsg({ kind: "ok", text: "Budget saved." });
    } catch (e: any) {
      setMsg({ kind: "err", text: String(e?.message ?? e) });
    } finally {
      setBusy(false);
    }
  }

  if (!thresholds) {
    return (
      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
        <h2 className="text-sm font-semibold text-neutral-300 mb-2">Budget</h2>
        <div className="text-sm text-neutral-500">Loading…</div>
      </section>
    );
  }

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <h2 className="text-sm font-semibold text-neutral-300 mb-1">Budget</h2>
      <p className="text-xs text-neutral-500 mb-3">
        Alerts fire once per day (or per week) when your CLI spend goes above these limits.
        Leave blank to disable.
      </p>
      <form onSubmit={save} className="space-y-3 max-w-md">
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="block text-xs text-neutral-400 mb-1">Daily budget (USD)</span>
            <input
              type="number"
              step="0.01"
              min="0"
              value={daily}
              onChange={(e) => setDaily(e.target.value)}
              placeholder="e.g. 25"
              className="w-full px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
            />
          </label>
          <label className="block">
            <span className="block text-xs text-neutral-400 mb-1">Weekly budget (USD)</span>
            <input
              type="number"
              step="0.01"
              min="0"
              value={weekly}
              onChange={(e) => setWeekly(e.target.value)}
              placeholder="e.g. 150"
              className="w-full px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
            />
          </label>
        </div>
        <label className="block">
          <span className="block text-xs text-neutral-400 mb-1">CPU spike threshold (%)</span>
          <input
            type="number"
            min="0"
            max="100"
            step="1"
            value={cpu}
            onChange={(e) => setCpu(e.target.value)}
            className="w-24 px-3 py-2 rounded-md bg-neutral-950 border border-neutral-700 focus:outline-none focus:border-emerald-500"
          />
          <span className="text-xs text-neutral-600 ml-2">
            Alerts when one CLI process exceeds this for one scan.
          </span>
        </label>
        <button
          type="submit"
          disabled={busy}
          className="px-4 py-2 rounded-md bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-medium disabled:opacity-50"
        >
          {busy ? "Saving…" : "Save"}
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
  switch (state.state) {
    case "idle":
      return (
        <button
          onClick={onCheck}
          className="px-4 py-2 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700"
        >
          Check for updates
        </button>
      );
    case "checking":
      return <div className="text-sm text-neutral-400">Checking…</div>;
    case "up-to-date":
      return (
        <div className="text-sm text-emerald-300">You're on the latest version.</div>
      );
    case "available":
      return (
        <div className="text-sm text-neutral-300">
          Found {state.version} — downloading…
        </div>
      );
    case "downloading":
      return (
        <div className="space-y-1">
          <div className="text-xs text-neutral-400">Downloading {state.pct}%</div>
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
          <span className="text-sm text-emerald-300">
            Update installed. Restart to apply.
          </span>
          <button
            onClick={onRelaunch}
            className="px-3 py-1.5 text-xs rounded-md bg-emerald-600 hover:bg-emerald-500 text-white"
          >
            Restart now
          </button>
        </div>
      );
    case "error":
      return (
        <div className="text-sm text-red-300">
          Update failed: {state.text.length > 160 ? state.text.slice(0, 160) + "…" : state.text}
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
  if (!snapshot && loading) {
    return <Skeleton />;
  }
  if (!snapshot) return null;

  const sessions = snapshot.sessions;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <div className="text-xs text-neutral-500">
          {sessions.length} active · {snapshot.total_processes_seen} processes scanned · refreshed{" "}
          {new Date(snapshot.collected_at).toLocaleTimeString()}
        </div>
        <button
          onClick={onRefresh}
          disabled={loading}
          className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50"
        >
          {loading ? "Refreshing…" : "Refresh now"}
        </button>
      </div>

      {sessions.length === 0 ? (
        <div className="text-sm text-neutral-500 italic py-10 text-center">
          No AI CLI sessions running right now.
        </div>
      ) : (
        <div className="overflow-hidden rounded-lg border border-neutral-800">
          <table className="w-full text-sm">
            <thead className="bg-neutral-900/60 text-left text-xs text-neutral-400">
              <tr>
                <th className="px-3 py-2">Provider</th>
                <th className="px-3 py-2">Project</th>
                <th className="px-3 py-2">Name</th>
                <th className="px-3 py-2 text-right">CPU</th>
                <th className="px-3 py-2 text-right">Memory</th>
                <th className="px-3 py-2 text-right">Confidence</th>
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
                  <td className="px-3 py-2 text-right font-mono">{s.cpu_usage.toFixed(1)}%</td>
                  <td className="px-3 py-2 text-right font-mono">{s.memory_mb} MB</td>
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
  const color =
    c === "high" ? "bg-emerald-400" : c === "medium" ? "bg-amber-400" : "bg-neutral-500";
  return (
    <span className="inline-flex items-center gap-1.5 text-xs text-neutral-400">
      <span className={`w-1.5 h-1.5 rounded-full ${color}`} />
      {c}
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
            ? "Nothing pressing — no active alerts."
            : `${alerts.length} active alert${alerts.length === 1 ? "" : "s"}`}
        </div>
        <button
          onClick={onRefresh}
          disabled={loading}
          className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50"
        >
          {loading ? "Refreshing…" : "Refresh"}
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
            {alert.related_provider && <span>provider: {alert.related_provider}</span>}
            {alert.related_project_name && (
              <span>project: {alert.related_project_name}</span>
            )}
            <span>{new Date(alert.created_at).toLocaleString()}</span>
          </div>
        </div>
      </div>
    </div>
  );
}

function EmptyAlertsHint() {
  return (
    <div className="p-6 rounded-lg border border-neutral-800 bg-neutral-900/30 text-sm text-neutral-400">
      <div className="font-semibold text-neutral-300 mb-1">All quiet 🌙</div>
      <p>
        Alerts fire when daily / weekly budgets are exceeded or when a single AI CLI process
        hits a CPU spike. Set your budget in <span className="font-semibold">Settings → Budget</span>.
      </p>
    </div>
  );
}

function EntriesTable({ entries }: { entries: DailyEntry[] }) {
  if (entries.length === 0) {
    return <div className="text-sm text-neutral-500 italic">No usage today.</div>;
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
  return (
    <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
      {[0, 1, 2, 3].map((i) => (
        <div
          key={i}
          className="h-24 rounded-lg border border-neutral-800 bg-neutral-900/40 animate-pulse"
        />
      ))}
    </div>
  );
}
