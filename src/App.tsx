import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
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

  useEffect(() => {
    refreshConfig();
    runScan();
    refreshSessions();
  }, [refreshConfig, runScan, refreshSessions]);

  // Sessions tab refreshes every 10s while visible
  useEffect(() => {
    if (tab !== "sessions") return;
    const id = setInterval(refreshSessions, 10_000);
    return () => clearInterval(id);
  }, [tab, refreshSessions]);

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
          <Placeholder title="Alerts" subtitle="CPU/quota/budget alerts ship in Sprint 3." />
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
        <h2 className="text-sm font-semibold text-neutral-400 mb-2">Today's breakdown</h2>
        <EntriesTable
          entries={scan.entries.filter((e) => e.date === scan.today_key && e.model !== CLAUDE_MSG_BUCKET)}
        />
      </section>
    </div>
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

function Placeholder({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="flex flex-col items-center justify-center text-center py-20">
      <div className="text-lg font-semibold">{title}</div>
      <div className="text-sm text-neutral-500 mt-2 max-w-md">{subtitle}</div>
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
