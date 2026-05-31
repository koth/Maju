import { useCallback, useEffect, useState } from "react";
import type {
  AgentCliId,
  AgentInstallResult,
  AgentProviderProfile,
  AgentSettingsSnapshot,
  AppTheme,
  ClaudeWoaChannel,
  ClaudeWoaLoginStart,
  LspSettingsSnapshot,
  LspServerConfigInput,
} from "../../types";
import {
  openExternalUrl,
  settingsDetectAgents,
  settingsGetAgentSnapshot,
  settingsGetLspSnapshot,
  settingsInstallAgent,
  settingsProbeLspServer,
  settingsResetLspServer,
  settingsSaveAgentProviderSecret,
  settingsSaveClaudeWoaConfig,
  settingsSaveLspServer,
  settingsStartClaudeWoaLogin,
  settingsGetClaudeWoaLogin,
  settingsCancelClaudeWoaLogin,
  settingsRefreshClaudeWoaToken,
  settingsSelectAgentProviderProfile,
  settingsSelectAgent,
  settingsSelectTheme,
} from "../../lib/tauri";
import {
  checkForAppUpdate,
  getCurrentAppVersion,
  installPendingAppUpdate,
  type AppUpdateInfo,
  type AppUpdateProgress,
} from "../../lib/updater";
import { APP_THEMES, applyAppTheme } from "../../theme";
import "./SettingsPage.css";

export type AgentSettingsTab = Extract<AgentCliId, "codebuddy" | "codex-acp" | "claude-agent-acp">;
type UpdateStatus = "idle" | "checking" | "up-to-date" | "available" | "installing" | "installed" | "error";

export interface SettingsStartupNotice {
  kind: "woa" | "codex_byok";
  message?: string | null;
}

interface Props {
  onBack: () => void;
  onThemeChange?: (theme: AppTheme) => void;
  startupNotice?: SettingsStartupNotice | null;
  initialAgentTab?: AgentSettingsTab;
  onStartupNoticeDismissed?: () => void;
}

const AGENT_SETTINGS_TABS: Array<{ id: AgentSettingsTab; label: string }> = [
  { id: "claude-agent-acp", label: "Claude" },
  { id: "codex-acp", label: "Codex" },
  { id: "codebuddy", label: "CodeBuddy" },
];

function modelListLabel(models: string[]): string {
  return `模型：${models.join("、")}`;
}

function renderModelChip(models?: string[] | null) {
  if (!models?.length) return null;
  const label = modelListLabel(models);
  return (
    <span className="settings-model-chip" title={label} aria-label={label} tabIndex={0}>
      模型
    </span>
  );
}

export function SettingsPage({
  initialAgentTab,
  startupNotice,
  onBack,
  onStartupNoticeDismissed,
  onThemeChange,
}: Props) {
  const [activePane, setActivePane] = useState<"general" | "lsp">("general");
  const [activeAgentTab, setActiveAgentTab] = useState<AgentSettingsTab>(initialAgentTab ?? "claude-agent-acp");
  const [visibleStartupNotice, setVisibleStartupNotice] = useState<SettingsStartupNotice | null>(startupNotice ?? null);
  const [snapshot, setSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [lspSnapshot, setLspSnapshot] = useState<LspSettingsSnapshot | null>(null);
  const [lspDrafts, setLspDrafts] = useState<Record<string, LspServerConfigInput>>({});
  const [loading, setLoading] = useState(true);
  const [busyAgent, setBusyAgent] = useState<AgentCliId | null>(null);
  const [busyCodexAcp, setBusyCodexAcp] = useState(false);
  const [busyTheme, setBusyTheme] = useState<AppTheme | null>(null);
  const [busyLsp, setBusyLsp] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lspError, setLspError] = useState<string | null>(null);
  const [installResult, setInstallResult] = useState<AgentInstallResult | null>(null);
  const [probeMessages, setProbeMessages] = useState<Record<string, string>>({});
  const [codexProfileId, setCodexProfileId] = useState("venus");
  const [byokProfileId, setByokProfileId] = useState("deepseek");
  const [byokProfileInitialized, setByokProfileInitialized] = useState(false);
  const [codexVenusApiKey, setCodexVenusApiKey] = useState("");
  const [codexAcpApiKey, setCodexAcpApiKey] = useState("");
  const [timiAiApiKey, setTimiAiApiKey] = useState("");
  const [codexAcpMessage, setCodexAcpMessage] = useState<string | null>(null);
  const [codexAcpMessageTarget, setCodexAcpMessageTarget] = useState<"channel" | "byok">("channel");
  const [claudeProfileId, setClaudeProfileId] = useState("byok");
  const [claudeVenusApiKey, setClaudeVenusApiKey] = useState("");
  const [claudeWoaChannel, setClaudeWoaChannel] = useState<ClaudeWoaChannel>("default");
  const [claudeWoaModelsText, setClaudeWoaModelsText] = useState("");
  const [claudeWoaLogin, setClaudeWoaLogin] = useState<ClaudeWoaLoginStart | null>(null);
  const [claudeWoaMessage, setClaudeWoaMessage] = useState<string | null>(null);
  const [busyClaudeWoa, setBusyClaudeWoa] = useState(false);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>("idle");
  const [updateInfo, setUpdateInfo] = useState<AppUpdateInfo | null>(null);
  const [updateMessage, setUpdateMessage] = useState<string | null>(null);
  const [updateProgress, setUpdateProgress] = useState<AppUpdateProgress | null>(null);

  const applyLspSnapshot = useCallback((nextSnapshot: LspSettingsSnapshot) => {
    setLspSnapshot(nextSnapshot);
    setLspDrafts(Object.fromEntries(nextSnapshot.servers.map((server) => [
      server.languageId,
      {
        languageId: server.languageId,
        enabled: server.enabled,
        command: server.command,
        args: server.args,
      },
    ])));
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    setLspError(null);
    try {
      const [nextSnapshot, nextLspSnapshot] = await Promise.all([
        settingsGetAgentSnapshot(),
        settingsGetLspSnapshot(),
      ]);
      setSnapshot(nextSnapshot);
      applyLspSnapshot(nextLspSnapshot);
      onThemeChange?.(applyAppTheme(nextSnapshot.settings.theme));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [applyLspSnapshot, onThemeChange]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    let cancelled = false;
    getCurrentAppVersion()
      .then((version) => {
        if (!cancelled) {
          setAppVersion(version);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setAppVersion(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (initialAgentTab) {
      setActivePane("general");
      setActiveAgentTab(initialAgentTab);
    }
  }, [initialAgentTab]);

  useEffect(() => {
    setVisibleStartupNotice(startupNotice ?? null);
  }, [startupNotice?.kind]);

  useEffect(() => {
    if (initialAgentTab) return;
    const selectedAgent = snapshot?.settings.selected_agent;
    if (selectedAgent === "codebuddy" || selectedAgent === "codex-acp" || selectedAgent === "claude-agent-acp") {
      setActiveAgentTab(selectedAgent);
    }
  }, [initialAgentTab, snapshot?.settings.selected_agent]);

  useEffect(() => {
    if (snapshot?.codex_acp.selected_profile_id) {
      setCodexProfileId(snapshot.codex_acp.selected_profile_id);
    }
  }, [snapshot?.codex_acp.selected_profile_id]);

  useEffect(() => {
    if (!snapshot || byokProfileInitialized) return;
    const byokProfiles = snapshot.codex_acp.profiles.filter((profile) =>
      profile.requires_credential && profile.id !== "venus" && profile.id !== "timiai",
    );
    const selected = snapshot.codex_acp.selected_profile_id;
    if (selected !== "default" && selected !== "venus" && selected !== "timiai" && selected !== "byok") {
      setByokProfileId(selected);
    } else if (selected === "byok") {
      setByokProfileId(byokProfiles.find((profile) => profile.configured)?.id ?? byokProfiles[0]?.id ?? "deepseek");
    } else if (byokProfiles[0]) {
      setByokProfileId(byokProfiles[0].id);
    }
    setByokProfileInitialized(true);
  }, [byokProfileInitialized, snapshot]);

  useEffect(() => {
    if (!snapshot) return;
    setClaudeProfileId(snapshot.claude_woa.selected_profile_id);
    setClaudeWoaChannel(snapshot.claude_woa.channel);
    setClaudeWoaModelsText(snapshot.settings.claude_woa.available_models.join("\n"));
  }, [snapshot]);

  useEffect(() => {
    if (!claudeWoaLogin) return;
    const timer = window.setInterval(async () => {
      try {
        const status = await settingsGetClaudeWoaLogin(claudeWoaLogin.login_id);
        if (status.state === "succeeded" && status.snapshot) {
          setSnapshot(status.snapshot);
          setClaudeWoaLogin(null);
          setClaudeWoaMessage("WOA 登录已完成");
        } else if (status.state !== "pending") {
          setClaudeWoaLogin(null);
          setClaudeWoaMessage(status.message ?? `WOA 登录${status.state}`);
        }
      } catch (e) {
        setClaudeWoaLogin(null);
        setError(String(e));
      }
    }, 2000);
    return () => window.clearInterval(timer);
  }, [claudeWoaLogin]);

  const dismissStartupNotice = useCallback(() => {
    setVisibleStartupNotice(null);
    onStartupNoticeDismissed?.();
  }, [onStartupNoticeDismissed]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (visibleStartupNotice) {
        dismissStartupNotice();
        return;
      }
      onBack();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [dismissStartupNotice, onBack, visibleStartupNotice]);

  const handleDetect = useCallback(async () => {
    setError(null);
    try {
      setSnapshot(await settingsDetectAgents());
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const handleSelect = useCallback(async (agent: AgentCliId) => {
    setBusyAgent(agent);
    setError(null);
    try {
      setSnapshot(await settingsSelectAgent(agent));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyAgent(null);
    }
  }, []);

  const handleThemeSelect = useCallback(async (theme: AppTheme) => {
    setBusyTheme(theme);
    setError(null);
    try {
      const nextSnapshot = await settingsSelectTheme(theme);
      setSnapshot(nextSnapshot);
      onThemeChange?.(applyAppTheme(nextSnapshot.settings.theme));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyTheme(null);
    }
  }, [onThemeChange]);

  const handleCheckForUpdate = useCallback(async () => {
    setUpdateStatus("checking");
    setUpdateInfo(null);
    setUpdateProgress(null);
    setUpdateMessage(null);
    try {
      const nextUpdate = await checkForAppUpdate();
      if (!nextUpdate) {
        setUpdateStatus("up-to-date");
        setUpdateMessage("当前已是最新版本");
        return;
      }
      setAppVersion(nextUpdate.currentVersion);
      setUpdateInfo(nextUpdate);
      setUpdateStatus("available");
      setUpdateMessage(`发现新版本 ${nextUpdate.version}`);
    } catch (e) {
      setUpdateStatus("error");
      setUpdateMessage(String(e));
    }
  }, []);

  const handleInstallUpdate = useCallback(async () => {
    if (!updateInfo) return;
    setUpdateStatus("installing");
    setUpdateProgress(null);
    setUpdateMessage(`正在安装 ${updateInfo.version}`);
    try {
      await installPendingAppUpdate((progress) => {
        setUpdateProgress(progress);
        setUpdateMessage(formatUpdateProgress(progress));
      });
      setUpdateStatus("installed");
      setUpdateMessage("更新已安装，正在重启");
    } catch (e) {
      setUpdateStatus("error");
      setUpdateMessage(String(e));
    }
  }, [updateInfo]);

  const handleInstall = useCallback(async (agent: AgentCliId) => {
    setBusyAgent(agent);
    setError(null);
    setInstallResult(null);
    try {
      const result = await settingsInstallAgent(agent);
      setInstallResult(result);
      setSnapshot(result.snapshot);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyAgent(null);
    }
  }, []);

  const handleSaveByokProviderKey = useCallback(async () => {
    const key = codexAcpApiKey.trim();
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("byok");
    if (!byokProfileId) {
      setError("请选择 BYOK 模型来源");
      return;
    }
    if (!key) {
      setError("API key 不能为空");
      return;
    }
    setBusyCodexAcp(true);
    try {
      const codexSnapshot = await settingsSaveAgentProviderSecret("codex", byokProfileId, key);
      const nextSnapshot = await settingsSaveAgentProviderSecret("claude", byokProfileId, key);
      setSnapshot({
        ...nextSnapshot,
        codex_acp: codexSnapshot.codex_acp,
        settings: {
          ...nextSnapshot.settings,
          codex_connection_mode: codexSnapshot.settings.codex_connection_mode,
          selected_codex_provider_profile_id: codexSnapshot.settings.selected_codex_provider_profile_id,
        },
      });
      setCodexAcpApiKey("");
      setCodexAcpMessageTarget("byok");
      setCodexAcpMessage(`${providerLabel(codexSnapshot.codex_acp.profiles, byokProfileId)} API key 已更新，后续新建会话生效`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCodexAcp(false);
    }
  }, [byokProfileId, codexAcpApiKey]);

  const handleSelectCodexChannel = useCallback(async (channel: "default" | "venus" | "woa" | "timiai" | "byok") => {
    const byokProfiles = snapshot?.codex_acp.profiles.filter((profile) =>
      profile.requires_credential && profile.id !== "venus" && profile.id !== "timiai",
    ) ?? [];
    const selectedByokProfileId = byokProfiles.find((profile) => profile.id === byokProfileId)?.id
      ?? (codexProfileId !== "default" && codexProfileId !== "venus" && codexProfileId !== "woa" && codexProfileId !== "timiai" && codexProfileId !== "byok" ? codexProfileId : undefined)
      ?? byokProfiles.find((profile) => profile.configured)?.id
      ?? byokProfiles[0]?.id;
    const nextProfileId =
      channel === "default"
        ? "default"
        : channel === "venus"
          ? "venus"
          : channel === "woa"
            ? "woa"
            : channel === "timiai"
              ? "timiai"
              : "byok";
    if (!nextProfileId || snapshot?.codex_acp.selected_profile_id === nextProfileId) return;
    setBusyCodexAcp(true);
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("channel");
    try {
      const nextSnapshot = await settingsSelectAgentProviderProfile("codex", nextProfileId);
      setSnapshot(nextSnapshot);
      setCodexProfileId(nextProfileId);
      if (channel === "byok") {
        setByokProfileId(selectedByokProfileId ?? byokProfileId);
      }
      setCodexAcpApiKey("");
      setCodexVenusApiKey("");
      setTimiAiApiKey("");
      setCodexAcpMessageTarget("channel");
      setCodexAcpMessage(`Codex 通道已切换到 ${channel === "default" ? "默认" : channel === "venus" ? "Venus" : channel === "woa" ? "WOA" : channel === "timiai" ? "TimiAI" : "BYOK"}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCodexAcp(false);
    }
  }, [byokProfileId, codexProfileId, snapshot?.codex_acp.profiles, snapshot?.codex_acp.selected_profile_id]);

  const handleSaveCodexVenusKey = useCallback(async () => {
    const key = codexVenusApiKey.trim();
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("channel");
    if (!key) {
      setError("API key 不能为空");
      return;
    }
    setBusyCodexAcp(true);
    try {
      const nextSnapshot = await settingsSaveAgentProviderSecret("codex", "venus", key);
      setSnapshot(nextSnapshot);
      setCodexProfileId("venus");
      setCodexVenusApiKey("");
      setCodexAcpMessageTarget("channel");
      setCodexAcpMessage("Venus API key 已保存，Codex 通道已切换到 Venus");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCodexAcp(false);
    }
  }, [codexVenusApiKey]);

  const handleSaveTimiAiKey = useCallback(async (family: "codex" | "claude") => {
    const key = timiAiApiKey.trim();
    setError(null);
    setCodexAcpMessage(null);
    if (family === "codex") {
      setCodexAcpMessageTarget("channel");
    }
    setClaudeWoaMessage(null);
    if (!key) {
      setError("API key 不能为空");
      return;
    }
    if (family === "codex") {
      setBusyCodexAcp(true);
    } else {
      setBusyClaudeWoa(true);
    }
    try {
      const nextSnapshot = await settingsSaveAgentProviderSecret(family, "timiai", key);
      setSnapshot(nextSnapshot);
      setTimiAiApiKey("");
      if (family === "codex") {
        setCodexProfileId("timiai");
        setCodexAcpMessageTarget("channel");
        setCodexAcpMessage("TimiAI key 已保存，Codex / Claude 可共用");
      } else {
        setClaudeProfileId("timiai");
        setClaudeWoaMessage("TimiAI key 已保存，Codex / Claude 可共用");
      }
    } catch (e) {
      setError(String(e));
    } finally {
      if (family === "codex") {
        setBusyCodexAcp(false);
      } else {
        setBusyClaudeWoa(false);
      }
    }
  }, [timiAiApiKey]);

  const handleSelectClaudeChannel = useCallback(async (channel: "woa" | "venus" | "timiai" | "byok") => {
    const byokProfiles = snapshot?.claude_woa.profiles.filter((profile) =>
      profile.requires_credential && profile.id !== "venus" && profile.id !== "timiai",
    ) ?? [];
    const nextProfileId =
      channel === "woa"
        ? "woa"
        : channel === "venus"
          ? "venus"
          : channel === "timiai"
            ? "timiai"
            : claudeProfileId !== "woa" && claudeProfileId !== "venus" && claudeProfileId !== "timiai"
              ? claudeProfileId
              : byokProfiles.find((profile) => profile.configured)?.id ?? byokProfiles[0]?.id;
    const normalizedNextProfileId = channel === "byok" ? "byok" : nextProfileId;
    if (!normalizedNextProfileId || snapshot?.claude_woa.selected_profile_id === normalizedNextProfileId) return;
    setBusyClaudeWoa(true);
    setError(null);
    setClaudeWoaMessage(null);
    try {
      const nextSnapshot = await settingsSelectAgentProviderProfile("claude", normalizedNextProfileId);
      setSnapshot(nextSnapshot);
      setClaudeProfileId(normalizedNextProfileId);
      setClaudeVenusApiKey("");
      setTimiAiApiKey("");
      setClaudeWoaMessage(`Claude 通道已切换到 ${channel === "woa" ? "WOA" : channel === "venus" ? "Venus" : channel === "timiai" ? "TimiAI" : "BYOK"}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyClaudeWoa(false);
    }
  }, [claudeProfileId, snapshot?.claude_woa.profiles, snapshot?.claude_woa.selected_profile_id]);

  const handleSaveClaudeVenusKey = useCallback(async () => {
    const key = claudeVenusApiKey.trim();
    setError(null);
    setClaudeWoaMessage(null);
    if (!key) {
      setError("API key 不能为空");
      return;
    }
    setBusyClaudeWoa(true);
    try {
      await settingsSaveAgentProviderSecret("claude", "venus", key);
      const nextSnapshot = await settingsSelectAgentProviderProfile("claude", "venus");
      setSnapshot(nextSnapshot);
      setClaudeProfileId("venus");
      setClaudeVenusApiKey("");
      setClaudeWoaMessage("Venus API key 已保存，Claude 通道已切换到 Venus");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyClaudeWoa(false);
    }
  }, [claudeVenusApiKey]);

  const handleSaveClaudeWoaConfig = useCallback(async (channel = claudeWoaChannel) => {
    setBusyClaudeWoa(true);
    setError(null);
    setClaudeWoaMessage(null);
    try {
      const nextSnapshot = await settingsSaveClaudeWoaConfig({
        channel,
        tokenPath: null,
        availableModels: parseClaudeWoaModels(claudeWoaModelsText),
      });
      setSnapshot(nextSnapshot);
      setClaudeWoaMessage("Claude WOA 配置已保存");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyClaudeWoa(false);
    }
  }, [claudeWoaChannel, claudeWoaModelsText]);

  const handleStartClaudeWoaLogin = useCallback(async () => {
    setBusyClaudeWoa(true);
    setError(null);
    setClaudeWoaMessage(null);
    try {
      await settingsSaveClaudeWoaConfig({
        channel: claudeWoaChannel,
        tokenPath: null,
        availableModels: parseClaudeWoaModels(claudeWoaModelsText),
      });
      const login = await settingsStartClaudeWoaLogin();
      setClaudeWoaLogin(login);
      setClaudeWoaMessage(`打开 ${login.verification_uri} 并输入 ${login.user_code}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyClaudeWoa(false);
    }
  }, [claudeWoaChannel, claudeWoaModelsText]);

  const handleCancelClaudeWoaLogin = useCallback(async () => {
    if (!claudeWoaLogin) return;
    setBusyClaudeWoa(true);
    try {
      await settingsCancelClaudeWoaLogin(claudeWoaLogin.login_id);
      setClaudeWoaLogin(null);
      setClaudeWoaMessage("WOA 登录已取消");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyClaudeWoa(false);
    }
  }, [claudeWoaLogin]);

  const handleRefreshClaudeWoaToken = useCallback(async () => {
    setBusyClaudeWoa(true);
    setError(null);
    setClaudeWoaMessage(null);
    try {
      const nextSnapshot = await settingsRefreshClaudeWoaToken();
      setSnapshot(nextSnapshot);
      setClaudeWoaMessage("WOA token 已刷新");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyClaudeWoa(false);
    }
  }, []);

  const handleOpenClaudeWoaLoginUrl = useCallback(async () => {
    if (!claudeWoaLogin) return;
    try {
      await openExternalUrl(claudeWoaLogin.verification_uri_complete ?? claudeWoaLogin.verification_uri);
    } catch (e) {
      setError(String(e));
    }
  }, [claudeWoaLogin]);

  const updateLspDraft = useCallback((
    languageId: string,
    patch: Partial<LspServerConfigInput>,
  ) => {
    setLspDrafts((drafts) => ({
      ...drafts,
      [languageId]: {
        ...drafts[languageId],
        languageId,
        ...patch,
      },
    }));
  }, []);

  const handleProbeLsp = useCallback(async (languageId: string) => {
    const draft = lspDrafts[languageId];
    if (!draft) return;
    setBusyLsp(languageId);
    setLspError(null);
    try {
      const result = await settingsProbeLspServer(draft.command);
      setProbeMessages((messages) => ({
        ...messages,
        [languageId]: result.available
          ? `已找到：${result.resolvedPath ?? draft.command}`
          : result.message ?? "未找到命令",
      }));
    } catch (e) {
      setLspError(String(e));
    } finally {
      setBusyLsp(null);
    }
  }, [lspDrafts]);

  const handleSaveLsp = useCallback(async (languageId: string) => {
    const draft = lspDrafts[languageId];
    if (!draft) return;
    setBusyLsp(languageId);
    setLspError(null);
    try {
      const nextSnapshot = await settingsSaveLspServer(draft);
      applyLspSnapshot(nextSnapshot);
      setProbeMessages((messages) => ({ ...messages, [languageId]: "已保存" }));
    } catch (e) {
      setLspError(String(e));
    } finally {
      setBusyLsp(null);
    }
  }, [applyLspSnapshot, lspDrafts]);

  const handleResetLsp = useCallback(async (languageId: string) => {
    setBusyLsp(languageId);
    setLspError(null);
    try {
      const nextSnapshot = await settingsResetLspServer(languageId);
      applyLspSnapshot(nextSnapshot);
      setProbeMessages((messages) => ({ ...messages, [languageId]: "已恢复默认" }));
    } catch (e) {
      setLspError(String(e));
    } finally {
      setBusyLsp(null);
    }
  }, [applyLspSnapshot]);

  const renderAgentRuntime = (agentId: AgentSettingsTab) => {
    if (!snapshot) return null;
    const agent = snapshot.agents.find((item) => item.id === agentId);
    if (!agent) return null;
    return (
      <div className="settings-provider-detail settings-agent-runtime">
        <span className={`settings-row-badge ${agent.installed ? "is-installed" : "is-missing"}`}>
          {agent.installed ? "已安装" : "未安装"}
        </span>
        <div className="settings-row-actions">
          {agent.installed ? (
            <button
              type="button"
              className={`settings-btn ${agent.selected ? "is-selected" : ""}`}
              disabled={agent.selected || busyAgent === agent.id || !!snapshot.env_override}
              onClick={() => handleSelect(agent.id)}
            >
              {agent.selected ? "当前默认" : busyAgent === agent.id ? "保存中..." : "设为默认"}
            </button>
          ) : (
            <button
              type="button"
              className="settings-btn is-install"
              disabled={busyAgent === agent.id}
              onClick={() => handleInstall(agent.id)}
            >
              {busyAgent === agent.id ? "下载中..." : agent.id === "codex-acp" ? "下载" : "安装"}
            </button>
          )}
        </div>
      </div>
    );
  };

  const renderByokPool = () => {
    if (!snapshot) return null;
    const byokProfiles = snapshot.codex_acp.profiles.filter((profile) =>
      profile.requires_credential && profile.id !== "venus" && profile.id !== "timiai",
    );
    const profile = byokProfiles.find((item) => item.id === byokProfileId) ?? byokProfiles[0];
    if (!profile) return null;
    return (
      <section className="settings-provider-config settings-byok-config">
        <div className="settings-provider-config-head">
          <div>
            <span>BYOK 模型池</span>
            <p>保存自己的 API key。</p>
          </div>
          <span className="settings-provider-active">
            {byokProfiles.filter((item) => item.configured).length}/{byokProfiles.length} 已配置
          </span>
        </div>
        <label className="settings-field">
          <span>模型来源</span>
          <select
            className="settings-provider-select"
            aria-label="byok_provider_profile"
            value={profile.id}
            disabled={busyCodexAcp}
            onChange={(event) => {
              setByokProfileId(event.currentTarget.value);
              setCodexAcpApiKey("");
              setCodexAcpMessage(null);
              setCodexAcpMessageTarget("byok");
            }}
          >
            {byokProfiles.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}{item.configured ? " · 已配置" : " · 未配置"}
              </option>
            ))}
          </select>
        </label>
        <div className="settings-provider-detail">
          <span className={`settings-row-badge ${profile.configured ? "is-installed" : "is-missing"}`}>
            {profile.configured ? "已配置" : "未配置"}
          </span>
          {renderModelChip(profile.models)}
        </div>
        <label className="settings-field settings-provider-key-field">
          <span>{profile.credential_label ?? `${profile.label} API key`}</span>
          <input
            aria-label="byok_api_key"
            type="password"
            autoComplete="off"
            placeholder={profile.configured ? `输入新的 ${profile.label} API key 以替换` : `输入 ${profile.label} API key`}
            value={codexAcpApiKey}
            onChange={(event) => setCodexAcpApiKey(event.currentTarget.value)}
          />
        </label>
        <div className="settings-provider-config-actions">
          {codexAcpMessageTarget === "byok" && codexAcpMessage && <span className="settings-provider-config-message">{codexAcpMessage}</span>}
          <button
            type="button"
            className="settings-btn"
            disabled={busyCodexAcp || !codexAcpApiKey.trim()}
            onClick={handleSaveByokProviderKey}
          >
            {busyCodexAcp ? "保存中..." : `保存 ${profile.label} key`}
          </button>
        </div>
      </section>
    );
  };

  const renderTimiAiConfig = (family: "codex" | "claude") => {
    if (!snapshot) return null;
    const profile = (family === "codex" ? snapshot.codex_acp.profiles : snapshot.claude_woa.profiles)
      .find((item) => item.id === "timiai");
    const busy = family === "codex" ? busyCodexAcp : busyClaudeWoa;
    const message = family === "codex" ? codexAcpMessage : claudeWoaMessage;
    const showMessage = family === "claude" || codexAcpMessageTarget === "channel";
    return (
      <>
        <div className="settings-provider-detail">
          <span className={`settings-row-badge ${profile?.configured ? "is-installed" : "is-missing"}`}>
            {profile?.configured ? "已配置" : "未配置"}
          </span>
          {renderModelChip(profile?.models)}
        </div>
        <label className="settings-field settings-provider-key-field">
          <span>TimiAI key</span>
          <input
            aria-label={`${family}_timiai_api_key`}
            type="password"
            autoComplete="off"
            placeholder={profile?.configured ? "输入新的 TimiAI key 以替换" : "输入 TimiAI key"}
            value={timiAiApiKey}
            onChange={(event) => setTimiAiApiKey(event.currentTarget.value)}
          />
        </label>
        <div className="settings-provider-config-actions">
          {showMessage && message && <span className="settings-provider-config-message">{message}</span>}
          <button
            type="button"
            className="settings-btn"
            disabled={busy || !timiAiApiKey.trim()}
            onClick={() => handleSaveTimiAiKey(family)}
          >
            {busy ? "保存中..." : `保存 ${family === "codex" ? "Codex" : "Claude"} TimiAI key`}
          </button>
        </div>
      </>
    );
  };

  const renderWoaTokenControls = (messagePrefix: "Codex" | "Claude") => {
    if (!snapshot) return null;
    return (
      <>
        <div className="settings-provider-config-message">
          {snapshot.claude_woa.token.malformed
            ? "WOA 登录状态异常，请重新登录"
            : snapshot.claude_woa.token.exists
              ? `WOA 已登录${snapshot.claude_woa.token.refresh_needed ? "，需要刷新" : ""}`
              : "尚未登录 WOA"}
        </div>
        {claudeWoaLogin && (
          <div className="settings-warning">
            <span>
              打开{" "}
              <button type="button" className="settings-inline-link" onClick={handleOpenClaudeWoaLoginUrl}>
                {claudeWoaLogin.verification_uri_complete ?? claudeWoaLogin.verification_uri}
              </button>
              ，输入 <code>{claudeWoaLogin.user_code}</code>
            </span>
          </div>
        )}
        <div className="settings-provider-config-actions">
          {claudeWoaMessage && <span className="settings-provider-config-message">{claudeWoaMessage}</span>}
          {claudeWoaLogin ? (
            <button type="button" className="settings-btn" disabled={busyClaudeWoa} onClick={handleCancelClaudeWoaLogin}>
              取消登录
            </button>
          ) : (
            <button type="button" className="settings-btn" disabled={busyClaudeWoa} onClick={handleStartClaudeWoaLogin}>
              {busyClaudeWoa ? "处理中..." : "WOA 登录"}
            </button>
          )}
          <button
            type="button"
            className="settings-btn"
            disabled={busyClaudeWoa || !snapshot.claude_woa.token.exists || snapshot.claude_woa.token.malformed}
            onClick={handleRefreshClaudeWoaToken}
          >
            刷新登录
          </button>
          {messagePrefix === "Claude" && (
            <button type="button" className="settings-btn" disabled={busyClaudeWoa} onClick={() => handleSaveClaudeWoaConfig()}>
              保存模型列表
            </button>
          )}
        </div>
      </>
    );
  };

  const startupNoticeCopy = visibleStartupNotice
    ? startupNoticeCopyFor(visibleStartupNotice)
    : null;

  return (
    <div className="settings-page">
      <aside className="settings-sidebar">
        <button type="button" className="settings-back" onClick={onBack}>
          <span className="settings-back-arrow">←</span> 返回应用
        </button>

        <div className="settings-nav-group">
          <span className="settings-nav-label">应用</span>
          <button
            type="button"
            className={`settings-nav-item ${activePane === "general" ? "is-active" : ""}`}
            onClick={() => setActivePane("general")}
          >
            通用
          </button>
          <button
            type="button"
            className={`settings-nav-item ${activePane === "lsp" ? "is-active" : ""}`}
            onClick={() => setActivePane("lsp")}
          >
            LSP
          </button>
        </div>
      </aside>

      <main className="settings-content">
        <header className="settings-content-header">
          <h1>{activePane === "general" ? "通用" : "LSP"}</h1>
          <p>
            {activePane === "general"
              ? "外观、默认提供者和智能体配置。"
              : "管理编辑器诊断、悬浮提示和补全使用的 language server。"}
          </p>
        </header>

        {activePane === "general" && (
          <>
        <section className="settings-section">
          <h2 className="settings-section-title">主题</h2>
          <p className="settings-section-desc">选择深色或浅色界面。</p>
          <div className="settings-theme-grid">
            {APP_THEMES.map((theme) => {
              const selected = snapshot?.settings.theme === theme.id;
              return (
                <button
                  key={theme.id}
                  type="button"
                  className={`settings-theme-card ${selected ? "is-selected" : ""}`}
                  disabled={loading || busyTheme !== null || selected}
                  onClick={() => handleThemeSelect(theme.id)}
                >
                  <span className="settings-theme-swatches" aria-hidden="true">
                    {theme.swatches.map((color) => (
                      <span key={color} style={{ background: color }} />
                    ))}
                  </span>
                  <span className="settings-theme-copy">
                    <span className="settings-theme-title">{theme.label}</span>
                    <span className="settings-theme-desc">{selected ? "当前主题" : theme.description}</span>
                  </span>
                  {busyTheme === theme.id && <span className="settings-theme-saving">保存中...</span>}
                </button>
              );
            })}
          </div>
        </section>

        <section className="settings-section">
          <h2 className="settings-section-title">应用更新</h2>
          <p className="settings-section-desc">检查 GitHub Release 上的 Kodex 桌面更新。</p>
          <div className="settings-update-panel">
            <div className="settings-update-copy">
              <div className="settings-row-title">Kodex{appVersion ? ` ${appVersion}` : ""}</div>
              <div className="settings-row-meta">
                {updateInfo ? `可更新到 ${updateInfo.version}` : "通过 Tauri updater 校验签名后安装"}
              </div>
            </div>
            <div className="settings-row-actions">
              <button
                type="button"
                className="settings-btn"
                disabled={updateStatus === "checking" || updateStatus === "installing"}
                onClick={handleCheckForUpdate}
              >
                {updateStatus === "checking" ? "检查中..." : "检查更新"}
              </button>
              {updateStatus === "available" && (
                <button type="button" className="settings-btn is-install" onClick={handleInstallUpdate}>
                  安装并重启
                </button>
              )}
            </div>
          </div>
          {updateMessage && (
            <div className={updateStatus === "error" ? "settings-error" : updateStatus === "available" ? "settings-warning" : "settings-status"}>
              {updateMessage}
            </div>
          )}
          {updateInfo?.body && updateStatus === "available" && (
            <div className="settings-update-notes">{updateInfo.body}</div>
          )}
          {updateStatus === "installing" && updateProgress?.contentLength && (
            <progress
              className="settings-update-progress"
              max={updateProgress.contentLength}
              value={Math.min(updateProgress.downloadedBytes, updateProgress.contentLength)}
              aria-label="更新下载进度"
            />
          )}
        </section>

        <section className="settings-section">
          <h2 className="settings-section-title">智能体</h2>
          <p className="settings-section-desc">选择默认智能体和可用模型来源。</p>

          {loading && <div className="settings-status">加载中...</div>}
          {error && (
            <div className="settings-error">
              <span>{error}</span>
              <button type="button" className="settings-link-btn" onClick={load}>重试</button>
            </div>
          )}
          {snapshot?.env_override && (
            <div className="settings-warning">
              <code>ACP_AGENT_COMMAND</code> 已设置，将覆盖此选择：<code>{snapshot.env_override}</code>
            </div>
          )}
          {installResult && (
            <div className={installResult.success ? "settings-success" : "settings-error"}>
              <span>{installResult.message}</span>
              {installResult.manual_instruction && <div><code>{installResult.manual_instruction}</code></div>}
            </div>
          )}

          {snapshot && (
            <div className="settings-agent-settings">
              <div className="settings-agent-tabs" role="tablist" aria-label="Agent settings">
                {AGENT_SETTINGS_TABS.map((tab) => (
                  <button
                    key={tab.id}
                    type="button"
                    role="tab"
                    aria-selected={activeAgentTab === tab.id}
                    className={`settings-agent-tab ${activeAgentTab === tab.id ? "is-active" : ""}`}
                    onClick={() => setActiveAgentTab(tab.id)}
                  >
                    {tab.label}
                  </button>
                ))}
              </div>

              <div className="settings-agent-tab-panel">
                {activeAgentTab === "codebuddy" && (() => {
                  return (
                    <div className="settings-provider-config">
                      <div className="settings-provider-config-head">
                        <div>
                          <span>CodeBuddy</span>
                        </div>
                        <span className="settings-provider-active">
                          {snapshot.settings.selected_agent === "codebuddy" ? "当前默认" : "可选"}
                        </span>
                      </div>
                      {renderAgentRuntime("codebuddy")}
                    </div>
                  );
                })()}

                {activeAgentTab === "codex-acp" && (
                  <>
                    <div className="settings-provider-config">
                      <div className="settings-provider-config-head">
                        <div>
                          <span>Codex 通道</span>
                          <p>选择 Codex 的默认通道。</p>
                        </div>
                        <span className="settings-provider-active">
                          当前：{snapshot.codex_acp.selected_profile_id === "default" ? "默认" : snapshot.codex_acp.selected_profile_id === "venus" ? "Venus" : snapshot.codex_acp.selected_profile_id === "woa" ? "WOA" : snapshot.codex_acp.selected_profile_id === "timiai" ? "TimiAI" : "BYOK"}
                        </span>
                      </div>
                      {renderAgentRuntime("codex-acp")}
                      <div className="settings-provider-options" role="radiogroup" aria-label="Codex channel">
                        {(["default", "woa", "timiai", "venus", "byok"] as const).map((channel) => {
                          const selected = channel === "default"
                            ? snapshot.codex_acp.selected_profile_id === "default"
                            : channel === "woa"
                              ? snapshot.codex_acp.selected_profile_id === "woa"
                            : channel === "timiai"
                              ? snapshot.codex_acp.selected_profile_id === "timiai"
                            : channel === "venus"
                              ? snapshot.codex_acp.selected_profile_id === "venus"
                              : snapshot.codex_acp.selected_profile_id !== "default" && snapshot.codex_acp.selected_profile_id !== "woa" && snapshot.codex_acp.selected_profile_id !== "timiai" && snapshot.codex_acp.selected_profile_id !== "venus";
                          return (
                            <button
                              key={channel}
                              type="button"
                              className={`settings-provider-option ${selected ? "is-selected" : ""}`}
                              onClick={() => handleSelectCodexChannel(channel)}
                              disabled={busyCodexAcp}
                              aria-pressed={selected}
                            >
                              <span className="settings-provider-option-main">
                                <span>{channel === "default" ? "默认" : channel === "woa" ? "WOA" : channel === "timiai" ? "TimiAI" : channel === "venus" ? "Venus" : "BYOK"}</span>
                                <span>
                                  {channel === "default"
                                    ? "本机默认配置"
                                    : channel === "woa"
                                      ? "WOA 登录"
                                    : channel === "timiai"
                                      ? "TimiAI"
                                    : channel === "venus"
                                      ? "Venus"
                                      : "自带 API key"}
                                </span>
                              </span>
                              <span className={`settings-row-badge ${selected ? "is-installed" : "is-missing"}`}>
                                {selected ? "当前" : "可选"}
                              </span>
                            </button>
                          );
                        })}
                      </div>
                      {snapshot.codex_acp.selected_profile_id === "woa" && (
                        <>
                          {renderWoaTokenControls("Codex")}
                        </>
                      )}
                      {snapshot.codex_acp.selected_profile_id === "timiai" && renderTimiAiConfig("codex")}
                      {snapshot.codex_acp.selected_profile_id === "venus" && (
                        <>
                          <label className="settings-field settings-provider-key-field">
                            <span>Venus API key</span>
                            <input
                              aria-label="codex_venus_api_key"
                              type="password"
                              autoComplete="off"
                              placeholder={snapshot.codex_acp.venus_key_configured ? "输入新的 Venus API key 以替换" : "输入 Venus API key"}
                              value={codexVenusApiKey}
                              onChange={(event) => setCodexVenusApiKey(event.currentTarget.value)}
                            />
                          </label>
                          <div className="settings-provider-config-actions">
                            <button
                              type="button"
                              className="settings-btn"
                              disabled={busyCodexAcp || !codexVenusApiKey.trim()}
                              onClick={handleSaveCodexVenusKey}
                            >
                              {busyCodexAcp ? "保存中..." : "保存 Codex Venus key"}
                            </button>
                          </div>
                        </>
                      )}
                      {codexAcpMessageTarget === "channel" && snapshot.codex_acp.selected_profile_id !== "timiai" && codexAcpMessage && (
                        <div className="settings-provider-config-message">{codexAcpMessage}</div>
                      )}
                    </div>
                    {renderByokPool()}
                  </>
                )}

                {activeAgentTab === "claude-agent-acp" && (
                  <>
                    <div className="settings-provider-config">
                      <div className="settings-provider-config-head">
                        <div>
                          <span>Claude 通道</span>
                          <p>选择 Claude 的默认通道。</p>
                        </div>
                        <span className="settings-provider-active">
                          当前：{claudeProfileId === "woa" ? "WOA" : claudeProfileId === "timiai" ? "TimiAI" : claudeProfileId === "venus" ? "Venus" : "BYOK"}
                        </span>
                      </div>
                      {renderAgentRuntime("claude-agent-acp")}
                      <div className="settings-provider-options" role="radiogroup" aria-label="Claude channel">
                        {(["woa", "timiai", "venus", "byok"] as const).map((channel) => {
                          const selected = channel === "woa"
                            ? claudeProfileId === "woa"
                            : channel === "timiai"
                              ? claudeProfileId === "timiai"
                            : channel === "venus"
                              ? claudeProfileId === "venus"
                              : claudeProfileId !== "woa" && claudeProfileId !== "timiai" && claudeProfileId !== "venus";
                          return (
                            <button
                              key={channel}
                              type="button"
                              className={`settings-provider-option ${selected ? "is-selected" : ""}`}
                              onClick={() => handleSelectClaudeChannel(channel)}
                              disabled={busyClaudeWoa}
                              aria-pressed={selected}
                            >
                              <span className="settings-provider-option-main">
                                <span>{channel === "woa" ? "WOA" : channel === "timiai" ? "TimiAI" : channel === "venus" ? "Venus" : "BYOK"}</span>
                                <span>
                                  {channel === "woa"
                                    ? "Tencent WOA 登录"
                                    : channel === "timiai"
                                      ? "TimiAI"
                                    : channel === "venus"
                                      ? "Venus"
                                      : "自带 API key"}
                                </span>
                              </span>
                              <span className={`settings-row-badge ${selected ? "is-installed" : "is-missing"}`}>
                                {selected ? "当前" : "可选"}
                              </span>
                            </button>
                          );
                        })}
                      </div>
                      {claudeProfileId === "woa" && (
                        <>
                          <div className="settings-provider-options" role="radiogroup" aria-label="Claude WOA channel">
                            {(["default", "offline"] as ClaudeWoaChannel[]).map((channel) => (
                              <button
                                key={channel}
                                type="button"
                                className={`settings-provider-option ${claudeWoaChannel === channel ? "is-selected" : ""}`}
                                onClick={() => {
                                  setClaudeWoaChannel(channel);
                                  handleSaveClaudeWoaConfig(channel);
                                }}
                                disabled={busyClaudeWoa}
                                aria-pressed={claudeWoaChannel === channel}
                              >
                                <span className="settings-provider-option-main">
                                  <span>{channel === "default" ? "在线" : "离线"}</span>
                                  <span>{channel === "default" ? "推荐" : "备用"}</span>
                                </span>
                                <span className={`settings-row-badge ${claudeWoaChannel === channel ? "is-installed" : "is-missing"}`}>
                                  {claudeWoaChannel === channel ? "当前" : "可选"}
                                </span>
                              </button>
                            ))}
                          </div>
                          <label className="settings-field settings-provider-models-field">
                            <span>模型列表</span>
                            <textarea
                              aria-label="claude_woa_models"
                              value={claudeWoaModelsText}
                              onChange={(event) => setClaudeWoaModelsText(event.currentTarget.value)}
                              placeholder={"claude-opus-4-7[1m]\nclaude-opus-4-6[1m]"}
                              spellCheck={false}
                            />
                          </label>
                          {renderWoaTokenControls("Claude")}
                        </>
                      )}
                      {claudeProfileId === "timiai" && renderTimiAiConfig("claude")}
                      {claudeProfileId === "venus" && (
                        <>
                          <label className="settings-field settings-provider-key-field">
                            <span>Venus API key</span>
                            <input
                              aria-label="claude_venus_api_key"
                              type="password"
                              autoComplete="off"
                              placeholder={
                                snapshot.claude_woa.profiles.find((profile) => profile.id === "venus")?.configured
                                  ? "输入新的 Venus API key 以替换"
                                  : "输入 Venus API key"
                              }
                              value={claudeVenusApiKey}
                              onChange={(event) => setClaudeVenusApiKey(event.currentTarget.value)}
                            />
                          </label>
                          <div className="settings-provider-config-actions">
                            {claudeWoaMessage && <span className="settings-provider-config-message">{claudeWoaMessage}</span>}
                            <button
                              type="button"
                              className="settings-btn"
                              disabled={busyClaudeWoa || !claudeVenusApiKey.trim()}
                              onClick={handleSaveClaudeVenusKey}
                            >
                              {busyClaudeWoa ? "保存中..." : "保存 Claude Venus key"}
                            </button>
                          </div>
                        </>
                      )}
                      {claudeProfileId !== "woa" && claudeProfileId !== "timiai" && claudeProfileId !== "venus" && claudeWoaMessage && (
                        <div className="settings-provider-config-message">{claudeWoaMessage}</div>
                      )}
                    </div>
                    {renderByokPool()}
                  </>
                )}
              </div>
            </div>
          )}

          <div className="settings-detect-row">
            <button type="button" className="settings-link-btn" onClick={handleDetect} disabled={loading}>
              重新检测已安装的 CLI
            </button>
          </div>
        </section>
          </>
        )}

        {activePane === "lsp" && (
        <section className="settings-section">
          <h2 className="settings-section-title">LSP 语言服务</h2>
          <p className="settings-section-desc">管理编辑器诊断、悬浮提示和补全使用的 language server。</p>
          {lspError && <div className="settings-error">{lspError}</div>}
          <div className="settings-lsp-list">
            {lspSnapshot?.servers.map((server) => {
              const draft = lspDrafts[server.languageId] ?? {
                languageId: server.languageId,
                enabled: server.enabled,
                command: server.command,
                args: server.args,
              };
              const argsText = draft.args.join(" ");
              const dirty =
                draft.enabled !== server.enabled ||
                draft.command !== server.command ||
                argsText !== server.args.join(" ");
              return (
                <article key={server.languageId} className="settings-lsp-card">
                  <div className="settings-lsp-head">
                    <div>
                      <div className="settings-row-title">{server.displayName}</div>
                      <div className="settings-row-meta">
                        <code>{server.languageId}</code>
                        {server.running && <span className="settings-row-badge is-installed">运行中</span>}
                        {!server.enabled && <span className="settings-row-badge is-missing">已禁用</span>}
                        {server.enabled && server.available && <span className="settings-row-badge is-installed">可用</span>}
                        {server.enabled && !server.available && <span className="settings-row-badge is-missing">缺少命令</span>}
                      </div>
                    </div>
                    <label className="settings-switch">
                      <input
                        type="checkbox"
                        checked={draft.enabled}
                        onChange={(event) => updateLspDraft(server.languageId, { enabled: event.currentTarget.checked })}
                      />
                      <span>启用</span>
                    </label>
                  </div>
                  <label className="settings-field">
                    <span>命令</span>
                    <input
                      value={draft.command}
                      onChange={(event) => updateLspDraft(server.languageId, { command: event.currentTarget.value })}
                      placeholder={server.defaultCommand}
                    />
                  </label>
                  <label className="settings-field">
                    <span>参数</span>
                    <input
                      value={argsText}
                      onChange={(event) => updateLspDraft(server.languageId, {
                        args: splitArgs(event.currentTarget.value),
                      })}
                      placeholder={server.defaultArgs.join(" ")}
                    />
                  </label>
                  <div className="settings-lsp-foot">
                    <span className="settings-lsp-message">
                      {probeMessages[server.languageId] ??
                        server.message ??
                        server.resolvedPath ??
                        "已使用默认配置"}
                    </span>
                    <div className="settings-row-actions">
                      <button
                        type="button"
                        className="settings-btn"
                        disabled={busyLsp === server.languageId}
                        onClick={() => handleProbeLsp(server.languageId)}
                      >
                        探测
                      </button>
                      <button
                        type="button"
                        className="settings-btn"
                        disabled={!dirty || busyLsp === server.languageId}
                        onClick={() => handleSaveLsp(server.languageId)}
                      >
                        保存
                      </button>
                      <button
                        type="button"
                        className="settings-btn"
                        disabled={!server.customized || busyLsp === server.languageId}
                        onClick={() => handleResetLsp(server.languageId)}
                      >
                        重置
                      </button>
                    </div>
                  </div>
                </article>
              );
            })}
          </div>
        </section>
        )}
      </main>

      {startupNoticeCopy && (
        <div className="settings-startup-backdrop" role="presentation">
          <div
            className="settings-startup-dialog"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="settings-startup-title"
            aria-describedby="settings-startup-message"
          >
            <div className="settings-startup-kicker">初始化未完成</div>
            <h2 id="settings-startup-title">{startupNoticeCopy.title}</h2>
            <p id="settings-startup-message">{startupNoticeCopy.message}</p>
            <div className="settings-startup-actions">
              <button type="button" className="settings-btn is-install" autoFocus onClick={dismissStartupNotice}>
                {startupNoticeCopy.action}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

function startupNoticeCopyFor(notice: SettingsStartupNotice) {
  const detail = notice.message ? ` 检测错误：${notice.message}` : "";
  const kind = notice.kind;
  if (kind === "woa") {
    return {
      title: "内网通道还没设置好",
      message: "检测到当前在公司内网，但 WOA、TimiAI 或 Venus 还没有任意一个配置完成。未完成前新建会话不能正常使用，请先在这里完成其中一个内网通道。",
      action: "去设置",
    };
  }
  return {
    title: "Codex BYOK 还没设置好",
    message: notice.message
      ? `没有确认当前在公司内网，已按外网路径兜底。Codex BYOK 还没有配置，未完成前新建会话不能正常使用，请先在这里保存一个 BYOK API key。${detail}`
      : "检测到当前不在公司内网，Codex BYOK 还没有配置。未完成前新建会话不能正常使用，请先在这里保存一个 BYOK API key。",
    action: "去设置",
  };
}

function splitArgs(value: string): string[] {
  return value
    .split(/\s+/)
    .map((arg) => arg.trim())
    .filter(Boolean);
}

function parseClaudeWoaModels(value: string): string[] {
  const models: string[] = [];
  for (const rawModel of value.split(/[\n,]/)) {
    const model = rawModel.trim();
    if (model && !models.includes(model)) {
      models.push(model);
    }
  }
  return models;
}

function formatUpdateProgress(progress: AppUpdateProgress): string {
  if (progress.phase === "finished") {
    return "更新包下载完成，正在安装";
  }
  if (progress.contentLength && progress.contentLength > 0) {
    return `正在下载更新包 ${formatBytes(progress.downloadedBytes)} / ${formatBytes(progress.contentLength)}`;
  }
  return `正在下载更新包 ${formatBytes(progress.downloadedBytes)}`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const kib = bytes / 1024;
  if (kib < 1024) {
    return `${kib.toFixed(1)} KiB`;
  }
  return `${(kib / 1024).toFixed(1)} MiB`;
}

function providerLabel(profiles: AgentProviderProfile[], profileId: string): string {
  return profiles.find((profile) => profile.id === profileId)?.label ?? profileId;
}
