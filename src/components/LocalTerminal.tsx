import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

/**
 * v0.11.0 (T2.3a) — the in-app LOCAL terminal pane.
 *
 * This slice is the crash-safe SKELETON: it mounts an xterm.js terminal,
 * fits it to the container, and re-fits on resize. Launching a session
 * (Start -> `terminal_start`) and the single-flight streaming pump land in
 * T2.3b; the backend command surface (terminal_start / write / read /
 * resize / close / status) already exists.
 *
 * Crash-safety: the CI launch-smoke renders every tab headlessly, so xterm
 * init is wrapped in try/catch and renders a fallback message on failure
 * rather than white-screening the whole app.
 */
export function LocalTerminal() {
  const { t } = useTranslation();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const [initError, setInitError] = useState<string | null>(null);

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
      // The container may be zero-sized while a tab is off-screen; a failed
      // fit must not abort mounting the terminal.
      try {
        fit.fit();
      } catch {
        /* not sized yet — the ResizeObserver below re-fits once it is */
      }
      fitRef.current = fit;
      term.writeln(t("terminal.hint"));
      observer = new ResizeObserver(() => {
        try {
          fitRef.current?.fit();
        } catch {
          /* pane hidden (display:none) -> fit throws; ignore */
        }
      });
      observer.observe(el);
    } catch (e) {
      setInitError(String(e));
    }
    return () => {
      observer?.disconnect();
      fitRef.current = null;
      try {
        term?.dispose();
      } catch {
        /* ignore */
      }
    };
    // Mount once. `t` is captured at mount: the one-shot hint line isn't
    // re-localized on a language switch, which avoids tearing down a (soon,
    // T2.3b) live terminal on lang change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <section className="p-4 rounded-lg border border-neutral-800 bg-neutral-900/40 space-y-3">
      <div>
        <h2 className="text-sm font-semibold">{t("terminal.section_title")}</h2>
        <p className="text-xs text-neutral-400">
          {t("terminal.section_subtitle")}
        </p>
      </div>
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
