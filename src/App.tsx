import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { check as checkUpdate } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { useTranslation } from "react-i18next";
import { SUPPORTED_LANGS, setLang, type LangCode } from "./i18n";
import {
  formatBytes,
  formatInt,
  formatRelativeMinutes,
  formatRelativeShort,
  formatRelativeShortParts,
  isStaleProviderRow,
  lastNLocalDates,
  rowsToCsv,
  secondsToShortParts,
} from "./lib/format";
import {
  loadHiddenProviders,
  saveHiddenProviders,
  toggleHiddenProvider,
} from "./lib/providerVisibility";
import { providerColor, providerMonogram } from "./lib/providerTheme";
import {
  activityLevel,
  buildActivity,
  cacheHitRate,
  cacheHitRateOf,
  computeStreaks,
} from "./lib/activity";
import { CURRENCIES, formatMoney, loadCurrency, saveCurrency, type FxRates } from "./lib/money";
import {
  RANGE_PRESETS,
  MIN_DAYS,
  MAX_DAYS,
  clampDays,
  isPreset,
  loadRangeDays,
  saveRangeDays,
} from "./lib/dateRange";
import {
  DEFAULT_WARN_THRESHOLDS,
  warningFractions,
  placeOnRemainingBar,
} from "./lib/quotaMarkers";
import {
  windowMinutesForTier,
  computePace,
  parseResetMs,
  type PaceStatus,
} from "./lib/pace";
import appIcon from "./assets/app-icon.png";
import "./App.css";

// Multi-currency display: a context carrying `fmt(usd)` (converts a USD cost to
// the user's chosen currency) plus the current currency + setter (for the
// Settings selector). Defaults to plain USD so any consumer rendered outside the
// provider (or before FX rates load) still shows USD.
type MoneyCtx = {
  fmt: (usd: number) => string;
  currency: string;
  setCurrency: (code: string) => void;
};
const MoneyContext = createContext<MoneyCtx>({
  fmt: (usd) => formatMoney(usd, "USD", null),
  currency: "USD",
  setCurrency: () => {},
});
function useMoney(): (usd: number) => string {
  return useContext(MoneyContext).fmt;
}
function useCurrencySetting(): { currency: string; setCurrency: (code: string) => void } {
  const { currency, setCurrency } = useContext(MoneyContext);
  return { currency, setCurrency };
}

// Scan-window ("date range") setting: the number of days the local usage scan
// looks back. Persisted in localStorage (see lib/dateRange.ts) and provided
// app-wide so the Settings selector can change it and `runScan` reads it. The
// window drives every local-scan surface (Overview tiles, activity heat strip,
// provider breakdown, entries table, export) because they all key off
// `scan.days_scanned`. Defaults to 30 for consumers outside the provider.
type ScanRangeCtx = { days: number; setDays: (days: number) => void };
const ScanRangeContext = createContext<ScanRangeCtx>({
  days: 30,
  setDays: () => {},
});
function useScanRange(): ScanRangeCtx {
  return useContext(ScanRangeContext);
}

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

type OriginUsage = {
  kind: string; // "native" | "wsl"
  distro: string | null;
  tokens: number;
  files: number;
};

type ScanResult = {
  entries: DailyEntry[];
  total_cost_usd: number;
  total_tokens: number;
  today_key: string;
  days_scanned: number;
  files_scanned: number;
  origin_usage?: OriginUsage[];
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

// v0.6.0 — Remote Approvals wire-shape (mirrors Rust supabase.rs +
// macOS Swift Models.swift:867-1005). All Optional<> on Mac → optional
// here so server-side schema additions don't break decode.
type RemotePermissionRequest = {
  id: string;
  session_id: string | null;
  device_id: string;
  device_name: string | null;
  provider: string;
  tool_name: string;
  summary: string;
  /** "low" / "medium" / "high" — unknown values render as neutral pill. */
  risk: string;
  status: string;
  created_at: string;
  expires_at: string;
};

type RemoteSession = {
  id: string;
  device_id: string;
  device_name: string | null;
  provider: string;
  cwd_basename: string;
  cwd_hmac: string | null;
  /** "pending" / "running" / "stopped" / "errored" — unknown renders muted. */
  status: string;
  client_label: string | null;
  created_at: string;
  last_event_at: string | null;
};

// v0.10.1 — Swarm View wire shapes (macOS/iOS parity). Mirror the Rust
// supabase::RemoteSwarm / RemoteSwarmDevice (which mirror the Mac
// Models.swift). `handle` is the opaque `swarm-<6hex>`; no repo/branch
// name ever crosses the wire.
type RemoteSwarm = {
  swarm_key: string;
  handle: string;
  is_linked_worktree: boolean;
  providers: string[];
  agents: number;
  blocked: number;
  oldest_blocked_age_s: number;
  last_seen_s_ago: number;
};

type RemoteSwarmDevice = {
  device_id: string;
  updated_at: string;
  age_s: number;
  stale: boolean;
  swarms: RemoteSwarm[];
};

type TabKey =
  | "overview"
  | "providers"
  | "sessions"
  | "machine"
  | "swarm"
  | "alerts"
  | "settings";

// Canonical tab order — drives Ctrl/Cmd+1..N switching and the smoke-mode
// tab-traversal render pass. Keep in sync with the `tabs` array below.
const ALL_TAB_KEYS: TabKey[] = [
  "overview",
  "providers",
  "sessions",
  "machine",
  "swarm",
  "alerts",
  "settings",
];

const CLAUDE_MSG_BUCKET = "__claude_msg__";

export default function App() {
  const { t, i18n } = useTranslation();
  const tabs: { key: TabKey; label: string }[] = [
    { key: "overview", label: t("tab.overview") },
    { key: "providers", label: t("tab.providers") },
    { key: "sessions", label: t("tab.sessions") },
    { key: "machine", label: t("tab.machine") },
    { key: "swarm", label: t("tab.swarm") },
    { key: "alerts", label: t("tab.alerts") },
    { key: "settings", label: t("tab.settings") },
  ];
  const [tab, setTab] = useState<TabKey>("overview");
  // Multi-currency display (A2). Currency choice persists in localStorage; FX
  // rates are fetched once on mount (Rust caches ~6 h). `fmt` converts a USD
  // cost to the chosen currency; provided app-wide via MoneyContext.
  const [currency, setCurrency] = useState<string>(loadCurrency);
  const [fxRates, setFxRates] = useState<FxRates | null>(null);
  useEffect(() => {
    let cancelled = false;
    invoke<FxRates>("get_fx_rates")
      .then((r) => {
        if (!cancelled) setFxRates(r);
      })
      .catch(() => {
        // Keep the USD fallback — a failed/slow FX fetch never blanks costs.
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const fmt = useCallback(
    (usd: number) => formatMoney(usd, currency, fxRates?.rates ?? null),
    [currency, fxRates],
  );
  function changeCurrency(code: string) {
    setCurrency(code);
    saveCurrency(code);
  }
  // Scan-window ("date range") — how many days back the local scan reaches.
  // Lazy-init from localStorage; write-through on change (clamped to the same
  // 1..180 the backend accepts). `runScan` depends on this, so changing it
  // re-scans and every `scan.days_scanned`-keyed surface updates.
  const [rangeDays, setRangeDays] = useState<number>(loadRangeDays);
  const changeRangeDays = useCallback((days: number) => {
    const d = clampDays(days);
    setRangeDays(d);
    saveRangeDays(d);
  }, []);
  // v0.10.0 — keyboard shortcut help overlay. Triggered by
  // Ctrl/Cmd + Shift + / (the ?-menu binding power users expect).
  const [shortcutHelpOpen, setShortcutHelpOpen] = useState(false);
  const [scan, setScan] = useState<ScanResult | null>(null);
  const [config, setConfig] = useState<ConfigView | null>(null);
  const [sessions, setSessions] = useState<SessionsSnapshot | null>(null);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [alerts, setAlerts] = useState<Alert[] | null>(null);
  const [alertsLoading, setAlertsLoading] = useState(false);
  // v0.5.3 — App-level updater state (was UI-component-local in
  // Settings panel). The header banner consumes this directly so a
  // click triggers install rather than tab-switching. UpdatesSection
  // is now a pure presentation component (Codex P1+P2: single
  // source of truth, no double state machine, no double-click race).
  const [updater, dispatchUpdater] = useReducer(updaterReducer, { state: "idle" });
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastSync, setLastSync] = useState<{ at: Date; report: SyncReport } | null>(null);

  // v0.6.0 — Remote Approvals state. App-level so the header badge
  // and the sheet share a single source of truth (Codex-pattern
  // matching what we did for the v0.5.3 updater state). Toggle is
  // optimistically updated on user click but MUST revert on PATCH
  // failure (Gemini 3.1 Pro v0.6.0 review P0 — privacy-critical
  // feature must not lie about its server-side state).
  const [remoteControlEnabled, setRemoteControlEnabled] = useState<boolean | null>(null);
  const [remoteControlSaving, setRemoteControlSaving] = useState(false);
  const [pendingApprovals, setPendingApprovals] = useState<RemotePermissionRequest[]>([]);
  const [remoteSessions, setRemoteSessions] = useState<RemoteSession[]>([]);
  const [remoteRefreshedAt, setRemoteRefreshedAt] = useState<Date | null>(null);
  const [showApprovalsSheet, setShowApprovalsSheet] = useState(false);
  // Adaptive polling counter as a REF, not state — avoids triggering
  // effect re-runs when the count updates. Without this, the polling
  // effect's deps include the count → setRemoteQuietPollCount inside
  // refreshRemoteState would invalidate the effect on every fetch
  // and cause a re-fire-immediately loop (Gemini 3.1 Pro v0.6.0
  // post-implementation review P0 — DDoS-shaped infinite fetch loop
  // at fetch latency, ~1 RPS per client).
  const remoteQuietPollCountRef = useRef(0);

  const refreshRemoteState = useCallback(async () => {
    try {
      // First fetch the gate setting; bail if disabled.
      const enabled = await invoke<boolean>("get_remote_control_setting");
      setRemoteControlEnabled(enabled);
      setRemoteRefreshedAt(new Date());
      if (!enabled) {
        // Toggle off → empty arrays so badge / sheet / sessions
        // section all hide cleanly. Do not surface "no approvals"
        // empty state when the user explicitly disabled.
        setPendingApprovals([]);
        setRemoteSessions([]);
        remoteQuietPollCountRef.current = 0;
        return;
      }
      const [pending, sessions] = await Promise.all([
        invoke<RemotePermissionRequest[]>("get_remote_pending_approvals"),
        invoke<RemoteSession[]>("list_remote_sessions"),
      ]);
      setPendingApprovals(pending);
      setRemoteSessions(sessions);
      remoteQuietPollCountRef.current =
        pending.length === 0 ? remoteQuietPollCountRef.current + 1 : 0;
    } catch (e) {
      // Non-fatal: leave previous state in place. Don't surface as a
      // hard error — the user might just be offline transiently.
      console.warn("refreshRemoteState failed (non-fatal):", e);
    }
  }, []);

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
      const result = await invoke<ScanResult>("scan_usage", { days: rangeDays });
      setScan(result);
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [rangeDays]);

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

  // v0.11.0 — headless launch-smoke marker. On mount (i.e. the React
  // tree actually rendered inside WebView2/WebKitGTK) tell the backend
  // to drop a `frontend-ready` marker file. In production this is a
  // no-op: the Rust command only writes when CLI_PULSE_SMOKE_MARKER is
  // set in the environment (the CI launch-smoke job sets it). This is
  // the signal that catches the v0.2.11 white-screen class — if React
  // never mounts (bundle fails to load, JS throws before commit,
  // WebView2 fails to render), this effect never runs, the marker is
  // never written, and the CI smoke job fails. One extra IPC call on
  // mount; negligible cost when the env var is absent.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      let active = false;
      try {
        active = await invoke<boolean>("smoke_is_active");
      } catch {
        // treat as not-smoke (production path)
      }
      if (active && !cancelled) {
        // Smoke mode: render EVERY tab once before signalling ready. If any
        // tab throws during render, the ErrorBoundary unmounts <App/> and
        // the marker below is never written → the CI launch-smoke fails.
        // This extends the mount-only gate to catch the v0.2.11 tab-crash
        // class for every tab, not just the default one.
        for (const k of ALL_TAB_KEYS) {
          if (cancelled) return;
          setTab(k);
          // Let React commit + run the tab's initial effects before moving on.
          await new Promise((r) => setTimeout(r, 150));
        }
        if (cancelled) return;
        setTab("overview");
      }
      // Marker write is a no-op in production (env unset); in smoke mode it
      // is the "frontend mounted (+ all tabs rendered)" signal CI polls for.
      invoke("smoke_mark_frontend_ready").catch(() => {});
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    refreshConfig();
    refreshSessions();
    refreshAlerts();

    // Silent update check on first mount. We DON'T auto-download —
    // the user still chooses when to install (banner click in v0.5.3
    // or Settings → Updates). Failure is non-fatal (offline, GitHub
    // down, etc.); we don't even surface it as an error state — that
    // would render an angry banner on transient blips. The dispatch
    // path stays in idle.
    (async () => {
      try {
        const upd = await checkUpdate();
        if (upd) {
          dispatchUpdater({
            type: "available",
            version: upd.version,
            body: upd.body,
          });
        }
      } catch (e) {
        console.warn("update check failed:", e);
      }
    })();
  }, [refreshConfig, refreshSessions, refreshAlerts]);

  // Local usage scan — its own effect so it fires on mount AND whenever the
  // date-range setting changes (runScan depends on rangeDays). Kept separate
  // from the mount effect above so a date-range flip re-scans WITHOUT also
  // re-running the config/sessions/alerts refetch or the one-shot update check.
  useEffect(() => {
    runScan();
  }, [runScan]);

  // v0.5.6 — push localized tray copy on initial mount AND every
  // language change. Isolated into its own useEffect (NOT bundled
  // with the main mount effect above) so changing language doesn't
  // re-trigger expensive scans / sessions polls / update checks.
  // Per Gemini 3.1 Pro v0.5.6 P1: adding `t` to the main effect's
  // deps would cause `runScan()` to fire on every language flip.
  //
  // Reads `i18n.t` (the live translator) rather than the closure-
  // captured `t` from useTranslation, because the closure-captured
  // value is bound to the language active at the previous render.
  // After `setLang`, the new render's `t` resolves to the new
  // language — but we want the push to happen as soon as i18next
  // has switched resources, even before the React tree re-renders.
  // Using `i18n.t` directly resolves against the current i18next
  // language at call time, regardless of which render owns the
  // useEffect.
  useEffect(() => {
    pushTrayCopyFromI18n((key) => i18n.t(key));
  }, [i18n.language, i18n]);

  // v0.10.0 — global keyboard shortcuts. Power users have asked for
  // this; the v0.6.1 Esc-modal fix already showed the foundation
  // works in Tauri 2 / WebView2.
  //
  //   Ctrl/Cmd + R       rescan local logs
  //   Ctrl/Cmd + ,       go to Settings tab
  //   Ctrl/Cmd + 1..5    switch between tabs (Overview / Providers /
  //                      Sessions / Alerts / Settings)
  //   Ctrl/Cmd + Shift + /  toggle shortcut help overlay (the
  //                      ?-menu power users expect)
  //
  // Esc-to-close-modal stays per-modal (existing in v0.6.1) so each
  // modal can decide whether the Esc should also clear other state.
  //
  // We DON'T bind anything that would shadow text-input shortcuts:
  // - Ctrl/Cmd + A / X / C / V are left to the browser
  // - Ctrl/Cmd + S is NOT bound (would conflict with "save" muscle
  //   memory in any future text-edit features)
  // - Esc is left to per-modal handlers
  //
  // Active-element check skips shortcuts that are typing-keys when
  // an input/textarea/select has focus, so users don't lose their
  // typing if they happen to type "1" while in a textarea.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const meta = e.ctrlKey || e.metaKey;
      // Only trap shortcuts with the modifier — no bare-key bindings.
      if (!meta) return;
      // Ctrl/Cmd + Shift + / — open help overlay. Note: many keyboard
      // layouts emit "?" for Shift+/, others emit "/"; check both.
      if (e.shiftKey && (e.key === "/" || e.key === "?")) {
        e.preventDefault();
        setShortcutHelpOpen((v) => !v);
        return;
      }
      // Pure-modifier shortcuts. Block while focused in an input
      // element (text inputs / textareas / selects) so we don't
      // hijack the user's edit. Exception: Cmd+R/Ctrl+R still works
      // because rescan is a global app action.
      const target = e.target as HTMLElement | null;
      const inEditableField =
        target?.tagName === "INPUT" ||
        target?.tagName === "TEXTAREA" ||
        target?.tagName === "SELECT" ||
        target?.isContentEditable;

      // Ctrl/Cmd + R — rescan. Always allowed (overrides browser refresh).
      if (e.key === "r" || e.key === "R") {
        e.preventDefault();
        runScan();
        return;
      }
      // Ctrl/Cmd + , — Settings tab.
      if (e.key === ",") {
        e.preventDefault();
        setTab("settings");
        return;
      }
      // Ctrl/Cmd + 1..5 — tab switch. Skip when typing.
      if (!inEditableField && /^[1-7]$/.test(e.key)) {
        e.preventDefault();
        const idx = parseInt(e.key, 10) - 1;
        if (ALL_TAB_KEYS[idx]) setTab(ALL_TAB_KEYS[idx]);
        return;
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [runScan]);

  // v0.5.3 — runs the full check+download+install flow. Idempotent:
  // calling twice while a download is in flight is a no-op (reducer
  // guards `download_progress` against stale dispatches and
  // `check_started` against re-entry mid-download). Used by both the
  // header banner click and the Settings → Updates "Check now" button.
  const doCheckUpdate = useCallback(async () => {
    dispatchUpdater({ type: "check_started" });
    try {
      const upd = await checkUpdate();
      if (!upd) {
        dispatchUpdater({ type: "up_to_date" });
        return;
      }
      dispatchUpdater({
        type: "available",
        version: upd.version,
        body: upd.body,
      });
      let total = 0;
      let downloaded = 0;
      // Throttle progress dispatches to 5 % buckets (Gemini P1: a
      // 7 MB NSIS at typical chunk size = ~1 700 chunks; dispatching
      // setState on every one re-renders the App tree per chunk and
      // locks the UI during the entire download).
      let lastDispatchedPct = -5;
      await upd.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? 0;
          dispatchUpdater({ type: "download_started" });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          const pct = total > 0 ? Math.round((downloaded / total) * 100) : 0;
          if (pct - lastDispatchedPct >= 5 || pct === 100) {
            lastDispatchedPct = pct;
            dispatchUpdater({ type: "download_progress", pct });
          }
        } else if (event.event === "Finished") {
          dispatchUpdater({ type: "download_finished" });
        }
      });
    } catch (e: any) {
      dispatchUpdater({ type: "error", text: String(e) });
    }
  }, []);

  const doRelaunchAfterUpdate = useCallback(async () => {
    try {
      await relaunch();
    } catch (e: any) {
      dispatchUpdater({ type: "error", text: String(e) });
    }
  }, []);

  // Sessions tab refreshes every 10s while visible
  useEffect(() => {
    if (tab !== "sessions") return;
    const id = setInterval(refreshSessions, 10_000);
    return () => clearInterval(id);
  }, [tab, refreshSessions]);

  // v0.6.0 — Remote Approvals adaptive polling. Cadence:
  //   • 30s while there's anything pending (responsive Approve/Deny)
  //   • 60s after 3 consecutive quiet polls (no pending seen)
  //   • 120s after 6 consecutive quiet polls
  // Per Gemini 3.1 Pro v0.6.0 review P1: fixed 30s across every
  // desktop generates unnecessary DB load on otherwise-idle accounts.
  // Adaptive backoff caps the steady-state cost while keeping the
  // response time tight when there IS something to act on.
  //
  // Implementation: self-rescheduling setTimeout chain instead of
  // setInterval, because (a) the interval ms changes per tick based
  // on the quiet-poll count, and (b) using setInterval with the count
  // in the effect deps caused a DDoS-shaped re-fire loop (Gemini
  // post-impl review P0). Each tick computes the next ms from the
  // ref-tracked count AFTER refreshRemoteState resolves, so the
  // schedule reflects the just-observed pending state. `cancelled`
  // guards both pre- and post-await against unmount races.
  useEffect(() => {
    if (!config?.paired) return;
    let cancelled = false;
    let timer: number | undefined;
    const tick = async () => {
      if (cancelled) return;
      await refreshRemoteState();
      if (cancelled) return;
      const count = remoteQuietPollCountRef.current;
      const nextMs = count >= 6 ? 120_000 : count >= 3 ? 60_000 : 30_000;
      timer = window.setTimeout(tick, nextMs);
    };
    tick();
    return () => {
      cancelled = true;
      if (timer !== undefined) clearTimeout(timer);
    };
  }, [config?.paired, refreshRemoteState]);

  // Alerts tab refreshes every 30s while visible
  useEffect(() => {
    if (tab !== "alerts") return;
    const id = setInterval(refreshAlerts, 30_000);
    return () => clearInterval(id);
  }, [tab, refreshAlerts]);

  return (
    <MoneyContext.Provider value={{ fmt, currency, setCurrency: changeCurrency }}>
    <ScanRangeContext.Provider value={{ days: rangeDays, setDays: changeRangeDays }}>
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
          {(() => {
            // v0.5.3 — banner derived from App-level updater state.
            // Click handler routes by state: available → start
            // download; downloading → no-op (visible spinner is
            // enough); ready → relaunch; error → navigate to Settings
            // for detail. Granular pct stays in Settings → Updates;
            // the header banner shows a static "下载中…" to avoid
            // re-render flicker (Gemini P1).
            const u = updater;
            if (
              u.state !== "available" &&
              u.state !== "downloading" &&
              u.state !== "ready" &&
              u.state !== "error"
            ) {
              return null;
            }
            const onClick = () => {
              if (u.state === "ready") {
                doRelaunchAfterUpdate();
              } else if (u.state === "available") {
                doCheckUpdate();
              } else if (u.state === "error") {
                setTab("settings");
              }
              // downloading → no-op (defensive; the button is also
              // visually muted via the className branch below)
            };
            const text =
              u.state === "available"
                ? `⬆ ${t("updater.banner_available", { version: u.version })} · ${t("updater.banner_action")}`
                : u.state === "downloading"
                  ? `⬇ ${t("updater.banner_downloading")}`
                  : u.state === "ready"
                    ? `✓ ${t("updater.banner_ready")}`
                    : `⚠ ${t("updater.banner_error")}`;
            const className =
              u.state === "downloading"
                ? "px-2.5 py-1 text-xs rounded-md bg-emerald-950/40 border border-emerald-800/50 text-emerald-300/70 cursor-default"
                : u.state === "error"
                  ? "px-2.5 py-1 text-xs rounded-md bg-red-950/60 border border-red-700 text-red-200 hover:bg-red-900/60"
                  : "px-2.5 py-1 text-xs rounded-md bg-emerald-950/60 border border-emerald-700 text-emerald-200 hover:bg-emerald-900/60";
            return (
              <button
                onClick={onClick}
                className={className}
                disabled={u.state === "downloading"}
                title={text}
              >
                {text}
              </button>
            );
          })()}
          {/* v0.6.0 — Remote Approvals pending badge. Renders only
              when Remote Control is on (toggle in Settings → Privacy)
              AND there's at least one pending request from any of
              the user's paired devices. Click opens the sheet. */}
          {remoteControlEnabled && pendingApprovals.length > 0 && (
            <button
              onClick={() => setShowApprovalsSheet(true)}
              className="px-2.5 py-1 text-xs rounded-md bg-amber-950/60 border border-amber-700 text-amber-200 hover:bg-amber-900/60"
              title={t("remote.badge_tooltip")}
            >
              🔔 {t("remote.badge_pending_count", { count: pendingApprovals.length })}
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

      {/* v0.6.0 — Remote Approvals sheet (modal overlay, mounts only
          when toggled open). State + decide handlers live here at App
          level so the badge click + sheet decisions share the same
          `pendingApprovals` array — Codex pattern carrying over from
          the v0.5.3 updater state lift. */}
      {showApprovalsSheet && (
        <RemoteApprovalsSheet
          enabled={remoteControlEnabled === true}
          pending={pendingApprovals}
          onClose={() => setShowApprovalsSheet(false)}
          onDecided={async () => {
            // Optimistic-removal happens inside the sheet; this
            // refetches from the server to reconcile after the
            // (potentially racy) decide RPC.
            await refreshRemoteState();
          }}
        />
      )}

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
            remoteSessions={remoteSessions}
            remoteControlEnabled={remoteControlEnabled === true}
            onRemoteSessionAction={refreshRemoteState}
          />
        )}
        {tab === "machine" && (
          <MachineTab
            paired={!!config?.paired}
            currentDeviceId={config?.device_id ?? null}
            scan={scan}
          />
        )}
        {tab === "swarm" && (
          <Swarm
            paired={!!config?.paired}
            remoteControlEnabled={remoteControlEnabled === true}
          />
        )}
        {tab === "alerts" && (
          <Alerts
            alerts={alerts}
            loading={alertsLoading}
            onRefresh={refreshAlerts}
            paired={!!config?.paired}
          />
        )}
        {tab === "settings" && (
          <Settings
            config={config}
            scan={scan}
            lastSync={lastSync}
            updater={updater}
            remoteControlEnabled={remoteControlEnabled}
            remoteControlSaving={remoteControlSaving}
            remoteRefreshedAt={remoteRefreshedAt}
            onSetRemoteControlEnabled={async (enabled) => {
              // Optimistic flip + revert-on-failure (Gemini v0.6.0
              // P0). Privacy posture must never lie about its state
              // server-side, so we MUST revert if the PATCH errors.
              const previous = remoteControlEnabled;
              setRemoteControlSaving(true);
              setRemoteControlEnabled(enabled);
              try {
                await invoke("set_remote_control_setting", { enabled });
                // Refresh from server after PATCH commits — picks up
                // any cross-device toggle and verifies our write
                // landed.
                await refreshRemoteState();
              } catch (e: any) {
                // Revert + surface error. The Settings section's
                // internal toast handles the user-visible part.
                setRemoteControlEnabled(previous);
                throw e;
              } finally {
                setRemoteControlSaving(false);
              }
            }}
            onCheckUpdate={doCheckUpdate}
            onRelaunchAfterUpdate={doRelaunchAfterUpdate}
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
      {shortcutHelpOpen && (
        <ShortcutHelpOverlay onClose={() => setShortcutHelpOpen(false)} />
      )}
    </div>
    </ScanRangeContext.Provider>
    </MoneyContext.Provider>
  );
}

/// v0.10.0 — keyboard shortcut help overlay. Listed in one
/// alphabetical order; modifier shown first ("Ctrl/Cmd"). The list
/// MUST stay in sync with the keydown handler in `App` — if you
/// add a binding there, add a row here. The i18n.test.ts
/// critical-labels gate covers the labels but not the bindings,
/// so the eyeball check on this list is the gate.
function ShortcutHelpOverlay({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  // Cmd on macOS, Ctrl elsewhere. Tauri 2 doesn't (yet) expose the
  // user's platform via JS — sniff via navigator.platform as the
  // pre-Tauri-2 conventional approach.
  const isMac =
    typeof navigator !== "undefined" &&
    /(Mac|iPhone|iPad|iPod)/i.test(navigator.platform);
  const mod = isMac ? "⌘" : "Ctrl";
  const shortcuts: Array<{ keys: string; label: string }> = [
    { keys: `${mod}+R`, label: t("shortcuts.rescan") },
    { keys: `${mod}+,`, label: t("shortcuts.settings") },
    { keys: `${mod}+1`, label: t("shortcuts.tab_overview") },
    { keys: `${mod}+2`, label: t("shortcuts.tab_providers") },
    { keys: `${mod}+3`, label: t("shortcuts.tab_sessions") },
    { keys: `${mod}+4`, label: t("shortcuts.tab_machine") },
    { keys: `${mod}+5`, label: t("shortcuts.tab_swarm") },
    { keys: `${mod}+6`, label: t("shortcuts.tab_alerts") },
    { keys: `${mod}+7`, label: t("shortcuts.tab_settings") },
    { keys: `${mod}+Shift+/`, label: t("shortcuts.toggle_help") },
    { keys: "Esc", label: t("shortcuts.close_modal") },
  ];
  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/50 p-4"
      onClick={onClose}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcut-help-title"
        className="w-full max-w-md rounded-lg border border-neutral-800 bg-neutral-950 p-4 space-y-3 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-baseline justify-between">
          <h2
            id="shortcut-help-title"
            className="text-sm font-semibold text-neutral-200"
          >
            {t("shortcuts.title")}
          </h2>
          <button
            type="button"
            onClick={onClose}
            className="px-2 py-0.5 text-[10px] rounded border border-neutral-700 hover:bg-neutral-800 text-neutral-300"
            aria-label={t("action.close")}
          >
            ✕
          </button>
        </div>
        <ul className="space-y-1.5 text-xs">
          {shortcuts.map((s) => (
            <li
              key={s.keys}
              className="flex items-baseline justify-between gap-3"
            >
              <span className="text-neutral-400">{s.label}</span>
              <kbd className="px-1.5 py-0.5 rounded font-mono text-[10px] bg-neutral-900 border border-neutral-700 text-neutral-300">
                {s.keys}
              </kbd>
            </li>
          ))}
        </ul>
      </div>
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

type CostForecast = {
  predicted_month_total: number;
  lower_bound: number;
  upper_bound: number;
  actual_to_date: number;
  data_point_count: number;
  current_day_of_month: number;
  days_in_month: number;
  is_reliable: boolean;
};

type TopProject = {
  project: string;
  cost_usd: number;
  session_count: number;
  last_active: string;
};

// v0.5.3 — server-stored alerts shape, mirrors `supabase::ServerAlert`.
// `severity` matches the local `Alert["severity"]` enum at runtime
// (server stores "Info"/"Warning"/"Critical" as text in the alerts
// table). Typed as `string` here to keep the wire shape honest;
// callers narrow via `severityRank` before rendering.
type ServerAlert = {
  id: string;
  type: string;
  severity: string;
  title: string;
  message: string | null;
  created_at: string;
  related_project_id: string | null;
  related_project_name: string | null;
  related_session_id: string | null;
  related_session_name: string | null;
  related_provider: string | null;
  related_device_name: string | null;
  is_read: boolean | null;
  is_resolved: boolean | null;
  // v0.10.1 — alert lifecycle (macOS parity).
  acknowledged_at?: string | null;
  snoozed_until?: string | null;
};

const UNKNOWN_PROJECT = "<unknown>";

// v0.5.3 — single source of truth for updater state, lifted from
// the Settings panel to App-level so the header banner click can
// trigger install directly (v0.4.x and v0.5.x VM verifies both
// reported "banner click doesn't dispatch install" — root cause
// was the banner only running setTab("settings"), not actually
// reaching `downloadAndInstall`).
//
// Reducer (vs raw setState) per Codex 3.1 v0.5.3 review: it
// consolidates legal transitions and lets the
// `download_progress` arm guard against stale dispatches that
// could land after `download_finished` in a race. The
// `download_progress` callback is throttled at the dispatch
// site to ~one update per 5 % (Gemini P1: setting `pct` on
// every chunk re-renders the entire App tree, locking the UI
// during a 7 MB download).
type UpdaterState =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "up-to-date" }
  | { state: "available"; version: string; body?: string }
  | { state: "downloading"; pct: number }
  | { state: "ready" }
  | { state: "error"; text: string };

type UpdaterAction =
  | { type: "check_started" }
  | { type: "up_to_date" }
  | { type: "available"; version: string; body?: string }
  | { type: "download_started" }
  | { type: "download_progress"; pct: number }
  | { type: "download_finished" }
  | { type: "error"; text: string }
  | { type: "reset" };

function updaterReducer(state: UpdaterState, action: UpdaterAction): UpdaterState {
  switch (action.type) {
    case "check_started":
      // Don't reset state if a download is already in flight or
      // ready — protects against double-click races where the user
      // clicks the banner twice before React updates the click
      // handler. Per Codex P1.
      if (state.state === "downloading" || state.state === "ready") return state;
      return { state: "checking" };
    case "up_to_date":
      return { state: "up-to-date" };
    case "available":
      return { state: "available", version: action.version, body: action.body };
    case "download_started":
      return { state: "downloading", pct: 0 };
    case "download_progress":
      // Stale-dispatch guard: a `Progress` callback can fire after
      // `Finished` if the channel is buffered. If we've already
      // moved past `downloading`, drop the late progress.
      if (state.state !== "downloading") return state;
      // Skip identical-pct dispatches (caller already throttles
      // to 5 % buckets but be defensive against duplicates).
      if (state.pct === action.pct) return state;
      return { state: "downloading", pct: action.pct };
    case "download_finished":
      return { state: "ready" };
    case "error":
      return { state: "error", text: action.text };
    case "reset":
      return { state: "idle" };
    default:
      return state;
  }
}

// Activity strip + usage streaks for the Overview (token-monitor learning).
// Pure logic lives in `lib/activity.ts`; this just renders a compact per-day
// heat strip over the local scan window plus current/longest streaks.
function ActivitySection({ scan }: { scan: ScanResult }) {
  const { t } = useTranslation();
  const { activity, streaks, max, cache } = useMemo(() => {
    const activity = buildActivity(scan.entries, scan.days_scanned);
    const streaks = computeStreaks(activity);
    const max = activity.reduce((m, d) => Math.max(m, d.tokens), 0);
    const cache = cacheHitRate(scan.entries);
    return { activity, streaks, max, cache };
  }, [scan]);

  // Nothing worth showing if the whole window is idle.
  if (!activity.some((d) => d.active)) return null;

  // Heat levels 0–4 → neutral (empty) then an emerald ramp.
  const levelClass = [
    "bg-neutral-800",
    "bg-emerald-900",
    "bg-emerald-700",
    "bg-emerald-500",
    "bg-emerald-400",
  ] as const;

  return (
    <section>
      <div className="flex items-center justify-between gap-3 mb-2">
        <h2 className="text-sm font-semibold text-neutral-400">
          {t("overview.activity_title", { days: scan.days_scanned })}
        </h2>
        <div className="flex items-center gap-3 text-xs tabular-nums shrink-0">
          {cache !== null && (
            <span className="text-neutral-400" title={t("overview.cache_hit_hint")}>
              {t("overview.cache_hit", { pct: Math.round(cache) })}
            </span>
          )}
          <span className="text-emerald-400">
            {t("overview.streak_current", { n: streaks.current })}
          </span>
          <span className="text-neutral-400">
            {t("overview.streak_longest", { n: streaks.longest })}
          </span>
        </div>
      </div>
      <div className="flex flex-wrap gap-1">
        {activity.map((d) => {
          const level = activityLevel(d.tokens, max, d.active);
          const label = d.active
            ? t("overview.activity_tokens", { tokens: formatInt(d.tokens) })
            : t("overview.activity_idle");
          return (
            <span
              key={d.date}
              className={`w-3 h-3 rounded-sm ${levelClass[level]}`}
              title={`${d.date} · ${label}`}
            />
          );
        })}
      </div>
    </section>
  );
}

function Overview({
  scan,
  loading,
  paired,
}: {
  scan: ScanResult | null;
  loading: boolean;
  paired: boolean;
}) {
  // v0.5.3 — `alerts` prop removed. RiskSignalsCard now fetches its
  // own data via `get_server_alerts`. The Alerts tab still uses
  // App-level `alerts` state for the full list (different lifecycle,
  // 30 s polling on tab focus), but Overview no longer needs it.
  const { t } = useTranslation();
  const fmt = useMoney();
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

  // v0.10.1 — provider usage breakdown for the Overview "Provider usage"
  // section (macOS parity — OverviewTab.swift costSection per-provider
  // breakdown). Computed from the local N-day scan (works offline),
  // excluding the Claude message-count bucket. Bars rank by I/O tokens
  // (non-zero even for flat-rate subscription usage where cost is $0);
  // cost shown as a secondary figure. Top 6 by tokens.
  const byProvider = useMemo(() => {
    if (!scan) return [];
    // Track input + cached separately (alongside the input+output `tokens`
    // headline) so each row can show a prompt-cache hit rate. `tokens` stays
    // input+output for the bar/label; `cachePct` uses cached / (input+cached).
    const m = new Map<
      string,
      { tokens: number; cost: number; input: number; cached: number }
    >();
    for (const e of scan.entries) {
      if (e.model === CLAUDE_MSG_BUCKET) continue;
      const cur = m.get(e.provider) ?? { tokens: 0, cost: 0, input: 0, cached: 0 };
      cur.tokens += e.input_tokens + e.output_tokens;
      cur.cost += e.cost_usd ?? 0;
      cur.input += e.input_tokens;
      cur.cached += e.cached_tokens;
      m.set(e.provider, cur);
    }
    return Array.from(m.entries())
      .map(([provider, v]) => ({
        provider,
        tokens: v.tokens,
        cost: v.cost,
        cachePct: cacheHitRateOf(v.input, v.cached),
      }))
      .filter((p) => p.tokens > 0)
      .sort((a, b) => b.tokens - a.tokens)
      .slice(0, 6);
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
              value={fmt(serverDash.today_cost)}
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
          <StatCard label={t("overview.today_cost")} value={today ? fmt(today.cost) : "—"} hint={scan.today_key} />
          <StatCard label={t("overview.today_tokens")} value={today ? formatInt(today.tokens) : "—"} hint={t("overview.tokens_hint")} />
          <StatCard label={t("overview.today_messages")} value={today ? formatInt(today.msgs) : "—"} hint={t("overview.claude_only_hint")} />
          <StatCard
            label={t("overview.last_n_days_cost", { days: scan.days_scanned })}
            value={fmt(scan.total_cost_usd)}
            hint={t("overview.files_scanned_hint", { n: scan.files_scanned })}
          />
        </div>
      </section>

      {/* Activity strip + usage streaks (learned from token-monitor's
          home-screen heatmap + streaks). Local-scan only; hidden when the
          window has no active days. */}
      <ActivitySection scan={scan} />

      {/* v0.10.1 — Provider usage breakdown (macOS parity, OverviewTab
          costSection). Brand-colored bars ranked by I/O tokens from the
          local scan; works offline. Hidden when no provider has usage. */}
      {byProvider.length > 0 && (
        <section>
          <h2 className="text-sm font-semibold text-neutral-400 mb-2">
            {t("overview.provider_usage_title", { days: scan.days_scanned })}
          </h2>
          <div className="space-y-2">
            {(() => {
              const maxTokens = Math.max(...byProvider.map((p) => p.tokens), 1);
              return byProvider.map((p) => {
                const color = providerColor(p.provider);
                const pct = (p.tokens / maxTokens) * 100;
                return (
                  <div key={p.provider} className="flex items-center gap-3">
                    <span
                      className="w-2 h-2 rounded-full shrink-0"
                      style={{ backgroundColor: color }}
                    />
                    <span
                      className="text-sm w-20 shrink-0 truncate"
                      title={p.provider}
                    >
                      {p.provider}
                    </span>
                    <div className="flex-1 h-2 bg-neutral-800 rounded overflow-hidden">
                      <div
                        className="h-full rounded"
                        style={{ width: `${pct}%`, backgroundColor: color }}
                      />
                    </div>
                    <span className="text-xs text-neutral-400 tabular-nums w-32 text-right shrink-0">
                      {t("providers.io_tokens", { value: formatInt(p.tokens) })}
                    </span>
                    {/* Prompt-cache hit rate for this provider over the window.
                        A fixed-width slot (kept even when null) so the token/
                        cost columns stay aligned across rows. */}
                    <span
                      className="text-xs text-neutral-500 tabular-nums w-16 text-right shrink-0"
                      title={t("overview.cache_hit_hint")}
                    >
                      {p.cachePct !== null
                        ? t("overview.cache_hit", { pct: Math.round(p.cachePct) })
                        : ""}
                    </span>
                    <span className="text-xs font-mono text-neutral-300 w-16 text-right shrink-0">
                      {fmt(p.cost)}
                    </span>
                  </div>
                );
              });
            })()}
          </div>
        </section>
      )}

      {/* v0.5.1 — Insights row: cost forecast + risk signals.
          Mac sibling parity (CostForecastEngine + RiskSignalsList in
          CLI Pulse Bar's OverviewTab.swift). 2-column at md:+ to
          balance with the trend chart and not steal vertical real
          estate from the existing tiles. Cards self-render their
          own loading / error / empty states; a transient backend
          failure on one card doesn't take down the rest of Overview
          (Gemini 3.1 Pro v0.5.0 review: per-card error states are
          a hard requirement). Hidden when not paired — both cards
          need server data. */}
      {paired && (
        <section className="grid md:grid-cols-2 lg:grid-cols-3 gap-4">
          <CostForecastCard paired={paired} />
          <RiskSignalsCard paired={paired} />
          <TopProjectsCard paired={paired} />
        </section>
      )}

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

/// v0.5.1 — month-end cost forecast card. Wraps the v0.5.0
/// `get_cost_forecast` Tauri command. Shows predicted total + 1-stddev
/// bound range + reliability hint (or amber "need more data" when the
/// `is_reliable` flag is false). Per-card error fallback is a
/// minimal red-bordered hint — keeps the rest of Overview rendering
/// even if Supabase is unreachable.
function CostForecastCard({ paired }: { paired: boolean }) {
  const { t } = useTranslation();
  const fmt = useMoney();
  const [forecast, setForecast] = useState<CostForecast | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  // v0.5.2 — poll every 60 s while mounted (Gemini 3.1 Pro v0.5.2
  // review P1: without polling, a manual sync_now leaves Forecast
  // stale until the user navigates away and back, eroding trust
  // in the dashboard). 60 s matches the relevant change cadence:
  // forecast inputs are aggregated daily, so faster polling is
  // wasted server load.
  useEffect(() => {
    if (!paired) return;
    let cancelled = false;
    const fetchOnce = async () => {
      try {
        const f = await invoke<CostForecast | null>("get_cost_forecast");
        if (cancelled) return;
        setForecast(f);
        setError(null);
      } catch (e: any) {
        if (cancelled) return;
        setError(String(e));
      } finally {
        if (!cancelled) setLoaded(true);
      }
    };
    fetchOnce();
    const id = setInterval(fetchOnce, 60_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [paired]);

  return (
    <div className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <h2 className="text-sm font-semibold text-neutral-400 mb-2">
        {t("overview.forecast_title")}
      </h2>
      {!loaded && (
        <div className="text-sm text-neutral-500">{t("misc.loading")}</div>
      )}
      {loaded && error && (
        <div className="text-xs text-red-400/80">
          {t("overview.forecast_failed")}
        </div>
      )}
      {loaded && !error && !forecast && (
        // Backend returned None (not paired or no daily-usage at all).
        // Card is gated on `paired` already, so this only fires on the
        // brand-new account / fresh-install case.
        <div className="text-xs text-neutral-500">
          {t("overview.forecast_no_data")}
        </div>
      )}
      {loaded && !error && forecast && (
        <>
          <div className="text-2xl font-semibold tabular-nums">
            {fmt(forecast.predicted_month_total)}
          </div>
          <div className="text-xs text-neutral-500 mt-0.5 tabular-nums">
            {t("overview.forecast_bounds", {
              lower: fmt(forecast.lower_bound),
              upper: fmt(forecast.upper_bound),
            })}
          </div>
          <div className="text-xs text-neutral-600 mt-1">
            {forecast.is_reliable
              ? t("overview.forecast_based_on", {
                  count: forecast.data_point_count,
                })
              : t("overview.forecast_unreliable")}
          </div>
        </>
      )}
    </div>
  );
}

/// v0.5.1 — risk signals card. Sources from `preview_alerts` (which
/// the App-level state already fetches every 30 s from the Alerts
/// tab path) — no new backend Tauri command. Renders top-3
/// unresolved alerts as severity-iconed labels (Gemini 3.1 Pro
/// v0.4.20 review's accessibility note: differentiate by ICON not
/// just color).
///
/// Severity icons are Unicode glyphs rather than a new SVG / lucide
/// dep — avoids ~10 KB of bundle for 3 symbols. Color mirrors Mac
/// sibling RiskSignalsList styling (red / amber / blue per tier).
///
/// Empty state ("looking good") fires when the alerts array is
/// non-null but length 0. Loading state fires when alerts is null
/// (the first preview_alerts hasn't completed yet).
/// v0.5.3 — Risk signals card now sources from the SERVER `alerts`
/// table via `get_server_alerts` (PostgREST GET), not the local
/// `preview_alerts` output. This unifies the card's data with the
/// `dashboard_summary.unresolved_alerts` tile so they no longer
/// diverge (v0.5.0+v0.5.1+v0.5.2 VM verify caught: tile said 7
/// unresolved, card said "looking good").
///
/// 3 distinct render states:
/// - **loading**: card mounted, first fetch in flight
/// - **error/offline**: fetch failed (no network, auth expired,
///   server unreachable). NOT rendered as the empty "Looking
///   good" state — that would falsely reassure a user whose phone
///   has surfaced budget alerts via push that the desktop can't
///   see right now (Gemini P2 v0.5.3 review).
/// - **success-empty**: fetch succeeded, 0 unresolved alerts. The
///   "Looking good" state.
/// - **success-with-data**: top 3 by severity DESC + "+N more"
///   overflow.
function RiskSignalsCard({ paired }: { paired: boolean }) {
  const { t } = useTranslation();
  const [alerts, setAlerts] = useState<ServerAlert[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  // Poll every 60 s while the card is mounted (matches Forecast +
  // TopProjects cadence). Per Gemini 3.1 Pro v0.5.2 review P1: a
  // background sync_now or push-driven server-side alert append
  // would otherwise be invisible until the user navigates away
  // and back.
  useEffect(() => {
    if (!paired) {
      setAlerts([]);
      setError(null);
      setLoaded(true);
      return;
    }
    let cancelled = false;
    const fetchOnce = async () => {
      try {
        const v = await invoke<ServerAlert[]>("get_server_alerts");
        if (cancelled) return;
        setAlerts(v);
        setError(null);
      } catch (e: any) {
        if (cancelled) return;
        // Don't clobber the previously-fetched alerts on a
        // transient error — keep the last good list visible
        // (slightly stale > totally absent). Surface the error
        // separately so the card can show an offline indicator.
        setError(String(e));
      } finally {
        if (!cancelled) setLoaded(true);
      }
    };
    fetchOnce();
    const id = setInterval(fetchOnce, 60_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [paired]);

  // Sort by severity DESC before slicing (Gemini 3.1 Pro v0.5.1
  // review P1). Server returns ordered by created_at; we
  // re-rank for severity priority on the client so a fresh Info
  // doesn't bump an older Critical out of the visible top-3.
  const ranked = alerts
    ? [...alerts].sort(
        (a, b) => severityRank(b.severity) - severityRank(a.severity),
      )
    : null;
  const displayCount = 3;
  const overflow = ranked ? Math.max(0, ranked.length - displayCount) : 0;

  return (
    <div className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <h2 className="text-sm font-semibold text-neutral-400 mb-2">
        {t("overview.risk_signals_title")}
      </h2>
      {!loaded ? (
        <div className="text-sm text-neutral-500">{t("misc.loading")}</div>
      ) : error && !ranked ? (
        // Error AND no previously-fetched data → offline state.
        // Distinct from the empty "Looking good" state.
        <div className="flex items-center gap-2 text-xs text-red-400/80">
          <SeverityIcon severity="Warning" />
          <span>{t("overview.risk_signals_offline")}</span>
        </div>
      ) : !ranked || ranked.length === 0 ? (
        <div className="flex items-center gap-2 text-sm text-emerald-400">
          <SeverityIcon severity="Info" intent="ok" />
          <span>{t("overview.risk_no_signals")}</span>
        </div>
      ) : (
        <ul className="space-y-1.5 text-sm">
          {ranked.slice(0, displayCount).map((a) => (
            <li key={a.id} className="flex items-start gap-2">
              <SeverityIcon
                severity={a.severity as Alert["severity"]}
              />
              <div className="flex-1 min-w-0">
                <div className="font-medium truncate" title={a.title}>
                  {a.title}
                </div>
                {a.related_project_name && (
                  <div className="text-xs text-neutral-500 truncate">
                    {a.related_project_name}
                  </div>
                )}
              </div>
            </li>
          ))}
          {overflow > 0 && (
            <li className="text-xs text-neutral-600 pt-1">
              {t("overview.risk_more_count", { count: overflow })}
            </li>
          )}
          {error && (
            // Stale-data hint: we have a previous successful fetch
            // but the latest poll failed. Don't hide the data, but
            // do tell the user we couldn't refresh.
            <li className="text-[10px] text-amber-500/70 pt-1">
              {t("overview.risk_signals_stale")}
            </li>
          )}
        </ul>
      )}
    </div>
  );
}

function severityRank(s: Alert["severity"] | string): number {
  if (s === "Critical") return 3;
  if (s === "Warning") return 2;
  return 1; // Info
}

/// v0.5.2 — top-projects card. Sources from the new `get_top_projects`
/// Tauri command which aggregates sessions table rows client-side
/// in Rust (per pre-flight check: `daily_usage_metrics` has no
/// `project` column server-side, so projects only live on
/// `sessions`). Renders top 5 by cost with session count and last-
/// active relative time.
///
/// Self-renders error / loading / empty states (Gemini 3.1 Pro
/// v0.5.0 review hard requirement). The `<unknown>` bucket from
/// the backend gets pretty-printed as a localized "(no project)"
/// label so we don't show literal angle brackets in the UI.
function TopProjectsCard({ paired }: { paired: boolean }) {
  const { t } = useTranslation();
  const fmt = useMoney();
  const [projects, setProjects] = useState<TopProject[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

  // v0.5.2 — poll every 60 s, same rationale as `CostForecastCard`.
  // Aggregating sessions table client-side per call is cheap
  // (server-side LIMIT 5000 + projection + RLS); the 60 s cadence
  // matches the project-attribution data's effective refresh rate
  // (sessions sync every 120 s background + on manual click).
  useEffect(() => {
    if (!paired) return;
    let cancelled = false;
    const fetchOnce = async () => {
      try {
        const p = await invoke<TopProject[]>("get_top_projects");
        if (cancelled) return;
        setProjects(p);
        setError(null);
      } catch (e: any) {
        if (cancelled) return;
        setError(String(e));
      } finally {
        if (!cancelled) setLoaded(true);
      }
    };
    fetchOnce();
    const id = setInterval(fetchOnce, 60_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [paired]);

  return (
    <div className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <h2 className="text-sm font-semibold text-neutral-400 mb-2">
        {t("overview.top_projects_title")}
      </h2>
      {!loaded && <div className="text-sm text-neutral-500">{t("misc.loading")}</div>}
      {loaded && error && (
        <div className="text-xs text-red-400/80">
          {t("overview.top_projects_failed")}
        </div>
      )}
      {loaded && !error && projects && projects.length === 0 && (
        <div className="text-xs text-neutral-500">
          {t("overview.top_projects_empty")}
        </div>
      )}
      {loaded && !error && projects && projects.length > 0 && (
        <ul className="space-y-1.5 text-sm">
          {projects.map((p) => {
            const parts = p.last_active
              ? formatRelativeShortParts(p.last_active)
              : null;
            return (
              <li key={p.project} className="flex items-baseline gap-2">
                <span
                  className="font-medium truncate flex-1 min-w-0"
                  title={p.project === UNKNOWN_PROJECT ? "" : p.project}
                >
                  {p.project === UNKNOWN_PROJECT
                    ? t("overview.top_projects_unknown")
                    : p.project}
                </span>
                <span className="text-xs text-neutral-300 tabular-nums">
                  {fmt(p.cost_usd)}
                </span>
                {parts && (
                  <span className="text-[10px] text-neutral-600 tabular-nums">
                    {t(`time.unit_${parts.unit}`, { count: parts.value })}
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/// v0.5.1 — inline SVG icons for severity (Gemini 3.1 Pro v0.5.1
/// review P1). Replaces Unicode glyphs (⛔/⚠/ℹ) which render as
/// system emoji on Win+Linux and IGNORE CSS `color` (forced
/// multicolor), defeating the accessibility-by-icon-PLUS-color
/// design. SVG paths from lucide.dev (MIT) — `alert-octagon`,
/// `alert-triangle`, `info`. No new dep; ~30 lines for 3 icons
/// is cheaper than pulling the whole `lucide-react` package.
///
/// `intent` lets the empty-state ✓-style call site reuse the Info
/// shape with green styling without conflating "informational
/// alert" and "all clear."
function SeverityIcon({
  severity,
  intent,
}: {
  severity: Alert["severity"];
  intent?: "ok";
}) {
  const baseClass = "shrink-0 w-3.5 h-3.5 mt-0.5";
  if (intent === "ok") {
    // Check-circle for "looking good" empty state.
    return (
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={`${baseClass} text-emerald-400`}
        role="img"
        aria-label="all clear"
      >
        <path d="M22 11.08V12a10 10 0 1 1-5.93-9.14" />
        <polyline points="22 4 12 14.01 9 11.01" />
      </svg>
    );
  }
  if (severity === "Critical") {
    return (
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={`${baseClass} text-red-400`}
        role="img"
        aria-label="critical"
      >
        <polygon points="7.86 2 16.14 2 22 7.86 22 16.14 16.14 22 7.86 22 2 16.14 2 7.86 7.86 2" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
    );
  }
  if (severity === "Warning") {
    return (
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={`${baseClass} text-amber-400`}
        role="img"
        aria-label="warning"
      >
        <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
        <line x1="12" y1="9" x2="12" y2="13" />
        <line x1="12" y1="17" x2="12.01" y2="17" />
      </svg>
    );
  }
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={`${baseClass} text-blue-400`}
      role="img"
      aria-label="info"
    >
      <circle cx="12" cy="12" r="10" />
      <line x1="12" y1="16" x2="12" y2="12" />
      <line x1="12" y1="8" x2="12.01" y2="8" />
    </svg>
  );
}

function CostTrendChart({ scan }: { scan: ScanResult }) {
  const { t } = useTranslation();
  const fmt = useMoney();
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
        <span className="ml-auto font-mono">{t("overview.max_per_day", { value: fmt(maxCost) })}</span>
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

// v0.4.20 — per-provider collector status surfaced from
// `get_last_collector_status`. Empty Vec until the first background
// `collect_all` cycle runs (~20s after launch). UI treats absent
// entries as "no error known" (no badge), matching v0.4.15's stale-
// indicator policy.
type CollectorStatus = {
  provider: string;
  ok: boolean;
  /// One-line failure message; null on success or "user not configured".
  error: string | null;
  /// LOCAL-ONLY human-readable status line from the collector snapshot
  /// (e.g. "$12.34 balance", "Connected"). Absent when the collector has
  /// nothing to say beyond the gauge. Never comes from the server.
  status_text?: string | null;
};

type DailyUsageRow = {
  metric_date: string;
  provider: string;
  model: string;
  input_tokens: number;
  cached_tokens: number;
  output_tokens: number;
  cost: number;
};

// v0.13.0 — per-provider 30-day I/O token history (macOS ProviderUsageHistory
// parity). ioTokens = input + output EXCLUDING cached (the invariant); bars are
// gap-filled over the last 30 LOCAL days so a paired account's chart matches the
// Mac's. Server data (get_daily_usage) — empty state when not signed in / no
// history rather than a fake chart.
function ProviderUsageChart({
  bars,
  color,
}: {
  bars: { date: string; io: number }[];
  color: string;
}) {
  const { t } = useTranslation();
  const total = bars.reduce((s, b) => s + b.io, 0);
  if (bars.length === 0 || total === 0) {
    return <div className="text-xs text-neutral-500">{t("providers.chart_no_history")}</div>;
  }
  const max = Math.max(...bars.map((b) => b.io), 1);
  const W = 100;
  const H = 30;
  const gap = 0.6;
  const bw = (W - gap * (bars.length - 1)) / bars.length;
  return (
    <div>
      <div className="flex items-baseline justify-between mb-1">
        <span className="text-xs text-neutral-400">{t("providers.chart_title")}</span>
        <span className="text-[10px] text-neutral-600 tabular-nums">
          {t("providers.chart_io_total", { value: formatInt(total) })}
        </span>
      </div>
      <svg
        viewBox={`0 0 ${W} ${H}`}
        className="w-full h-10"
        preserveAspectRatio="none"
        role="img"
        aria-label={t("providers.chart_title")}
      >
        {bars.map((b, i) => {
          const h = b.io > 0 ? Math.max(0.75, (b.io / max) * H) : 0;
          return (
            <rect key={b.date} x={i * (bw + gap)} y={H - h} width={bw} height={h} fill={color} opacity={0.85}>
              <title>{`${b.date}: ${formatInt(b.io)}`}</title>
            </rect>
          );
        })}
      </svg>
      <div className="flex justify-between text-[10px] text-neutral-600 mt-0.5 tabular-nums">
        <span>{bars[0]?.date}</span>
        <span>{bars[bars.length - 1]?.date}</span>
      </div>
    </div>
  );
}

// v1.30 F2a — warning-threshold ticks overlaid on a REMAINING-oriented quota
// bar. An "80% used" tick sits at left:20% (1−f), matching the Mac's
// QuotaBarMarkers.place(onRemainingBar:true). Render inside a `relative` bar
// container, after the fill. (The expected-pace marker needs per-tier
// windowMinutes the desktop doesn't carry yet — a gated follow-up.)
function QuotaBarTicks({ thresholds = DEFAULT_WARN_THRESHOLDS }: { thresholds?: number[] }) {
  const { t } = useTranslation();
  const fracs = warningFractions(thresholds);
  if (fracs.length === 0) return null;
  return (
    <>
      {fracs.map((f) => (
        <div
          key={f}
          className="absolute top-0 bottom-0 w-px bg-neutral-200/60"
          style={{ left: `${placeOnRemainingBar(f) * 100}%` }}
          title={t("providers.warn_threshold", { pct: Math.round(f * 100) })}
          aria-hidden="true"
        />
      ))}
    </>
  );
}

// v1.38 F1/F2b — expected-pace marker on a REMAINING-oriented quota bar. Sits at
// `placeOnRemainingBar(expectedFraction)` = where the remaining fill WOULD be if
// usage exactly tracked elapsed time. If the actual fill is left of the marker
// you're using ahead of pace; right of it, under. Distinct sky-blue vs the
// neutral warning ticks. Only rendered when a pace is computable (known window +
// reset timestamp — see lib/pace.ts). Render inside the `relative` bar, after
// the fill + ticks.
function QuotaPaceMarker({ expectedFraction }: { expectedFraction: number }) {
  const { t } = useTranslation();
  return (
    <div
      className="absolute top-0 bottom-0 w-0.5 bg-sky-300"
      style={{ left: `${placeOnRemainingBar(expectedFraction) * 100}%` }}
      title={t("providers.pace_marker_hint", {
        pct: Math.round(expectedFraction * 100),
      })}
      aria-hidden="true"
    />
  );
}

// i18n key + text color for a pace status. "ahead" (burning faster than time) is
// the only cautionary one → amber; on-track green; under-pace muted.
function paceLabel(status: PaceStatus): { key: string; className: string } {
  switch (status) {
    case "ahead":
      return { key: "providers.pace_ahead", className: "text-amber-400" };
    case "under":
      return { key: "providers.pace_under", className: "text-neutral-500" };
    default:
      return { key: "providers.pace_on_track", className: "text-emerald-400" };
  }
}

type ServiceStatusRow = {
  provider: string;
  indicator: "operational" | "maintenance" | "minor" | "major" | "critical" | "unknown";
  description: string;
  page_url: string | null;
};

// Dot color by severity (v0.14). `unknown` → null (no dot — nothing meaningful).
function serviceStatusColor(indicator: ServiceStatusRow["indicator"]): string | null {
  switch (indicator) {
    case "operational":
      return "#34d399";
    case "maintenance":
      return "#38bdf8";
    case "minor":
      return "#fbbf24";
    case "major":
      return "#fb923c";
    case "critical":
      return "#f87171";
    default:
      return null;
  }
}

function Providers({ scan, paired }: { scan: ScanResult | null; paired: boolean }) {
  const { t } = useTranslation();
  const fmt = useMoney();
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  // v0.13.0 — per-provider 30-day usage history (server read; sign-in
  // required). Fetched once when paired; the chart renders in each provider's
  // expanded section. null = not yet loaded / not paired.
  const [dailyUsage, setDailyUsage] = useState<DailyUsageRow[] | null>(null);
  useEffect(() => {
    if (!paired) {
      setDailyUsage(null);
      return;
    }
    let cancelled = false;
    invoke<DailyUsageRow[]>("get_daily_usage", { days: 30 })
      .then((rows) => {
        if (!cancelled) setDailyUsage(rows);
      })
      .catch(() => {
        // Non-fatal (offline / not signed in) — show the empty state.
        if (!cancelled) setDailyUsage([]);
      });
    return () => {
      cancelled = true;
    };
  }, [paired]);
  // Aggregate to per-provider gap-filled 30-day I/O bars. ioTokens =
  // input + output EXCLUDING cached (the cross-platform invariant); bucket on
  // the LOCAL metric_date (the server already buckets by user-local day).
  const chartByProvider = useMemo(() => {
    const dates = lastNLocalDates(30);
    const idx = new Map(dates.map((d, i) => [d, i]));
    const perProvider = new Map<string, number[]>();
    for (const r of dailyUsage ?? []) {
      const i = idx.get(r.metric_date);
      if (i === undefined) continue;
      const io = (r.input_tokens || 0) + (r.output_tokens || 0);
      const arr = perProvider.get(r.provider) ?? new Array(30).fill(0);
      arr[i] += io;
      perProvider.set(r.provider, arr);
    }
    const out = new Map<string, { date: string; io: number }[]>();
    for (const [prov, arr] of perProvider) {
      out.set(
        prov,
        arr.map((io, i) => ({ date: dates[i], io })),
      );
    }
    return out;
  }, [dailyUsage]);
  // v0.10.1 — per-provider visibility filter. Users with several paired
  // providers can mute the ones they don't track. Persisted to
  // localStorage as the HIDDEN set (so a provider that only starts
  // reporting later defaults to visible). See lib/providerVisibility.ts.
  const [hidden, setHidden] = useState<Set<string>>(() => loadHiddenProviders());

  // v0.3.4 — fetch server-side provider quota / plan / tiers when paired.
  // Keyed by `paired` so a sign-in/sign-out cycle re-fetches. Errors are
  // swallowed to a soft-empty state — the local-scan card below stays
  // useful regardless.
  const [serverRows, setServerRows] = useState<ProviderSummaryRow[] | null>(null);
  const [serverError, setServerError] = useState<string | null>(null);
  // v0.4.19 — Force refresh now button state. Gemini review P1: the
  // button MUST be `disabled` while in-flight (not just spinner-only)
  // or spam-clicks fire concurrent sync_now invocations against
  // provider rate limits.
  const [refreshing, setRefreshing] = useState(false);
  // v0.4.20 — per-provider collector status. Refreshed on mount and
  // after every forceRefresh. The 120s background sync also updates
  // it server-side, but we don't poll on a timer — the next tick will
  // refresh the cache and the user can click "Refresh now" to see the
  // current state immediately.
  const [collectorStatus, setCollectorStatus] = useState<CollectorStatus[]>([]);

  const fetchSummary = useCallback(async () => {
    try {
      const rows = await invoke<ProviderSummaryRow[]>("get_provider_summary");
      setServerRows(rows);
      setServerError(null);
    } catch (e: any) {
      setServerRows(null);
      setServerError(String(e));
    }
  }, []);

  const fetchCollectorStatus = useCallback(async () => {
    try {
      const rows = await invoke<CollectorStatus[]>("get_last_collector_status");
      setCollectorStatus(rows);
    } catch {
      // Best-effort — a missing/failing status query is not worth
      // flagging in the UI; absent entries fall back to "no badge".
      setCollectorStatus([]);
    }
  }, []);

  useEffect(() => {
    if (!paired) {
      setServerRows(null);
      setServerError(null);
      setCollectorStatus([]);
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
    invoke<CollectorStatus[]>("get_last_collector_status")
      .then((rows) => {
        if (!cancelled) setCollectorStatus(rows);
      })
      .catch(() => {
        if (!cancelled) setCollectorStatus([]);
      });
    return () => {
      cancelled = true;
    };
  }, [paired]);

  // v0.4.22 — auto-refresh provider summary + collector status every
  // 30 s while the Providers tab is mounted. Without this, the
  // displayed `updated_at` only changed on mount or after a manual
  // "Refresh quota now" click — meaning a user idling on the Providers
  // tab for 5 min saw the same timestamp from the initial fetch even
  // though the background sync had landed 2 fresh rows in the
  // meantime. The new "synced X ago" line on each card is meaningless
  // without this poll. Cadence chosen at 30 s to match the alerts
  // tab's poll and to be ≤ the 120 s background-sync interval (so
  // the displayed relative-age never drifts more than ~30 s behind
  // server reality).
  useEffect(() => {
    if (!paired) return;
    // Mount guard — Gemini v0.4.22 P3 catch: an in-flight Promise.all
    // resolving after the tab unmounts (or `paired` flips) would
    // otherwise call set* on a dead component. The clearInterval
    // alone doesn't cover an already-running tick.
    let cancelled = false;
    const tick = async () => {
      try {
        const [rows, status] = await Promise.all([
          invoke<ProviderSummaryRow[]>("get_provider_summary"),
          invoke<CollectorStatus[]>("get_last_collector_status"),
        ]);
        if (cancelled) return;
        setServerRows(rows);
        setServerError(null);
        setCollectorStatus(status);
      } catch (e: any) {
        if (cancelled) return;
        setServerError(String(e));
      }
    };
    const id = setInterval(tick, 30_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [paired]);

  // v0.4.19 — manual force-refresh. Calls sync_now (re-runs all 6
  // provider collectors + uploads) then re-fetches provider_summary
  // so the freshly-synced data appears without waiting 120s for
  // the next background tick. `disabled` while in-flight per
  // Gemini review P1.
  // v0.4.20 — also re-fetch the collector status afterwards so any
  // newly-fixed (or freshly-broken) providers update their badge
  // without waiting for the next 120s background cycle.
  async function forceRefresh() {
    if (refreshing || !paired) return;
    setRefreshing(true);
    try {
      await invoke("sync_now");
      await Promise.all([fetchSummary(), fetchCollectorStatus()]);
    } catch (e: any) {
      setServerError(String(e));
    } finally {
      setRefreshing(false);
    }
  }
  // Index server rows by provider name for O(1) lookup during render.
  const serverByProvider = useMemo(() => {
    const m = new Map<string, ProviderSummaryRow>();
    if (serverRows) {
      for (const row of serverRows) m.set(row.provider, row);
    }
    return m;
  }, [serverRows]);

  // v0.4.20 — index collector status by provider for the error badge.
  const statusByProvider = useMemo(() => {
    const m = new Map<string, CollectorStatus>();
    for (const row of collectorStatus) m.set(row.provider, row);
    return m;
  }, [collectorStatus]);

  // v0.14 — provider service-status (public Statuspage; no auth). Fetched once
  // on mount; the Rust side caches ~5min so re-opening the tab is cheap.
  const [serviceStatus, setServiceStatus] = useState<ServiceStatusRow[]>([]);
  useEffect(() => {
    let cancelled = false;
    invoke<ServiceStatusRow[]>("get_service_statuses")
      .then((s) => {
        if (!cancelled) setServiceStatus(s);
      })
      .catch(() => {
        // Non-fatal — no badges rather than an error.
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const serviceStatusByProvider = useMemo(
    () => new Map(serviceStatus.map((s) => [s.provider, s])),
    [serviceStatus],
  );

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

  // v0.10.1 — flip a provider in/out of the hidden set and persist.
  function toggleHidden(provider: string) {
    const next = toggleHiddenProvider(hidden, provider);
    setHidden(next);
    saveHiddenProviders(next);
  }

  // v0.10.1 — clear the filter (used by the bar chip + all-hidden hint).
  function showAllProviders() {
    const empty = new Set<string>();
    setHidden(empty);
    saveHiddenProviders(empty);
  }

  // Cards the user hasn't muted. The bar scaling + empty-state checks
  // below all key off the visible set, so hiding the priciest provider
  // rescales the remaining bars instead of leaving them all tiny.
  const visible = grouped.filter((g) => !hidden.has(g.provider));
  const maxCost = Math.max(...visible.map((g) => g.cost), 1);

  return (
    <div className="space-y-3">
      {paired && (
        <div className="flex justify-end">
          <button
            type="button"
            onClick={forceRefresh}
            disabled={refreshing}
            className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-50 disabled:cursor-not-allowed"
            title={t("providers.force_refresh_tooltip") || ""}
          >
            {refreshing ? t("providers.force_refresh_loading") : t("providers.force_refresh_button")}
          </button>
        </div>
      )}
      {paired && serverError && (
        <div className="text-xs text-neutral-500 px-3 py-2 rounded-md border border-neutral-800/60 bg-neutral-900/30">
          {t("providers.server_unavailable")}
        </div>
      )}
      {/* v0.10.1 — per-provider visibility filter. Only worth showing
          when there's more than one provider to choose between. Chips
          render in the same cost-sorted order as the cards; a muted
          (line-through) chip means hidden. A separate "Show all" chip
          appears once anything is hidden. */}
      {grouped.length > 1 && (
        <div className="flex items-center gap-2 flex-wrap text-xs">
          <span className="text-neutral-500">{t("providers.visibility_label")}</span>
          {grouped.map((g) => {
            const isHidden = hidden.has(g.provider);
            return (
              <button
                key={g.provider}
                type="button"
                onClick={() => toggleHidden(g.provider)}
                aria-pressed={!isHidden}
                title={
                  isHidden
                    ? t("providers.visibility_show_tooltip", { provider: g.provider })
                    : t("providers.visibility_hide_tooltip", { provider: g.provider })
                }
                className={
                  isHidden
                    ? "px-2 py-0.5 rounded-full border border-neutral-800 text-neutral-600 line-through hover:border-neutral-700"
                    : "px-2 py-0.5 rounded-full border border-emerald-800/60 bg-emerald-950/40 text-emerald-300 hover:bg-emerald-900/40"
                }
              >
                <span
                  className="inline-block w-1.5 h-1.5 rounded-full mr-1 align-middle"
                  style={{ backgroundColor: providerColor(g.provider) }}
                />
                {g.provider}
              </button>
            );
          })}
          {hidden.size > 0 && (
            <button
              type="button"
              onClick={showAllProviders}
              className="px-2 py-0.5 rounded-full border border-neutral-700 text-neutral-300 hover:bg-neutral-800"
            >
              {t("providers.visibility_show_all")}
            </button>
          )}
        </div>
      )}
      {visible.map((v) => {
        const isOpen = expanded.has(v.provider);
        const barPct = (v.cost / maxCost) * 100;
        const sortedModels = Array.from(v.models.entries())
          .sort((a, b) => b[1].cost - a[1].cost)
          .slice(0, 10);
        const srv = serverByProvider.get(v.provider);
        const color = providerColor(v.provider);
        return (
          <div
            key={v.provider}
            className="rounded-lg border border-neutral-800 bg-neutral-900/40 overflow-hidden"
            style={{ borderLeftColor: color, borderLeftWidth: "3px" }}
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
                    <span
                      className="inline-flex items-center justify-center w-5 h-5 rounded text-[10px] font-bold shrink-0"
                      style={{
                        backgroundColor: `${color}22`,
                        color,
                        border: `1px solid ${color}55`,
                      }}
                      aria-hidden="true"
                    >
                      {providerMonogram(v.provider)}
                    </span>
                    <span className="font-semibold">{v.provider}</span>
                    {(() => {
                      // v0.14 — provider service-status dot (public Statuspage).
                      const ss = serviceStatusByProvider.get(v.provider);
                      const color = ss ? serviceStatusColor(ss.indicator) : null;
                      if (!ss || !color) return null;
                      const label = `${t(`providers.status_${ss.indicator}`)}${
                        ss.description ? ` · ${ss.description}` : ""
                      }`;
                      const dot = (
                        <span
                          className="w-2 h-2 rounded-full shrink-0"
                          style={{ backgroundColor: color }}
                          title={label}
                          aria-label={label}
                          role="img"
                        />
                      );
                      return ss.page_url ? (
                        <a
                          href={ss.page_url}
                          target="_blank"
                          rel="noreferrer noopener"
                          onClick={(e) => e.stopPropagation()}
                          className="shrink-0 flex items-center"
                        >
                          {dot}
                        </a>
                      ) : (
                        dot
                      );
                    })()}
                    {srv?.plan_type && <PlanBadge plan={srv.plan_type} />}
                    {/* v0.4.20 — error badge takes precedence over the
                        v0.4.15 stale badge: a known collect() failure
                        is more actionable than "data went stale". A
                        provider can be both errored and stale, but
                        showing both badges crowds the row — pick one. */}
                    {(() => {
                      const st = statusByProvider.get(v.provider);
                      if (st && !st.ok && st.error) {
                        return (
                          <span
                            className="px-1.5 py-0.5 text-xs rounded bg-red-950/60 border border-red-800 text-red-300"
                            title={t("providers.error_tooltip", { reason: st.error })}
                          >
                            {t("providers.error_badge")}
                          </span>
                        );
                      }
                      if (srv && isStaleProviderRow(srv.updated_at)) {
                        return (
                          <span
                            className="px-1.5 py-0.5 text-xs rounded bg-amber-950/60 border border-amber-800 text-amber-300"
                            title={t("providers.stale_tooltip", {
                              age: formatRelativeMinutes(srv.updated_at!),
                            })}
                          >
                            {t("providers.stale_badge")}
                          </span>
                        );
                      }
                      return null;
                    })()}
                    {/* v0.4.22 — show last-sync recency next to the
                        badges. The v0.4.15 stale badge fires only
                        after 6 min; this fills the gap so users see
                        sync activity, not just absence-of-error. The
                        polling effect refreshes serverRows every 30 s
                        so this naturally re-renders. Hidden when
                        updated_at is missing or unparseable.
                        v0.5.0 — unit localized via `time.unit_<u>`
                        keys (was hardcoded English "s/min/hr/d";
                        v0.4.23 VM caught zh-CN reading the bare "s"
                        as visually empty before CJK chars). */}
                    {(() => {
                      const parts = srv?.updated_at
                        ? formatRelativeShortParts(srv.updated_at)
                        : null;
                      if (!parts) return null;
                      return (
                        <span
                          className="text-[10px] text-neutral-500 tabular-nums"
                          title={t("providers.synced_ago_tooltip")}
                        >
                          {t("providers.synced_ago", {
                            age: t(`time.unit_${parts.unit}`, {
                              count: parts.value,
                            }),
                          })}
                        </span>
                      );
                    })()}
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
                  {/* LOCAL-ONLY status line (e.g. "$12.34 balance",
                      "Connected") from the collector snapshot via
                      get_last_collector_status. Reserved for balance /
                      status-only providers whose raw gauge is meaningless;
                      real-gauge providers leave it unset. Never synced. */}
                  {(() => {
                    const stx = statusByProvider.get(v.provider)?.status_text;
                    return stx ? (
                      <div className="text-xs text-neutral-400 mt-0.5 tabular-nums">
                        {stx}
                      </div>
                    ) : null;
                  })()}
                  <div className="mt-2 h-1 bg-neutral-800 rounded overflow-hidden max-w-xs">
                    <div
                      className="h-full bg-gradient-to-r from-emerald-500 to-cyan-500"
                      style={{ width: `${barPct}%` }}
                    />
                  </div>
                </div>
              </div>
              <div className="text-right shrink-0">
                <div className="font-mono text-lg">{fmt(v.cost)}</div>
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
                      // v1.38 F1/F2b — usage pace vs the reset window. Null
                      // (no marker/text) unless the tier name encodes a known
                      // fixed window AND a reset timestamp is present; see
                      // lib/pace.ts (we never assume a default window).
                      const usedFraction =
                        tier.quota > 0 ? (tier.quota - remaining) / tier.quota : 0;
                      const pace = computePace({
                        usedFraction,
                        windowMinutes: windowMinutesForTier(tier.name),
                        resetTimeMs: parseResetMs(tier.reset_time),
                        nowMs: Date.now(),
                      });
                      const paceMeta = pace ? paceLabel(pace.status) : null;
                      return (
                        <div key={tier.name} className="text-xs">
                          <div className="flex justify-between text-neutral-400 mb-0.5">
                            <span className="flex items-center gap-1.5">
                              <span>{tier.name}</span>
                              {pace && paceMeta && (
                                <span
                                  className={`text-[10px] ${paceMeta.className}`}
                                  title={t("providers.pace_hint", {
                                    used: Math.round(pace.usedFraction * 100),
                                    expected: Math.round(pace.expectedFraction * 100),
                                  })}
                                >
                                  {t(paceMeta.key)}
                                </span>
                              )}
                            </span>
                            <span className="font-mono">
                              {t("providers.tier_left", {
                                remaining: formatInt(tier.remaining),
                                quota: formatInt(tier.quota),
                              })}
                            </span>
                          </div>
                          <div className="relative h-1.5 bg-neutral-800 rounded overflow-hidden">
                            <div
                              className={`h-full bg-gradient-to-r ${color}`}
                              style={{ width: `${Math.min(100, remainingPct)}%` }}
                            />
                            <QuotaBarTicks />
                            {pace && (
                              <QuotaPaceMarker expectedFraction={pace.expectedFraction} />
                            )}
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
                        <div className="relative h-1.5 bg-neutral-800 rounded overflow-hidden">
                          <div
                            className={`h-full bg-gradient-to-r ${color}`}
                            style={{ width: `${Math.min(100, remainingPct)}%` }}
                          />
                          <QuotaBarTicks />
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
                        <td className="py-1.5 text-right font-mono">{fmt(m.cost)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}

            {isOpen && (
              <div className="px-4 pb-4 pt-1 border-t border-neutral-800/50">
                <ProviderUsageChart bars={chartByProvider.get(v.provider) ?? []} color={color} />
              </div>
            )}
          </div>
        );
      })}
      {grouped.length === 0 && (
        <div className="text-sm text-neutral-500">{t("providers.no_usage")}</div>
      )}
      {/* v0.10.1 — everything is hidden by the filter. Distinct from the
          no_usage state above (there IS data, the user just muted it
          all). Offer a one-click way back. */}
      {grouped.length > 0 && visible.length === 0 && (
        <div className="flex items-center gap-3 text-sm text-neutral-500">
          <span>{t("providers.all_hidden")}</span>
          <button
            type="button"
            onClick={showAllProviders}
            className="px-2 py-0.5 text-xs rounded-full border border-neutral-700 text-neutral-300 hover:bg-neutral-800"
          >
            {t("providers.visibility_show_all")}
          </button>
        </div>
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
  updater,
  remoteControlEnabled,
  remoteControlSaving,
  remoteRefreshedAt,
  onSetRemoteControlEnabled,
  onCheckUpdate,
  onRelaunchAfterUpdate,
  onPaired,
  onUnpaired,
  onSynced,
}: {
  config: ConfigView | null;
  scan: ScanResult | null;
  lastSync: { at: Date; report: SyncReport } | null;
  // v0.5.3 — updater state lifted to App-level. Settings is now a
  // pure presentation consumer; no local useState. Codex P1+P2.
  updater: UpdaterState;
  // v0.6.0 — Remote Approvals state lifted to App-level (same
  // pattern). The Privacy section receives current state + a
  // single setter that handles optimistic-flip + revert-on-error.
  remoteControlEnabled: boolean | null;
  remoteControlSaving: boolean;
  remoteRefreshedAt: Date | null;
  onSetRemoteControlEnabled: (enabled: boolean) => Promise<void>;
  onCheckUpdate: () => Promise<void>;
  onRelaunchAfterUpdate: () => Promise<void>;
  onPaired: () => Promise<void>;
  onUnpaired: () => Promise<void>;
  onSynced: (r: SyncReport) => void;
}) {
  const { t } = useTranslation();
  const fmt = useMoney();
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
  // v0.5.3 — local `updater` + `doCheckUpdate` removed. State now
  // lives at App-level (single source of truth) and is passed in
  // via props. The UpdaterPanel below receives the same state +
  // callbacks the App uses for the header banner.

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
                cost: fmt(lastSync.report.total_cost_usd),
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

      <DateRangeSection />

      <CurrencySection />

      <ExportSection scan={scan} />

      <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
        <h2 className="text-sm font-semibold text-neutral-300">{t("settings.updates_heading")}</h2>
        <UpdaterPanel
          state={updater}
          onCheck={onCheckUpdate}
          onRelaunch={onRelaunchAfterUpdate}
        />
        <p className="text-xs text-neutral-600" dangerouslySetInnerHTML={{ __html: t("settings.updates_hint") }} />
      </section>

      {/* v0.4.7 — Integrations placed at true bottom of Settings tab,
          after Updates, per v0.4.6 dev plan §3 (was incorrectly between
          Export and Updates in v0.4.6 first ship; VM verification flagged
          the discrepancy). Kept this position so the section visually
          reads as "advanced / opt-in" tail of the Settings tab. */}
      <IntegrationsSection />

      {/* v0.6.0 — Privacy & Remote Control toggle. Sits ABOVE Danger
          Zone because (a) it's not destructive (toggle off is fully
          reversible), and (b) Danger Zone should remain the visually
          last block since it owns the most-irreversible actions. */}
      {paired && (
        <RemotePrivacySection
          enabled={remoteControlEnabled}
          saving={remoteControlSaving}
          refreshedAt={remoteRefreshedAt}
          onSetEnabled={onSetRemoteControlEnabled}
        />
      )}

      {/* v0.5.4 — Danger Zone at the absolute bottom of Settings.
          Visual distance + red-tinted border + last-position is the
          conventional "destructive actions live here" pattern from
          Vercel / Discord / Slack. Type-to-confirm gate on delete
          per Gemini 3.1 Pro decision: dev-tool audience, friction is
          a feature. Per Codex P1: RPC fires FIRST in the backend,
          then best-effort local clear — see lib.rs::delete_account_and_unpair. */}
      <DangerZoneSection paired={paired} onUnpaired={onUnpaired} />

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
  deepseek_api_key_set: boolean;
  zai_api_key_set: boolean;
  crof_api_key_set: boolean;
  minimax_api_key_set: boolean;
  moonshot_api_key_set: boolean;
  venice_api_key_set: boolean;
  kimi_k2_api_key_set: boolean;
  augment_cookie_set: boolean;
  perplexity_cookie_set: boolean;
  t3chat_cookie_set: boolean;
  stepfun_cookie_set: boolean;
  warp_api_key_set: boolean;
  kimi_auth_token_set: boolean;
  grok_cookie_set: boolean;
  glm_api_key_set: boolean;
  volcano_api_key_set: boolean;
  groq_api_key_set: boolean;
  mistral_cookie_set: boolean;
  deepgram_api_key_set: boolean;
  elevenlabs_api_key_set: boolean;
  kilo_api_key_set: boolean;
  alibaba_api_key_set: boolean;
  openai_admin_key_set: boolean;
  codebuff_api_key_set: boolean;
  manus_cookie_set: boolean;
  abacus_cookie_set: boolean;
  openrouter_base_url: string | null;
  env_override_cursor: boolean;
  env_override_copilot: boolean;
  env_override_openrouter_key: boolean;
  env_override_openrouter_url: boolean;
  env_override_deepseek: boolean;
  env_override_zai: boolean;
  env_override_crof: boolean;
  env_override_minimax: boolean;
  env_override_moonshot: boolean;
  env_override_venice: boolean;
  env_override_kimi_k2: boolean;
  env_override_augment: boolean;
  env_override_perplexity: boolean;
  env_override_t3chat: boolean;
  env_override_stepfun: boolean;
  env_override_warp: boolean;
  env_override_kimi: boolean;
  env_override_grok: boolean;
  env_override_glm: boolean;
  env_override_volcano: boolean;
  env_override_groq: boolean;
  env_override_mistral: boolean;
  env_override_deepgram: boolean;
  env_override_elevenlabs: boolean;
  env_override_kilo: boolean;
  env_override_alibaba: boolean;
  env_override_openai_admin: boolean;
  env_override_codebuff: boolean;
  env_override_manus: boolean;
  env_override_abacus: boolean;
  // v0.4.20 — active credentials backend, surfaced as a Storage line
  // at the top of the Integrations panel. Mirrors the diagnostic-snapshot
  // field of the same name. Per Gemini 3.1 Pro v0.4.20 review: degraded
  // file fallback gets a ⚠ icon with a tooltip — plain text alone is
  // too easy to miss.
  storage_backend: "os_keychain" | "file";
};

type ProviderCredsUpdateKey =
  | "cursor_cookie"
  | "copilot_token"
  | "openrouter_api_key"
  | "openrouter_base_url"
  | "deepseek_api_key"
  | "zai_api_key"
  | "crof_api_key"
  | "minimax_api_key"
  | "moonshot_api_key"
  | "venice_api_key"
  | "kimi_k2_api_key"
  | "augment_cookie"
  | "perplexity_cookie"
  | "t3chat_cookie"
  | "stepfun_cookie"
  | "warp_api_key"
  | "kimi_auth_token"
  | "grok_cookie"
  | "glm_api_key"
  | "volcano_api_key"
  | "groq_api_key"
  | "mistral_cookie"
  | "deepgram_api_key"
  | "elevenlabs_api_key"
  | "kilo_api_key"
  | "alibaba_api_key"
  | "openai_admin_key"
  | "codebuff_api_key"
  | "manus_cookie"
  | "abacus_cookie";

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
    {
      provider: "DeepSeek",
      key: "deepseek_api_key",
      labelKey: "settings.integrations.deepseek_api_key_label",
      helpKey: "settings.integrations.deepseek_api_key_help",
      isSet: view.deepseek_api_key_set,
      envOverride: view.env_override_deepseek,
      envVar: "DEEPSEEK_API_KEY",
    },
    {
      provider: "z.ai",
      key: "zai_api_key",
      labelKey: "settings.integrations.zai_api_key_label",
      helpKey: "settings.integrations.zai_api_key_help",
      isSet: view.zai_api_key_set,
      envOverride: view.env_override_zai,
      envVar: "Z_AI_API_KEY",
    },
    {
      provider: "Crof",
      key: "crof_api_key",
      labelKey: "settings.integrations.crof_api_key_label",
      helpKey: "settings.integrations.crof_api_key_help",
      isSet: view.crof_api_key_set,
      envOverride: view.env_override_crof,
      envVar: "CROF_API_KEY",
    },
    {
      provider: "MiniMax",
      key: "minimax_api_key",
      labelKey: "settings.integrations.minimax_api_key_label",
      helpKey: "settings.integrations.minimax_api_key_help",
      isSet: view.minimax_api_key_set,
      envOverride: view.env_override_minimax,
      envVar: "MINIMAX_API_KEY",
    },
    {
      provider: "Moonshot",
      key: "moonshot_api_key",
      labelKey: "settings.integrations.moonshot_api_key_label",
      helpKey: "settings.integrations.moonshot_api_key_help",
      isSet: view.moonshot_api_key_set,
      envOverride: view.env_override_moonshot,
      envVar: "MOONSHOT_API_KEY",
    },
    {
      provider: "Venice",
      key: "venice_api_key",
      labelKey: "settings.integrations.venice_api_key_label",
      helpKey: "settings.integrations.venice_api_key_help",
      isSet: view.venice_api_key_set,
      envOverride: view.env_override_venice,
      envVar: "VENICE_API_KEY",
    },
    {
      provider: "Kimi K2",
      key: "kimi_k2_api_key",
      labelKey: "settings.integrations.kimi_k2_api_key_label",
      helpKey: "settings.integrations.kimi_k2_api_key_help",
      isSet: view.kimi_k2_api_key_set,
      envOverride: view.env_override_kimi_k2,
      envVar: "KIMI_K2_API_KEY",
    },
    {
      provider: "Augment",
      key: "augment_cookie",
      labelKey: "settings.integrations.augment_cookie_label",
      helpKey: "settings.integrations.augment_cookie_help",
      isSet: view.augment_cookie_set,
      envOverride: view.env_override_augment,
      envVar: "AUGMENT_COOKIE",
    },
    {
      provider: "Perplexity",
      key: "perplexity_cookie",
      labelKey: "settings.integrations.perplexity_cookie_label",
      helpKey: "settings.integrations.perplexity_cookie_help",
      isSet: view.perplexity_cookie_set,
      envOverride: view.env_override_perplexity,
      envVar: "PERPLEXITY_COOKIE",
    },
    {
      provider: "T3 Chat",
      key: "t3chat_cookie",
      labelKey: "settings.integrations.t3chat_cookie_label",
      helpKey: "settings.integrations.t3chat_cookie_help",
      isSet: view.t3chat_cookie_set,
      envOverride: view.env_override_t3chat,
      envVar: "T3CHAT_COOKIE",
    },
    {
      provider: "StepFun",
      key: "stepfun_cookie",
      labelKey: "settings.integrations.stepfun_cookie_label",
      helpKey: "settings.integrations.stepfun_cookie_help",
      isSet: view.stepfun_cookie_set,
      envOverride: view.env_override_stepfun,
      envVar: "STEPFUN_COOKIE",
    },
    {
      provider: "Warp",
      key: "warp_api_key",
      labelKey: "settings.integrations.warp_api_key_label",
      helpKey: "settings.integrations.warp_api_key_help",
      isSet: view.warp_api_key_set,
      envOverride: view.env_override_warp,
      envVar: "WARP_API_KEY",
    },
    {
      provider: "Kimi",
      key: "kimi_auth_token",
      labelKey: "settings.integrations.kimi_auth_token_label",
      helpKey: "settings.integrations.kimi_auth_token_help",
      isSet: view.kimi_auth_token_set,
      envOverride: view.env_override_kimi,
      envVar: "KIMI_AUTH_TOKEN",
    },
    {
      provider: "Grok",
      key: "grok_cookie",
      labelKey: "settings.integrations.grok_cookie_label",
      helpKey: "settings.integrations.grok_cookie_help",
      isSet: view.grok_cookie_set,
      envOverride: view.env_override_grok,
      envVar: "GROK_COOKIE",
    },
    {
      provider: "GLM",
      key: "glm_api_key",
      labelKey: "settings.integrations.glm_api_key_label",
      helpKey: "settings.integrations.glm_api_key_help",
      isSet: view.glm_api_key_set,
      envOverride: view.env_override_glm,
      envVar: "GLM_API_KEY",
    },
    {
      provider: "Volcano Engine",
      key: "volcano_api_key",
      labelKey: "settings.integrations.volcano_api_key_label",
      helpKey: "settings.integrations.volcano_api_key_help",
      isSet: view.volcano_api_key_set,
      envOverride: view.env_override_volcano,
      envVar: "ARK_API_KEY",
    },
    {
      provider: "Groq",
      key: "groq_api_key",
      labelKey: "settings.integrations.groq_api_key_label",
      helpKey: "settings.integrations.groq_api_key_help",
      isSet: view.groq_api_key_set,
      envOverride: view.env_override_groq,
      envVar: "GROQ_API_KEY",
    },
    {
      provider: "Mistral",
      key: "mistral_cookie",
      labelKey: "settings.integrations.mistral_cookie_label",
      helpKey: "settings.integrations.mistral_cookie_help",
      isSet: view.mistral_cookie_set,
      envOverride: view.env_override_mistral,
      envVar: "MISTRAL_COOKIE",
    },
    {
      provider: "Deepgram",
      key: "deepgram_api_key",
      labelKey: "settings.integrations.deepgram_api_key_label",
      helpKey: "settings.integrations.deepgram_api_key_help",
      isSet: view.deepgram_api_key_set,
      envOverride: view.env_override_deepgram,
      envVar: "DEEPGRAM_API_KEY",
    },
    {
      provider: "ElevenLabs",
      key: "elevenlabs_api_key",
      labelKey: "settings.integrations.elevenlabs_api_key_label",
      helpKey: "settings.integrations.elevenlabs_api_key_help",
      isSet: view.elevenlabs_api_key_set,
      envOverride: view.env_override_elevenlabs,
      envVar: "ELEVENLABS_API_KEY",
    },
    {
      provider: "Kilo",
      key: "kilo_api_key",
      labelKey: "settings.integrations.kilo_api_key_label",
      helpKey: "settings.integrations.kilo_api_key_help",
      isSet: view.kilo_api_key_set,
      envOverride: view.env_override_kilo,
      envVar: "KILO_API_KEY",
    },
    {
      provider: "Alibaba",
      key: "alibaba_api_key",
      labelKey: "settings.integrations.alibaba_api_key_label",
      helpKey: "settings.integrations.alibaba_api_key_help",
      isSet: view.alibaba_api_key_set,
      envOverride: view.env_override_alibaba,
      envVar: "ALIBABA_CODING_PLAN_API_KEY",
    },
    {
      provider: "OpenAI Admin",
      key: "openai_admin_key",
      labelKey: "settings.integrations.openai_admin_key_label",
      helpKey: "settings.integrations.openai_admin_key_help",
      isSet: view.openai_admin_key_set,
      envOverride: view.env_override_openai_admin,
      envVar: "OPENAI_ADMIN_KEY",
    },
    {
      provider: "Codebuff",
      key: "codebuff_api_key",
      labelKey: "settings.integrations.codebuff_api_key_label",
      helpKey: "settings.integrations.codebuff_api_key_help",
      isSet: view.codebuff_api_key_set,
      envOverride: view.env_override_codebuff,
      envVar: "CODEBUFF_API_KEY",
    },
    {
      provider: "Manus",
      key: "manus_cookie",
      labelKey: "settings.integrations.manus_cookie_label",
      helpKey: "settings.integrations.manus_cookie_help",
      isSet: view.manus_cookie_set,
      envOverride: view.env_override_manus,
      envVar: "MANUS_SESSION_TOKEN",
    },
    {
      provider: "Abacus AI",
      key: "abacus_cookie",
      labelKey: "settings.integrations.abacus_cookie_label",
      helpKey: "settings.integrations.abacus_cookie_help",
      isSet: view.abacus_cookie_set,
      envOverride: view.env_override_abacus,
      envVar: "ABACUS_COOKIE",
    },
  ];

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-4">
      <h2 className="text-sm font-semibold text-neutral-300">
        {t("settings.integrations.heading")}
      </h2>
      <p className="text-xs text-neutral-500">{t("settings.integrations.description")}</p>
      <div className="text-xs text-neutral-500">
        {t("settings.integrations.storage_label")}:{" "}
        {view.storage_backend === "os_keychain" ? (
          <span className="text-emerald-300">
            {t("settings.integrations.storage_os_keychain")}
          </span>
        ) : (
          <>
            <span className="text-amber-300">
              {t("settings.integrations.storage_file")}
            </span>{" "}
            <span
              className="cursor-help text-amber-400"
              title={t("settings.integrations.storage_file_tooltip") || ""}
              aria-label="info"
            >
              ⚠
            </span>
          </>
        )}
      </div>

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
              {/* v0.4.18 — UX parity with the 3 secret rows above. The URL
                  isn't a secret so we skip the confirm modal (which is
                  there for "expensive-to-recreate token" semantics) and
                  clear directly. VM verification of v0.4.17 caught the
                  inconsistency: the only way to clear the URL was to
                  manually empty the field and save. */}
              {view.openrouter_base_url && (
                <button
                  type="button"
                  onClick={async () => {
                    setDrafts({ ...drafts, openrouter_base_url: "" });
                    await saveField("openrouter_base_url", "");
                  }}
                  disabled={savingField === "openrouter_base_url"}
                  className="px-3 py-1 text-xs rounded bg-red-950/40 hover:bg-red-900/60 border border-red-900 text-red-300 disabled:opacity-40"
                >
                  {t("settings.integrations.clear_button")}
                </button>
              )}
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

// Settings → Date range. Chooses how many days back the LOCAL usage scan
// reaches; drives every `scan.days_scanned`-keyed surface (Overview tiles,
// activity heat strip, provider breakdown, entries table, export). Persisted in
// localStorage (lib/dateRange.ts); the backend clamps to 1..180 as well.
function DateRangeSection() {
  const { t } = useTranslation();
  const { days, setDays } = useScanRange();
  const custom = !isPreset(days);
  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-2">
      <h2 className="text-sm font-semibold text-neutral-300">
        {t("settings.range_heading")}
      </h2>
      <p className="text-xs text-neutral-500">{t("settings.range_hint")}</p>
      <div className="flex flex-wrap items-center gap-2">
        {RANGE_PRESETS.map((preset) => {
          const active = days === preset;
          return (
            <button
              key={preset}
              type="button"
              onClick={() => setDays(preset)}
              className={`px-3 py-1.5 rounded-md text-sm border transition-colors ${
                active
                  ? "border-emerald-500 bg-emerald-500/10 text-emerald-300"
                  : "border-neutral-700 bg-neutral-800 text-neutral-300 hover:border-neutral-600"
              }`}
            >
              {t("settings.range_option", { days: preset })}
            </button>
          );
        })}
        <label
          className={`flex items-center gap-1.5 px-3 py-1.5 rounded-md text-sm border ${
            custom
              ? "border-emerald-500 bg-emerald-500/10 text-emerald-300"
              : "border-neutral-700 bg-neutral-800 text-neutral-400"
          }`}
        >
          <span>{t("settings.range_custom")}</span>
          <input
            type="number"
            min={MIN_DAYS}
            max={MAX_DAYS}
            value={custom ? days : ""}
            placeholder="…"
            onChange={(e) => {
              const n = Number.parseInt(e.target.value, 10);
              // Ignore an empty / non-numeric field (keep the last value)
              // rather than snapping to a clamped default mid-edit.
              if (!Number.isNaN(n)) setDays(n);
            }}
            className="w-14 bg-transparent border-0 border-b border-neutral-600 px-1 py-0 text-sm text-neutral-100 focus:outline-none focus:border-emerald-500"
          />
          <span className="text-neutral-500">{t("settings.range_custom_unit")}</span>
        </label>
      </div>
    </section>
  );
}

// Settings → Display currency. Costs are computed + stored in USD; this only
// changes how they're shown (converted via daily FX rates; see lib/money.ts).
function CurrencySection() {
  const { t } = useTranslation();
  const { currency, setCurrency } = useCurrencySetting();
  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-2">
      <h2 className="text-sm font-semibold text-neutral-300">{t("settings.currency_heading")}</h2>
      <p className="text-xs text-neutral-500">{t("settings.currency_hint")}</p>
      <select
        value={currency}
        onChange={(e) => setCurrency(e.target.value)}
        className="px-3 py-2 rounded-md bg-neutral-800 border border-neutral-700 text-sm text-neutral-100"
      >
        {CURRENCIES.map((c) => (
          <option key={c.code} value={c.code}>
            {c.code === "USD" ? c.code : `${c.code} (${c.symbol})`}
          </option>
        ))}
      </select>
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

// v0.5.4 — Settings → Danger Zone. Two destructive actions:
//   - Clear local caches: reversible (next sync re-fetches everything)
//   - Delete cloud account: irreversible, type-to-confirm gated
//
// Plan/review highlights baked in here:
//   - Type-to-confirm phrase comes from i18n, so each language has its
//     own literal (`DELETE` / `删除` / `削除`). String equality, not
//     fuzzy match — Gemini decision: dev-tool audience, friction is a
//     feature.
//   - The ordering of the BACKEND call is RPC-first then local-clear
//     (Codex + Gemini P1) — see lib.rs::delete_account_and_unpair. The
//     frontend just calls the command and reacts to its result.
//   - On delete success we call `onUnpaired()`, the same handler the
//     "Unpair device" button uses; that refreshes config + auth status
//     so the UI flips back to the OTP sign-in form on its own.
//   - When the device isn't paired, "Delete cloud account" is hidden
//     entirely — there's no JWT to mint and no server row to delete.
//     Clear caches stays available because the on-disk scan cache
//     persists across sign-out and a user might want to wipe it.
type DangerState =
  | { kind: "idle" }
  | { kind: "confirming-clear" }
  | { kind: "clearing" }
  | { kind: "confirming-delete"; typed: string }
  | { kind: "deleting" }
  | { kind: "done-clear" }
  | { kind: "done-delete-error"; error: string }
  | { kind: "done-clear-error"; error: string };

function DangerZoneSection({
  paired,
  onUnpaired,
}: {
  paired: boolean;
  onUnpaired: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const [state, setState] = useState<DangerState>({ kind: "idle" });

  // The localized confirmation phrase. Different per language by design
  // (Gemini decision: keep the literal-match pattern, but localize the
  // word). String equality is intentional — fuzzy matching would defeat
  // the friction-is-feature aspect of the gate.
  const deletePhrase = t("settings.danger.delete_phrase");

  async function doClearCaches() {
    setState({ kind: "clearing" });
    try {
      await invoke("clear_local_caches");
      setState({ kind: "done-clear" });
      // Auto-fade the success toast so the user isn't stuck in a "click
      // here to dismiss" state machine.
      setTimeout(() => {
        setState((cur) => (cur.kind === "done-clear" ? { kind: "idle" } : cur));
      }, 2500);
    } catch (e: any) {
      setState({ kind: "done-clear-error", error: String(e) });
    }
  }

  async function doDeleteAccount() {
    setState({ kind: "deleting" });
    try {
      await invoke("delete_account_and_unpair");
      // onUnpaired refreshes config + auth status on the App level,
      // which in turn flips this Settings tab back to the OTP sign-in
      // form. By the time onUnpaired resolves, this DangerZoneSection
      // has already been re-rendered with paired=false, hiding the
      // delete branch entirely. The local state reset here is mostly
      // defensive — if the parent's onUnpaired is somehow no-op, we
      // at least don't get stuck in "deleting" forever.
      await onUnpaired();
      setState({ kind: "idle" });
    } catch (e: any) {
      // RPC error: the SERVER STATE IS UNCHANGED (we ordered RPC
      // first; on failure the local clear didn't run either). User
      // can retry, no recovery action needed.
      setState({ kind: "done-delete-error", error: String(e) });
    }
  }

  const deleteButtonEnabled =
    state.kind === "confirming-delete" && state.typed === deletePhrase;

  return (
    <section className="p-4 rounded-lg border border-red-900/40 bg-red-950/10 space-y-4">
      <div className="flex items-center gap-2">
        <span className="text-red-400">⚠</span>
        <h2 className="text-sm font-semibold text-red-200">
          {t("settings.danger.heading")}
        </h2>
      </div>

      {/* Clear local caches — reversible. Always available regardless of
          paired state because the on-disk scan cache persists across
          sign-out, and a user who just signed out may still want to wipe. */}
      <div className="space-y-2">
        <div>
          <h3 className="text-sm text-neutral-200">
            {t("settings.danger.clear_caches_title")}
          </h3>
          <p className="text-xs text-neutral-500">
            {t("settings.danger.clear_caches_body")}
          </p>
        </div>
        {state.kind === "confirming-clear" ? (
          <div className="flex gap-2">
            <button
              type="button"
              onClick={doClearCaches}
              className="px-3 py-1.5 rounded-md bg-amber-900/60 hover:bg-amber-800 text-sm border border-amber-800 text-amber-100"
            >
              {t("settings.danger.clear_caches_confirm_button")}
            </button>
            <button
              type="button"
              onClick={() => setState({ kind: "idle" })}
              className="px-3 py-1.5 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700"
            >
              {t("action.cancel")}
            </button>
          </div>
        ) : (
          <button
            type="button"
            disabled={state.kind === "clearing" || state.kind === "deleting"}
            onClick={() => setState({ kind: "confirming-clear" })}
            className="px-3 py-1.5 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700 disabled:opacity-50"
          >
            {state.kind === "clearing"
              ? t("settings.danger.clear_caches_processing")
              : t("settings.danger.clear_caches_button")}
          </button>
        )}
      </div>

      {/* Delete cloud account — irreversible, type-to-confirm gate. Hidden
          when not paired (no JWT to mint, no server row to delete). */}
      {paired && (
        <div className="space-y-2 pt-3 border-t border-red-900/30">
          <div>
            <h3 className="text-sm text-neutral-200">
              {t("settings.danger.delete_account_title")}
            </h3>
            <p className="text-xs text-neutral-500">
              {t("settings.danger.delete_account_body")}
            </p>
          </div>
          {state.kind === "confirming-delete" ? (
            <div className="space-y-2">
              <label className="block text-xs text-red-300">
                {t("settings.danger.delete_account_confirm_prompt", {
                  phrase: deletePhrase,
                })}
              </label>
              <input
                type="text"
                value={state.typed}
                onChange={(e) =>
                  setState({ kind: "confirming-delete", typed: e.target.value })
                }
                spellCheck={false}
                autoComplete="off"
                autoFocus
                className="w-full max-w-xs px-2 py-1 text-sm font-mono bg-neutral-950 border border-red-900/60 rounded text-neutral-200 focus:outline-none focus:border-red-500"
              />
              <div className="flex gap-2">
                <button
                  type="button"
                  disabled={!deleteButtonEnabled}
                  onClick={doDeleteAccount}
                  className="px-3 py-1.5 rounded-md bg-red-900/70 hover:bg-red-800 text-sm border border-red-800 text-red-100 disabled:opacity-40 disabled:cursor-not-allowed"
                >
                  {t("settings.danger.delete_account_confirm_button")}
                </button>
                <button
                  type="button"
                  onClick={() => setState({ kind: "idle" })}
                  className="px-3 py-1.5 rounded-md bg-neutral-800 hover:bg-neutral-700 text-sm border border-neutral-700"
                >
                  {t("action.cancel")}
                </button>
              </div>
            </div>
          ) : (
            <button
              type="button"
              disabled={state.kind === "clearing" || state.kind === "deleting"}
              onClick={() =>
                setState({ kind: "confirming-delete", typed: "" })
              }
              className="px-3 py-1.5 rounded-md bg-red-950/60 hover:bg-red-900/60 text-sm border border-red-900 text-red-200 disabled:opacity-50"
            >
              {state.kind === "deleting"
                ? t("settings.danger.delete_account_processing")
                : t("settings.danger.delete_account_button")}
            </button>
          )}
        </div>
      )}

      {/* Status messages (idle states get no banner; only the terminal
          "done" / "error" states render here). */}
      {state.kind === "done-clear" && (
        <div className="px-3 py-2 rounded-md bg-emerald-950/50 border border-emerald-900 text-emerald-200 text-xs">
          {t("settings.danger.clear_caches_done")}
        </div>
      )}
      {state.kind === "done-clear-error" && (
        <div className="px-3 py-2 rounded-md bg-red-950/60 border border-red-900 text-red-200 text-xs">
          {t("settings.danger.action_failed", { err: state.error })}
        </div>
      )}
      {state.kind === "done-delete-error" && (
        <div className="px-3 py-2 rounded-md bg-red-950/60 border border-red-900 text-red-200 text-xs">
          {t("settings.danger.action_failed", { err: state.error })}
        </div>
      )}
    </section>
  );
}

// v0.6.0 — Remote Approvals UI components (Slice 1: view + decide).
//
// Three components:
//   - RemoteApprovalsSheet — modal overlay listing pending approvals
//   - RemotePrivacySection — Settings card with the toggle + consent
//   - RemoteSessionsSection — Sessions-tab read-only managed list
//
// All three share App-level state lifted up via props (Codex-pattern
// matching v0.5.3 updater state lift). The sheet mounts only when
// `showApprovalsSheet === true`; the section renders only when
// `paired && remoteControlEnabled`.

function RemoteApprovalsSheet({
  enabled,
  pending,
  onClose,
  onDecided,
}: {
  enabled: boolean;
  pending: RemotePermissionRequest[];
  onClose: () => void;
  /** Called after a decide RPC succeeds OR fails (so the parent can
   *  reconcile via refreshRemoteState — handles the cross-device race
   *  where another device decided the same request first). */
  onDecided: () => Promise<void>;
}) {
  const { t } = useTranslation();
  // Local optimistic-removal: when user clicks Approve/Deny, we hide
  // the row immediately so the UI feels responsive even before the
  // RPC returns. If the RPC fails, we surface the error and trigger
  // a parent refresh which puts the row back if it really IS still
  // pending (Gemini v0.6.0 P1 / Q6).
  //
  // pendingDecisions is a Map<request_id, kind> (per Gemini post-impl
  // P2.1) so multiple Approve/Deny clicks across different rows
  // don't overwrite each other's loading state. The original
  // single-object `pendingDecision` would let user click Approve on
  // row B while row A's RPC was still in flight, and the `finally`
  // block would clear A's loading state prematurely.
  const [pendingDecisions, setPendingDecisions] = useState<
    Map<string, "approve" | "deny">
  >(new Map());
  const [error, setError] = useState<string | null>(null);
  // Locally hidden ids — flipped on Approve/Deny click; reset on
  // sheet remount or after error reconciliation.
  const [hiddenIds, setHiddenIds] = useState<Set<string>>(new Set());

  // Escape-key closes the sheet. v0.6.0 used `window.addEventListener`
  // alone; VM verify 2026-05-07 (clipulse-win-test) found that didn't
  // work in Tauri's Webview2 for the consent dialog (same pattern).
  // v0.6.1 hotfix: belt-and-braces — keep the window listener AND
  // add `onKeyDown` on the dialog wrapper (next, in JSX) AND
  // autoFocus the close button so the modal has a real focus
  // target. At least one path catches Esc regardless of focus
  // state or event-routing quirks.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const visible = pending.filter((r) => !hiddenIds.has(r.id));

  const decide = async (req: RemotePermissionRequest, kind: "approve" | "deny") => {
    setPendingDecisions((m) => {
      const next = new Map(m);
      next.set(req.id, kind);
      return next;
    });
    setError(null);
    setHiddenIds((s) => new Set(s).add(req.id));
    try {
      await invoke("decide_remote_approval", {
        requestId: req.id,
        decision: kind,
        scope: "once",
      });
      // Trigger parent refetch so the optimistic state reconciles
      // against the server.
      await onDecided();
    } catch (e: any) {
      const msg = String(e);
      // Gemini v0.6.0 Q6: cross-device race where another device
      // already decided this request. Surface a specific copy
      // ("already decided on another device") AND revert the
      // optimistic hide so the user sees the list refresh.
      if (msg.includes("ALREADY_DECIDED")) {
        setError(t("remote.error_already_decided"));
      } else {
        setError(t("remote.action_failed", { err: msg }));
      }
      // Revert hidden — let the parent refresh repopulate. If the
      // request really IS gone (race), it won't come back; if it's
      // still pending (RPC failed for another reason), the user
      // can retry.
      setHiddenIds((s) => {
        const next = new Set(s);
        next.delete(req.id);
        return next;
      });
      await onDecided();
    } finally {
      setPendingDecisions((m) => {
        const next = new Map(m);
        next.delete(req.id);
        return next;
      });
    }
  };

  const ageOf = (createdAt: string) => {
    const seconds = Math.max(
      0,
      Math.floor((Date.now() - Date.parse(createdAt)) / 1000)
    );
    if (seconds < 60) return t("time.unit_s", { count: seconds });
    if (seconds < 3600) return t("time.unit_min", { count: Math.floor(seconds / 60) });
    if (seconds < 86_400) return t("time.unit_hr", { count: Math.floor(seconds / 3600) });
    return t("time.unit_d", { count: Math.floor(seconds / 86_400) });
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4"
      onClick={onClose}
      // v0.6.1 hotfix layer 2: onKeyDown on the WRAPPER catches Esc
      // bubbled from any descendant. autoFocus on the Cancel button
      // (below) makes sure the bubble path exists immediately on
      // open. See useEffect comment above for the full multi-layer
      // rationale.
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
      role="presentation"
    >
      <div
        className="w-full max-w-xl max-h-[80vh] flex flex-col rounded-lg border border-neutral-800 bg-neutral-900 shadow-xl"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-labelledby="remote-approvals-title"
        tabIndex={-1}
      >
        <header className="flex items-center justify-between p-4 border-b border-neutral-800">
          <h2 id="remote-approvals-title" className="text-sm font-semibold text-neutral-200">
            {t("remote.title")}
          </h2>
          <button
            type="button"
            onClick={onClose}
            className="px-2 py-1 text-xs rounded border border-neutral-700 hover:bg-neutral-800"
            // v0.6.1 hotfix layer 3: autoFocus gives the modal a real
            // focus target on first render so Esc has a sane bubble
            // path (otherwise focus stays on whatever opened the
            // sheet, behind the modal, and Tauri's Webview2 doesn't
            // route Esc through React's window listener reliably).
            autoFocus
          >
            {t("action.cancel")}
          </button>
        </header>
        <div className="flex-1 overflow-y-auto p-4 space-y-3">
          {error && (
            <div className="px-3 py-2 rounded bg-red-950/60 border border-red-900 text-red-200 text-xs">
              {error}
            </div>
          )}
          {!enabled ? (
            <div className="text-xs text-neutral-500 italic py-6 text-center">
              {t("remote.disabled_hint")}
            </div>
          ) : visible.length === 0 ? (
            <div className="text-xs text-neutral-500 italic py-6 text-center">
              {t("remote.empty_pending")}
            </div>
          ) : (
            visible.map((req) => (
              <RemoteApprovalRow
                key={req.id}
                req={req}
                age={ageOf(req.created_at)}
                decisionInFlight={pendingDecisions.get(req.id) ?? null}
                onApprove={() => decide(req, "approve")}
                onDeny={() => decide(req, "deny")}
              />
            ))
          )}
        </div>
      </div>
    </div>
  );
}

function RemoteApprovalRow({
  req,
  age,
  decisionInFlight,
  onApprove,
  onDeny,
}: {
  req: RemotePermissionRequest;
  age: string;
  decisionInFlight: "approve" | "deny" | null;
  onApprove: () => void;
  onDeny: () => void;
}) {
  const { t } = useTranslation();
  const isHigh = req.risk === "high";
  const riskClass =
    req.risk === "high"
      ? "bg-red-950/60 text-red-300 border-red-900"
      : req.risk === "medium"
        ? "bg-amber-950/60 text-amber-300 border-amber-900"
        : // Per Gemini v0.6.0 Q4: low / unknown render neutral, NOT
          // green — green reads as "approved" rather than "low risk".
          "bg-neutral-800 text-neutral-400 border-neutral-700";
  const riskLabel =
    req.risk === "high"
      ? t("remote.risk_high")
      : req.risk === "medium"
        ? t("remote.risk_medium")
        : req.risk === "low"
          ? t("remote.risk_low")
          : req.risk; // unknown class → render verbatim
  return (
    <div className="rounded-md border border-neutral-800 bg-neutral-950/40 p-3 space-y-2">
      <div className="flex items-center justify-between text-xs">
        <span className="text-neutral-400">
          {req.device_name ?? t("remote.row_unknown_device")} · {req.provider}
        </span>
        <span className={`px-1.5 py-0.5 rounded border text-[10px] ${riskClass}`}>
          {riskLabel}
        </span>
      </div>
      <div className="font-mono text-xs text-neutral-200">{req.tool_name}</div>
      <div className="text-xs text-neutral-400">{req.summary}</div>
      <div className="flex items-center justify-between gap-2 pt-1">
        <span className="text-[10px] text-neutral-600">
          {t("remote.age_ago", { age })}
        </span>
        <div className="flex gap-2">
          <button
            type="button"
            onClick={onApprove}
            disabled={isHigh || decisionInFlight !== null}
            title={isHigh ? t("remote.high_risk_blocked_tooltip") : undefined}
            className="px-3 py-1 text-xs rounded bg-emerald-950/60 hover:bg-emerald-900/60 border border-emerald-900 text-emerald-200 disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {decisionInFlight === "approve"
              ? t("remote.approve_processing")
              : t("remote.approve_button")}
          </button>
          <button
            type="button"
            onClick={onDeny}
            disabled={decisionInFlight !== null}
            className="px-3 py-1 text-xs rounded bg-neutral-800 hover:bg-neutral-700 border border-neutral-700 disabled:opacity-40"
          >
            {decisionInFlight === "deny"
              ? t("remote.deny_processing")
              : t("remote.deny_button")}
          </button>
        </div>
      </div>
    </div>
  );
}

function RemotePrivacySection({
  enabled,
  saving,
  refreshedAt,
  onSetEnabled,
}: {
  enabled: boolean | null;
  saving: boolean;
  refreshedAt: Date | null;
  onSetEnabled: (enabled: boolean) => Promise<void>;
}) {
  const { t } = useTranslation();
  // Two-stage interaction (Gemini v0.6.0 Q3: match Mac's full consent
  // dialog on first-enable). Click toggle when off → show consent
  // dialog; click "Enable Remote Control" inside dialog → fire the
  // PATCH. Click toggle when on → fire PATCH directly (no dialog
  // needed for turning OFF; that strictly tightens privacy).
  const [showConsent, setShowConsent] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // v0.6.0 Esc handler used `window.addEventListener` only. VM verify
  // 2026-05-07 (clipulse-win-test) found this didn't actually close
  // the dialog in Tauri's Webview2 — even after Tab-cycling focus
  // INTO the dialog, Esc was eaten somewhere upstream. v0.6.1 hotfix:
  // belt-and-braces with THREE Esc dismissal paths so at least one
  // works regardless of focus state or event-routing quirks:
  //   1. window listener (this one — was alone in v0.6.0)
  //   2. onKeyDown on the dialog wrapper (next, in JSX) — fires on
  //      bubble from any focused element inside the modal
  //   3. autoFocus on the Cancel button (also next, in JSX) — gives
  //      something inside the modal a real focus target so Esc has
  //      a sane bubble path
  useEffect(() => {
    if (!showConsent) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowConsent(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [showConsent]);

  const handleToggleClick = async () => {
    if (enabled === false || enabled === null) {
      setShowConsent(true);
      return;
    }
    setError(null);
    try {
      await onSetEnabled(false);
    } catch (e: any) {
      setError(t("remote.action_failed", { err: String(e) }));
    }
  };

  const confirmEnable = async () => {
    setError(null);
    setShowConsent(false);
    try {
      await onSetEnabled(true);
    } catch (e: any) {
      setError(t("remote.action_failed", { err: String(e) }));
    }
  };

  const ageOf = (date: Date | null) => {
    if (!date) return null;
    const seconds = Math.max(0, Math.floor((Date.now() - date.getTime()) / 1000));
    if (seconds < 60) return t("time.unit_s", { count: seconds });
    if (seconds < 3600) return t("time.unit_min", { count: Math.floor(seconds / 60) });
    return t("time.unit_hr", { count: Math.floor(seconds / 3600) });
  };

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
      <div>
        <h2 className="text-sm font-semibold text-neutral-300 mb-1">
          {t("settings.privacy_heading")}
        </h2>
        <p className="text-xs text-neutral-500">{t("settings.privacy_body")}</p>
      </div>
      <div className="flex items-center justify-between gap-3">
        <span className="text-sm text-neutral-200">
          {t("settings.privacy_toggle_label")}
        </span>
        <button
          type="button"
          onClick={handleToggleClick}
          disabled={saving || enabled === null}
          className={`relative w-11 h-6 rounded-full transition-colors ${
            enabled === true
              ? "bg-emerald-700"
              : "bg-neutral-700"
          } disabled:opacity-50`}
          role="switch"
          aria-checked={enabled === true}
        >
          <span
            className={`absolute top-0.5 left-0.5 w-5 h-5 rounded-full bg-white transition-transform ${
              enabled === true ? "translate-x-5" : "translate-x-0"
            }`}
          />
        </button>
      </div>
      <div className="text-xs text-neutral-600">
        {enabled === true
          ? t("settings.privacy_status_on")
          : enabled === false
            ? t("settings.privacy_status_off")
            : t("misc.loading")}
        {refreshedAt && (
          <>
            {" "}
            {t("settings.privacy_status_refreshed", {
              age: ageOf(refreshedAt),
            })}
          </>
        )}
      </div>
      {error && (
        <div className="px-3 py-2 rounded bg-red-950/60 border border-red-900 text-red-200 text-xs">
          {error}
        </div>
      )}
      {/* v0.7.0 — Claude hook installer. Renders only when Remote
          Control is ON, because installing the hook makes no sense
          unless the user has opted in. Hidden when null/false. */}
      {enabled === true && <ClaudeHookInstaller />}
      {showConsent && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-4"
          onClick={() => setShowConsent(false)}
          // v0.6.1 hotfix layer 2: onKeyDown on the WRAPPER catches
          // Esc bubbled from any descendant. The autoFocus on the
          // Cancel button (below) ensures the modal has a focused
          // element, so the bubble path is sane even immediately
          // after the dialog opens.
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              setShowConsent(false);
            }
          }}
          role="presentation"
        >
          <div
            className="w-full max-w-md rounded-lg border border-neutral-800 bg-neutral-900 p-5 space-y-3 shadow-xl"
            onClick={(e) => e.stopPropagation()}
            role="dialog"
            aria-modal="true"
            aria-labelledby="privacy-consent-title"
            tabIndex={-1}
          >
            <h3
              id="privacy-consent-title"
              className="text-sm font-semibold text-neutral-200"
            >
              {t("settings.privacy_consent_title")}
            </h3>
            <ul className="space-y-1.5 text-xs text-neutral-400">
              <li>• {t("settings.privacy_consent_body_b1")}</li>
              <li>• {t("settings.privacy_consent_body_b2")}</li>
              <li>• {t("settings.privacy_consent_body_b3")}</li>
            </ul>
            <div className="flex justify-end gap-2 pt-2">
              <button
                type="button"
                onClick={() => setShowConsent(false)}
                className="px-3 py-1.5 text-xs rounded border border-neutral-700 hover:bg-neutral-800"
                // v0.6.1 hotfix layer 3: autoFocus gives the modal a
                // real focus target on first render. Without this,
                // Tauri's Webview2 may leave focus on the toggle
                // button BEHIND the modal — Esc keypresses then
                // never bubble through a path the dialog wrapper
                // can intercept.
                autoFocus
              >
                {t("action.cancel")}
              </button>
              <button
                type="button"
                onClick={confirmEnable}
                className="px-3 py-1.5 text-xs rounded bg-emerald-700 hover:bg-emerald-600 text-white"
              >
                {t("settings.privacy_consent_enable_button")}
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
  );
}

// v0.7.0 — Claude hook installer. Reads current install status on
// mount and on every successful Install click. Three states:
//   * not_installed → button "Install Claude hook"
//   * installed_matches_binary → "✓ Hook installed" (disabled-looking)
//     + secondary "Reinstall" button
//   * installed_stale_binary → "⚠ Hook points to old install" + button
//     "Update path"
//
// The Tauri command does atomic settings.json edits. Idempotent. The
// install path is derived from std::env::current_exe() so users who
// move the install (e.g. uninstall + reinstall to a different dir)
// can re-run to update.
type HookStatus = "not_installed" | "installed_matches_binary" | "installed_stale_binary";

type InstallResult =
  | { kind: "installed"; settings_path: string }
  | { kind: "already_up_to_date"; settings_path: string }
  | { kind: "updated"; settings_path: string; previous: string };

function ClaudeHookInstaller() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<HookStatus | null>(null);
  const [installing, setInstalling] = useState(false);
  const [lastResult, setLastResult] = useState<InstallResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshStatus = useCallback(async () => {
    try {
      const s = await invoke<HookStatus>("get_claude_hook_status");
      setStatus(s);
    } catch (e: any) {
      // Resolution failures (no home dir etc.) are rare; surface
      // but don't block the rest of the Privacy section.
      setError(t("settings.hook_install_status_failed", { err: String(e) }));
    }
  }, [t]);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  const doInstall = async () => {
    setInstalling(true);
    setError(null);
    try {
      const result = await invoke<InstallResult>("install_claude_hook");
      setLastResult(result);
      await refreshStatus();
    } catch (e: any) {
      setError(t("settings.hook_install_failed", { err: String(e) }));
    } finally {
      setInstalling(false);
    }
  };

  const isInstalled = status === "installed_matches_binary";
  const isStale = status === "installed_stale_binary";

  return (
    <div className="border-t border-neutral-800 pt-3 space-y-2">
      <div>
        <h3 className="text-xs font-semibold text-neutral-300">
          {t("settings.hook_install_heading")}
        </h3>
        <p className="text-xs text-neutral-500 mt-0.5">
          {t("settings.hook_install_body")}
        </p>
      </div>
      {/* Status pill — visible at all times so the user can see
          install state at a glance */}
      <div className="text-xs">
        {status === null ? (
          <span className="text-neutral-500">{t("misc.loading")}</span>
        ) : isInstalled ? (
          <span className="text-emerald-400">
            ✓ {t("settings.hook_install_status_ok")}
          </span>
        ) : isStale ? (
          <span className="text-amber-400">
            ⚠ {t("settings.hook_install_status_stale")}
          </span>
        ) : (
          <span className="text-neutral-500">
            {t("settings.hook_install_status_missing")}
          </span>
        )}
      </div>
      <div className="flex gap-2">
        <button
          type="button"
          onClick={doInstall}
          disabled={installing}
          className={`px-3 py-1.5 text-xs rounded ${
            isInstalled
              ? "border border-neutral-700 hover:bg-neutral-800 text-neutral-300"
              : "bg-emerald-700 hover:bg-emerald-600 text-white"
          } disabled:opacity-50`}
        >
          {installing
            ? t("settings.hook_install_installing")
            : isInstalled
              ? t("settings.hook_install_reinstall_button")
              : isStale
                ? t("settings.hook_install_update_button")
                : t("settings.hook_install_install_button")}
        </button>
      </div>
      {lastResult && !error && (
        <div className="px-3 py-2 rounded bg-emerald-950/40 border border-emerald-900 text-emerald-200 text-xs space-y-0.5">
          <div>
            {lastResult.kind === "installed"
              ? t("settings.hook_install_done_installed")
              : lastResult.kind === "already_up_to_date"
                ? t("settings.hook_install_done_unchanged")
                : t("settings.hook_install_done_updated")}
          </div>
          <div className="font-mono text-[10px] text-emerald-200/60 break-all">
            {lastResult.settings_path}
          </div>
        </div>
      )}
      {error && (
        <div className="px-3 py-2 rounded bg-red-950/60 border border-red-900 text-red-200 text-xs">
          {error}
        </div>
      )}
    </div>
  );
}

// v0.6.2 — managed sessions are now actionable. Per-row state:
//   - "idle" — buttons visible, no in-flight command
//   - "prompting" — inline text input expanded, waiting for user
//     to type + submit
//   - "sending"/"stopping"/"interrupting" — RPC in flight
//   - "error" — show error toast under the row, revert to idle
//
// Send/Stop/Interrupt only enabled for status === "running"; pending
// sessions can still receive a Stop (cancel the start) but Send and
// Interrupt are gated. Stopped/errored rows hide their buttons.
type RowMode =
  | { kind: "idle" }
  | { kind: "prompting"; draft: string }
  | { kind: "sending" }
  | { kind: "stopping" }
  | { kind: "interrupting" }
  | { kind: "error"; message: string };

/// v0.9.2 — Spawn-session launcher (restored from v0.8.0; no UI
/// changes since v0.8.0). Renders as a small "+ Start new session"
/// button; clicking opens an inline dialog with a cwd field, optional
/// label, and a Start submit. On success calls `onSpawned` so the
/// parent refreshes the session list.
///
/// Privacy posture: full cwd path stays LOCAL. Server only sees:
///   - cwd_basename (last path segment, ≤255 chars; displayable)
///   - cwd_hmac (HMAC-SHA256 of the full path, 32-byte hex)
/// The HMAC uses a per-user secret stored in the OS keychain.
function SpawnSessionLauncher({ onSpawned }: { onSpawned: () => Promise<void> }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [cwd, setCwd] = useState("");
  const [label, setLabel] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reset = () => {
    setCwd("");
    setLabel("");
    setError(null);
    setSubmitting(false);
  };

  const close = () => {
    setOpen(false);
    reset();
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const trimmedCwd = cwd.trim();
    if (!trimmedCwd) return;
    setSubmitting(true);
    setError(null);
    try {
      await invoke<string>("request_remote_session_start", {
        args: {
          cwd: trimmedCwd,
          cwd_basename: null,
          client_label: label.trim() || null,
          provider: "claude",
        },
      });
      await onSpawned();
      close();
    } catch (err: any) {
      setError(String(err));
      setSubmitting(false);
    }
  };

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        className="px-2 py-0.5 text-[10px] rounded bg-emerald-950/60 hover:bg-emerald-900/60 border border-emerald-900 text-emerald-200"
      >
        + {t("remote.session_start_button")}
      </button>
    );
  }

  return (
    <div className="fixed inset-0 z-30 flex items-center justify-center bg-black/50 p-4">
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="spawn-session-title"
        className="w-full max-w-md rounded-lg border border-neutral-800 bg-neutral-950 p-4 space-y-3 shadow-xl"
        onKeyDown={(e) => {
          if (e.key === "Escape" && !submitting) {
            e.stopPropagation();
            close();
          }
        }}
      >
        <h2
          id="spawn-session-title"
          className="text-sm font-semibold text-neutral-200"
        >
          {t("remote.session_start_dialog_title")}
        </h2>
        <form onSubmit={handleSubmit} className="space-y-3">
          <div>
            <label
              htmlFor="spawn-cwd"
              className="block text-xs text-neutral-300 mb-1"
            >
              {t("remote.session_start_cwd_label")}
            </label>
            <input
              id="spawn-cwd"
              type="text"
              value={cwd}
              onChange={(e) => setCwd(e.target.value)}
              placeholder={t("remote.session_start_cwd_placeholder")}
              autoFocus
              required
              className="w-full px-2 py-1.5 text-xs font-mono bg-neutral-900 border border-neutral-800 rounded text-neutral-200 focus:outline-none focus:border-emerald-500"
            />
            <p className="mt-1 text-[10px] text-neutral-500 leading-snug">
              {t("remote.session_start_cwd_help")}
            </p>
          </div>
          <div>
            <label
              htmlFor="spawn-label"
              className="block text-xs text-neutral-300 mb-1"
            >
              {t("remote.session_start_label_label")}
            </label>
            <input
              id="spawn-label"
              type="text"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              placeholder={t("remote.session_start_label_placeholder")}
              className="w-full px-2 py-1.5 text-xs bg-neutral-900 border border-neutral-800 rounded text-neutral-200 focus:outline-none focus:border-emerald-500"
            />
          </div>
          <div>
            <span className="block text-xs text-neutral-300 mb-1">
              {t("remote.session_start_provider_label")}
            </span>
            {/* v0.9.2: Claude-only provider for now. Codex / shell
                are gated on Mac's v1.14+ Multi-CLI design. */}
            <div className="px-2 py-1.5 text-xs bg-neutral-900 border border-neutral-800 rounded text-neutral-300">
              {t("remote.session_start_provider_claude")}
            </div>
          </div>
          {error && (
            <div className="px-2 py-1.5 rounded bg-red-950/60 border border-red-900 text-red-200 text-[10px]">
              {t("remote.session_start_failed", { err: error })}
            </div>
          )}
          <div className="flex gap-2 justify-end pt-1">
            <button
              type="button"
              onClick={close}
              disabled={submitting}
              className="px-3 py-1 text-xs rounded border border-neutral-700 hover:bg-neutral-800 text-neutral-300 disabled:opacity-40"
            >
              {t("action.cancel")}
            </button>
            <button
              type="submit"
              disabled={submitting || cwd.trim().length === 0}
              className="px-3 py-1 text-xs rounded bg-emerald-700 hover:bg-emerald-600 text-white disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {submitting
                ? t("remote.session_start_processing")
                : t("remote.session_start_submit")}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}

function RemoteSessionsSection({
  sessions,
  enabled,
  onActionDone,
}: {
  sessions: RemoteSession[];
  enabled: boolean;
  /** Called after a Send/Stop/Interrupt completes (success OR failure)
   *  so the parent can refresh the list to pick up the new status. */
  onActionDone: () => Promise<void>;
}) {
  const { t } = useTranslation();
  // Per-row mode keyed by session id. Map vs object for cleaner
  // immutable updates.
  const [rowModes, setRowModes] = useState<Map<string, RowMode>>(new Map());

  // v0.6.2 Gemini post-impl P2: prune row-mode entries for sessions
  // that are no longer in the parent's list. Without this, a long-
  // running app would slowly accumulate state for stopped/expired
  // sessions across many polls. Bound IS small (~tens of sessions
  // per session-lifetime) so the leak is O(KB), not OOM-shaped, but
  // still worth cleaning up — keeps the state diff readable in
  // devtools and prevents stale "error" rows from re-appearing if
  // a session ID gets recycled.
  useEffect(() => {
    setRowModes((prev) => {
      const liveIds = new Set(sessions.map((s) => s.id));
      let changed = false;
      const next = new Map<string, RowMode>();
      for (const [id, mode] of prev) {
        if (liveIds.has(id)) {
          next.set(id, mode);
        } else {
          changed = true;
        }
      }
      return changed ? next : prev;
    });
  }, [sessions]);

  // Hidden when Remote Control is off (Gemini v0.6.0 Q2: read-only
  // section needs explicit context — better to omit when feature is
  // off than to render an empty card).
  if (!enabled) return null;
  // Status label routed through i18n (Gemini post-impl P2.2 — raw
  // English status was rendered to zh-CN/ja users). Falls back to
  // the raw value for unknown classes the server may emit later.
  const statusLabel = (status: string): string => {
    const key = `remote.session_status_${status}`;
    const translated = t(key);
    return translated === key ? status : translated;
  };

  const setMode = (id: string, mode: RowMode) => {
    setRowModes((m) => {
      const next = new Map(m);
      next.set(id, mode);
      return next;
    });
  };

  const sendCommand = async (
    sessionId: string,
    kind: "prompt" | "stop" | "interrupt",
    payload: string | null,
    inflightMode: "sending" | "stopping" | "interrupting"
  ) => {
    setMode(sessionId, { kind: inflightMode });
    try {
      await invoke("send_remote_session_command", {
        sessionId,
        kind,
        payload,
      });
      // Success: clear row mode and refresh parent. The next list
      // poll will reflect status changes (running → stopped on Stop;
      // last_event_at bump on Send).
      setMode(sessionId, { kind: "idle" });
      await onActionDone();
    } catch (e: any) {
      setMode(sessionId, { kind: "error", message: String(e) });
      await onActionDone();
    }
  };

  return (
    <section className="rounded-lg border border-neutral-800 bg-neutral-900/40 p-4 space-y-2">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-semibold text-neutral-300">
          {t("remote.sessions_heading")}
        </h3>
        {/* v0.9.2 — "+ Start new session" CTA. Restored from v0.8.0
            (gated to enabled === true at the section level above). */}
        <SpawnSessionLauncher onSpawned={onActionDone} />
      </div>
      {sessions.length === 0 ? (
        <div className="text-xs text-neutral-500 italic py-2">
          {t("remote.sessions_empty")}
        </div>
      ) : (
        <ul className="space-y-1.5">
          {sessions.map((s) => {
            const mode = rowModes.get(s.id) ?? { kind: "idle" };
            const isRunning = s.status === "running";
            const isPending = s.status === "pending";
            const isTerminal =
              s.status === "stopped" || s.status === "errored";
            const inFlight =
              mode.kind === "sending" ||
              mode.kind === "stopping" ||
              mode.kind === "interrupting";
            return (
              <li
                key={s.id}
                className="text-xs text-neutral-300 px-2 py-1.5 rounded bg-neutral-950/40 border border-neutral-800 space-y-1.5"
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="min-w-0 flex-1 truncate">
                    {s.device_name ?? t("remote.row_unknown_device")} ·{" "}
                    {s.provider} ·{" "}
                    <span className="font-mono">{s.cwd_basename}</span>
                  </span>
                  <span
                    className={
                      isRunning
                        ? "text-emerald-400 text-[10px]"
                        : isPending
                          ? "text-amber-400 text-[10px]"
                          : "text-neutral-500 text-[10px]"
                    }
                  >
                    {statusLabel(s.status)}
                  </span>
                </div>
                {/* Action buttons — hidden for terminal-state rows.
                    Send/Interrupt enabled only when running; Stop
                    works for both pending and running (Stop on
                    pending = cancel-the-start). */}
                {!isTerminal && mode.kind !== "prompting" && (
                  <div className="flex gap-1">
                    <button
                      type="button"
                      disabled={!isRunning || inFlight}
                      onClick={() =>
                        setMode(s.id, { kind: "prompting", draft: "" })
                      }
                      className="px-2 py-0.5 text-[10px] rounded bg-emerald-950/60 hover:bg-emerald-900/60 border border-emerald-900 text-emerald-200 disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                      {mode.kind === "sending"
                        ? t("remote.session_sending")
                        : t("remote.session_send_button")}
                    </button>
                    <button
                      type="button"
                      disabled={inFlight}
                      onClick={() =>
                        sendCommand(s.id, "stop", null, "stopping")
                      }
                      className="px-2 py-0.5 text-[10px] rounded bg-red-950/60 hover:bg-red-900/60 border border-red-900 text-red-200 disabled:opacity-40 disabled:cursor-not-allowed"
                    >
                      {mode.kind === "stopping"
                        ? t("remote.session_stopping")
                        : t("remote.session_stop_button")}
                    </button>
                    <button
                      type="button"
                      disabled={!isRunning || inFlight}
                      onClick={() =>
                        sendCommand(s.id, "interrupt", null, "interrupting")
                      }
                      className="px-2 py-0.5 text-[10px] rounded bg-amber-950/60 hover:bg-amber-900/60 border border-amber-900 text-amber-200 disabled:opacity-40 disabled:cursor-not-allowed"
                      title={t("remote.session_interrupt_tooltip")}
                    >
                      {mode.kind === "interrupting"
                        ? t("remote.session_interrupting")
                        : t("remote.session_interrupt_button")}
                    </button>
                  </div>
                )}
                {/* Inline prompt input — expands when user clicks Send.
                    Trims on submit; empty disables the submit button. */}
                {mode.kind === "prompting" && (
                  <form
                    onSubmit={(e) => {
                      e.preventDefault();
                      const text = mode.draft.trim();
                      if (!text) return;
                      sendCommand(s.id, "prompt", text, "sending");
                    }}
                    className="space-y-1"
                  >
                    <textarea
                      value={mode.draft}
                      onChange={(e) =>
                        setMode(s.id, {
                          kind: "prompting",
                          draft: e.target.value,
                        })
                      }
                      rows={2}
                      maxLength={8192}
                      placeholder={t("remote.session_prompt_placeholder")}
                      className="w-full px-2 py-1 text-xs font-mono bg-neutral-950 border border-emerald-900/60 rounded text-neutral-200 focus:outline-none focus:border-emerald-500"
                      autoFocus
                      // Esc cancels the prompt mode, Enter submits
                      // (Shift+Enter for newline). Matches typical
                      // chat-input UX.
                      onKeyDown={(e) => {
                        if (e.key === "Escape") {
                          setMode(s.id, { kind: "idle" });
                          e.stopPropagation();
                        } else if (e.key === "Enter" && !e.shiftKey) {
                          e.preventDefault();
                          const text = mode.draft.trim();
                          if (text) {
                            sendCommand(s.id, "prompt", text, "sending");
                          }
                        }
                      }}
                    />
                    <div className="flex gap-1 justify-end">
                      <button
                        type="button"
                        onClick={() => setMode(s.id, { kind: "idle" })}
                        className="px-2 py-0.5 text-[10px] rounded border border-neutral-700 hover:bg-neutral-800 text-neutral-300"
                      >
                        {t("action.cancel")}
                      </button>
                      <button
                        type="submit"
                        disabled={mode.draft.trim().length === 0}
                        className="px-2 py-0.5 text-[10px] rounded bg-emerald-700 hover:bg-emerald-600 text-white disabled:opacity-40 disabled:cursor-not-allowed"
                      >
                        {t("remote.session_prompt_submit")}
                      </button>
                    </div>
                  </form>
                )}
                {mode.kind === "error" && (
                  <div className="px-2 py-1 rounded bg-red-950/60 border border-red-900 text-red-200 text-[10px]">
                    {t("remote.action_failed", { err: mode.message })}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
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

/// v0.9.2 — Agent loop diagnostic. Returns null when the agent isn't
/// running (not paired, in recovery mode, or kill-switched via env var).
type AgentDiagnostic = {
  running_count: number;
  lifetime_count: number;
  last_tick_seconds_ago: number | null;
};

/// v0.9.2 — Render the agent diagnostic block. Three lines: running
/// count, lifetime count, last-tick age. Renders a single "not
/// running" line when the agent is null (not paired / recovery mode
/// / env kill-switch). Restored from v0.8.0 with no UI changes.
function AgentDiagnosticBlock({ diag }: { diag: AgentDiagnostic | null }) {
  const { t } = useTranslation();
  if (diag === null) {
    return (
      <div className="text-xs text-neutral-500 italic">
        {t("remote.agent_status_not_running")}
      </div>
    );
  }
  const lastTickLine = (() => {
    if (diag.last_tick_seconds_ago === null) {
      return t("remote.agent_status_never_ticked");
    }
    const fakeTs = new Date(
      Date.now() - diag.last_tick_seconds_ago * 1000
    ).toISOString();
    const parts = formatRelativeShortParts(fakeTs);
    if (!parts) return t("remote.agent_status_never_ticked");
    const age = t(`time.unit_${parts.unit}` as const, { count: parts.value });
    return t("remote.agent_status_last_tick", { age });
  })();
  return (
    <div className="space-y-0.5 text-xs text-neutral-400 border-l-2 border-neutral-800 pl-3">
      <div className="font-medium text-neutral-300">
        {t("remote.agent_status_heading")}
      </div>
      <div>
        {t("remote.agent_status_running", { count: diag.running_count })}
      </div>
      <div>
        {t("remote.agent_status_lifetime", { count: diag.lifetime_count })}
      </div>
      <div>{lastTickLine}</div>
    </div>
  );
}

/// v0.9.3 — Save diagnostic bundle button. Calls the
/// `save_diagnostic_bundle` Tauri command which zips logs +
/// crash-history + diagnostic snapshot to `~/Downloads/`. Three
/// states: idle / saving / done (shows the path for ~6s).
function SaveDiagnosticBundleButton() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<
    "idle" | "saving" | { kind: "done"; path: string } | { kind: "error"; msg: string }
  >("idle");

  const mountedRef = useRef(true);
  useEffect(() => () => { mountedRef.current = false; }, []);

  async function save() {
    if (status === "saving") return;
    setStatus("saving");
    try {
      const result = await invoke<{ path: string; entries: string[] }>(
        "save_diagnostic_bundle"
      );
      if (mountedRef.current) {
        setStatus({ kind: "done", path: result.path });
        setTimeout(() => {
          if (mountedRef.current) setStatus("idle");
        }, 6000);
      }
    } catch (e: any) {
      if (mountedRef.current) {
        setStatus({ kind: "error", msg: String(e) });
        setTimeout(() => {
          if (mountedRef.current) setStatus("idle");
        }, 6000);
      }
    }
  }

  const label = (() => {
    if (status === "idle") return t("settings.about_save_bundle_button");
    if (status === "saving") return `… ${t("settings.about_save_bundle_saving")}`;
    if (typeof status === "object" && status.kind === "done") {
      return `✓ ${t("settings.about_save_bundle_done")}`;
    }
    if (typeof status === "object" && status.kind === "error") {
      return `✘ ${t("settings.about_save_bundle_failed")}`;
    }
    return "";
  })();

  return (
    <button
      onClick={save}
      disabled={status === "saving"}
      className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-60 disabled:cursor-not-allowed"
      title={
        typeof status === "object" && status.kind === "done"
          ? status.path
          : t("settings.about_save_bundle_tooltip")
      }
    >
      {label}
    </button>
  );
}

function AboutSection({ paired }: { paired: boolean }) {
  const { t } = useTranslation();
  const [diag, setDiag] = useState<DiagnosticSnapshot | null>(null);
  const [agentDiag, setAgentDiag] = useState<AgentDiagnostic | null>(null);
  const [copied, setCopied] = useState(false);
  // v0.4.22 — Sentry diagnostic emit. The desktop project's lifetime
  // issue count was 0 since instrumentation went in (2026-04-22), but
  // that's ambiguous: it could mean "no panics" or "DSN never
  // reached server." This button collapses the ambiguity — a
  // verified test event in the dashboard means the chain is live.
  const [sentryStatus, setSentryStatus] = useState<"idle" | "sending" | "sent">("idle");
  // Mount tracker for sendSentryTest — guards both the post-await
  // setSentryStatus("sent") and the 4s setTimeout reset against
  // unmount-during-flight (Gemini v0.4.22 P3).
  const mountedRef = useRef(true);
  useEffect(() => () => { mountedRef.current = false; }, []);

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

  // v0.9.2 — agent diagnostic polling. Polls every 5 s while the
  // About panel is mounted so users see live counters update. The
  // Tauri command returns null when the agent loop isn't running
  // (not paired / recovery mode / CLI_PULSE_DISABLE_REMOTE_AGENT
  // env set) — `AgentDiagnosticBlock` renders a single "not running"
  // line for null. Restored from v0.8.0 (was removed in v0.8.1).
  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      invoke<AgentDiagnostic | null>("agent_diagnostic")
        .then((d) => {
          if (!cancelled) setAgentDiag(d);
        })
        .catch((e) => {
          if (!cancelled) console.warn("agent_diagnostic failed", e);
        });
    };
    refresh();
    const id = window.setInterval(refresh, 5_000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
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

  async function sendSentryTest() {
    if (sentryStatus !== "idle") return;
    setSentryStatus("sending");
    try {
      await invoke("emit_test_sentry_event");
      // Guard against unmount-during-await — Gemini v0.4.22 P3.
      if (mountedRef.current) {
        setSentryStatus("sent");
        // 4s: long enough to read the confirmation, short enough that
        // repeated clicks (after verifying in dashboard) aren't blocked.
        setTimeout(() => {
          if (mountedRef.current) setSentryStatus("idle");
        }, 4000);
      }
    } catch (e) {
      console.warn("emit_test_sentry_event failed", e);
      if (mountedRef.current) setSentryStatus("idle");
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
          {/* v0.9.2 — Agent diagnostic block. Lives in About so it's
              co-located with the rest of the platform diagnostic
              info; users copying the diagnostic block get the agent
              counters too if they're surfacing a Sessions issue.
              Restored from v0.8.0 (was removed in v0.8.1). */}
          <AgentDiagnosticBlock diag={agentDiag} />
          <div className="flex items-center gap-2 flex-wrap">
            <button
              onClick={copyDiag}
              className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800"
            >
              {copied ? `✓ ${t("settings.about_copied")}` : t("settings.about_copy_diagnostics")}
            </button>
            <button
              onClick={sendSentryTest}
              disabled={sentryStatus !== "idle"}
              className="px-3 py-1.5 text-xs rounded-md border border-neutral-700 hover:bg-neutral-800 disabled:opacity-60 disabled:cursor-not-allowed"
              title={t("settings.about_sentry_test_tooltip")}
            >
              {sentryStatus === "sent"
                ? `✓ ${t("settings.about_sentry_test_sent")}`
                : sentryStatus === "sending"
                  ? `… ${t("settings.about_sentry_test_sending")}`
                  : t("settings.about_sentry_test_button")}
            </button>
            {/* v0.9.3 — Save diagnostic bundle to ~/Downloads/.
                One-click triage helper: zips cli-pulse.log,
                remote-hook.log, crash-history.jsonl, diagnostic
                snapshot, and version info. The user attaches the
                zip to a bug report deliberately. */}
            <SaveDiagnosticBundleButton />
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

// v0.5.6 — push fully-localized tray copy to the backend so the
// tray menu re-renders in the user's chosen app language without
// waiting for the next 120s refresh-loop tick. Called from:
//   1. The LanguageSection's onChange handler (after setLang)
//   2. App-level mount, once at startup (so the tray reflects the
//      detected language even before the user touches the
//      switcher)
//
// Failure is non-fatal: tray.install() may have failed (Linux
// without libayatana-appindicator3) and there's nothing to update,
// in which case force_tray_menu_refresh is a no-op on the backend
// side. Any other error gets swallowed via .catch — a tray
// out-of-sync isn't worth crashing the UI flow.
function pushTrayCopyFromI18n(t: (key: string) => string): Promise<void> {
  return invoke<void>("force_tray_menu_refresh", {
    copy: {
      headerLabel: t("tray.header_label"),
      monthSoFarTemplate: t("tray.month_so_far_template"),
      forecastTemplate: t("tray.forecast_template"),
      syncedAgoTemplate: t("tray.synced_ago_template"),
      syncedNever: t("tray.synced_never"),
      notPaired: t("tray.not_paired"),
      noData: t("tray.no_data"),
      openLabel: t("tray.open_label"),
      quitLabel: t("tray.quit_label"),
    },
  }).catch((e: any) => {
    // Tray install may have failed on this platform; not fatal.
    console.debug("force_tray_menu_refresh failed (non-fatal):", e);
  });
}

function LanguageSection() {
  const { t, i18n } = useTranslation();
  const current = (i18n.language || "en") as LangCode;
  // We don't need a per-section tray push here anymore — the
  // App-level `useEffect([i18n.language])` watches i18next's
  // language directly and re-pushes whenever it changes, so all
  // setLang() invocations (this dropdown, future programmatic
  // calls, etc.) flow through the single effect. Per Gemini
  // 3.1 Pro v0.5.6 P1: a per-handler push using the closure-
  // captured `t` would resolve against the OLD language because
  // React hasn't re-rendered yet at the moment `await setLang`
  // returns. The useEffect path uses `i18n.t` (live translator)
  // and fires after the re-render commits, sidestepping the issue.
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
  // v0.5.3 — uses the App-level `UpdaterState` shared type instead
  // of the inline duplicate that lived here through v0.5.2.
  state: UpdaterState;
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
    case "error": {
      // v0.9.0 — categorize the raw error so users get an actionable
      // message instead of "os error 3". Maps common Tauri-updater
      // failure shapes onto the new `updater.error_<category>` keys
      // (which include retry hints and a "Download manually" link).
      // The raw error stays in the message via {{error}} interpolation
      // for bug-report copy-paste.
      const cat = categorizeUpdateError(state.text);
      const truncated =
        state.text.length > 160 ? state.text.slice(0, 160) + "…" : state.text;
      return (
        <div className="space-y-1">
          <div className="text-sm text-red-300">
            {t(cat.key, { error: truncated })}
          </div>
          {cat.showManualDownload && (
            <a
              href="https://github.com/JasonYeYuhe/cli-pulse-desktop/releases/latest"
              target="_blank"
              rel="noreferrer"
              className="text-xs text-emerald-400 hover:underline"
            >
              {t("updater.error_manual_download")} ↗
            </a>
          )}
        </div>
      );
    }
  }
}

/// v0.9.0 — categorize a raw tauri-plugin-updater error into one of
/// a handful of buckets, each mapped to an actionable i18n key. The
/// raw error string is preserved in the rendered message so bug
/// reports can quote the OS error code; the category just decides
/// the explanation text + whether to show a manual-download link.
///
/// Patterns we recognize (all case-insensitive substring match):
///   - `os error 3` / `not found` / `path not found` → path_not_found
///     (the v0.5.3 per-user-NSIS bug; manual download recommended)
///   - `os error 5` / `denied` / `permission` → permissions
///   - `network` / `connection` / `dns` / `timeout` / `os error 10` /
///     `unreachable` / `tls` / `cert` → network
///   - `os error 112` / `disk full` / `space` → disk_full
///   - `signature` / `invalid` / `corrupt` → signature
///   - everything else → unknown
function categorizeUpdateError(raw: string): {
  key: string;
  showManualDownload: boolean;
} {
  const s = raw.toLowerCase();
  if (
    s.includes("os error 3") ||
    s.includes("path not found") ||
    s.includes("the system cannot find")
  ) {
    return {
      key: "updater.error_path_not_found",
      showManualDownload: true,
    };
  }
  if (
    s.includes("os error 5") ||
    s.includes("permission denied") ||
    s.includes("access is denied")
  ) {
    return { key: "updater.error_permissions", showManualDownload: true };
  }
  if (
    s.includes("network") ||
    s.includes("connection") ||
    s.includes("dns") ||
    s.includes("timeout") ||
    s.includes("os error 10") ||
    s.includes("unreachable") ||
    s.includes("tls") ||
    s.includes("certificate")
  ) {
    return { key: "updater.error_network", showManualDownload: false };
  }
  if (
    s.includes("os error 112") ||
    s.includes("disk full") ||
    s.includes("no space")
  ) {
    return { key: "updater.error_disk_full", showManualDownload: false };
  }
  if (
    s.includes("signature") ||
    s.includes("invalid update") ||
    s.includes("corrupt")
  ) {
    return { key: "updater.error_signature", showManualDownload: false };
  }
  return { key: "updater.error_unknown", showManualDownload: true };
}

// v0.5.5 — Activity Timeline chart. Renders a 24h horizontal bar
// chart of session activity by provider lane, sourced from the
// `sessions` table via `get_sessions_history`. Cross-device view
// (NOT the local-process snapshot the row table below shows).
//
// Plan/review highlights baked in:
//   - DATA SOURCE FIX (Codex P1): use `sessions` table, NOT
//     `list_sessions`. The latter is a current-process snapshot of
//     this device, capped at 12 most-active processes; would render
//     a chart that looks plausible but draws the wrong dataset.
//   - LANE HEIGHT (Gemini decision): 24px per lane × 6 providers
//     ≈ 144px total. The v1 plan's 240px / 40px-per-lane was too
//     chunky for desktop and made empty lanes feel like "broken
//     layout" to the user.
//   - MEMO KEY (Gemini P2): use the full join of
//     `${id}-${last_active_at}` across all sessions, NOT the v1
//     plan's `length + sessions[0]?.last_active_at`. The latter
//     only catches additions and updates to the FIRST session;
//     non-first session edits would silently miss the recompute.
//
// The 6 lane order (top-to-bottom) is: Claude / Codex / Cursor /
// Copilot / Gemini / OpenRouter. Sessions whose `provider` doesn't
// match any known lane fall into a 7th "Other" lane at the bottom.
type SessionHistoryRow = {
  id: string;
  provider: string;
  project: string | null;
  started_at: string;
  last_active_at: string;
  estimated_cost: number | null;
  total_usage: number | null;
  requests: number | null;
};

const TIMELINE_LANES = [
  { provider: "claude", labelKey: "providers.claude_label", color: "#d97706" }, // amber
  { provider: "codex", labelKey: "providers.codex_label", color: "#10b981" }, // emerald
  { provider: "cursor", labelKey: "providers.cursor_label", color: "#06b6d4" }, // cyan
  { provider: "copilot", labelKey: "providers.copilot_label", color: "#8b5cf6" }, // violet
  { provider: "gemini", labelKey: "providers.gemini_label", color: "#3b82f6" }, // blue
  {
    provider: "openrouter",
    labelKey: "providers.openrouter_label",
    color: "#ec4899",
  }, // pink
] as const;

const TIMELINE_OTHER_COLOR = "#737373"; // neutral-500
const TIMELINE_LANE_HEIGHT = 24;
const TIMELINE_LABEL_WIDTH = 80;
const TIMELINE_TICK_HEIGHT = 16;
const TIMELINE_HOURS = 24;
const TIMELINE_POLL_MS = 30_000;

type TimelineState =
  | { kind: "loading" }
  | { kind: "loaded"; rows: SessionHistoryRow[]; fetchedAt: Date }
  | { kind: "stale"; rows: SessionHistoryRow[]; fetchedAt: Date; error: string }
  | { kind: "empty" }
  | { kind: "error"; error: string };

function ActivityTimelineChart() {
  const { t } = useTranslation();
  const [state, setState] = useState<TimelineState>({ kind: "loading" });
  const [hoveredId, setHoveredId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const rows = await invoke<SessionHistoryRow[]>("get_sessions_history", {
        hours: TIMELINE_HOURS,
      });
      if (rows.length === 0) {
        setState({ kind: "empty" });
      } else {
        setState({ kind: "loaded", rows, fetchedAt: new Date() });
      }
    } catch (e: any) {
      const msg = String(e);
      // Stale-data hint: if a previous fetch succeeded and this one
      // failed, keep showing the old data with a banner. Per the
      // v0.5.3 RiskSignalsCard pattern: fully clearing to error wipes
      // useful context the user already had.
      setState((cur) => {
        if (cur.kind === "loaded" || cur.kind === "stale") {
          return {
            kind: "stale",
            rows: cur.rows,
            fetchedAt: cur.fetchedAt,
            error: msg,
          };
        }
        return { kind: "error", error: msg };
      });
    }
  }, []);

  // Initial fetch + 30s poll while mounted. The poll cadence is
  // separate from the parent's 10s local-process snapshot — this
  // chart's data source is server-side and only changes on each
  // device's helper_sync (every 2 min), so 30s is plenty fresh.
  useEffect(() => {
    refresh();
    const id = setInterval(refresh, TIMELINE_POLL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  // Stable rows reference for the memo key. v0.5.5 reviewer P2 fix:
  // using `length + first.last_active_at` misses non-first updates;
  // using the full join is O(n) per cycle but n ≤ 1000 (server cap)
  // so it's free in practice.
  const rows = state.kind === "loaded" || state.kind === "stale" ? state.rows : [];
  const memoKey = useMemo(
    () => rows.map((r) => `${r.id}-${r.last_active_at}`).join(","),
    [rows]
  );

  // Time anchor for the chart's "now" — must change every poll cycle
  // even when the row set is unchanged, otherwise the bars freeze in
  // place and stop sliding left as time advances. Per Gemini 3.1 Pro
  // v0.5.5 P1: `Date.now()` inside `useMemo` keyed only on `memoKey`
  // would trap the time evaluation from the last row-set update; an
  // idle stretch with no new sessions would make the chart visibly
  // stale even though the header reads "refreshed 30 s ago." Tying
  // the memo to fetchedAt invalidates it once per poll regardless of
  // row contents.
  const fetchedAtMs =
    state.kind === "loaded" || state.kind === "stale"
      ? state.fetchedAt.getTime()
      : 0;

  // Compute layout once per (memoKey, fetchedAt). Each row becomes a
  // horizontal bar in its provider's lane; x position from started_at,
  // width from (last_active_at - started_at). Sessions older than the
  // window are clipped at the left edge; sessions whose started_at
  // falls before the window but last_active_at is inside (the user
  // had a session running across the window boundary) get rendered
  // from the left edge to last_active_at.
  //
  // Z-order intent: SVG paints in document order (first child is
  // bottom, last child is top). The PostgREST GET returns
  // `started_at.desc` (newest first); we reverse here so newest bars
  // render LAST → end up on top of overlapping older bars when
  // multiple sessions share a lane during the same minute. Per Gemini
  // 3.1 Pro v0.5.5 P1: without the reverse, newest bars would be
  // hidden behind older ones (the comment in supabase.rs claimed
  // intent was already met but the SVG paint order contradicted it).
  const layout = useMemo(() => {
    const now = fetchedAtMs > 0 ? fetchedAtMs : Date.now();
    const windowStart = now - TIMELINE_HOURS * 3600 * 1000;
    const allLanes = [
      ...TIMELINE_LANES.map((l) => l.provider as string),
      "other",
    ];
    const bars: Array<{
      row: SessionHistoryRow;
      laneIndex: number;
      laneLabel: string;
      x: number;
      width: number;
      color: string;
      clippedAtStart: boolean;
    }> = [];
    // Iterate oldest-first so the array's tail (newest) renders on
    // top in SVG paint order — fixes the v0.5.5 P1 Z-order intent.
    for (let i = rows.length - 1; i >= 0; i--) {
      const row = rows[i];
      const startedMs = Date.parse(row.started_at);
      const lastMs = Date.parse(row.last_active_at);
      if (Number.isNaN(startedMs) || Number.isNaN(lastMs)) continue;
      if (lastMs < windowStart) continue; // entirely before the window
      const laneIdx = TIMELINE_LANES.findIndex(
        (l) => l.provider === row.provider.toLowerCase()
      );
      const laneIndex = laneIdx === -1 ? TIMELINE_LANES.length : laneIdx;
      const color =
        laneIdx === -1 ? TIMELINE_OTHER_COLOR : TIMELINE_LANES[laneIdx].color;
      const effectiveStart = Math.max(startedMs, windowStart);
      const xRatio = (effectiveStart - windowStart) / (TIMELINE_HOURS * 3600 * 1000);
      const widthRatio =
        Math.max(lastMs - effectiveStart, 60_000) / (TIMELINE_HOURS * 3600 * 1000);
      bars.push({
        row,
        laneIndex,
        laneLabel: allLanes[laneIndex],
        x: xRatio,
        width: widthRatio,
        color,
        clippedAtStart: startedMs < windowStart,
      });
    }
    return bars;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [memoKey, fetchedAtMs]);

  const totalLanes = TIMELINE_LANES.length + 1; // +1 for "other"
  const chartHeight = totalLanes * TIMELINE_LANE_HEIGHT + TIMELINE_TICK_HEIGHT;

  return (
    <section className="rounded-lg border border-neutral-800 bg-neutral-900/40 p-4 space-y-2">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-semibold text-neutral-300">
          {t("sessions.timeline_title")}
        </h3>
        {(state.kind === "loaded" || state.kind === "stale") && (
          <span className="text-xs text-neutral-500">
            {t("sessions.timeline_last_refresh", {
              time: state.fetchedAt.toLocaleTimeString(),
            })}
          </span>
        )}
      </div>
      {state.kind === "stale" && (
        <div className="text-xs text-amber-400">
          {t("sessions.timeline_stale", { error: state.error })}
        </div>
      )}
      {state.kind === "loading" && (
        <div className="text-xs text-neutral-500 italic">
          {t("sessions.timeline_loading")}
        </div>
      )}
      {state.kind === "empty" && (
        <div className="text-xs text-neutral-500 italic py-4 text-center">
          {t("sessions.timeline_empty")}
        </div>
      )}
      {state.kind === "error" && (
        <div className="text-xs text-red-400">
          {t("sessions.timeline_failed", { error: state.error })}
        </div>
      )}
      {(state.kind === "loaded" || state.kind === "stale") && (
        <div className="relative">
          <svg
            viewBox={`0 0 1000 ${chartHeight}`}
            preserveAspectRatio="none"
            className="w-full"
            style={{ height: chartHeight, display: "block" }}
            role="img"
            aria-label={t("sessions.timeline_aria")}
          >
            {/* Provider-lane backgrounds + labels */}
            {[...TIMELINE_LANES, null].map((lane, i) => (
              <g key={i}>
                <rect
                  x={TIMELINE_LABEL_WIDTH}
                  y={i * TIMELINE_LANE_HEIGHT}
                  width={1000 - TIMELINE_LABEL_WIDTH}
                  height={TIMELINE_LANE_HEIGHT}
                  fill={i % 2 === 0 ? "#0a0a0a" : "#171717"}
                />
                <text
                  x={TIMELINE_LABEL_WIDTH - 6}
                  y={i * TIMELINE_LANE_HEIGHT + TIMELINE_LANE_HEIGHT / 2 + 4}
                  fill="#a3a3a3"
                  fontSize="10"
                  textAnchor="end"
                  fontFamily="ui-sans-serif, system-ui, sans-serif"
                >
                  {lane
                    ? t(lane.labelKey, { defaultValue: lane.provider })
                    : t("sessions.timeline_other_lane")}
                </text>
              </g>
            ))}
            {/* Hour ticks every 4h */}
            {[0, 4, 8, 12, 16, 20, 24].map((h) => {
              const x = TIMELINE_LABEL_WIDTH + ((1000 - TIMELINE_LABEL_WIDTH) * h) / 24;
              const tickY = totalLanes * TIMELINE_LANE_HEIGHT;
              return (
                <g key={h}>
                  <line
                    x1={x}
                    y1={0}
                    x2={x}
                    y2={tickY}
                    stroke="#262626"
                    strokeWidth="1"
                  />
                  <text
                    x={x}
                    y={tickY + 12}
                    fill="#737373"
                    fontSize="9"
                    textAnchor="middle"
                    fontFamily="ui-sans-serif, system-ui, sans-serif"
                  >
                    {h === 0
                      ? t("sessions.timeline_x_now_minus", { hours: 24 })
                      : h === 24
                        ? t("sessions.timeline_x_now")
                        : t("sessions.timeline_x_now_minus", { hours: 24 - h })}
                  </text>
                </g>
              );
            })}
            {/* Session bars. v0.5.5 reviewer P2 fixes:
                - tabIndex + onFocus/onBlur for keyboard / screen-reader
                  accessibility. <rect> doesn't natively receive focus,
                  so a keyboard-only user can't trigger the tooltip
                  without these (Gemini 3.1 Pro P2).
                - Tooltip text built from filtered segments instead of
                  template-string concatenation. Avoids the dangling
                  newline + bullet when one of the optional fields is
                  null but a later one isn't (Gemini P2 — would render
                  e.g. "Claude · my-project\n · 5 req" with the empty
                  cost line preserved). */}
            {layout.map((bar, i) => {
              const x =
                TIMELINE_LABEL_WIDTH + (1000 - TIMELINE_LABEL_WIDTH) * bar.x;
              const w = Math.max(
                3,
                (1000 - TIMELINE_LABEL_WIDTH) * bar.width
              );
              const y = bar.laneIndex * TIMELINE_LANE_HEIGHT + 4;
              const h = TIMELINE_LANE_HEIGHT - 8;
              const hovered = hoveredId === bar.row.id;
              const projectLabel =
                bar.row.project ?? t("overview.top_projects_unknown");
              const detailParts: string[] = [];
              if (bar.row.estimated_cost != null) {
                detailParts.push(`cost: $${bar.row.estimated_cost.toFixed(4)}`);
              }
              if (bar.row.requests != null) {
                detailParts.push(`${bar.row.requests} req`);
              }
              const tooltip =
                detailParts.length > 0
                  ? `${bar.row.provider} · ${projectLabel}\n${detailParts.join(" · ")}`
                  : `${bar.row.provider} · ${projectLabel}`;
              return (
                <rect
                  key={`${bar.row.id}-${i}`}
                  x={x}
                  y={y}
                  width={w}
                  height={h}
                  rx={2}
                  fill={bar.color}
                  fillOpacity={hovered ? 1 : 0.75}
                  stroke={hovered ? "#fff" : "transparent"}
                  strokeWidth={hovered ? 1 : 0}
                  tabIndex={0}
                  role="button"
                  aria-label={tooltip}
                  onMouseEnter={() => setHoveredId(bar.row.id)}
                  onMouseLeave={() => setHoveredId(null)}
                  onFocus={() => setHoveredId(bar.row.id)}
                  onBlur={() => setHoveredId(null)}
                  style={{ cursor: "pointer", outline: "none" }}
                >
                  <title>{tooltip}</title>
                </rect>
              );
            })}
          </svg>
        </div>
      )}
    </section>
  );
}

function Sessions({
  snapshot,
  loading,
  onRefresh,
  remoteSessions,
  remoteControlEnabled,
  onRemoteSessionAction,
}: {
  snapshot: SessionsSnapshot | null;
  loading: boolean;
  onRefresh: () => void;
  // v0.6.0 — cross-device managed sessions surfaced above the
  // local-process snapshot. Actionable in v0.6.2 (Slice 2): per-row
  // Send / Stop / Interrupt buttons.
  remoteSessions: RemoteSession[];
  remoteControlEnabled: boolean;
  /** Called after a managed-session command completes (success OR
   *  failure) so the parent can refresh and pick up the new status
   *  immediately rather than waiting for the next adaptive-poll
   *  tick. */
  onRemoteSessionAction: () => Promise<void>;
}) {
  const { t } = useTranslation();
  if (!snapshot && loading) {
    return <Skeleton />;
  }
  if (!snapshot) return null;

  const sessions = snapshot.sessions;

  return (
    <div className="space-y-4">
      <RemoteSessionsSection
        sessions={remoteSessions}
        enabled={remoteControlEnabled}
        onActionDone={onRemoteSessionAction}
      />

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

      {/* v0.5.5 — Activity Timeline. Cross-device 24h history view from
          the `sessions` table (NOT the local-process snapshot above,
          which is this device's currently-running processes only —
          see lib.rs::get_sessions_history). Self-managing polling at
          30s cadence; renders its own loading/error/empty states. */}
      <ActivityTimelineChart />

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
                  <td className="px-3 py-2 font-medium">
                    {/* v0.10.1 — provider brand-color dot (macOS parity,
                        reuses lib/providerTheme). */}
                    <span className="inline-flex items-center gap-2">
                      <span
                        className="w-2 h-2 rounded-full shrink-0"
                        style={{ backgroundColor: providerColor(s.provider) }}
                      />
                      {s.provider}
                    </span>
                  </td>
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

// v0.10.1 — Swarm View (macOS/iOS parity, ports SwarmTab.swift). A live
// grid of every parallel agent swarm the user's paired devices observe,
// attention-sorted so the swarm that needs a human is on top. Self-polls
// every 10 s while paired (matches the Mac cadence). All data is opaque
// (handle = `swarm-<6hex>`); no repo/branch name crosses the wire.
// ---- System Monitor "Machine" tab (v1.38 parity — LOCAL only) ----

type MachineProcess = {
  pid: number;
  name: string;
  cpu_percent: number;
  mem_bytes: number;
};

type MachineTemp = { label: string; celsius: number };
type MachineBattery = { percent: number; state: string };

type MachineSnapshot = {
  cpu_percent: number;
  cpu_core_count: number;
  mem_total_bytes: number;
  mem_used_bytes: number;
  mem_percent: number;
  process_count: number;
  top_processes: MachineProcess[];
  temperatures: MachineTemp[];
  battery: MachineBattery | null;
  collected_at: string;
};

// Green < 70% ≤ amber < 90% ≤ red. Same thresholds for CPU + memory.
function loadColor(pct: number): string {
  if (pct >= 90) return "#f87171";
  if (pct >= 70) return "#fbbf24";
  return "#34d399";
}

// Battery colour is INVERTED vs load: low charge is the danger. Charging is
// always green regardless of level.
function batteryColor(b: MachineBattery): string {
  if (b.state === "charging" || b.state === "full") return "#34d399";
  if (b.percent < 20) return "#f87171";
  if (b.percent < 40) return "#fbbf24";
  return "#34d399";
}

// Warm palette for temperatures (°C): <60 green, <80 amber, else red.
function tempColor(celsius: number): string {
  if (celsius >= 80) return "#f87171";
  if (celsius >= 60) return "#fbbf24";
  return "#34d399";
}

function MachineGauge({
  label,
  percent,
  detail,
}: {
  label: string;
  percent: number;
  detail: string;
}) {
  const pct = Math.max(0, Math.min(100, Number.isFinite(percent) ? percent : 0));
  const color = loadColor(pct);
  return (
    <div className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
      <div className="flex items-baseline justify-between mb-2">
        <span className="text-xs text-neutral-400">{label}</span>
        <span className="text-lg font-semibold tabular-nums" style={{ color }}>
          {pct.toFixed(0)}%
        </span>
      </div>
      <div
        className="h-2 rounded-full bg-neutral-800 overflow-hidden"
        role="progressbar"
        aria-valuenow={Math.round(pct)}
        aria-valuemin={0}
        aria-valuemax={100}
      >
        <div
          className="h-full rounded-full transition-[width] duration-500"
          style={{ width: `${pct}%`, backgroundColor: color }}
        />
      </div>
      <div className="mt-2 text-xs text-neutral-500 tabular-nums">{detail}</div>
    </div>
  );
}

type DeviceHealthRow = {
  id: string;
  name: string | null;
  device_type: string | null;
  status: string | null;
  cpu_usage: number | null;
  memory_usage: number | null;
  cpu_temp_c: number | null;
  battery_charge_pct: number | null;
  battery_state: string | null;
  last_seen_at: string | null;
  sensors_updated_at: string | null;
};

// Cross-device health read-back — the READ half of the device-health pillar
// (the heartbeat writes; this shows your OTHER devices' last-reported CPU/mem/
// temp/battery/status). Server read (get_devices, RLS-scoped to your account);
// 30s poll. Only rendered when paired.
function FleetHealth({ currentDeviceId }: { currentDeviceId: string | null }) {
  const { t } = useTranslation();
  const [devices, setDevices] = useState<DeviceHealthRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const d = await invoke<DeviceHealthRow[]>("get_devices");
        if (cancelled) return;
        setDevices(d);
        setError(null);
      } catch (e) {
        if (cancelled) return;
        setError(String(e));
      }
    };
    tick();
    const id = setInterval(tick, 30_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  if (error && !devices) {
    return (
      <div className="flex items-start gap-2 text-xs text-amber-400/90 p-3 rounded-lg border border-amber-900/60 bg-amber-950/20">
        <SeverityIcon severity="Warning" />
        <span>{t("machine.fleet_load_failed")}</span>
      </div>
    );
  }
  if (!devices) {
    return <div className="text-xs text-neutral-500">{t("machine.fleet_loading")}</div>;
  }
  if (devices.length === 0) {
    return <div className="text-xs text-neutral-500">{t("machine.fleet_none")}</div>;
  }

  return (
    <div>
      <h3 className="text-xs font-semibold text-neutral-400 mb-2">{t("machine.fleet_title")}</h3>
      <div className="space-y-1.5">
        {devices.map((d) => {
          const online = (d.status ?? "").toLowerCase() === "online";
          const isSelf = currentDeviceId != null && d.id === currentDeviceId;
          const metrics = [
            d.cpu_usage != null ? `CPU ${d.cpu_usage}%` : null,
            d.memory_usage != null ? `MEM ${d.memory_usage}%` : null,
            d.cpu_temp_c != null ? `${Math.round(d.cpu_temp_c)}°C` : null,
            d.battery_charge_pct != null
              ? `${t("machine.battery")} ${d.battery_charge_pct}%`
              : null,
          ]
            .filter(Boolean)
            .join(" · ");
          const seen = d.last_seen_at ? formatRelativeShort(d.last_seen_at) : null;
          return (
            <div
              key={d.id}
              className="flex items-center gap-2 text-xs p-2 rounded-md border border-neutral-800/70 bg-neutral-900/30"
            >
              <span
                className={`w-2 h-2 rounded-full shrink-0 ${online ? "bg-emerald-500" : "bg-neutral-600"}`}
                title={online ? t("machine.fleet_online") : t("machine.fleet_offline")}
                aria-hidden="true"
              />
              <span className="font-medium text-neutral-300 truncate max-w-[12rem]">
                {d.name || t("machine.fleet_unnamed")}
                {isSelf && <span className="text-neutral-500"> · {t("machine.fleet_this")}</span>}
              </span>
              {d.device_type && (
                <span className="text-neutral-600 hidden sm:inline">{d.device_type}</span>
              )}
              <span className="ml-auto tabular-nums text-neutral-400 shrink-0">
                {metrics || "—"}
              </span>
              {seen && (
                <span className="text-[10px] text-neutral-600 shrink-0" title={d.last_seen_at ?? ""}>
                  {seen}
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function MachineTab({
  paired,
  currentDeviceId,
  scan,
}: {
  paired: boolean;
  currentDeviceId: string | null;
  scan: ScanResult | null;
}) {
  const { t } = useTranslation();
  const [snap, setSnap] = useState<MachineSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Self-poll every 2s while mounted; independent of the app's other
  // pollers. Local-only, so it works whether or not the device is paired.
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const s = await invoke<MachineSnapshot>("get_machine_snapshot");
        if (cancelled) return;
        setSnap(s);
        setError(null);
      } catch (e) {
        if (cancelled) return;
        setError(String(e));
      }
    };
    tick();
    const id = setInterval(tick, 2000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  if (error && !snap) {
    return (
      <div className="flex items-start gap-2 text-xs text-amber-400/90 p-3 rounded-lg border border-amber-900/60 bg-amber-950/20">
        <SeverityIcon severity="Warning" />
        <span>{t("machine.load_failed")}</span>
      </div>
    );
  }
  if (!snap) {
    return <div className="text-xs text-neutral-500">{t("machine.loading")}</div>;
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-neutral-300">{t("machine.title")}</h2>
        <span className="text-xs text-neutral-500 tabular-nums">
          {t("machine.process_count", { count: snap.process_count })}
        </span>
      </div>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-3">
        <MachineGauge
          label={t("machine.cpu")}
          percent={snap.cpu_percent}
          detail={t("machine.cpu_cores", { count: snap.cpu_core_count })}
        />
        <MachineGauge
          label={t("machine.memory")}
          percent={snap.mem_percent}
          detail={`${formatBytes(snap.mem_used_bytes)} / ${formatBytes(snap.mem_total_bytes)}`}
        />
        {snap.battery && (
          <div className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40">
            <div className="flex items-baseline justify-between mb-2">
              <span className="text-xs text-neutral-400">{t("machine.battery")}</span>
              <span
                className="text-lg font-semibold tabular-nums"
                style={{ color: batteryColor(snap.battery) }}
              >
                {Math.round(Math.max(0, Math.min(100, snap.battery.percent)))}%
              </span>
            </div>
            <div className="h-2 rounded-full bg-neutral-800 overflow-hidden">
              <div
                className="h-full rounded-full transition-[width] duration-500"
                style={{
                  width: `${Math.max(0, Math.min(100, snap.battery.percent))}%`,
                  backgroundColor: batteryColor(snap.battery),
                }}
              />
            </div>
            <div className="mt-2 text-xs text-neutral-500">
              {t(`machine.batt_${snap.battery.state}`, {
                defaultValue: t("machine.batt_unknown"),
              })}
            </div>
          </div>
        )}
      </div>

      {snap.temperatures.length > 0 && (
        <div>
          <h3 className="text-xs font-semibold text-neutral-400 mb-2">
            {t("machine.temperatures")}
          </h3>
          <div className="flex flex-wrap gap-2">
            {snap.temperatures.map((tp, i) => (
              <span
                key={`${tp.label}-${i}`}
                className="px-2.5 py-1 text-xs rounded-md border border-neutral-800 bg-neutral-900/40 tabular-nums"
                title={tp.label}
              >
                <span className="text-neutral-400">{tp.label || "—"}</span>{" "}
                <span style={{ color: tempColor(tp.celsius) }}>{tp.celsius.toFixed(0)}°C</span>
              </span>
            ))}
          </div>
        </div>
      )}

      <div>
        <h3 className="text-xs font-semibold text-neutral-400 mb-2">
          {t("machine.top_processes")}
        </h3>
        {snap.top_processes.length === 0 ? (
          <div className="text-xs text-neutral-500">{t("machine.no_processes")}</div>
        ) : (
          <div className="rounded-lg border border-neutral-800 overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="text-neutral-500 bg-neutral-900/60">
                <tr>
                  <th className="text-left font-medium px-3 py-2">
                    {t("machine.col_process")}
                  </th>
                  <th className="text-right font-medium px-3 py-2">{t("machine.col_pid")}</th>
                  <th className="text-right font-medium px-3 py-2">{t("machine.col_cpu")}</th>
                  <th className="text-right font-medium px-3 py-2">{t("machine.col_mem")}</th>
                </tr>
              </thead>
              <tbody>
                {snap.top_processes.map((p) => (
                  <tr key={p.pid} className="border-t border-neutral-800/70">
                    <td
                      className="px-3 py-1.5 text-neutral-300 truncate max-w-[16rem]"
                      title={p.name}
                    >
                      {p.name || "—"}
                    </td>
                    <td className="px-3 py-1.5 text-right text-neutral-500 tabular-nums">
                      {p.pid}
                    </td>
                    <td
                      className="px-3 py-1.5 text-right tabular-nums"
                      style={{ color: (p.cpu_percent ?? 0) >= 25 ? "#fbbf24" : undefined }}
                    >
                      {(p.cpu_percent ?? 0).toFixed(1)}%
                    </td>
                    <td className="px-3 py-1.5 text-right text-neutral-400 tabular-nums">
                      {formatBytes(p.mem_bytes)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <p className="mt-2 text-[11px] text-neutral-600">{t("machine.local_note")}</p>
      </div>

      {scan && <UsageSourcesSection scan={scan} />}

      {paired && <FleetHealth currentDeviceId={currentDeviceId} />}
    </div>
  );
}

// Native-vs-WSL usage split (Windows). Surfaces the otherwise-silent WSL merge:
// when the CLIs run inside a WSL distro, their logs live under the
// \\wsl.localhost\<distro>\ share and get scanned + merged into the totals. We
// derive the origin from each cached file's path (see scanner::origin_usage), so
// this needs no cache-schema change. Rendered ONLY when a WSL origin is actually
// present, so Mac / Linux / Windows-without-WSL users never see it.
function UsageSourcesSection({ scan }: { scan: ScanResult }) {
  const { t } = useTranslation();
  const origins = scan.origin_usage ?? [];
  const hasWsl = origins.some((o) => o.kind === "wsl");
  if (!hasWsl) return null;
  const total = origins.reduce((s, o) => s + o.tokens, 0);
  return (
    <div className="rounded-lg border border-neutral-800 bg-neutral-900/40 p-4">
      <h3 className="text-xs font-semibold text-neutral-300 mb-1">
        {t("machine.sources_heading")}
      </h3>
      <p className="text-[11px] text-neutral-500 mb-3">{t("machine.sources_hint")}</p>
      <div className="space-y-2">
        {origins.map((o) => {
          const label =
            o.kind === "wsl"
              ? t("machine.source_wsl", { distro: o.distro ?? "WSL" })
              : t("machine.source_native");
          const pct = total > 0 ? (o.tokens / total) * 100 : 0;
          return (
            <div
              key={`${o.kind}:${o.distro ?? ""}`}
              className="flex items-center gap-3 text-xs"
            >
              <span
                className="w-28 shrink-0 truncate text-neutral-300"
                title={label}
              >
                {label}
              </span>
              <div className="flex-1 h-2 bg-neutral-800 rounded overflow-hidden">
                <div
                  className="h-full rounded bg-gradient-to-r from-sky-500 to-cyan-500"
                  style={{ width: `${pct}%` }}
                />
              </div>
              <span className="w-32 text-right tabular-nums text-neutral-400 shrink-0">
                {t("machine.source_tokens", { value: formatInt(o.tokens) })}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Swarm({
  paired,
  remoteControlEnabled,
}: {
  paired: boolean;
  remoteControlEnabled: boolean;
}) {
  const { t } = useTranslation();
  const [devices, setDevices] = useState<RemoteSwarmDevice[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!paired) {
      setDevices(null);
      setError(null);
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const d = await invoke<RemoteSwarmDevice[]>("remote_list_swarms");
        if (cancelled) return;
        setDevices(d);
        setError(null);
      } catch (e: any) {
        if (cancelled) return;
        setError(String(e));
      }
    };
    tick();
    const id = setInterval(tick, 10_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [paired]);

  // Flatten device × swarm, attention-sorted: live before stale, then
  // blocked desc, agents desc, handle asc (verbatim port of the macOS
  // SwarmTab comparator).
  const entries = useMemo(() => {
    const out: { device: RemoteSwarmDevice; swarm: RemoteSwarm }[] = [];
    for (const d of devices ?? []) {
      for (const s of d.swarms) out.push({ device: d, swarm: s });
    }
    return out.sort((a, b) => {
      if (a.device.stale !== b.device.stale) return a.device.stale ? 1 : -1;
      if (a.swarm.blocked !== b.swarm.blocked) return b.swarm.blocked - a.swarm.blocked;
      if (a.swarm.agents !== b.swarm.agents) return b.swarm.agents - a.swarm.agents;
      return a.swarm.handle.localeCompare(b.swarm.handle);
    });
  }, [devices]);

  const age = (s: number) => {
    const p = secondsToShortParts(s);
    return t(`time.unit_${p.unit}`, { count: p.value });
  };

  const totalAgents = entries.reduce((n, e) => n + e.swarm.agents, 0);
  const totalBlocked = entries.reduce((n, e) => n + e.swarm.blocked, 0);

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-neutral-300">{t("swarm.title")}</h2>
        {entries.length > 0 && (
          <span
            className={`text-xs ${totalBlocked > 0 ? "text-amber-400" : "text-neutral-500"}`}
          >
            {t("swarm.summary", {
              swarms: entries.length,
              agents: totalAgents,
              blocked: totalBlocked,
            })}
          </span>
        )}
      </div>

      {!paired ? (
        <SwarmEmpty title={t("swarm.title")} subtitle={t("swarm.not_paired_hint")} />
      ) : !remoteControlEnabled ? (
        <SwarmEmpty title={t("swarm.title")} subtitle={t("swarm.disabled_hint")} />
      ) : error && entries.length === 0 ? (
        <div className="flex items-start gap-2 text-xs text-amber-400/90 p-3 rounded-lg border border-amber-900/60 bg-amber-950/20">
          <SeverityIcon severity="Warning" />
          <span>{t("swarm.load_failed")}</span>
        </div>
      ) : entries.length === 0 ? (
        <SwarmEmpty title={t("swarm.no_swarms")} subtitle={t("swarm.empty_hint")} />
      ) : (
        <div className="grid sm:grid-cols-2 gap-3">
          {entries.map(({ device, swarm }) => {
            const blocked = swarm.blocked > 0;
            const stale = device.stale;
            return (
              <div
                key={device.device_id + "/" + swarm.swarm_key}
                className={`rounded-lg border p-3 space-y-2 ${
                  blocked && !stale
                    ? "border-amber-700/60 bg-amber-950/10"
                    : "border-neutral-800 bg-neutral-900/40"
                } ${stale ? "opacity-70" : ""}`}
              >
                <div className="flex items-center gap-2">
                  <span
                    className="font-mono text-sm font-medium truncate"
                    title={swarm.handle}
                  >
                    {swarm.handle}
                  </span>
                  {swarm.is_linked_worktree && (
                    <span className="px-1.5 py-0.5 text-[9px] rounded-full border border-neutral-700 text-neutral-400">
                      {t("swarm.worktree")}
                    </span>
                  )}
                  <span className="flex-1" />
                  {blocked && (
                    <span
                      className={`px-1.5 py-0.5 text-[10px] font-bold rounded ${
                        stale ? "bg-neutral-700 text-neutral-300" : "bg-amber-600 text-amber-50"
                      }`}
                    >
                      {t("swarm.blocked_badge")}
                    </span>
                  )}
                </div>
                <div className="flex items-center gap-2 text-xs text-neutral-400">
                  <span>{t("swarm.agents", { count: swarm.agents })}</span>
                  {blocked && (
                    <>
                      <span className="text-neutral-600">·</span>
                      <span className={stale ? "text-neutral-400" : "text-amber-400"}>
                        {t("swarm.blocked_count", { count: swarm.blocked })}
                      </span>
                    </>
                  )}
                </div>
                {blocked && swarm.oldest_blocked_age_s > 0 && (
                  <div className="text-[11px] text-neutral-500">
                    {t("swarm.oldest_blocked", { age: age(swarm.oldest_blocked_age_s) })}
                  </div>
                )}
                {swarm.providers.length > 0 && (
                  <div className="flex flex-wrap gap-1.5">
                    {swarm.providers.map((p) => {
                      const c = providerColor(p);
                      return (
                        <span
                          key={p}
                          className="px-1.5 py-0.5 text-[10px] rounded-full"
                          style={{
                            backgroundColor: `${c}22`,
                            color: c,
                            border: `1px solid ${c}55`,
                          }}
                        >
                          {p}
                        </span>
                      );
                    })}
                  </div>
                )}
                {stale && (
                  <div className="text-[10px] text-neutral-500">
                    {t("swarm.stale")} · {t("swarm.last_seen", { age: age(device.age_s) })}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function SwarmEmpty({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="text-center py-12 px-4">
      <div className="text-sm font-medium text-neutral-300">{title}</div>
      <div className="text-xs text-neutral-500 mt-1 max-w-sm mx-auto">{subtitle}</div>
    </div>
  );
}

function Alerts({
  alerts,
  loading,
  onRefresh,
  paired,
}: {
  alerts: Alert[] | null;
  loading: boolean;
  onRefresh: () => void;
  paired: boolean;
}) {
  const { t } = useTranslation();
  // v0.10.1 — when paired, show persisted SERVER alerts with full
  // lifecycle (resolve / acknowledge / snooze + Open/Resolved/All
  // filter), ported from the macOS AlertsTab. The local-preview path
  // below stays for unpaired users: those alerts are client-computed
  // from the local scan each tick, not persisted, so they have no
  // lifecycle to act on.
  if (paired) return <ServerAlertsPanel />;
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

// v0.10.1 — paired-mode Alerts panel (macOS AlertsTab parity). Reads
// persisted server alerts (incl. resolved) via `list_alerts`, with an
// Open / Resolved / All filter, severity summary badges, Resolve-All,
// and per-row Acknowledge / Resolve / Snooze actions. Self-polls 30 s.
type ServerAlertFilter = "open" | "resolved" | "all";

function ServerAlertsPanel() {
  const { t } = useTranslation();
  const [alerts, setAlerts] = useState<ServerAlert[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<ServerAlertFilter>("open");
  const [busy, setBusy] = useState<Set<string>>(new Set());

  const refresh = useCallback(async () => {
    try {
      const a = await invoke<ServerAlert[]>("list_alerts");
      setAlerts(a);
      setError(null);
    } catch (e: any) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 30_000);
    return () => clearInterval(id);
  }, [refresh]);

  const now = Date.now();
  const isSnoozed = (a: ServerAlert) =>
    a.snoozed_until != null && Date.parse(a.snoozed_until) > now;

  const open = (alerts ?? []).filter((a) => !a.is_resolved && !isSnoozed(a));
  const resolved = (alerts ?? []).filter((a) => a.is_resolved);
  const visible =
    filter === "open" ? open : filter === "resolved" ? resolved : alerts ?? [];

  const rank = (s: string) => (s === "Critical" ? 0 : s === "Warning" ? 1 : 2);
  const sorted = [...visible].sort(
    (a, b) =>
      rank(a.severity) - rank(b.severity) || b.created_at.localeCompare(a.created_at),
  );

  const critical = open.filter((a) => a.severity === "Critical").length;
  const warning = open.filter((a) => a.severity === "Warning").length;

  async function act(id: string, fn: () => Promise<unknown>) {
    setBusy((b) => new Set(b).add(id));
    try {
      await fn();
      await refresh();
    } catch (e: any) {
      setError(String(e));
    } finally {
      setBusy((b) => {
        const n = new Set(b);
        n.delete(id);
        return n;
      });
    }
  }

  async function resolveAll() {
    for (const a of open) {
      try {
        await invoke("resolve_alert", { id: a.id });
      } catch (e: any) {
        setError(String(e));
      }
    }
    await refresh();
  }

  const filterBtn = (f: ServerAlertFilter, label: string) => (
    <button
      type="button"
      onClick={() => setFilter(f)}
      className={`px-3 py-1 text-xs rounded-md border ${
        filter === f
          ? "border-emerald-600 bg-emerald-950/50 text-emerald-200"
          : "border-neutral-700 text-neutral-400 hover:bg-neutral-800"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-2 flex-wrap">
        <div className="flex items-center gap-2">
          {filterBtn("open", t("alerts.filter_open"))}
          {filterBtn("resolved", t("alerts.filter_resolved"))}
          {filterBtn("all", t("alerts.filter_all"))}
        </div>
        <div className="flex items-center gap-2">
          {critical > 0 && (
            <span className="px-2 py-0.5 text-xs rounded-full bg-red-950/60 border border-red-800 text-red-300">
              {t("alerts.severity_critical", { count: critical })}
            </span>
          )}
          {warning > 0 && (
            <span className="px-2 py-0.5 text-xs rounded-full bg-amber-950/60 border border-amber-800 text-amber-300">
              {t("alerts.severity_warning", { count: warning })}
            </span>
          )}
          {filter === "open" && open.length > 0 && (
            <button
              type="button"
              onClick={resolveAll}
              className="px-2.5 py-1 text-xs rounded-md border border-emerald-700 text-emerald-300 hover:bg-emerald-900/40"
            >
              {t("alerts.resolve_all")}
            </button>
          )}
        </div>
      </div>

      {error && !alerts && (
        <div className="flex items-center gap-2 text-xs text-amber-400/90 p-3 rounded-lg border border-amber-900/60 bg-amber-950/20">
          <SeverityIcon severity="Warning" />
          <span>{t("alerts.load_failed")}</span>
        </div>
      )}

      {alerts && sorted.length === 0 ? (
        filter === "open" ? (
          <SwarmEmpty title={t("alerts.all_clear")} subtitle={t("alerts.all_clear_hint")} />
        ) : (
          <SwarmEmpty title={t("alerts.no_matching")} subtitle="" />
        )
      ) : (
        <div className="space-y-2">
          {sorted.map((a) => (
            <ServerAlertCard
              key={a.id}
              alert={a}
              busy={busy.has(a.id)}
              onAck={() => act(a.id, () => invoke("acknowledge_alert", { id: a.id }))}
              onResolve={() => act(a.id, () => invoke("resolve_alert", { id: a.id }))}
              onSnooze={(m) =>
                act(a.id, () => invoke("snooze_alert", { id: a.id, minutes: m }))
              }
            />
          ))}
        </div>
      )}
    </div>
  );
}

function ServerAlertCard({
  alert,
  busy,
  onAck,
  onResolve,
  onSnooze,
}: {
  alert: ServerAlert;
  busy: boolean;
  onAck: () => void;
  onResolve: () => void;
  onSnooze: (minutes: number) => void;
}) {
  const { t } = useTranslation();
  const [showSnooze, setShowSnooze] = useState(false);
  const sev = alert.severity as Alert["severity"];
  const accent =
    alert.severity === "Critical"
      ? "border-red-800 bg-red-950/40"
      : alert.severity === "Warning"
      ? "border-amber-800 bg-amber-950/30"
      : "border-neutral-800 bg-neutral-900/40";
  const chip =
    "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full border border-neutral-700/70 bg-neutral-800/40 text-neutral-300";
  const actionBtn = "px-2 py-0.5 text-[11px] rounded-full border disabled:opacity-50";
  return (
    <div
      className={`p-4 rounded-lg border ${
        alert.is_resolved ? "border-neutral-800 bg-neutral-900/20 opacity-70" : accent
      }`}
    >
      <div className="flex items-start gap-3">
        <SeverityIcon severity={sev} />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-semibold">{alert.title}</span>
            <span className="text-xs text-neutral-500 font-mono">{alert.type}</span>
          </div>
          {alert.message && (
            <div className="text-sm text-neutral-300 mt-1">{alert.message}</div>
          )}
          <div className="text-xs mt-2 flex flex-wrap items-center gap-2">
            {alert.related_provider && (
              <span className={chip}>
                <span
                  className="w-1.5 h-1.5 rounded-full shrink-0"
                  style={{ backgroundColor: providerColor(alert.related_provider) }}
                />
                {alert.related_provider}
              </span>
            )}
            {alert.related_project_name && (
              <span className={chip}>
                {t("misc.project_label", { name: alert.related_project_name })}
              </span>
            )}
            {alert.related_session_name && (
              <span className={chip}>
                {t("misc.session_label", { name: alert.related_session_name })}
              </span>
            )}
            {alert.related_device_name && (
              <span className={chip}>
                {t("misc.device_label", { name: alert.related_device_name })}
              </span>
            )}
            <span className="text-neutral-500">
              {new Date(alert.created_at).toLocaleString()}
            </span>
          </div>

          {alert.is_resolved ? (
            <div className="mt-2 inline-flex items-center gap-1.5 text-xs text-emerald-400">
              <SeverityIcon severity="Info" intent="ok" />
              {t("alerts.resolved_label")}
            </div>
          ) : (
            <div className="mt-3 flex items-center gap-2 flex-wrap">
              {!alert.is_read && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={onAck}
                  className={`${actionBtn} border-sky-800 text-sky-300 hover:bg-sky-900/40`}
                >
                  {t("alerts.action_ack")}
                </button>
              )}
              <button
                type="button"
                disabled={busy}
                onClick={onResolve}
                className={`${actionBtn} border-emerald-800 text-emerald-300 hover:bg-emerald-900/40`}
              >
                {t("alerts.action_resolve")}
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => setShowSnooze((v) => !v)}
                className={`${actionBtn} border-amber-800 text-amber-300 hover:bg-amber-900/40`}
              >
                {showSnooze ? t("action.cancel") : t("alerts.action_snooze")}
              </button>
              {showSnooze &&
                [15, 30, 60, 120].map((m) => (
                  <button
                    key={m}
                    type="button"
                    disabled={busy}
                    onClick={() => {
                      onSnooze(m);
                      setShowSnooze(false);
                    }}
                    className={`${actionBtn} border-neutral-700 text-neutral-300 hover:bg-neutral-800`}
                  >
                    {m}m
                  </button>
                ))}
            </div>
          )}
        </div>
      </div>
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
  // v0.10.1 — chip style for related-entity metadata (macOS parity).
  const chip =
    "inline-flex items-center gap-1.5 px-2 py-0.5 rounded-full border border-neutral-700/70 bg-neutral-800/40 text-neutral-300";
  return (
    <div className={`p-4 rounded-lg border ${accent}`}>
      <div className="flex items-start gap-3">
        {/* v0.10.1 — SVG severity icon (was an emoji, which renders as a
            fixed multicolor system glyph on Win/Linux and ignores CSS
            color — same reason RiskSignalsCard switched to SVG in v0.5.1). */}
        <SeverityIcon severity={alert.severity} />
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="text-sm font-semibold">{alert.title}</span>
            <span className="text-xs text-neutral-500 font-mono">{alert.type}</span>
          </div>
          <div className="text-sm text-neutral-300 mt-1">{alert.message}</div>
          {/* v0.10.1 — related-entity chips. Surfaces session + device,
              which the wire shape already carried but the card never
              rendered; the provider chip carries its brand-color dot. */}
          <div className="text-xs mt-2 flex flex-wrap items-center gap-2">
            {alert.related_provider && (
              <span className={chip}>
                <span
                  className="w-1.5 h-1.5 rounded-full shrink-0"
                  style={{ backgroundColor: providerColor(alert.related_provider) }}
                />
                {alert.related_provider}
              </span>
            )}
            {alert.related_project_name && (
              <span className={chip}>
                {t("misc.project_label", { name: alert.related_project_name })}
              </span>
            )}
            {alert.related_session_name && (
              <span className={chip}>
                {t("misc.session_label", { name: alert.related_session_name })}
              </span>
            )}
            {alert.related_device_name && (
              <span className={chip}>
                {t("misc.device_label", { name: alert.related_device_name })}
              </span>
            )}
            <span className="text-neutral-500">
              {new Date(alert.created_at).toLocaleString()}
            </span>
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
  const fmt = useMoney();
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
                {e.cost_usd != null ? fmt(e.cost_usd) : "—"}
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
