import { useCallback, useEffect, useRef, useState } from "react";
import type {
  AgentCliId,
  AgentInstallResult,
  AgentProviderProfile,
  AgentSettingsSnapshot,
  AppTheme,
  LspSettingsSnapshot,
  LspServerConfigInput,
  RemoteMachineProfile,
  RemoteMachineProfileInput,
  RemoteMachineProfilesSnapshot,
  RemoteValidationPhaseKind,
  RemoteValidationPhaseStatus,
} from "../../types";
import {
  settingsDetectAgents,
  settingsDeleteRemoteProfile,
  settingsGetAgentSnapshot,
  settingsGetLspSnapshot,
  settingsGetRemoteProfiles,
  settingsInstallAgent,
  settingsProbeLspServer,
  settingsResetLspServer,
  settingsSaveAgentProviderSecret,
  settingsResetProviderModels,
  settingsSaveProviderModels,
  settingsSaveLspServer,
  settingsSaveRemoteProfile,
  settingsSelectAgentProviderProfile,
  settingsSelectAgent,
  settingsSelectTheme,
  settingsValidateRemoteProfile,
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
export type SettingsPane = "general" | "remote" | "lsp";
type UpdateStatus = "idle" | "checking" | "up-to-date" | "available" | "installing" | "installed" | "error";
type RemoteProfileDraft = {
  id?: string | null;
  display_name: string;
  ssh_target: string;
  ssh_port: string;
};

export interface RemoteSettingsContext {
  profileId?: string | null;
  workspaceName: string;
  sshTarget: string;
  sshPort?: number | null;
  remotePath: string;
  agentLabel?: string | null;
}

export interface SettingsStartupNotice {
  kind: "codex_byok";
  message?: string | null;
}

interface Props {
  onBack: () => void;
  onThemeChange?: (theme: AppTheme) => void;
  startupNotice?: SettingsStartupNotice | null;
  initialPane?: SettingsPane;
  initialAgentTab?: AgentSettingsTab;
  remoteContext?: RemoteSettingsContext | null;
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

function parseProviderModelsDraft(value: string): string[] {
  const models: string[] = [];
  for (const line of value.split(/\r?\n/)) {
    const model = line.trim();
    if (!model || models.includes(model)) continue;
    models.push(model);
  }
  return models;
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
  initialPane,
  initialAgentTab,
  remoteContext,
  startupNotice,
  onBack,
  onStartupNoticeDismissed,
  onThemeChange,
}: Props) {
  const [activePane, setActivePane] = useState<SettingsPane>(initialPane ?? "general");
  const [activeAgentTab, setActiveAgentTab] = useState<AgentSettingsTab>(initialAgentTab ?? "claude-agent-acp");
  const [visibleStartupNotice, setVisibleStartupNotice] = useState<SettingsStartupNotice | null>(startupNotice ?? null);
  const [snapshot, setSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [lspSnapshot, setLspSnapshot] = useState<LspSettingsSnapshot | null>(null);
  const [remoteSnapshot, setRemoteSnapshot] = useState<RemoteMachineProfilesSnapshot>({ profiles: [] });
  const [lspDrafts, setLspDrafts] = useState<Record<string, LspServerConfigInput>>({});
  const [remoteDraft, setRemoteDraft] = useState<RemoteProfileDraft | null>(null);
  const [remoteValidationPaths, setRemoteValidationPaths] = useState<Record<string, string>>({});
  const [remoteValidationPasswords, setRemoteValidationPasswords] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [busyAgent, setBusyAgent] = useState<AgentCliId | null>(null);
  const [busyCodexAcp, setBusyCodexAcp] = useState(false);
  const [busyTheme, setBusyTheme] = useState<AppTheme | null>(null);
  const [busyLsp, setBusyLsp] = useState<string | null>(null);
  const [busyRemote, setBusyRemote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lspError, setLspError] = useState<string | null>(null);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [remoteMessage, setRemoteMessage] = useState<string | null>(null);
  const [installResult, setInstallResult] = useState<AgentInstallResult | null>(null);
  const [probeMessages, setProbeMessages] = useState<Record<string, string>>({});
  const [codexProfileId, setCodexProfileId] = useState("byok");
  const [byokProfileId, setByokProfileId] = useState("deepseek");
  const [byokProviderMenuOpen, setByokProviderMenuOpen] = useState(false);
  const [byokProfileInitialized, setByokProfileInitialized] = useState(false);
  const [codexAcpApiKey, setCodexAcpApiKey] = useState("");
  const [providerModelsDraft, setProviderModelsDraft] = useState("");
  const [busyProviderModels, setBusyProviderModels] = useState(false);
  const [codexAcpMessage, setCodexAcpMessage] = useState<string | null>(null);
  const [codexAcpMessageTarget, setCodexAcpMessageTarget] = useState<"channel" | "byok" | "models">("channel");
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>("idle");
  const [updateInfo, setUpdateInfo] = useState<AppUpdateInfo | null>(null);
  const [updateMessage, setUpdateMessage] = useState<string | null>(null);
  const [updateProgress, setUpdateProgress] = useState<AppUpdateProgress | null>(null);
  const byokProviderMenuRef = useRef<HTMLDivElement>(null);
  const settingsRemoteProfileId = remoteContext?.profileId ?? null;

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
  }, [settingsRemoteProfileId]);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    setLspError(null);
    try {
      const [nextSnapshot, nextLspSnapshot, nextRemoteSnapshot] = await Promise.all([
        settingsGetAgentSnapshot(settingsRemoteProfileId),
        settingsGetLspSnapshot(settingsRemoteProfileId),
        settingsGetRemoteProfiles(),
      ]);
      setSnapshot(nextSnapshot);
      applyLspSnapshot(nextLspSnapshot);
      setRemoteSnapshot(nextRemoteSnapshot);
      onThemeChange?.(applyAppTheme(nextSnapshot.settings.theme));
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [applyLspSnapshot, onThemeChange, settingsRemoteProfileId]);

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
    if (initialPane) {
      setActivePane(initialPane);
    }
  }, [initialPane]);

  useEffect(() => {
    if (initialAgentTab) {
      setActivePane(initialPane ?? "general");
      setActiveAgentTab(initialAgentTab);
    }
  }, [initialAgentTab, initialPane]);

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
    const byokProfiles = snapshot.codex_acp.profiles.filter((profile) => profile.requires_credential);
    const selected = snapshot.codex_acp.selected_profile_id;
    if (selected !== "default" && selected !== "byok") {
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
    const profile = snapshot.codex_acp.profiles.find((item) => item.id === byokProfileId);
    if (!profile) return;
    setProviderModelsDraft(profile.models.join("\n"));
  }, [byokProfileId, snapshot]);

  useEffect(() => {
    if (!byokProviderMenuOpen) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && byokProviderMenuRef.current?.contains(target)) return;
      setByokProviderMenuOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        setByokProviderMenuOpen(false);
      }
    };

    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("keydown", handleKeyDown);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [byokProviderMenuOpen]);

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
      setSnapshot(await settingsDetectAgents(settingsRemoteProfileId));
    } catch (e) {
      setError(String(e));
    }
  }, [settingsRemoteProfileId]);

  const handleSelect = useCallback(async (agent: AgentCliId) => {
    setBusyAgent(agent);
    setError(null);
    try {
      setSnapshot(await settingsSelectAgent(agent, settingsRemoteProfileId));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyAgent(null);
    }
  }, [settingsRemoteProfileId]);

  const handleThemeSelect = useCallback(async (theme: AppTheme) => {
    setBusyTheme(theme);
    setError(null);
    try {
      const nextSnapshot = await settingsSelectTheme(theme, settingsRemoteProfileId);
      setSnapshot(nextSnapshot);
      onThemeChange?.(applyAppTheme(nextSnapshot.settings.theme));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyTheme(null);
    }
  }, [onThemeChange, settingsRemoteProfileId]);

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
      const codexSnapshot = await settingsSaveAgentProviderSecret("codex", byokProfileId, key, settingsRemoteProfileId);
      const nextSnapshot = await settingsSaveAgentProviderSecret("claude", byokProfileId, key, settingsRemoteProfileId);
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
  }, [byokProfileId, codexAcpApiKey, settingsRemoteProfileId]);

  const handleSaveProviderModels = useCallback(async () => {
    const models = parseProviderModelsDraft(providerModelsDraft);
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("models");
    if (!byokProfileId) {
      setError("请选择 BYOK 模型来源");
      return;
    }
    if (!models.length) {
      setError("模型列表不能为空");
      return;
    }
    setBusyProviderModels(true);
    try {
      const nextSnapshot = await settingsSaveProviderModels(byokProfileId, models, settingsRemoteProfileId);
      setSnapshot(nextSnapshot);
      setCodexAcpMessageTarget("models");
      setCodexAcpMessage(`${providerLabel(nextSnapshot.codex_acp.profiles, byokProfileId)} 模型列表已更新，后续新建会话生效`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyProviderModels(false);
    }
  }, [byokProfileId, providerModelsDraft, settingsRemoteProfileId]);

  const handleResetProviderModels = useCallback(async () => {
    if (!byokProfileId) return;
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("models");
    setBusyProviderModels(true);
    try {
      const nextSnapshot = await settingsResetProviderModels(byokProfileId, settingsRemoteProfileId);
      setSnapshot(nextSnapshot);
      setCodexAcpMessageTarget("models");
      setCodexAcpMessage(`${providerLabel(nextSnapshot.codex_acp.profiles, byokProfileId)} 模型列表已恢复默认，后续新建会话生效`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyProviderModels(false);
    }
  }, [byokProfileId, settingsRemoteProfileId]);

  const handleSelectByokProfile = useCallback((profileId: string) => {
    setByokProfileId(profileId);
    setByokProviderMenuOpen(false);
    setCodexAcpApiKey("");
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("byok");
  }, []);

  const handleSelectCodexChannel = useCallback(async (channel: "default" | "byok") => {
    const byokProfiles = snapshot?.codex_acp.profiles.filter((profile) => profile.requires_credential) ?? [];
    const selectedByokProfileId = byokProfiles.find((profile) => profile.id === byokProfileId)?.id
      ?? (codexProfileId !== "default" && codexProfileId !== "byok" ? codexProfileId : undefined)
      ?? byokProfiles.find((profile) => profile.configured)?.id
      ?? byokProfiles[0]?.id;
    const nextProfileId = channel === "default" ? "default" : "byok";
    if (snapshot?.codex_acp.selected_profile_id === nextProfileId) return;
    setBusyCodexAcp(true);
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("channel");
    try {
      const nextSnapshot = await settingsSelectAgentProviderProfile("codex", nextProfileId, settingsRemoteProfileId);
      setSnapshot(nextSnapshot);
      setCodexProfileId(nextProfileId);
      if (channel === "byok") {
        setByokProfileId(selectedByokProfileId ?? byokProfileId);
      }
      setCodexAcpApiKey("");
      setCodexAcpMessageTarget("channel");
      setCodexAcpMessage(`Codex 通道已切换到 ${channel === "default" ? "默认" : "BYOK"}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCodexAcp(false);
    }
  }, [byokProfileId, codexProfileId, settingsRemoteProfileId, snapshot?.codex_acp.profiles, snapshot?.codex_acp.selected_profile_id]);

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
      const result = await settingsProbeLspServer(draft.command, settingsRemoteProfileId);
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
  }, [lspDrafts, settingsRemoteProfileId]);

  const handleSaveLsp = useCallback(async (languageId: string) => {
    const draft = lspDrafts[languageId];
    if (!draft) return;
    setBusyLsp(languageId);
    setLspError(null);
    try {
      const nextSnapshot = await settingsSaveLspServer(draft, settingsRemoteProfileId);
      applyLspSnapshot(nextSnapshot);
      setProbeMessages((messages) => ({ ...messages, [languageId]: "已保存" }));
    } catch (e) {
      setLspError(String(e));
    } finally {
      setBusyLsp(null);
    }
  }, [applyLspSnapshot, lspDrafts, settingsRemoteProfileId]);

  const handleResetLsp = useCallback(async (languageId: string) => {
    setBusyLsp(languageId);
    setLspError(null);
    try {
      const nextSnapshot = await settingsResetLspServer(languageId, settingsRemoteProfileId);
      applyLspSnapshot(nextSnapshot);
      setProbeMessages((messages) => ({ ...messages, [languageId]: "已恢复默认" }));
    } catch (e) {
      setLspError(String(e));
    } finally {
      setBusyLsp(null);
    }
  }, [applyLspSnapshot, settingsRemoteProfileId]);

  const startNewRemoteProfile = useCallback(() => {
    setRemoteDraft({
      display_name: "",
      ssh_target: "",
      ssh_port: "22",
    });
    setRemoteError(null);
    setRemoteMessage(null);
  }, []);

  const editRemoteProfile = useCallback((profile: RemoteMachineProfile) => {
    setRemoteDraft({
      id: profile.id,
      display_name: profile.display_name,
      ssh_target: profile.ssh_target,
      ssh_port: profile.ssh_port ? String(profile.ssh_port) : "",
    });
    setRemoteError(null);
    setRemoteMessage(null);
  }, []);

  const updateRemoteDraft = useCallback((patch: Partial<RemoteProfileDraft>) => {
    setRemoteDraft((draft) => draft ? { ...draft, ...patch } : draft);
  }, []);

  const handleSaveRemoteProfile = useCallback(async () => {
    if (!remoteDraft) return;
    setBusyRemote("save");
    setRemoteError(null);
    setRemoteMessage(null);
    try {
      const portText = remoteDraft.ssh_port.trim();
      const input: RemoteMachineProfileInput = {
        id: remoteDraft.id ?? null,
        display_name: remoteDraft.display_name.trim(),
        ssh_target: remoteDraft.ssh_target.trim(),
        ssh_port: portText ? Number(portText) : null,
      };
      const nextSnapshot = await settingsSaveRemoteProfile(input);
      setRemoteSnapshot(nextSnapshot);
      setRemoteDraft(null);
      setRemoteMessage("远程机器已保存");
    } catch (e) {
      setRemoteError(String(e));
    } finally {
      setBusyRemote(null);
    }
  }, [remoteDraft]);

  const handleDeleteRemoteProfile = useCallback(async (profileId: string) => {
    setBusyRemote(profileId);
    setRemoteError(null);
    setRemoteMessage(null);
    try {
      const nextSnapshot = await settingsDeleteRemoteProfile(profileId);
      setRemoteSnapshot(nextSnapshot);
      setRemoteDraft((draft) => draft?.id === profileId ? null : draft);
      setRemoteMessage("远程机器已删除");
    } catch (e) {
      setRemoteError(String(e));
    } finally {
      setBusyRemote(null);
    }
  }, []);

  const handleValidateRemoteProfile = useCallback(async (profile: RemoteMachineProfile) => {
    setBusyRemote(profile.id);
    setRemoteError(null);
    setRemoteMessage(null);
    try {
      const sshPassword = remoteValidationPasswords[profile.id] ?? "";
      const request = {
        profile_id: profile.id,
        remote_path: remoteValidationPaths[profile.id]?.trim() || "~",
        include_acp: false,
        ...(sshPassword ? { ssh_password: sshPassword } : {}),
      };
      const nextSnapshot = await settingsValidateRemoteProfile(request);
      setRemoteSnapshot(nextSnapshot);
      const updated = nextSnapshot.profiles.find((item) => item.id === profile.id);
      setRemoteMessage(updated?.last_validation?.ok ? "远程机器验证通过" : "远程机器验证失败");
    } catch (e) {
      setRemoteError(String(e));
    } finally {
      setRemoteValidationPasswords((passwords) => {
        if (!passwords[profile.id]) return passwords;
        const nextPasswords = { ...passwords };
        delete nextPasswords[profile.id];
        return nextPasswords;
      });
      setBusyRemote(null);
    }
  }, [remoteValidationPasswords, remoteValidationPaths]);

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
    const byokProfiles = snapshot.codex_acp.profiles.filter((profile) => profile.requires_credential);
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
        <div className="settings-field settings-provider-source-field">
          <span>模型来源</span>
          <div
            ref={byokProviderMenuRef}
            className={`settings-provider-select ${byokProviderMenuOpen ? "is-open" : ""}`}
            onBlur={(event) => {
              const nextFocus = event.relatedTarget;
              if (!(nextFocus instanceof Node) || !event.currentTarget.contains(nextFocus)) {
                setByokProviderMenuOpen(false);
              }
            }}
          >
            <button
              type="button"
              className="settings-provider-select-trigger"
              aria-label="byok_provider_profile"
              aria-haspopup="listbox"
              aria-expanded={byokProviderMenuOpen}
              aria-controls="byok-provider-profile-listbox"
              disabled={busyCodexAcp || busyProviderModels}
              onClick={() => setByokProviderMenuOpen((open) => !open)}
            >
              <span>{profile.label}{profile.configured ? " · 已配置" : " · 未配置"}</span>
              <span className="settings-provider-select-chevron" aria-hidden="true">v</span>
            </button>
            {byokProviderMenuOpen && !(busyCodexAcp || busyProviderModels) && (
              <div id="byok-provider-profile-listbox" className="settings-provider-select-menu" role="listbox" aria-label="BYOK 模型来源">
                {byokProfiles.map((item) => {
                  const selected = item.id === profile.id;
                  return (
                    <button
                      key={item.id}
                      type="button"
                      className={`settings-provider-select-option ${selected ? "is-selected" : ""}`}
                      role="option"
                      aria-selected={selected}
                      onClick={() => handleSelectByokProfile(item.id)}
                    >
                      {item.label}{item.configured ? " · 已配置" : " · 未配置"}
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        </div>
        <div className="settings-provider-detail">
          <span className={`settings-row-badge ${profile.configured ? "is-installed" : "is-missing"}`}>
            {profile.configured ? "已配置" : "未配置"}
          </span>
          {renderModelChip(profile.models)}
        </div>
        <label className="settings-field settings-provider-models-field">
          <span>模型列表</span>
          <textarea
            aria-label="byok_provider_models"
            value={providerModelsDraft}
            disabled={busyProviderModels}
            spellCheck={false}
            onChange={(event) => {
              setProviderModelsDraft(event.currentTarget.value);
              setCodexAcpMessage(null);
              setCodexAcpMessageTarget("models");
            }}
          />
        </label>
        <div className="settings-provider-config-actions">
          {codexAcpMessageTarget === "models" && codexAcpMessage && <span className="settings-provider-config-message">{codexAcpMessage}</span>}
          <button
            type="button"
            className="settings-btn"
            disabled={busyProviderModels || !providerModelsDraft.trim()}
            onClick={handleSaveProviderModels}
          >
            {busyProviderModels ? "保存中..." : "保存模型列表"}
          </button>
          <button
            type="button"
            className="settings-btn"
            disabled={busyProviderModels}
            onClick={handleResetProviderModels}
          >
            恢复默认
          </button>
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

  const renderRemotePane = () => {
    const duplicateTarget = remoteDraft
      ? remoteSnapshot.profiles.find((profile) =>
          profile.id !== remoteDraft.id &&
          normalizeRemoteTarget(profile.ssh_target, profile.ssh_port) ===
            normalizeRemoteTarget(remoteDraft.ssh_target, parseRemotePort(remoteDraft.ssh_port)),
        )
      : null;
    return (
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h2 className="settings-section-title">远程机器</h2>
            <p className="settings-section-desc">保存常用 Linux 开发机，默认验证用户目录；从 Workbench 连接机器后再打开项目。</p>
          </div>
          <button type="button" className="settings-btn is-install" onClick={startNewRemoteProfile}>
            添加远程机器
          </button>
        </div>
        {remoteContext && (
          <div className="settings-remote-context-card">
            <div className="settings-provider-config-head">
              <div>
                <span>当前远程上下文</span>
                <p>{remoteContext.workspaceName} · {remoteContext.sshTarget}{remoteContext.sshPort ? `:${remoteContext.sshPort}` : ""}</p>
              </div>
              <code>{remoteContext.remotePath}</code>
            </div>
            <div className="settings-remote-context-grid">
              <div>
                <span className="settings-row-meta">当前运行通道</span>
                <strong>{remoteContext.agentLabel ?? "未识别"}</strong>
              </div>
              <div className="settings-row-actions">
                <button type="button" className="settings-btn" onClick={() => {
                  setActivePane("general");
                  setActiveAgentTab("claude-agent-acp");
                }}>
                  Claude 通道
                </button>
                <button type="button" className="settings-btn" onClick={() => {
                  setActivePane("general");
                  setActiveAgentTab("codex-acp");
                }}>
                  Codex 通道
                </button>
                <button type="button" className="settings-btn" onClick={() => {
                  setActivePane("general");
                  setActiveAgentTab("codebuddy");
                }}>
                  CodeBuddy
                </button>
              </div>
            </div>
          </div>
        )}
        {remoteError && <div className="settings-error">{remoteError}</div>}
        {remoteMessage && <div className="settings-success">{remoteMessage}</div>}
        {remoteDraft && (
          <div className="settings-remote-editor">
            <div className="settings-provider-config-head">
              <div>
                <span>{remoteDraft.id ? "编辑远程机器" : "添加远程机器"}</span>
                <p>这里只保存机器名称、SSH 目标和端口；密码只在验证或连接机器时临时输入。</p>
              </div>
              <button type="button" className="settings-btn" onClick={() => setRemoteDraft(null)}>
                取消
              </button>
            </div>
            <label className="settings-field">
              <span>名称</span>
              <input
                aria-label="remote_profile_name"
                value={remoteDraft.display_name}
                onChange={(event) => updateRemoteDraft({ display_name: event.currentTarget.value })}
                placeholder="开发机"
              />
            </label>
            <label className="settings-field">
              <span>SSH 目标</span>
              <input
                aria-label="remote_profile_ssh_target"
                value={remoteDraft.ssh_target}
                onChange={(event) => updateRemoteDraft({ ssh_target: event.currentTarget.value })}
                placeholder="root@devbox 或 SSH config alias"
              />
            </label>
            <label className="settings-field">
              <span>端口</span>
              <input
                aria-label="remote_profile_ssh_port"
                inputMode="numeric"
                value={remoteDraft.ssh_port}
                onChange={(event) => updateRemoteDraft({ ssh_port: event.currentTarget.value.replace(/[^\d]/g, "") })}
                placeholder="22"
              />
            </label>
            {duplicateTarget && (
              <div className="settings-warning">
                已有同一 SSH 目标：{duplicateTarget.display_name}
              </div>
            )}
            <div className="settings-provider-config-actions">
              <button
                type="button"
                className="settings-btn is-install"
                disabled={busyRemote === "save" || !remoteDraft.display_name.trim() || !remoteDraft.ssh_target.trim()}
                onClick={handleSaveRemoteProfile}
              >
                {busyRemote === "save" ? "保存中..." : "保存远程机器"}
              </button>
            </div>
          </div>
        )}
        {remoteSnapshot.profiles.length === 0 && !remoteDraft ? (
          <div className="settings-empty-panel">
            <div className="settings-row-title">还没有远程机器</div>
            <p>添加一台 Linux 开发机后，可以验证 SSH 和默认用户目录，再从 Workbench 打开远程目录。</p>
            <button type="button" className="settings-btn is-install" onClick={startNewRemoteProfile}>
              添加远程机器
            </button>
          </div>
        ) : (
          <div className="settings-remote-list">
            {remoteSnapshot.profiles.map((profile) => (
              <article key={profile.id} className="settings-remote-card">
                <div className="settings-lsp-head">
                  <div>
                    <div className="settings-row-title">{profile.display_name}</div>
                    <div className="settings-row-meta">
                      <code>{formatRemoteTarget(profile)}</code>
                      <span className={`settings-row-badge ${profile.last_validation?.ok ? "is-installed" : "is-missing"}`}>
                        {profile.last_validation ? profile.last_validation.ok ? "已验证" : "验证失败" : "未验证"}
                      </span>
                    </div>
                  </div>
                  <div className="settings-row-actions">
                    <button type="button" className="settings-btn" onClick={() => editRemoteProfile(profile)}>
                      编辑
                    </button>
                    <button
                      type="button"
                      className="settings-btn"
                      disabled={busyRemote === profile.id}
                      onClick={() => handleDeleteRemoteProfile(profile.id)}
                    >
                      删除
                    </button>
                  </div>
                </div>
                <div className="settings-remote-validate-row">
                  <label className="settings-field">
                    <span>验证目录</span>
                    <input
                      aria-label={`remote_validate_path_${profile.id}`}
                      value={remoteValidationPaths[profile.id] ?? ""}
                      onChange={(event) => {
                        const value = event.currentTarget.value;
                        setRemoteValidationPaths((paths) => ({
                          ...paths,
                          [profile.id]: value,
                        }));
                      }}
                      placeholder="~"
                    />
                  </label>
                  <label className="settings-field">
                    <span>SSH 密码</span>
                    <input
                      aria-label={`remote_validate_password_${profile.id}`}
                      type="password"
                      autoComplete="off"
                      value={remoteValidationPasswords[profile.id] ?? ""}
                      onChange={(event) => {
                        const value = event.currentTarget.value;
                        setRemoteValidationPasswords((passwords) => ({
                          ...passwords,
                          [profile.id]: value,
                        }));
                      }}
                      placeholder="本次使用，不保存"
                    />
                  </label>
                  <button
                    type="button"
                    className="settings-btn"
                    disabled={busyRemote === profile.id}
                    onClick={() => handleValidateRemoteProfile(profile)}
                  >
                    {busyRemote === profile.id ? "验证中..." : "验证"}
                  </button>
                </div>
                <p className="settings-remote-note">验证目录为空时检查远程用户目录；不填密码时使用 SSH key、ssh-agent 或 SSH config。</p>
                {profile.last_validation && (
                  <div className="settings-remote-phases">
                    {profile.last_validation.phases.map((phase) => (
                      <span key={phase.phase} className={`settings-remote-phase is-${phase.status}`} title={phase.message ?? undefined}>
                        {remotePhaseLabel(phase.phase)} · {remotePhaseStatusLabel(phase.status)}
                      </span>
                    ))}
                    <span className="settings-row-meta">
                      {formatValidationTime(profile.last_validation.checked_at_ms)}
                    </span>
                  </div>
                )}
              </article>
            ))}
          </div>
        )}
      </section>
    );
  };

  const startupNoticeCopy = visibleStartupNotice
    ? startupNoticeCopyFor(visibleStartupNotice)
    : null;

  return (
    <div className="settings-page">
      <div className="settings-drag-strip" data-tauri-drag-region />
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
            className={`settings-nav-item ${activePane === "remote" ? "is-active" : ""}`}
            onClick={() => setActivePane("remote")}
          >
            远程
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
          <h1>{settingsPaneTitle(activePane)}</h1>
          <p>{settingsPaneDescription(activePane)}</p>
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
                          当前：{snapshot.codex_acp.selected_profile_id === "default" ? "默认" : "BYOK"}
                        </span>
                      </div>
                      {renderAgentRuntime("codex-acp")}
                      <div className="settings-provider-options" role="radiogroup" aria-label="Codex channel">
                        {(["default", "byok"] as const).map((channel) => {
                          const selected = channel === "default"
                            ? snapshot.codex_acp.selected_profile_id === "default"
                            : snapshot.codex_acp.selected_profile_id !== "default";
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
                                <span>{channel === "default" ? "默认" : "BYOK"}</span>
                                <span>
                                  {channel === "default"
                                    ? "本机默认配置"
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
                      {codexAcpMessageTarget === "channel" && codexAcpMessage && (
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
                          <p>Claude 使用共享 BYOK 模型池。</p>
                        </div>
                        <span className="settings-provider-active">
                          当前：BYOK
                        </span>
                      </div>
                      {renderAgentRuntime("claude-agent-acp")}
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

        {activePane === "remote" && renderRemotePane()}

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
  const detail = notice.message ? ` ${notice.message}` : "";
  return {
    title: "模型来源还没设置好",
    message: `还没有可用于新建会话的 provider。请保存 BYOK API key，或安装并配置 CodeBuddy。${detail}`,
    action: "去设置",
  };
}

function settingsPaneTitle(pane: SettingsPane): string {
  if (pane === "remote") return "远程";
  if (pane === "lsp") return "LSP";
  return "通用";
}

function settingsPaneDescription(pane: SettingsPane): string {
  if (pane === "remote") return "管理远程 Linux 开发机，并在打开远程目录前验证 SSH。";
  if (pane === "lsp") return "管理编辑器诊断、悬浮提示和补全使用的 language server。";
  return "外观、默认提供者和智能体配置。";
}

function parseRemotePort(value: string): number | null {
  const parsed = Number(value.trim());
  return Number.isInteger(parsed) && parsed > 0 ? parsed : null;
}

function normalizeRemoteTarget(target: string, port?: number | null): string {
  return `${target.trim().toLowerCase()}:${port ?? 22}`;
}

function formatRemoteTarget(profile: RemoteMachineProfile): string {
  return `${profile.ssh_target}${profile.ssh_port ? `:${profile.ssh_port}` : ""}`;
}

function remotePhaseLabel(phase: RemoteValidationPhaseKind): string {
  switch (phase) {
    case "ssh":
      return "SSH";
    case "remote_path":
      return "目录";
    case "agent_command":
      return "Agent";
    case "acp_ready":
      return "ACP";
  }
}

function remotePhaseStatusLabel(status: RemoteValidationPhaseStatus): string {
  switch (status) {
    case "succeeded":
      return "通过";
    case "failed":
      return "失败";
    case "skipped":
      return "跳过";
  }
}

function formatValidationTime(timestampMs: number): string {
  if (!timestampMs) return "";
  return `上次验证：${new Date(timestampMs).toLocaleString()}`;
}

function splitArgs(value: string): string[] {
  return value
    .split(/\s+/)
    .map((arg) => arg.trim())
    .filter(Boolean);
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
