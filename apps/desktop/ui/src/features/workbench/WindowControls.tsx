import { getCurrentWindow } from "@tauri-apps/api/window";
import "./WindowControls.css";

export function WindowControls() {
  const appWindow = getCurrentWindow();

  return (
    <div className="window-controls">
      <button
        className="window-control-btn window-control-minimize"
        onClick={() => appWindow.minimize()}
        title="Minimize"
      >
        <svg width="10" height="1" viewBox="0 0 10 1">
          <rect width="10" height="1" fill="currentColor" />
        </svg>
      </button>
      <button
        className="window-control-btn window-control-maximize"
        onClick={() => appWindow.toggleMaximize()}
        title="Maximize"
      >
        <svg width="10" height="10" viewBox="0 0 10 10">
          <rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
      <button
        className="window-control-btn window-control-close"
        onClick={() => appWindow.close()}
        title="Close"
      >
        <svg width="10" height="10" viewBox="0 0 10 10">
          <line x1="0" y1="0" x2="10" y2="10" stroke="currentColor" strokeWidth="1.2" />
          <line x1="10" y1="0" x2="0" y2="10" stroke="currentColor" strokeWidth="1.2" />
        </svg>
      </button>
    </div>
  );
}
