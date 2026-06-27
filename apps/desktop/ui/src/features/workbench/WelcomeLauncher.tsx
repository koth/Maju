import { useState, useEffect, useCallback } from "react";
import type { AgentSettingsSnapshot, RecentWorkspace, UiSnapshot } from "../../types";
import {
  settingsGetAgentSnapshot,
  settingsSelectAgent,
  settingsSelectAgentProviderProfile,
  startupPerfMark,
  workspaceOpen,
  workspaceGetRecent,
  workspaceRemoveRecent,
  workspaceRestoreOpen,
} from "../../lib/tauri";
import { isMacOS } from "../../lib/platform";
import { open } from "@tauri-apps/plugin-dialog";
import { WindowControls } from "./WindowControls";
import { RemoteOpenPanel } from "./RemoteOpenPanel";
import "./WelcomeLauncher.css";

interface Props {
  onWorkspaceOpened: (snapshot: UiSnapshot) => void;
  onOpenSettings: (options?: WelcomeSettingsOpenOptions) => void;
}

type InitialSetupKind = "codex_byok";

interface WelcomeSettingsOpenOptions {
  startupNotice?: { kind: InitialSetupKind; message?: string | null };
  initialAgentTab?: "codex-acp";
}

export function WelcomeLauncher({ onWorkspaceOpened, onOpenSettings }: Props) {
  const [recents, setRecents] = useState<RecentWorkspace[]>([]);
  const [agentSettings, setAgentSettings] = useState<AgentSettingsSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [setupBusy, setSetupBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [remoteExpanded, setRemoteExpanded] = useState(false);
  const [remoteInitial, setRemoteInitial] = useState<RecentWorkspace["remote"] | null>(null);

  const [autoOpened, setAutoOpened] = useState(false);

  useEffect(() => {
    let disposed = false;

    const loadInitialWorkspaces = async () => {
      try {
        const loadStart = performance.now();
        void startupPerfMark("welcome/load_initial_start", `performance_now=${loadStart.toFixed(1)}`);
        const recentStart = performance.now();
        const recentPromise = workspaceGetRecent();
        void recentPromise.then((list) => {
          void startupPerfMark(
            "welcome/get_recent_end",
            `count=${list.length} duration_ms=${(performance.now() - recentStart).toFixed(1)}`,
          );
          if (!disposed) {
            setRecents(list);
          }
        }).catch(() => undefined);

        if (autoOpened) return;
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
          onWorkspaceOpened(restored);
          return;
        }

        const list = await recentPromise;
        if (disposed) return;
        setRecents(list);
        const first = list.find((r) => r.exists && !r.remote);
        if (first) {
          const openStart = performance.now();
          void startupPerfMark("welcome/open_recent_start", first.path);
          const snapshot = await workspaceOpen(first.path);
          void startupPerfMark(
            "welcome/open_recent_end",
            `duration_ms=${(performance.now() - openStart).toFixed(1)} path=${first.path}`,
          );
          if (!disposed) onWorkspaceOpened(snapshot);
          return;
        }

        setLoading(false);
        const settings = await settingsGetAgentSnapshot().catch(() => null);
        if (disposed) return;
        setAgentSettings(settings);
        const requiredSetup = settings ? setupRecommendationFor(settings) : null;
        if (settings && requiredSetup) {
          setSetupBusy(true);
          try {
            await settingsSelectAgent("codex-acp");
            const nextSnapshot = await settingsSelectAgentProviderProfile("codex", "byok");
            if (disposed) return;
            setAgentSettings(nextSnapshot);
            onOpenSettings(settingsOpenOptionsForSetup(requiredSetup, nextSnapshot));
          } catch (_error) {
            if (!disposed) {
              onOpenSettings(settingsOpenOptionsForSetup(requiredSetup, settings));
            }
          } finally {
            if (!disposed) {
              setSetupBusy(false);
            }
          }
        }
      } catch (e) {
        if (!disposed) {
          const message = String(e);
          setError(message);
          setLoading(false);
          if (isAgentSetupError(message)) {
            onOpenSettings();
          }
        }
      }
    };

    loadInitialWorkspaces();
    return () => {
      disposed = true;
    };
  }, []);

  const handleUseCodexByok = useCallback(async () => {
    setSetupBusy(true);
    setError(null);
    try {
      await settingsSelectAgent("codex-acp");
      const nextSnapshot = await settingsSelectAgentProviderProfile("codex", "byok");
      setAgentSettings(nextSnapshot);
      onOpenSettings({ initialAgentTab: "codex-acp" });
    } catch (e) {
      setError(String(e));
      onOpenSettings({ initialAgentTab: "codex-acp" });
    } finally {
      setSetupBusy(false);
    }
  }, [onOpenSettings]);

  const handleOpenFolder = useCallback(async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected) return;
      setLoading(true);
      setError(null);
      const snapshot = await workspaceOpen(selected as string);
      onWorkspaceOpened(snapshot);
    } catch (e) {
      const message = String(e);
      setError(message);
      setLoading(false);
      if (isAgentSetupError(message)) {
        onOpenSettings(settingsOpenOptionsForCurrentState(agentSettings));
      }
    }
  }, [agentSettings, onOpenSettings, onWorkspaceOpened]);

  const handleOpenRecent = useCallback(
    async (path: string) => {
      try {
        setLoading(true);
        setError(null);
        const recent = recents.find((item) => item.path === path);
        if (recent?.remote) {
          setRemoteInitial(recent.remote);
          setRemoteExpanded(true);
          setLoading(false);
          return;
        }
        const snapshot = await workspaceOpen(path);
        onWorkspaceOpened(snapshot);
      } catch (e) {
        const message = String(e);
        setError(message);
        setLoading(false);
        if (isAgentSetupError(message)) {
          onOpenSettings(settingsOpenOptionsForCurrentState(agentSettings));
        }
      }
    },
    [agentSettings, onOpenSettings, onWorkspaceOpened, recents]
  );

  const handleRemoteWorkspaceOpened = useCallback((snapshot: UiSnapshot) => {
    setRemoteExpanded(false);
    setRemoteInitial(null);
    onWorkspaceOpened(snapshot);
  }, [onWorkspaceOpened]);

  const handleOpenRemotePanel = useCallback(() => {
    setRemoteInitial(null);
    setRemoteExpanded((expanded) => !expanded);
  }, []);

  const handleCancelRemotePanel = useCallback(() => {
    setRemoteExpanded(false);
    setRemoteInitial(null);
  }, []);

  const handleRemoveRecent = useCallback(async (path: string) => {
    await workspaceRemoveRecent(path);
    setRecents((prev) => prev.filter((r) => r.path !== path));
  }, []);

  const folderName = (path: string) => {
    const parts = path.replace(/\\/g, "/").split("/");
    return parts[parts.length - 1] || path;
  };
  const remoteDisplayPath = (remote: RecentWorkspace["remote"]) => {
    if (!remote) return "";
    const port = remote.ssh_port ? `:${remote.ssh_port}` : "";
    return `${remote.ssh_target}${port}:${remote.remote_path}`;
  };
  const setupRecommendation = agentSettings ? setupRecommendationFor(agentSettings) : null;
  const showByokOnboarding = setupRecommendation === "codex_byok";
  const titlebarClassName = `welcome-titlebar ${isMacOS() ? "is-macos" : ""}`;

  return (
    <div className="welcome">
      <div className={titlebarClassName} data-tauri-drag-region>
        <WindowControls />
      </div>
      <div className="welcome-content">
        <div className="welcome-brand">
          <pre className="welcome-ascii">
{` ██╗  ██╗ ██████╗ ██████╗ ███████╗██╗  ██╗
 ██║ ██╔╝██╔═══██╗██╔══██╗██╔════╝╚██╗██╔╝
 █████╔╝ ██║   ██║██║  ██║█████╗   ╚███╔╝ 
 ██╔═██╗ ██║   ██║██║  ██║██╔══╝   ██╔██╗ 
 ██║  ██╗╚██████╔╝██████╔╝███████╗██╔╝ ██╗
 ╚═╝  ╚═╝ ╚═════╝ ╚═════╝ ╚══════╝╚═╝  ╚═╝`}
          </pre>
          <p className="welcome-subtitle">智能体代码编辑器</p>
        </div>

        {showByokOnboarding && (
          <section className="welcome-byok is-required" aria-label="Codex BYOK 引导">
            <div className="welcome-byok-copy">
              <span className="welcome-byok-kicker">开始前需要完成</span>
              <h1>初始化 Codex BYOK</h1>
              <p>
                还没有可用的模型来源。配置 Codex BYOK 后即可打开本地或远程工作区。
              </p>
            </div>
            <div className="welcome-byok-actions">
              <button type="button" className="welcome-open-btn" disabled={setupBusy} onClick={handleUseCodexByok}>
                {setupBusy ? "处理中..." : "设置 Codex BYOK"}
              </button>
              <button type="button" className="welcome-secondary-btn" onClick={() => onOpenSettings(settingsOpenOptionsForSetup("codex_byok", agentSettings ?? undefined))}>
                打开设置
              </button>
            </div>
          </section>
        )}

        <section className="welcome-launcher" aria-label="打开工作区">
          <div className="welcome-launcher-copy">
            <span className="welcome-kicker">选择工作区</span>
            <h1>打开一个工作区</h1>
          </div>
          <div className="welcome-actions">
            <button
              className="welcome-primary-action"
              onClick={handleOpenFolder}
              disabled={loading}
            >
              <span className="welcome-action-icon"><LocalFolderIcon /></span>
              <span>{loading ? "正在打开..." : "打开本地文件夹"}</span>
            </button>
            <button
              type="button"
              className={`welcome-remote-entry ${remoteExpanded ? "is-active" : ""}`}
              onClick={handleOpenRemotePanel}
              aria-expanded={remoteExpanded}
            >
              <span className="welcome-action-icon"><RemoteHostIcon /></span>
              <span>打开远程目录</span>
            </button>
          </div>
        </section>

        {remoteExpanded && (
          <section className="welcome-remote-panel" aria-label="打开远程目录">
            <RemoteOpenPanel
              initialRemote={remoteInitial}
              onWorkspaceOpened={handleRemoteWorkspaceOpened}
              onOpenSettings={() => onOpenSettings()}
              onCancel={handleCancelRemotePanel}
            />
          </section>
        )}

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
                    <span className="recent-path">{r.remote ? remoteDisplayPath(r.remote) : r.path}</span>
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

function isAgentSetupError(message: string) {
  return (
    message.includes("BYOK") ||
    message.includes("API key") ||
    message.includes("请先填写") ||
    message.includes("请先在")
  );
}

function setupRecommendationFor(settings: AgentSettingsSnapshot): InitialSetupKind | null {
  return hasAnyConfiguredProvider(settings) ? null : "codex_byok";
}

function settingsOpenOptionsForSetup(
  kind: InitialSetupKind,
  _settings?: AgentSettingsSnapshot,
): WelcomeSettingsOpenOptions {
  return {
    startupNotice: { kind },
    initialAgentTab: "codex-acp",
  };
}

function settingsOpenOptionsForCurrentState(
  settings: AgentSettingsSnapshot | null,
): WelcomeSettingsOpenOptions | undefined {
  if (!settings) return undefined;
  const recommendation = setupRecommendationFor(settings);
  return recommendation ? settingsOpenOptionsForSetup(recommendation, settings) : undefined;
}

function hasAnyConfiguredProvider(settings: AgentSettingsSnapshot) {
  const codebuddyInstalled = settings.agents.some((agent) => agent.id === "codebuddy" && agent.installed);
  if (codebuddyInstalled) return true;
  return [...settings.codex_acp.profiles, ...settings.claude.profiles].some(
    (profile) => profile.requires_credential && profile.configured,
  );
}

function LocalFolderIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <path d="M2.5 6.2c0-1 .8-1.8 1.8-1.8h3.1l1.4 1.5h6.9c1 0 1.8.8 1.8 1.8v6.5c0 1-.8 1.8-1.8 1.8H4.3c-1 0-1.8-.8-1.8-1.8V6.2Z" />
      <path d="M2.5 8.1h15" />
    </svg>
  );
}

function RemoteHostIcon() {
  return (
    <svg viewBox="0 0 20 20" aria-hidden="true">
      <rect x="3" y="4" width="14" height="10" rx="1.7" />
      <path d="M7.2 16h5.6" />
      <path d="M10 14v2" />
      <path d="M7.2 8.1 5.8 9.5l1.4 1.4" />
      <path d="m12.8 8.1 1.4 1.4-1.4 1.4" />
    </svg>
  );
}
