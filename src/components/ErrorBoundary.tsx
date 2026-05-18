import { Component, type ErrorInfo, type ReactNode } from "react";

interface State {
  error: Error | null;
  info: ErrorInfo | null;
}

export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null, info: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    this.setState({ info });
    console.error("App render error:", error, info);
  }

  reset = (): void => {
    this.setState({ error: null, info: null });
  };

  render(): ReactNode {
    if (!this.state.error) return this.props.children;
    return (
      <div
        style={{
          padding: 24,
          background: "#1a1d27",
          color: "#e7e9ee",
          height: "100vh",
          overflowY: "auto",
          fontFamily: "Consolas, Menlo, monospace",
          fontSize: 13,
        }}
      >
        <h2 style={{ color: "#ff8888", marginTop: 0 }}>Render error</h2>
        <p style={{ color: "#8a93a6" }}>
          The UI hit an exception during render. Reload (Ctrl+R) once you've copied the trace, or click the button below to retry without reload.
        </p>
        <h3 style={{ color: "#ff8888" }}>{this.state.error.name}: {this.state.error.message}</h3>
        <pre style={{ whiteSpace: "pre-wrap", background: "#0f1115", padding: 12, borderRadius: 6, overflowX: "auto" }}>
          {this.state.error.stack}
        </pre>
        {this.state.info && (
          <>
            <h4 style={{ color: "#8a93a6" }}>Component stack</h4>
            <pre style={{ whiteSpace: "pre-wrap", background: "#0f1115", padding: 12, borderRadius: 6, overflowX: "auto" }}>
              {this.state.info.componentStack}
            </pre>
          </>
        )}
        <button
          onClick={this.reset}
          style={{
            marginTop: 12,
            background: "#7aa2ff",
            color: "#0f1115",
            border: "none",
            padding: "8px 16px",
            borderRadius: 6,
            cursor: "pointer",
            fontWeight: 600,
          }}
        >
          Retry render
        </button>
      </div>
    );
  }
}
