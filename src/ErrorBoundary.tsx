import { Component, type ErrorInfo, type ReactNode } from "react";

// React still requires a class component for error boundaries (no hook
// equivalent in React 19). Catches render-time exceptions in the subtree
// and shows a fallback panel instead of unmounting the tree to a blank
// white screen.
//
// History: in v0.2.10 the Sessions tab read `s.cpu_usage.toFixed(1)` where
// the backend had stripped `cpu_usage` via `#[serde(skip_serializing)]`,
// so the field arrived as `undefined`. Without an ErrorBoundary, React 18+
// unmounts the whole tree on render errors — Jason's screenshot showed
// every tab and the title bar gone, only the Tauri window chrome left.
// v0.2.11 fixes the backend serialization AND wraps the App in this
// boundary so the next mystery error shows itself instead of vanishing.

type Props = {
  children: ReactNode;
};

type State = {
  error: Error | null;
  info: ErrorInfo | null;
};

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, info: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    this.setState({ info });
    if (typeof console !== "undefined") {
      console.error("[ErrorBoundary]", error, info);
    }
  }

  render(): ReactNode {
    if (this.state.error === null) {
      return this.props.children;
    }
    const err = this.state.error;
    const stack = err.stack ?? "(no stack)";
    const componentStack = this.state.info?.componentStack ?? "(no component stack)";
    return (
      <div
        style={{
          padding: "24px",
          fontFamily: "ui-monospace, SFMono-Regular, Consolas, monospace",
          fontSize: "12px",
          background: "#0b0b0b",
          color: "#f5f5f5",
          height: "100vh",
          overflow: "auto",
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
        }}
      >
        <div
          style={{
            fontSize: "16px",
            fontWeight: 600,
            marginBottom: "12px",
            color: "#ff8888",
          }}
        >
          CLI Pulse — render error
        </div>
        <div style={{ marginBottom: "16px" }}>
          The UI hit an unrecoverable error. Reporting this to the developer
          (paste the text below into a GitHub issue) will help fix it.
        </div>
        <div
          style={{
            background: "#1a1a1a",
            border: "1px solid #333",
            padding: "12px",
            borderRadius: "4px",
            marginBottom: "12px",
          }}
        >
          <div style={{ fontWeight: 600, marginBottom: "6px" }}>{err.name}: {err.message}</div>
          {stack}
        </div>
        <div
          style={{
            background: "#1a1a1a",
            border: "1px solid #333",
            padding: "12px",
            borderRadius: "4px",
            marginBottom: "12px",
          }}
        >
          <div style={{ fontWeight: 600, marginBottom: "6px" }}>Component stack</div>
          {componentStack}
        </div>
        <button
          type="button"
          onClick={() => this.setState({ error: null, info: null })}
          style={{
            padding: "8px 16px",
            background: "#2a6df4",
            color: "white",
            border: "none",
            borderRadius: "4px",
            cursor: "pointer",
          }}
        >
          Try to recover
        </button>
      </div>
    );
  }
}
