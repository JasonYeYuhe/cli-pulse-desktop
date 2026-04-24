import { useEffect, useMemo, useState } from "react";
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
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function runScan() {
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
  }

  useEffect(() => {
    runScan();
  }, []);

  return (
    <div className="min-h-screen flex flex-col bg-neutral-950 text-neutral-100">
      <header className="border-b border-neutral-800 px-6 py-3 flex items-center justify-between">
        <div className="flex items-center gap-3">
          <div className="w-7 h-7 rounded bg-gradient-to-br from-emerald-400 to-cyan-500" />
          <div>
            <div className="font-semibold text-sm">CLI Pulse</div>
            <div className="text-xs text-neutral-500">Desktop · Sprint 0</div>
          </div>
        </div>
        <button
          onClick={runScan}
          disabled={loading}
          className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50"
        >
          {loading ? "Scanning…" : "Rescan"}
        </button>
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
        {tab === "sessions" && <Placeholder title="Sessions" subtitle="Live process list ships in Sprint 1." />}
        {tab === "alerts" && <Placeholder title="Alerts" subtitle="CPU/quota/budget alerts ship in Sprint 2." />}
        {tab === "settings" && <Placeholder title="Settings" subtitle="Pair with iPhone, login, autostart — Sprint 1." />}
      </main>
    </div>
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
        <StatCard
          label="Today — Cost"
          value={today ? formatUSD(today.cost) : "—"}
          hint={scan.today_key}
        />
        <StatCard
          label="Today — Tokens"
          value={today ? formatInt(today.tokens) : "—"}
          hint="input + output"
        />
        <StatCard
          label="Today — Messages"
          value={today ? formatInt(today.msgs) : "—"}
          hint="Claude only"
        />
        <StatCard
          label={`Last ${scan.days_scanned}d — Cost`}
          value={formatUSD(scan.total_cost_usd)}
          hint={`${scan.files_scanned} files scanned`}
        />
      </div>

      <section>
        <h2 className="text-sm font-semibold text-neutral-400 mb-2">
          Today's breakdown
        </h2>
        <EntriesTable
          entries={scan.entries.filter(
            (e) => e.date === scan.today_key && e.model !== CLAUDE_MSG_BUCKET
          )}
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
            <div className="text-xs text-neutral-500">
              {formatInt(v.input + v.output)} I/O tokens
            </div>
          </div>
        </div>
      ))}
      {grouped.length === 0 && (
        <div className="text-sm text-neutral-500">No usage found in last 30 days.</div>
      )}
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
