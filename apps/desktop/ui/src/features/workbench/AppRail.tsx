interface Props {
  sidebarCollapsed: boolean;
  onToggleThreads: () => void;
  onOpenSettings: () => void;
}

export function AppRail({
  sidebarCollapsed,
  onToggleThreads,
  onOpenSettings,
}: Props) {
  return (
    <aside className="app-rail" aria-label="应用导航">
      <div className="app-rail-top">
        <div className="app-rail-brand" title="Maju">
          <span>M</span>
        </div>
        <nav className="app-rail-nav" aria-label="主要功能导航">
          <button
            type="button"
            className={`app-rail-item ${sidebarCollapsed ? "" : "is-selected"}`}
            onClick={onToggleThreads}
            title={sidebarCollapsed ? "显示会话列表" : "隐藏会话列表"}
            aria-expanded={!sidebarCollapsed}
            aria-label={sidebarCollapsed ? "显示会话面板" : "隐藏会话面板"}
          >
            <span className="app-rail-icon">
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M5 7.5h14M5 12h10M5 16.5h7" />
              </svg>
            </span>
            <span className="app-rail-label">会话</span>
          </button>
        </nav>
      </div>
      <div className="app-rail-bottom">
        <button
          type="button"
          className="app-rail-item"
          onClick={onOpenSettings}
          title="设置"
          aria-label="打开设置"
        >
          <span className="app-rail-icon">
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.7 1.7 0 0 0 .34 1.88l.04.04a2 2 0 1 1-2.83 2.83l-.04-.04A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 .6V20a2 2 0 1 1-4 0v-.06a1.7 1.7 0 0 0-1-.6 1.7 1.7 0 0 0-1.88.34l-.04.04a2 2 0 1 1-2.83-2.83l.04-.04A1.7 1.7 0 0 0 4.6 15a1.7 1.7 0 0 0-.6-1H4a2 2 0 1 1 0-4h.06a1.7 1.7 0 0 0 .6-1 1.7 1.7 0 0 0-.34-1.88l-.04-.04a2 2 0 1 1 2.83-2.83l.04.04A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-.6V4a2 2 0 1 1 4 0v.06a1.7 1.7 0 0 0 1 .6 1.7 1.7 0 0 0 1.88-.34l.04-.04a2 2 0 1 1 2.83 2.83l-.04.04A1.7 1.7 0 0 0 19.4 9a1.7 1.7 0 0 0 .6 1H20a2 2 0 1 1 0 4h-.06a1.7 1.7 0 0 0-.54 1Z" />
            </svg>
          </span>
          <span className="app-rail-label">设置</span>
        </button>
      </div>
    </aside>
  );
}
