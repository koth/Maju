import { useState, useEffect, useCallback } from "react";
import type { RecentWorkspace } from "../../types";
import { workspaceOpen, workspaceGetRecent, workspaceRemoveRecent } from "../../lib/tauri";
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
    workspaceGetRecent()
      .then((list) => {
        setRecents(list);
        // Auto-open the most recent workspace if it exists
        const first = list.find((r) => r.exists);
        if (first && !autoOpened) {
          setAutoOpened(true);
          setLoading(true);
          workspaceOpen(first.path)
            .then(() => onWorkspaceOpened())
            .catch((e) => {
              setError(String(e));
              setLoading(false);
            });
        }
      })
      .catch(() => {});
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
        <h1 className="welcome-title">KODEX</h1>
        <p className="welcome-subtitle">ACP Coding Editor</p>

        <button
          className="welcome-open-btn"
          onClick={handleOpenFolder}
          disabled={loading}
        >
          {loading ? "Opening..." : "Open Folder"}
        </button>

        {error && <p className="welcome-error">{error}</p>}

        {recents.length > 0 && (
          <div className="welcome-recents">
            <h2 className="welcome-recents-title">Recent Workspaces</h2>
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
                      <span className="recent-missing">not found</span>
                    )}
                  </button>
                  <button
                    className="welcome-remove-btn"
                    onClick={(e) => {
                      e.stopPropagation();
                      handleRemoveRecent(r.path);
                    }}
                    title="Remove from recent"
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
