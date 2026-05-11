import { useState, useEffect, useCallback } from "react";
import type { RecentWorkspace } from "../../types";
import { startupPerfMark, workspaceOpen, workspaceGetRecent, workspaceRemoveRecent, workspaceRestoreOpen } from "../../lib/tauri";
import { open } from "@tauri-apps/plugin-dialog";
import { WindowControls } from "./WindowControls";
import "./WelcomeLauncher.css";

interface Props {
  onWorkspaceOpened: () => void;
}

export function WelcomeLauncher({ onWorkspaceOpened }: Props) {
  const [recents, setRecents] = useState<RecentWorkspace[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const [autoOpened, setAutoOpened] = useState(false);

  useEffect(() => {
    let disposed = false;

    const loadInitialWorkspaces = async () => {
      try {
        const loadStart = performance.now();
        void startupPerfMark("welcome/load_initial_start", `performance_now=${loadStart.toFixed(1)}`);
        const recentStart = performance.now();
        const list = await workspaceGetRecent();
        void startupPerfMark(
          "welcome/get_recent_end",
          `count=${list.length} duration_ms=${(performance.now() - recentStart).toFixed(1)}`,
        );
        if (disposed) return;
        setRecents(list);

        if (!autoOpened) {
          setAutoOpened(true);
          setLoading(true);
          const restoreStart = performance.now();
          void startupPerfMark("welcome/restore_open_start", "");
          const restored = await workspaceRestoreOpen();
          void startupPerfMark(
            "welcome/restore_open_end",
            `restored=${Boolean(restored)} duration_ms=${(performance.now() - restoreStart).toFixed(1)}`,
          );
          if (disposed) return;
          if (restored) {
            void startupPerfMark(
              "welcome/on_workspace_opened_restore",
              `total_duration_ms=${(performance.now() - loadStart).toFixed(1)}`,
            );
            onWorkspaceOpened();
            return;
          }

          const first = list.find((r) => r.exists);
          if (first) {
            const openStart = performance.now();
            void startupPerfMark("welcome/open_recent_start", first.path);
            await workspaceOpen(first.path);
            void startupPerfMark(
              "welcome/open_recent_end",
              `duration_ms=${(performance.now() - openStart).toFixed(1)} path=${first.path}`,
            );
            if (!disposed) onWorkspaceOpened();
          } else {
            setLoading(false);
          }
        }
      } catch (e) {
        if (!disposed) {
          setError(String(e));
          setLoading(false);
        }
      }
    };

    loadInitialWorkspaces();
    return () => {
      disposed = true;
    };
  }, []);

  const handleOpenFolder = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected) return;
      setLoading(true);
      setError(null);
      await workspaceOpen(selected as string);
      onWorkspaceOpened();
    } catch (e) {
      setError(String(e));
      setLoading(false);
    }
  }, [onWorkspaceOpened]);

  const handleOpenRecent = useCallback(
    async (path: string) => {
      try {
        setLoading(true);
        setError(null);
        await workspaceOpen(path);
        onWorkspaceOpened();
      } catch (e) {
        setError(String(e));
        setLoading(false);
      }
    },
    [onWorkspaceOpened]
  );

  const handleRemoveRecent = useCallback(async (path: string) => {
    await workspaceRemoveRecent(path);
    setRecents((prev) => prev.filter((r) => r.path !== path));
  }, []);

  const folderName = (path: string) => {
    const parts = path.replace(/\\/g, "/").split("/");
    return parts[parts.length - 1] || path;
  };

  return (
    <div className="welcome">
      <div className="welcome-titlebar" data-tauri-drag-region>
        <WindowControls />
      </div>
      <div className="welcome-content">
        <pre className="welcome-ascii">
{` ██╗  ██╗ ██████╗ ██████╗ ███████╗██╗  ██╗
 ██║ ██╔╝██╔═══██╗██╔══██╗██╔════╝╚██╗██╔╝
 █████╔╝ ██║   ██║██║  ██║█████╗   ╚███╔╝ 
 ██╔═██╗ ██║   ██║██║  ██║██╔══╝   ██╔██╗ 
 ██║  ██╗╚██████╔╝██████╔╝███████╗██╔╝ ██╗
 ╚═╝  ╚═╝ ╚═════╝ ╚═════╝ ╚══════╝╚═╝  ╚═╝`}
        </pre>
        <p className="welcome-subtitle">智能体代码编辑器</p>

        <button
          className="welcome-open-btn"
          onClick={handleOpenFolder}
          disabled={loading}
        >
          {loading ? "正在打开..." : "打开文件夹"}
        </button>

        {error && <p className="welcome-error">{error}</p>}

        {recents.length > 0 && (
          <div className="welcome-recents">
            <h2 className="welcome-recents-title">近期工作区</h2>
            <ul className="welcome-recents-list">
              {recents.map((r) => (
                <li
                  key={r.path}
                  className={`welcome-recent-item ${!r.exists ? "not-found" : ""}`}
                >
                  <button
                    className="welcome-recent-btn"
                    onClick={() => handleOpenRecent(r.path)}
                    disabled={!r.exists || loading}
                  >
                    <span className="recent-name">{folderName(r.path)}</span>
                    <span className="recent-path">{r.path}</span>
                    {!r.exists && (
                      <span className="recent-missing">未找到</span>
                    )}
                  </button>
                  <button
                    className="welcome-remove-btn"
                    onClick={(e) => {
                      e.stopPropagation();
                      handleRemoveRecent(r.path);
                    }}
                    title="从最近列表中移除"
                  >
                    x
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
