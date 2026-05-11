import React, { Component, type ReactNode } from "react";
import ReactDOM from "react-dom/client";
import { Workbench } from "./features/workbench/Workbench";
import { startupPerfMark } from "./lib/tauri";
import "./theme.css";

void startupPerfMark("main_loaded", `performance_now=${performance.now().toFixed(1)}`);

class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  constructor(props: { children: ReactNode }) {
    super(props);
    this.state = { error: null };
  }

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  render() {
    if (this.state.error) {
      return (
        <div style={{ padding: 40, color: "#d08770", fontFamily: "monospace", fontSize: 14, lineHeight: 1.6 }}>
          <h2 style={{ color: "#d08770" }}>React Render Error</h2>
          <pre style={{ whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
            {this.state.error.message}
          </pre>
          <pre style={{ whiteSpace: "pre-wrap", wordBreak: "break-all", fontSize: 12, color: "#aebdc2", marginTop: 16 }}>
            {this.state.error.stack}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}

void startupPerfMark("react_render_start", `performance_now=${performance.now().toFixed(1)}`);
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Workbench />
    </ErrorBoundary>
  </React.StrictMode>
);
requestAnimationFrame(() => {
  void startupPerfMark("first_animation_frame", `performance_now=${performance.now().toFixed(1)}`);
});
