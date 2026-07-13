import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

type StartInfo = { id: string; pid: number };
type Disposable = { dispose(): void };

/**
 * v0.11.0 (T2.3b) — the in-app LOCAL terminal pane, now live.
 *
 * Start spawns the user's own `claude` on this machine (terminal_start) and
 * streams it into xterm.js. Output uses a SINGLE-FLIGHT requestAnimationFrame
 * pump: each frame drains the bounded stdout ring via terminal_read (a raw
 * binary ArrayBuffer), writes it to xterm, and only THEN schedules the next
 * frame — so two destructive drains can never overlap and reorder the stream.
 * Keystrokes/paste/Ctrl-C flow via term.onData -> terminal_write; xterm's
 * onResize round-trips (debounced) to terminal_resize.
 *
 * Renderer: DOM only (no WebGL/canvas addon) so the headless launch-smoke
 * can't hit a getContext() crash. Exit detection, the backgrounded-window
 * fallback pump, and telemetry are T2.3c/T2.3d.
 */
export function LocalTerminal() {
  const { t } = useTranslation();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const [initError, setInitError] = useState<string | null>(null);

  // Session state (render) mirrored into refs (read by the ref-only pump).
  const [sessionId, setSessionId] = useState<string | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  // Streaming internals.
  const stoppedRef = useRef(true);
  const rafRef = useRef<number | null>(null);
  const onDataRef = useRef<Disposable | null>(null);
  const onResizeRef = useRef<Disposable | null>(null);
  const resizeTimerRef = useRef<number | null>(null);

  const setSession = useCallback((id: string | null) => {
    sessionIdRef.current = id;
    setSessionId(id);
  }, []);

  /** Stop the output pump + cancel any pending resize debounce (no I/O). */
  const stopStreaming = useCallback(() => {
    stoppedRef.current = true;
    if (rafRef.current != null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
    if (resizeTimerRef.current != null) {
      clearTimeout(resizeTimerRef.current);
      resizeTimerRef.current = null;
    }
  }, []);

  /** Detach the xterm input/resize listeners for the current session. */
  const detachListeners = useCallback(() => {
    onDataRef.current?.dispose();
    onDataRef.current = null;
    onResizeRef.current?.dispose();
    onResizeRef.current = null;
  }, []);

  /**
   * Single-flight rAF output pump. Reads uses REFS (not state) so it always
   * sees the live session id / terminal without being re-created, and it
   * schedules the next frame ONLY after the current read resolves.
   */
  const pump = useCallback(async () => {
    if (stoppedRef.current) return;
    const id = sessionIdRef.current;
    const term = termRef.current;
    if (!id || !term) return;
    try {
      const buf = await invoke<ArrayBuffer>("terminal_read", {
        id,
        maxBytes: 65536,
      });
      if (stoppedRef.current) return;
      if (buf.byteLength > 0) term.write(new Uint8Array(buf));
    } catch {
      // The session is gone (e.g. closed) — stop pumping.
      stoppedRef.current = true;
      return;
    }
    if (!stoppedRef.current) {
      rafRef.current = requestAnimationFrame(() => {
        void pump();
      });
    }
  }, []);

  const start = useCallback(async () => {
    const term = termRef.current;
    if (!term || busy || sessionIdRef.current) return;
    setBusy(true);
    setActionError(null);
    try {
      const info = await invoke<StartInfo>("terminal_start", { cwd: null });
      // The component may have unmounted (tab switch) while terminal_start was
      // in flight — the unmount cleanup couldn't close a session it didn't yet
      // know about. If our terminal is gone or was replaced, close the orphan
      // now so we never leak a spawned `claude` + PTY.
      if (termRef.current !== term) {
        invoke("terminal_close", { id: info.id }).catch(() => {});
        return;
      }
      setSession(info.id);
      term.clear();
      term.focus();
      // Sync the PTY size to the fitted pane BEFORE streaming — the Rust
      // default is 80x24, which would mis-wrap the CLI's output otherwise.
      try {
        fitRef.current?.fit();
      } catch {
        /* pane hidden */
      }
      await invoke("terminal_resize", {
        id: info.id,
        rows: term.rows,
        cols: term.cols,
      }).catch(() => {});
      // Keystrokes / paste / Ctrl-C(0x03) -> stdin.
      detachListeners();
      onDataRef.current = term.onData((d) => {
        const id = sessionIdRef.current;
        if (id) invoke("terminal_write", { id, data: d }).catch(() => {});
      });
      // Debounced resize round-trip (xterm fires onResize after fit()).
      onResizeRef.current = term.onResize(({ rows, cols }) => {
        const id = sessionIdRef.current;
        if (!id) return;
        if (resizeTimerRef.current != null) clearTimeout(resizeTimerRef.current);
        resizeTimerRef.current = window.setTimeout(() => {
          invoke("terminal_resize", { id, rows, cols }).catch(() => {});
        }, 100);
      });
      // Begin streaming.
      stoppedRef.current = false;
      void pump();
    } catch (e) {
      setActionError(String(e));
      setSession(null);
    } finally {
      setBusy(false);
    }
  }, [busy, detachListeners, pump, setSession]);

  const stop = useCallback(async () => {
    const id = sessionIdRef.current;
    stopStreaming();
    detachListeners();
    setSession(null);
    if (id) {
      setBusy(true);
      try {
        await invoke("terminal_close", { id });
      } catch {
        /* best-effort */
      } finally {
        setBusy(false);
      }
    }
    termRef.current?.writeln("\r\n" + t("terminal.stopped"));
  }, [detachListeners, setSession, stopStreaming, t]);

  // --- mount xterm (once); tear everything down on unmount ---
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    let term: Terminal | null = null;
    let observer: ResizeObserver | null = null;
    try {
      term = new Terminal({
        convertEol: false,
        scrollback: 5000,
        fontSize: 13,
        fontFamily:
          'ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
        theme: { background: "#0a0a0a" },
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(el);
      try {
        fit.fit();
      } catch {
        /* not sized yet — the ResizeObserver re-fits once it is */
      }
      termRef.current = term;
      fitRef.current = fit;
      term.writeln(t("terminal.hint"));
      observer = new ResizeObserver(() => {
        try {
          fitRef.current?.fit();
        } catch {
          /* pane hidden -> fit throws; ignore */
        }
      });
      observer.observe(el);
    } catch (e) {
      setInitError(String(e));
    }
    return () => {
      // Stop streaming and kill the session so nothing leaks on unmount.
      stopStreaming();
      detachListeners();
      const id = sessionIdRef.current;
      if (id) invoke("terminal_close", { id }).catch(() => {});
      sessionIdRef.current = null;
      observer?.disconnect();
      fitRef.current = null;
      try {
        termRef.current?.dispose();
      } catch {
        /* ignore */
      }
      termRef.current = null;
    };
    // Mount once. `t` is captured for the one-shot hint; the buttons + stop
    // banner re-localize via their own render/closures.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">{t("terminal.section_title")}</h2>
          <p className="text-xs text-neutral-400">
            {t("terminal.section_subtitle")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {sessionId ? (
            <button
              type="button"
              onClick={() => void stop()}
              disabled={busy}
              className="px-3 py-1.5 text-xs rounded border border-red-800 bg-red-950/40 text-red-200 hover:bg-red-900/40 disabled:opacity-50"
            >
              {t("terminal.stop_button")}
            </button>
          ) : (
            <button
              type="button"
              onClick={() => void start()}
              disabled={busy || !!initError}
              className="px-3 py-1.5 text-xs rounded border border-emerald-800 bg-emerald-950/40 text-emerald-200 hover:bg-emerald-900/40 disabled:opacity-50"
            >
              {busy ? t("terminal.starting") : t("terminal.start_button")}
            </button>
          )}
        </div>
      </div>
      {actionError && (
        <div className="text-xs text-amber-400">
          {t("terminal.start_failed")}: {actionError}
        </div>
      )}
      {initError ? (
        <div className="text-xs text-amber-400">
          {t("terminal.init_failed")}: {initError}
        </div>
      ) : (
        <div
          ref={containerRef}
          className="h-80 w-full overflow-hidden rounded border border-neutral-800 bg-black"
        />
      )}
    </section>
  );
}
