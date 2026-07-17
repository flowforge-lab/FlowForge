import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

/**
 * Last-resort UI when a render throws. Without this, React 19 clears `#root`
 * and the packaged app looks like a blank charcoal window with no clue why.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error("[ErrorBoundary]", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div
        style={{
          display: "flex",
          height: "100%",
          width: "100%",
          alignItems: "center",
          justifyContent: "center",
          padding: 24,
          boxSizing: "border-box",
          background: "#141414",
          color: "#e5e5e5",
          fontFamily: "system-ui, sans-serif",
        }}
      >
        <div style={{ maxWidth: 480, textAlign: "center" }}>
          <div style={{ fontSize: 16, fontWeight: 600, marginBottom: 8 }}>
            FlowForge hit a UI error
          </div>
          <div style={{ fontSize: 13, color: "#a3a3a3", marginBottom: 16 }}>
            {error.message || String(error)}
          </div>
          <button
            type="button"
            style={{
              fontSize: 13,
              padding: "6px 12px",
              borderRadius: 6,
              border: "1px solid #404040",
              background: "#262626",
              color: "#fafafa",
              cursor: "pointer",
            }}
            onClick={() => window.location.reload()}
          >
            Reload
          </button>
        </div>
      </div>
    );
  }
}
