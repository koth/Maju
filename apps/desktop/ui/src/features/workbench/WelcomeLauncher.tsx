import { useState, useEffect, useCallback } from "react";
import type { AgentCliId, AgentSettingsSnapshot, ClaudeWoaLoginStart, IoaEnvironmentStatus, RecentWorkspace, UiSnapshot } from "../../types";
import {
  settingsCancelClaudeWoaLogin,
  settingsDetectIoaEnvironment,
  settingsGetAgentSnapshot,
  settingsGetClaudeWoaLogin,
  settingsSaveClaudeWoaConfig,
  settingsSelectAgent,
  settingsSelectAgentProviderProfile,
  settingsStartClaudeWoaLogin,
  openExternalUrl,
  startupPerfMark,
  workspaceOpen,
  workspaceGetRecent,
  workspaceRemoveRecent,
  workspaceRestoreOpen,
} from "../../lib/tauri";
import { isMacOS } from "../../lib/platform";
import { open } from "@tauri-apps/plugin-dialog";
import { WindowControls } from "./WindowControls";
import "./WelcomeLauncher.css";

interface Props {
  onWorkspaceOpened: (snapshot: UiSnapshot) => void;
  onOpenSettings: (options?: WelcomeSettingsOpenOptions) => void;
}

type InitialSetupKind = "woa" | "codex_byok";
type WelcomeAgentSettingsTab = Extract<AgentCliId, "codex-acp" | "claude-agent-acp">;

interface WelcomeSettingsOpenOptions {
  startupNotice?: { kind: InitialSetupKind; message?: string | null };
  initialAgentTab?: WelcomeAgentSettingsTab;
}

const DEFAULT_IOA_ENVIRONMENT: IoaEnvironmentStatus = {
  is_company_export_ip: false,
  is_internal: false,
  company_environment: false,
  recommended_setup: "codex_byok",
  detected: false,
  timestamp_ms: 0,
  message: null,
};

const CODEX_BYOK_SOURCE_PROFILE_IDS = new Set(["deepseek", "kimi_code", "xiaomi_mimo"]);
const INTERNAL_SETUP_PROFILE_IDS = new Set(["timiai", "venus"]);

export function WelcomeLauncher({ onWorkspaceOpened, onOpenSettings }: Props) {
  const [recents, setRecents] = useState<RecentWorkspace[]>([]);
  const [agentSettings, setAgentSettings] = useState<AgentSettingsSnapshot | null>(null);
  const [ioaEnvironment, setIoaEnvironment] = useState<IoaEnvironmentStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [woaMessage, setWoaMessage] = useState<string | null>(null);
  const [woaLogin, setWoaLogin] = useState<ClaudeWoaLoginStart | null>(null);
  const [woaBusy, setWoaBusy] = useState(false);
  const [loading, setLoading] = useState(false);

  const [autoOpened, setAutoOpened] = useState(false);

  useEffect(() => {
    let disposed = false;

    const loadInitialWorkspaces = async () => {
      try {
        const loadStart = performance.now();
        void startupPerfMark("welcome/load_initial_start", `performance_now=${loadStart.toFixed(1)}`);
        const recentStart = performance.now();
        const [list, settings, env] = await Promise.all([
          workspaceGetRecent(),
          settingsGetAgentSnapshot().catch(() => null),
          settingsDetectIoaEnvironment().catch((error) => ({
            ...DEFAULT_IOA_ENVIRONMENT,
            message: String(error),
          })),
        ]);
        void startupPerfMark(
          "welcome/get_recent_end",
          `count=${list.length} duration_ms=${(performance.now() - recentStart).toFixed(1)}`,
        );
        if (disposed) return;
        setRecents(list);
        setAgentSettings(settings);
        setIoaEnvironment(env);
        void startupPerfMark(
          "welcome/ioa_env_detect_result",
          `detected=${env.detected} company_environment=${env.company_environment} is_company_export_ip=${env.is_company_export_ip} is_internal=${env.is_internal} recommended_setup=${env.recommended_setup} message=${env.message ?? ""}`,
        );

        if (!autoOpened) {
          setAutoOpened(true);
          const requiredSetup = settings ? setupRecommendationFor(settings, env) : null;
          if (settings && requiredSetup) {
            setLoading(false);
            onOpenSettings(settingsOpenOptionsForSetup(requiredSetup, settings, env));
            return;
          }
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

          const first = list.find((r) => r.exists);
          if (first) {
            const openStart = performance.now();
            void startupPerfMark("welcome/open_recent_start", first.path);
            const snapshot = await workspaceOpen(first.path);
            void startupPerfMark(
              "welcome/open_recent_end",
              `duration_ms=${(performance.now() - openStart).toFixed(1)} path=${first.path}`,
            );
            if (!disposed) onWorkspaceOpened(snapshot);
          } else {
            setLoading(false);
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

  useEffect(() => {
    if (!woaLogin) return;
    const timer = window.setInterval(async () => {
      try {
        const status = await settingsGetClaudeWoaLogin(woaLogin.login_id);
        if (status.state === "succeeded" && status.snapshot) {
          setAgentSettings(status.snapshot);
          setWoaLogin(null);
          setWoaMessage("WOA 登录已完成，可以打开工作区了");
        } else if (status.state !== "pending") {
          setWoaLogin(null);
          setWoaMessage(status.message ?? `WOA 登录${status.state}`);
        }
      } catch (e) {
        setWoaLogin(null);
        setError(String(e));
      }
    }, 2000);
    return () => window.clearInterval(timer);
  }, [woaLogin]);

  const handleStartWoaLogin = useCallback(async (agent?: Extract<AgentCliId, "codex-acp" | "claude-agent-acp">) => {
    setWoaBusy(true);
    setError(null);
    setWoaMessage(null);
    try {
      let nextSettings = agentSettings;
      if (agent) {
        nextSettings = await settingsSelectAgent(agent);
        nextSettings = await settingsSelectAgentProviderProfile(agent === "codex-acp" ? "codex" : "claude", "woa");
        setAgentSettings(nextSettings);
      }
      if (nextSettings && isWoaTokenUsable(nextSettings)) {
        setWoaMessage("WOA 通道已选择，可以打开工作区了");
        return;
      }
      const channel = nextSettings?.claude_woa.channel ?? "default";
      await settingsSaveClaudeWoaConfig({
        channel,
        tokenPath: null,
        availableModels: nextSettings?.settings.claude_woa.available_models ?? [],
      });
      const login = await settingsStartClaudeWoaLogin();
      setWoaLogin(login);
      setWoaMessage("在浏览器完成验证后，Kodex 会自动继续");
    } catch (e) {
      setError(String(e));
    } finally {
      setWoaBusy(false);
    }
  }, [agentSettings]);

  const handleUseCodexByok = useCallback(async () => {
    setWoaBusy(true);
    setError(null);
    setWoaMessage(null);
    try {
      await settingsSelectAgent("codex-acp");
      const nextSnapshot = await settingsSelectAgentProviderProfile("codex", "byok");
      setAgentSettings(nextSnapshot);
      onOpenSettings({ initialAgentTab: "codex-acp" });
    } catch (e) {
      setError(String(e));
      onOpenSettings({ initialAgentTab: "codex-acp" });
    } finally {
      setWoaBusy(false);
    }
  }, [onOpenSettings]);

  const handleCancelWoaLogin = useCallback(async () => {
    if (!woaLogin) return;
    setWoaBusy(true);
    try {
      await settingsCancelClaudeWoaLogin(woaLogin.login_id);
      setWoaLogin(null);
      setWoaMessage("WOA 登录已取消");
    } catch (e) {
      setError(String(e));
    } finally {
      setWoaBusy(false);
    }
  }, [woaLogin]);

  const handleOpenWoaLoginUrl = useCallback(async () => {
    if (!woaLogin) return;
    try {
      await openExternalUrl(woaLogin.verification_uri_complete ?? woaLogin.verification_uri);
    } catch (e) {
      setError(String(e));
    }
  }, [woaLogin]);

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
        onOpenSettings(settingsOpenOptionsForCurrentState(agentSettings, ioaEnvironment));
      }
    }
  }, [agentSettings, ioaEnvironment, onOpenSettings, onWorkspaceOpened]);

  const handleOpenRecent = useCallback(
    async (path: string) => {
      try {
        setLoading(true);
        setError(null);
        const snapshot = await workspaceOpen(path);
        onWorkspaceOpened(snapshot);
      } catch (e) {
        const message = String(e);
        setError(message);
        setLoading(false);
        if (isAgentSetupError(message)) {
          onOpenSettings(settingsOpenOptionsForCurrentState(agentSettings, ioaEnvironment));
        }
      }
    },
    [agentSettings, ioaEnvironment, onOpenSettings, onWorkspaceOpened]
  );

  const handleRemoveRecent = useCallback(async (path: string) => {
    await workspaceRemoveRecent(path);
    setRecents((prev) => prev.filter((r) => r.path !== path));
  }, []);

  const folderName = (path: string) => {
    const parts = path.replace(/\\/g, "/").split("/");
    return parts[parts.length - 1] || path;
  };
  const setupRecommendation = agentSettings && ioaEnvironment
    ? setupRecommendationFor(agentSettings, ioaEnvironment)
    : null;
  const showWoaOnboarding = setupRecommendation === "woa";
  const showByokOnboarding = setupRecommendation === "codex_byok";
  const mustCompleteSetup = agentSettings && ioaEnvironment
    ? shouldPauseWorkspaceAutoOpen(agentSettings, ioaEnvironment)
    : false;
  const woaTokenMessage = agentSettings?.claude_woa.token.message;
  const woaOnboardingKicker = "内网环境 · 开始前需要完成";
  const titlebarClassName = `welcome-titlebar ${isMacOS() ? "is-macos" : ""}`;

  return (
    <div className="welcome">
      <div className={titlebarClassName} data-tauri-drag-region>
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

        {showWoaOnboarding && (
          <section className={`welcome-woa ${mustCompleteSetup ? "is-required" : ""}`} aria-label="内网通道引导">
            <div className="welcome-woa-copy">
              <span className="welcome-woa-kicker">{mustCompleteSetup ? woaOnboardingKicker : "内网环境 · 推荐先完成"}</span>
              <h1>初始化内网通道</h1>
              <p>
                当前网络适合使用内网通道。完成 WOA、TimiAI 或 Venus 任意一个配置后即可打开工作区。
              </p>
            </div>
            {woaTokenMessage && <div className="welcome-woa-status">{woaTokenMessage}</div>}
            {woaLogin && (
              <div className="welcome-woa-code">
                <span>
                  打开{" "}
                  <button type="button" className="welcome-link-btn" onClick={handleOpenWoaLoginUrl}>
                    {woaLogin.verification_uri_complete ?? woaLogin.verification_uri}
                  </button>
                </span>
                <span>输入 <code>{woaLogin.user_code}</code></span>
              </div>
            )}
            {woaMessage && <div className="welcome-woa-message">{woaMessage}</div>}
            <div className="welcome-woa-actions">
              {woaLogin ? (
                <button type="button" className="welcome-secondary-btn" disabled={woaBusy} onClick={handleCancelWoaLogin}>
                  取消登录
                </button>
              ) : (
                <>
                  <button type="button" className="welcome-open-btn" disabled={woaBusy} onClick={() => handleStartWoaLogin("claude-agent-acp")}>
                    {woaBusy ? "处理中..." : "使用 Claude WOA"}
                  </button>
                  <button type="button" className="welcome-secondary-btn" disabled={woaBusy} onClick={() => handleStartWoaLogin("codex-acp")}>
                    使用 Codex WOA
                  </button>
                </>
              )}
              <button type="button" className="welcome-secondary-btn" onClick={() => onOpenSettings(settingsOpenOptionsForSetup("woa", agentSettings ?? undefined))}>
                打开设置
              </button>
            </div>
          </section>
        )}

        {showByokOnboarding && (
          <section className="welcome-woa is-required" aria-label="Codex BYOK 引导">
            <div className="welcome-woa-copy">
              <span className="welcome-woa-kicker">{ioaEnvironment?.detected ? "外网环境 · 开始前需要完成" : "环境检测失败 · 已按外网处理"}</span>
              <h1>初始化 Codex BYOK</h1>
              <p>
                当前网络不推荐 WOA 登录。请先给 Codex 配置一个自带 API key 的模型来源。
              </p>
            </div>
            {ioaEnvironment?.message && <div className="welcome-woa-status">{ioaEnvironment.message}</div>}
            <div className="welcome-woa-actions">
              <button type="button" className="welcome-open-btn" disabled={woaBusy} onClick={handleUseCodexByok}>
                {woaBusy ? "处理中..." : "设置 Codex BYOK"}
              </button>
              <button type="button" className="welcome-secondary-btn" onClick={() => onOpenSettings(settingsOpenOptionsForSetup("codex_byok", agentSettings ?? undefined))}>
                打开设置
              </button>
            </div>
          </section>
        )}

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

function isAgentSetupError(message: string) {
  return (
    message.includes("BYOK") ||
    message.includes("API key") ||
    message.includes("WOA") ||
    message.includes("请先填写") ||
    message.includes("请先在")
  );
}

function setupRecommendationFor(
  settings: AgentSettingsSnapshot,
  env: IoaEnvironmentStatus,
): InitialSetupKind | null {
  if (shouldPreferWoaSetup(env)) {
    return isInternalSetupReady(settings) ? null : "woa";
  }
  return hasConfiguredCodexByok(settings) ? null : "codex_byok";
}

function shouldPreferWoaSetup(env: IoaEnvironmentStatus) {
  return env.company_environment || env.recommended_setup === "woa";
}

function shouldPauseWorkspaceAutoOpen(
  settings: AgentSettingsSnapshot,
  env: IoaEnvironmentStatus,
) {
  return setupRecommendationFor(settings, env) !== null;
}

function settingsOpenOptionsForSetup(
  kind: InitialSetupKind,
  settings?: AgentSettingsSnapshot,
  env?: IoaEnvironmentStatus,
): WelcomeSettingsOpenOptions {
  return {
    startupNotice: {
      kind,
      ...(env?.message ? { message: env.message } : {}),
    },
    initialAgentTab: kind === "codex_byok" ? "codex-acp" : preferredWoaSettingsTab(settings),
  };
}

function settingsOpenOptionsForCurrentState(
  settings: AgentSettingsSnapshot | null,
  env: IoaEnvironmentStatus | null,
): WelcomeSettingsOpenOptions | undefined {
  if (!settings || !env) return undefined;
  const recommendation = setupRecommendationFor(settings, env);
  return recommendation ? settingsOpenOptionsForSetup(recommendation, settings, env) : undefined;
}

function preferredWoaSettingsTab(settings?: AgentSettingsSnapshot): WelcomeAgentSettingsTab {
  return settings?.settings.selected_agent === "codex-acp" ? "codex-acp" : "claude-agent-acp";
}

function isInternalSetupReady(settings: AgentSettingsSnapshot) {
  return isInternalWoaReady(settings) || hasConfiguredInternalProvider(settings);
}

function isInternalWoaReady(settings: AgentSettingsSnapshot) {
  const selectedClaudeWoa =
    settings.settings.selected_agent === "claude-agent-acp" &&
    settings.claude_woa.selected_profile_id === "woa";
  const selectedCodexWoa =
    settings.settings.selected_agent === "codex-acp" &&
    settings.codex_acp.selected_profile_id === "woa";
  return (selectedClaudeWoa || selectedCodexWoa) && isWoaTokenUsable(settings);
}

function isWoaTokenUsable(settings: AgentSettingsSnapshot) {
  return settings.claude_woa.token.exists && !settings.claude_woa.token.malformed;
}

function hasConfiguredInternalProvider(settings: AgentSettingsSnapshot) {
  return [...settings.codex_acp.profiles, ...settings.claude_woa.profiles].some(
    (profile) => INTERNAL_SETUP_PROFILE_IDS.has(profile.id) && profile.configured,
  );
}

function hasConfiguredCodexByok(settings: AgentSettingsSnapshot) {
  return settings.codex_acp.profiles.some(
    (profile) => CODEX_BYOK_SOURCE_PROFILE_IDS.has(profile.id) && profile.configured,
  );
}
