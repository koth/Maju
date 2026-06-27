import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { confirm } from "@tauri-apps/plugin-dialog";
import {
  ArchiveRestore,
  ChevronDown,
  Folder,
  ListFilter,
  Search,
  Trash2,
  X,
} from "lucide-react";
import type {
  AgentCliId,
  AgentInstallResult,
  AgentProviderProfile,
  AgentSettingsSnapshot,
  CustomProviderProtocol,
  AppTheme,
  ArchivedSessionListItem,
  LspSettingsSnapshot,
  LspServerConfigInput,
  RemoteMachineProfile,
  RemoteMachineProfileInput,
  RemoteMachineProfilesSnapshot,
  RemoteValidationPhaseKind,
  RemoteValidationPhaseStatus,
  UsageSummaryGroupBy,
  UsageSummaryRow,
  ImageGenerateProtocol,
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
  settingsClearProviderConfiguration,
  settingsResetProviderModels,
  settingsRemoveCustomProvider,
  settingsSaveCustomProvider,
  settingsSaveProviderModels,
  settingsSyncProviderModelsFromUrl,
  settingsSelectClaudeFastModel,
  settingsSaveLspServer,
  settingsSaveRemoteProfile,
  settingsSelectAgentProviderProfile,
  settingsSelectAgent,
  settingsSelectTheme,
  settingsSaveWebToolsProviderKey,
  settingsSaveWebToolsSettings,
  settingsSaveImageGenerateApiKey,
  settingsSaveImageGenerateSettings,
  settingsSaveImageViewSettings,
  settingsValidateRemoteProfile,
  sessionDeleteAllArchived,
  sessionDeleteArchived,
  sessionListArchived,
  sessionUnarchive,
  usageGetSummary,
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

export type AgentSettingsTab = Extract<
  AgentCliId,
  "codebuddy" | "codex-acp" | "claude-agent-acp"
>;
export type SettingsPane =
  | "general"
  | "web"
  | "image"
  | "archive"
  | "remote"
  | "usage"
  | "lsp";
type SettingsScope = "local" | "remote";
type UsageDateRange = "today" | "7d" | "30d" | "all";
type UsageWorkspaceScope = "current" | "all";
type UpdateStatus =
  | "idle"
  | "checking"
  | "up-to-date"
  | "available"
  | "installing"
  | "installed"
  | "error";
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

const WEB_TOOL_PROVIDER_OPTIONS = [
  { id: "brave", label: "Brave Search", apiKeyLabel: "Brave Search API key" },
  { id: "tavily", label: "Tavily", apiKeyLabel: "Tavily API key" },
] as const;

function webToolProviderMeta(provider?: string | null) {
  const id = provider?.trim() || "brave";
  return (
    WEB_TOOL_PROVIDER_OPTIONS.find((option) => option.id === id) ?? {
      id,
      label: id,
      apiKeyLabel: `${id} API key`,
    }
  );
}

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
    <span
      className="settings-model-chip"
      title={label}
      aria-label={label}
      tabIndex={0}
    >
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
  const [activePane, setActivePane] = useState<SettingsPane>(
    initialPane ?? "general",
  );
  const [activeAgentTab, setActiveAgentTab] = useState<AgentSettingsTab>(
    initialAgentTab ?? "claude-agent-acp",
  );
  const [settingsScope, setSettingsScope] = useState<SettingsScope>("local");
  const [visibleStartupNotice, setVisibleStartupNotice] =
    useState<SettingsStartupNotice | null>(startupNotice ?? null);
  const [snapshot, setSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [lspSnapshot, setLspSnapshot] = useState<LspSettingsSnapshot | null>(
    null,
  );
  const [remoteSnapshot, setRemoteSnapshot] =
    useState<RemoteMachineProfilesSnapshot>({ profiles: [] });
  const [archivedSessions, setArchivedSessions] = useState<
    ArchivedSessionListItem[]
  >([]);
  const [lspDrafts, setLspDrafts] = useState<
    Record<string, LspServerConfigInput>
  >({});
  const [remoteDraft, setRemoteDraft] = useState<RemoteProfileDraft | null>(
    null,
  );
  const [remoteValidationPaths, setRemoteValidationPaths] = useState<
    Record<string, string>
  >({});
  const [remoteValidationPasswords, setRemoteValidationPasswords] = useState<
    Record<string, string>
  >({});
  const [archivedSearch, setArchivedSearch] = useState("");
  const [archivedChatFilter, setArchivedChatFilter] = useState<
    "all" | "with_messages"
  >("all");
  const [archivedWorkspaceFilter, setArchivedWorkspaceFilter] = useState("all");
  const [loading, setLoading] = useState(true);
  const [archivedLoading, setArchivedLoading] = useState(false);
  const [busyAgent, setBusyAgent] = useState<AgentCliId | null>(null);
  const [busyCodexAcp, setBusyCodexAcp] = useState(false);
  const [busyTheme, setBusyTheme] = useState<AppTheme | null>(null);
  const [busyLsp, setBusyLsp] = useState<string | null>(null);
  const [busyRemote, setBusyRemote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lspError, setLspError] = useState<string | null>(null);
  const [remoteError, setRemoteError] = useState<string | null>(null);
  const [archivedError, setArchivedError] = useState<string | null>(null);
  const [remoteMessage, setRemoteMessage] = useState<string | null>(null);
  const [archivedMessage, setArchivedMessage] = useState<string | null>(null);
  const [busyArchivedSession, setBusyArchivedSession] = useState<string | null>(
    null,
  );
  const [deletingAllArchived, setDeletingAllArchived] = useState(false);
  const [installResult, setInstallResult] = useState<AgentInstallResult | null>(
    null,
  );
  const [probeMessages, setProbeMessages] = useState<Record<string, string>>(
    {},
  );
  const [codexProfileId, setCodexProfileId] = useState("byok");
  const [byokProfileId, setByokProfileId] = useState("deepseek");
  const [byokProviderMenuOpen, setByokProviderMenuOpen] = useState(false);
  const [byokProfileInitialized, setByokProfileInitialized] = useState(false);
  const [codexAcpApiKey, setCodexAcpApiKey] = useState("");
  const [providerModelsDraft, setProviderModelsDraft] = useState("");
  const [modelListUrlDraft, setModelListUrlDraft] = useState("");
  const [customProviderLabel, setCustomProviderLabel] = useState("");
  const [customProviderEndpoint, setCustomProviderEndpoint] = useState("");
  const [customProviderProtocol, setCustomProviderProtocol] =
    useState<CustomProviderProtocol>("chat_completions");
  const [customProviderApiKey, setCustomProviderApiKey] = useState("");
  const [customProviderEditorOpen, setCustomProviderEditorOpen] =
    useState(false);
  const [customProviderEditorMode, setCustomProviderEditorMode] =
    useState<"add" | "edit">("add");
  const [customProviderEditorId, setCustomProviderEditorId] =
    useState<string | null>(null);
  const [busyProviderModels, setBusyProviderModels] = useState(false);
  const [busyCustomProvider, setBusyCustomProvider] = useState(false);
  const [busyClaudeFastModel, setBusyClaudeFastModel] = useState(false);
  const [busyWebTools, setBusyWebTools] = useState(false);
  const [busyImage, setBusyImage] = useState(false);
  const [imageMessage, setImageMessage] = useState<string | null>(null);
  const [imageViewDraftProvider, setImageViewDraftProvider] = useState("");
  const [imageViewDraftModel, setImageViewDraftModel] = useState("");
  const [imageGenDraftProtocol, setImageGenDraftProtocol] =
    useState<ImageGenerateProtocol>("openai_images");
  const [imageGenDraftBaseUrl, setImageGenDraftBaseUrl] = useState("");
  const [imageGenDraftModel, setImageGenDraftModel] = useState("");
  const [imageGenDraftSize, setImageGenDraftSize] = useState("1024x1024");
  const [imageGenDraftApiKeyEnv, setImageGenDraftApiKeyEnv] = useState("");
  const [imageGenApiKey, setImageGenApiKey] = useState("");
  const [codexAcpMessage, setCodexAcpMessage] = useState<string | null>(null);
  const [codexAcpMessageTarget, setCodexAcpMessageTarget] = useState<
    "channel" | "byok" | "models" | "custom" | "claude-fast"
  >("channel");
  const [webToolsApiKey, setWebToolsApiKey] = useState("");
  const [webToolsMessage, setWebToolsMessage] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>("idle");
  const [updateInfo, setUpdateInfo] = useState<AppUpdateInfo | null>(null);
  const [updateMessage, setUpdateMessage] = useState<string | null>(null);
  const [updateProgress, setUpdateProgress] =
    useState<AppUpdateProgress | null>(null);
  const [usageGroupBy, setUsageGroupBy] =
    useState<UsageSummaryGroupBy>("model");
  const [usageDateRange, setUsageDateRange] = useState<UsageDateRange>("all");
  const [usageWorkspaceScope, setUsageWorkspaceScope] =
    useState<UsageWorkspaceScope>("current");
  const [usageIncludeArchived, setUsageIncludeArchived] = useState(false);
  const [usageRows, setUsageRows] = useState<UsageSummaryRow[]>([]);
  const [usageLoading, setUsageLoading] = useState(false);
  const [usageError, setUsageError] = useState<string | null>(null);
  const byokProviderMenuRef = useRef<HTMLDivElement>(null);
  const canUseRemoteSettings = !!remoteContext?.profileId;
  const settingsRemoteProfileId =
    settingsScope === "remote" ? (remoteContext?.profileId ?? null) : null;
  const editingRemoteSettings =
    settingsScope === "remote" && !!settingsRemoteProfileId;

  const applyLspSnapshot = useCallback((nextSnapshot: LspSettingsSnapshot) => {
    setLspSnapshot(nextSnapshot);
    setLspDrafts(
      Object.fromEntries(
        nextSnapshot.servers.map((server) => [
          server.languageId,
          {
            languageId: server.languageId,
            enabled: server.enabled,
            command: server.command,
            args: server.args,
          },
        ]),
      ),
    );
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    setLspError(null);
    setRemoteError(null);

    const remoteProfilesPromise = settingsGetRemoteProfiles()
      .then(setRemoteSnapshot)
      .catch((e) => setRemoteError(String(e)));

    try {
      const nextSnapshot = await settingsGetAgentSnapshot(
        settingsRemoteProfileId,
      );
      setSnapshot(nextSnapshot);
      if (!editingRemoteSettings) {
        onThemeChange?.(applyAppTheme(nextSnapshot.settings.theme));
      }
    } catch (e) {
      setError(String(e));
    }

    try {
      const nextLspSnapshot = await settingsGetLspSnapshot(
        settingsRemoteProfileId,
      );
      applyLspSnapshot(nextLspSnapshot);
    } catch (e) {
      setLspError(String(e));
    } finally {
      await remoteProfilesPromise;
      setLoading(false);
    }
  }, [
    applyLspSnapshot,
    editingRemoteSettings,
    onThemeChange,
    settingsRemoteProfileId,
  ]);

  const loadArchivedSessions = useCallback(async () => {
    setArchivedLoading(true);
    setArchivedError(null);
    try {
      setArchivedSessions(await sessionListArchived());
    } catch (e) {
      setArchivedError(String(e));
    } finally {
      setArchivedLoading(false);
    }
  }, []);

  const loadUsageSummary = useCallback(async () => {
    setUsageLoading(true);
    setUsageError(null);
    try {
      const range = usageDateRangeBounds(usageDateRange);
      setUsageRows(
        await usageGetSummary({
          group_by: usageGroupBy,
          all_workspaces: usageWorkspaceScope === "all",
          include_archived: usageIncludeArchived,
          from: range.from,
          to: range.to,
        }),
      );
    } catch (e) {
      setUsageError(String(e));
    } finally {
      setUsageLoading(false);
    }
  }, [usageDateRange, usageGroupBy, usageIncludeArchived, usageWorkspaceScope]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (activePane !== "archive") return;
    loadArchivedSessions();
  }, [activePane, loadArchivedSessions]);

  useEffect(() => {
    if (activePane !== "usage") return;
    loadUsageSummary();
  }, [activePane, loadUsageSummary]);

  useEffect(() => {
    if (archivedWorkspaceFilter === "all") return;
    if (
      archivedSessions.some(
        (session) => session.workspace_root === archivedWorkspaceFilter,
      )
    )
      return;
    setArchivedWorkspaceFilter("all");
  }, [archivedSessions, archivedWorkspaceFilter]);

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
    if (
      selectedAgent === "codebuddy" ||
      selectedAgent === "codex-acp" ||
      selectedAgent === "claude-agent-acp"
    ) {
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
    const byokProfiles = selectableByokSourceProfiles(
      snapshot.codex_acp.profiles,
    );
    const selected = snapshot.codex_acp.selected_profile_id;
    if (
      selected !== "default" &&
      selected !== "byok" &&
      byokProfiles.some((profile) => profile.id === selected)
    ) {
      setByokProfileId(selected);
    } else if (selected === "byok") {
      setByokProfileId(
        byokProfiles.find((profile) => profile.configured)?.id ??
          byokProfiles[0]?.id ??
          "deepseek",
      );
    } else if (byokProfiles[0]) {
      setByokProfileId(byokProfiles[0].id);
    }
    setByokProfileInitialized(true);
  }, [byokProfileInitialized, snapshot]);

  useEffect(() => {
    if (!snapshot) return;
    const profile = snapshot.codex_acp.profiles.find(
      (item) => item.id === byokProfileId,
    );
    if (!profile) return;
    setProviderModelsDraft(profile.models.join("\n"));
    setModelListUrlDraft(profile.model_list_url ?? "");
    if (profile.custom) {
      setCustomProviderLabel(
        profile.label === "Custom Provider" ? "" : profile.label,
      );
      setCustomProviderEndpoint(profile.base_url ?? "");
      setCustomProviderProtocol(profile.protocol ?? "chat_completions");
    }
  }, [byokProfileId, snapshot]);

  useEffect(() => {
    if (!byokProviderMenuOpen) return;

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (
        target instanceof Node &&
        byokProviderMenuRef.current?.contains(target)
      )
        return;
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

  const openRemoteAgentSettings = useCallback(
    (tab: AgentSettingsTab) => {
      if (!canUseRemoteSettings) return;
      setSettingsScope("remote");
      setActivePane("general");
      setActiveAgentTab(tab);
    },
    [canUseRemoteSettings],
  );

  const returnToLocalSettings = useCallback(() => {
    setSettingsScope("local");
    setError(null);
    setLspError(null);
  }, []);

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

  const handleSelect = useCallback(
    async (agent: AgentCliId) => {
      setBusyAgent(agent);
      setError(null);
      try {
        setSnapshot(await settingsSelectAgent(agent, settingsRemoteProfileId));
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyAgent(null);
      }
    },
    [settingsRemoteProfileId],
  );

  const handleThemeSelect = useCallback(
    async (theme: AppTheme) => {
      setBusyTheme(theme);
      setError(null);
      try {
        const nextSnapshot = await settingsSelectTheme(
          theme,
          settingsRemoteProfileId,
        );
        setSnapshot(nextSnapshot);
        onThemeChange?.(applyAppTheme(nextSnapshot.settings.theme));
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyTheme(null);
      }
    },
    [onThemeChange, settingsRemoteProfileId],
  );

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
      const codexSnapshot = await settingsSaveAgentProviderSecret(
        "codex",
        byokProfileId,
        key,
        settingsRemoteProfileId,
      );
      const nextSnapshot = await settingsSaveAgentProviderSecret(
        "claude",
        byokProfileId,
        key,
        settingsRemoteProfileId,
      );
      setSnapshot({
        ...nextSnapshot,
        codex_acp: codexSnapshot.codex_acp,
        settings: {
          ...nextSnapshot.settings,
          codex_connection_mode: codexSnapshot.settings.codex_connection_mode,
          selected_codex_provider_profile_id:
            codexSnapshot.settings.selected_codex_provider_profile_id,
        },
      });
      setCodexAcpApiKey("");
      setCodexAcpMessageTarget("byok");
      setCodexAcpMessage(
        `${providerLabel(codexSnapshot.codex_acp.profiles, byokProfileId)} API key 已更新，后续新建会话生效`,
      );
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
      const nextSnapshot = await settingsSaveProviderModels(
        byokProfileId,
        models,
        settingsRemoteProfileId,
      );
      setSnapshot(nextSnapshot);
      setCodexAcpMessageTarget("models");
      setCodexAcpMessage(
        `${providerLabel(nextSnapshot.codex_acp.profiles, byokProfileId)} 模型列表已更新，后续新建会话生效`,
      );
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
      const nextSnapshot = await settingsResetProviderModels(
        byokProfileId,
        settingsRemoteProfileId,
      );
      setSnapshot(nextSnapshot);
      setCodexAcpMessageTarget("models");
      setCodexAcpMessage(
        `${providerLabel(nextSnapshot.codex_acp.profiles, byokProfileId)} 模型列表已恢复默认，后续新建会话生效`,
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyProviderModels(false);
    }
  }, [byokProfileId, settingsRemoteProfileId]);

  const handleSyncProviderModelsFromUrl = useCallback(async () => {
    const modelListUrl = modelListUrlDraft.trim();
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("models");
    if (!byokProfileId) {
      setError("请选择 BYOK 模型来源");
      return;
    }
    if (!modelListUrl) {
      setError("模型列表 URL 不能为空");
      return;
    }
    setBusyProviderModels(true);
    try {
      const nextSnapshot = await settingsSyncProviderModelsFromUrl(
        byokProfileId,
        modelListUrl,
        settingsRemoteProfileId,
      );
      setSnapshot(nextSnapshot);
      const profile = nextSnapshot.codex_acp.profiles.find(
        (item) => item.id === byokProfileId,
      );
      if (profile) {
        setProviderModelsDraft(profile.models.join("\n"));
        setModelListUrlDraft(profile.model_list_url ?? modelListUrl);
      }
      setCodexAcpMessageTarget("models");
      setCodexAcpMessage(
        `${providerLabel(nextSnapshot.codex_acp.profiles, byokProfileId)} 模型列表已同步，后续新建会话生效`,
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyProviderModels(false);
    }
  }, [byokProfileId, modelListUrlDraft, settingsRemoteProfileId]);

  const handleSaveCustomProvider = useCallback(async () => {
    const label = customProviderLabel.trim();
    const endpoint = customProviderEndpoint.trim();
    const apiKey = customProviderApiKey.trim();
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("custom");
    if (!label) {
      setError("Provider 名称不能为空");
      return;
    }
    if (!endpoint) {
      setError("Endpoint 不能为空");
      return;
    }
    if (customProviderEditorMode === "add" && !apiKey) {
      setError("API key 不能为空");
      return;
    }
    setBusyCustomProvider(true);
    try {
      const nextSnapshot = await settingsSaveCustomProvider(
        {
          providerId: customProviderEditorMode === "edit" ? customProviderEditorId : null,
          label,
          endpoint,
          protocol: customProviderProtocol,
          apiKey,
          modelListUrl: null,
        },
        settingsRemoteProfileId,
      );
      setSnapshot(nextSnapshot);
      const nextProfile =
        nextSnapshot.codex_acp.profiles.find(
          (item) => item.id === customProviderEditorId,
        ) ??
        nextSnapshot.codex_acp.profiles.find(
          (item) => item.custom && item.label === label,
        ) ??
        nextSnapshot.codex_acp.profiles.find(
          (item) => item.custom && item.base_url === endpoint,
        );
      if (nextProfile) {
        setByokProfileId(nextProfile.id);
      }
      setCustomProviderEditorOpen(false);
      setCustomProviderApiKey("");
      setCustomProviderEditorId(null);
      if (nextProfile) {
        setProviderModelsDraft(nextProfile.models.join("\n"));
        setModelListUrlDraft(nextProfile.model_list_url ?? "");
      }
      setCustomProviderEditorMode("add");
      setCodexAcpMessageTarget("custom");
      setCodexAcpMessage(
        customProviderEditorMode === "edit"
          ? "自定义 provider 已更新，后续新建会话生效"
          : "自定义 provider 已保存，后续新建会话生效",
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCustomProvider(false);
    }
  }, [
    customProviderApiKey,
    customProviderEditorId,
    customProviderEndpoint,
    customProviderLabel,
    customProviderEditorMode,
    customProviderProtocol,
    settingsRemoteProfileId,
  ]);

  const handleRemoveCustomProvider = useCallback(async (profile: AgentProviderProfile) => {
    const accepted = await confirm(
      `确定移除 ${profile.label}？此操作会删除 endpoint、模型列表和已保存的 API key。`,
    );
    if (!accepted) return;
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("custom");
    setBusyCustomProvider(true);
    try {
      const nextSnapshot = await settingsRemoveCustomProvider(
        profile.id,
        settingsRemoteProfileId,
      );
      setSnapshot(nextSnapshot);
      const nextByokSource = selectableByokSourceProfiles(
        nextSnapshot.codex_acp.profiles,
      ).find((item) => item.id !== profile.id);
      setByokProfileId(nextByokSource?.id ?? "timiai");
      setProviderModelsDraft("");
      setModelListUrlDraft("");
      setCustomProviderLabel("");
      setCustomProviderEndpoint("");
      setCustomProviderProtocol("chat_completions");
      setCustomProviderApiKey("");
      setCustomProviderEditorMode("add");
      setCustomProviderEditorId(null);
      setCustomProviderEditorOpen(false);
      setCodexAcpMessageTarget("custom");
      setCodexAcpMessage("自定义 provider 已移除，后续新建会话生效");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCustomProvider(false);
    }
  }, [settingsRemoteProfileId]);

  const handleClearByokProviderConfiguration = useCallback(
    async (profile: AgentProviderProfile) => {
      const accepted = await confirm(
        `确定清除 ${profile.label} 设置？此操作会删除已保存的 API key、模型列表和列表 URL。`,
      );
      if (!accepted) return;
      setError(null);
      setCodexAcpMessage(null);
      setCodexAcpMessageTarget("byok");
      setByokProviderMenuOpen(false);
      setBusyProviderModels(true);
      try {
        const nextSnapshot = await settingsClearProviderConfiguration(
          profile.id,
          settingsRemoteProfileId,
        );
        setSnapshot(nextSnapshot);
        const nextByokSource = selectableByokSourceProfiles(
          nextSnapshot.codex_acp.profiles,
        ).find((item) => item.id !== profile.id);
        setByokProfileId(nextByokSource?.id ?? "timiai");
        setProviderModelsDraft(nextByokSource?.models.join("\n") ?? "");
        setModelListUrlDraft(nextByokSource?.model_list_url ?? "");
        setCodexAcpMessageTarget("byok");
        setCodexAcpMessage(`${profile.label} 设置已清除，后续新建会话生效`);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyProviderModels(false);
      }
    },
    [settingsRemoteProfileId],
  );

  const handleOpenCustomProviderEditor = useCallback(() => {
    setCustomProviderEditorMode("add");
    setCustomProviderEditorId(null);
    setCustomProviderLabel("");
    setCustomProviderEndpoint("");
    setCustomProviderProtocol("chat_completions");
    setCustomProviderApiKey("");
    setByokProviderMenuOpen(false);
    setCustomProviderEditorOpen(true);
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("custom");
  }, []);

  const handleOpenCustomProviderEdit = useCallback(
    (profile: AgentProviderProfile) => {
      if (!profile.custom) return;
      setCustomProviderEditorMode("edit");
      setCustomProviderEditorId(profile.id);
      setCustomProviderLabel(
        profile.label === "Custom Provider" ? "" : profile.label,
      );
      setCustomProviderEndpoint(profile.base_url ?? "");
      setCustomProviderProtocol(profile.protocol ?? "chat_completions");
      setCustomProviderApiKey("");
      setByokProviderMenuOpen(false);
      setCustomProviderEditorOpen(true);
      setCodexAcpMessage(null);
      setCodexAcpMessageTarget("custom");
    },
    [],
  );

  const handleCloseCustomProviderEditor = useCallback(() => {
    if (busyCustomProvider) return;
    setCustomProviderEditorOpen(false);
  }, [busyCustomProvider]);

  useEffect(() => {
    if (!customProviderEditorOpen) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        handleCloseCustomProviderEditor();
      }
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [customProviderEditorOpen, handleCloseCustomProviderEditor]);

  const handleSelectByokProfile = useCallback((profileId: string) => {
    setByokProfileId(profileId);
    setByokProviderMenuOpen(false);
    setCustomProviderEditorOpen(false);
    setCodexAcpApiKey("");
    setCodexAcpMessage(null);
    setCodexAcpMessageTarget("byok");
  }, []);

  const handleSelectClaudeFastModel = useCallback(
    async (modelId: string) => {
      setError(null);
      setCodexAcpMessage(null);
      setCodexAcpMessageTarget("claude-fast");
      setBusyClaudeFastModel(true);
      try {
        const nextSnapshot = await settingsSelectClaudeFastModel(
          modelId || null,
          settingsRemoteProfileId,
        );
        setSnapshot(nextSnapshot);
        setCodexAcpMessageTarget("claude-fast");
        setCodexAcpMessage("Claude 快速模型已更新，后续新建会话生效");
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyClaudeFastModel(false);
      }
    },
    [settingsRemoteProfileId],
  );

  const handleToggleWebTools = useCallback(
    async (enabled: boolean) => {
      const provider = snapshot?.web_tools.provider ?? "brave";
      setBusyWebTools(true);
      setError(null);
      setWebToolsMessage(null);
      try {
        const nextSnapshot = await settingsSaveWebToolsSettings(
          enabled,
          provider,
        );
        setSnapshot(nextSnapshot);
        setWebToolsMessage(
          enabled
            ? "Web 工具已启用，后续新建或重连本机会话生效"
            : "Web 工具已关闭",
        );
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyWebTools(false);
      }
    },
    [snapshot?.web_tools.provider],
  );

  const handleSelectWebToolsProvider = useCallback(
    async (provider: string) => {
      const enabled = snapshot?.web_tools.enabled ?? false;
      if (provider === snapshot?.web_tools.provider) return;
      setBusyWebTools(true);
      setError(null);
      setWebToolsMessage(null);
      try {
        const nextSnapshot = await settingsSaveWebToolsSettings(
          enabled,
          provider,
        );
        setSnapshot(nextSnapshot);
        setWebToolsApiKey("");
        setWebToolsMessage(
          `Web 工具搜索来源已切换到 ${webToolProviderMeta(provider).label}`,
        );
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyWebTools(false);
      }
    },
    [snapshot?.web_tools.enabled, snapshot?.web_tools.provider],
  );

  const handleSaveWebToolsKey = useCallback(async () => {
    const key = webToolsApiKey.trim();
    const provider = snapshot?.web_tools.provider ?? "brave";
    const providerMeta = webToolProviderMeta(provider);
    setError(null);
    setWebToolsMessage(null);
    if (!key) {
      setError(`${providerMeta.apiKeyLabel} 不能为空`);
      return;
    }
    setBusyWebTools(true);
    try {
      const nextSnapshot = await settingsSaveWebToolsProviderKey(provider, key);
      setSnapshot(nextSnapshot);
      setWebToolsApiKey("");
      setWebToolsMessage(
        `${providerMeta.apiKeyLabel} 已保存，后续新建或重连本机会话生效`,
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyWebTools(false);
    }
  }, [snapshot?.web_tools.provider, webToolsApiKey]);

  // Sync image settings drafts from the snapshot.
  useEffect(() => {
    if (!snapshot?.image) return;
    const image = snapshot.image;
    setImageViewDraftProvider(image.view_provider);
    setImageViewDraftModel(image.view_model);
    setImageGenDraftProtocol(image.generate_protocol);
    setImageGenDraftBaseUrl(image.generate_base_url);
    setImageGenDraftModel(image.generate_model);
    setImageGenDraftSize(image.generate_default_size || "1024x1024");
    setImageGenApiKey("");
  }, [
    snapshot?.image?.view_provider,
    snapshot?.image?.view_model,
    snapshot?.image?.generate_protocol,
    snapshot?.image?.generate_base_url,
    snapshot?.image?.generate_model,
    snapshot?.image?.generate_default_size,
  ]);

  // Image view models must follow the *draft* provider so the picker updates
  // as soon as the user changes the provider dropdown, before saving. The
  // snapshot's `view_models` is keyed off the *saved* provider, so derive the
  // list from the matching BYOK profile's catalog models instead. Fall back to
  // `snapshot.image.view_models` (the saved-provider list) when the draft is
  // unset so an already-configured provider still shows its models on load.
  const imageViewModelOptions = useMemo(() => {
    if (!snapshot) return [];
    if (!imageViewDraftProvider) {
      return snapshot.image?.view_models ?? [];
    }
    const profile = snapshot.codex_acp.profiles.find(
      (item) => item.id === imageViewDraftProvider,
    );
    return profile?.models ?? snapshot.image?.view_models ?? [];
  }, [
    snapshot,
    imageViewDraftProvider,
    snapshot?.image?.view_models,
  ]);

  const imageViewDirty =
    imageViewDraftProvider !== (snapshot?.image?.view_provider ?? "") ||
    imageViewDraftModel !== (snapshot?.image?.view_model ?? "");

  const handleSaveImageView = useCallback(async () => {
    setBusyImage(true);
    setImageMessage(null);
    try {
      const next = await settingsSaveImageViewSettings(
        snapshot?.image?.enabled ?? false,
        imageViewDraftProvider,
        imageViewDraftModel,
      );
      setSnapshot(next);
      setImageMessage("识图配置已保存。");
    } catch (e) {
      setError(String(e));
      setImageMessage(String(e));
    } finally {
      setBusyImage(false);
    }
  }, [
    snapshot?.image?.enabled,
    imageViewDraftProvider,
    imageViewDraftModel,
  ]);

  const handleToggleImageEnabled = useCallback(
    async (enabled: boolean) => {
      setBusyImage(true);
      setImageMessage(null);
      try {
        const next = await settingsSaveImageViewSettings(
          enabled,
          imageViewDraftProvider,
          imageViewDraftModel,
        );
        setSnapshot(next);
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyImage(false);
      }
    },
    [imageViewDraftProvider, imageViewDraftModel],
  );

  const imageGenerateDirty =
    imageGenDraftProtocol !== (snapshot?.image?.generate_protocol ?? "openai_images") ||
    imageGenDraftBaseUrl !== (snapshot?.image?.generate_base_url ?? "") ||
    imageGenDraftModel !== (snapshot?.image?.generate_model ?? "") ||
    imageGenDraftSize !== (snapshot?.image?.generate_default_size ?? "1024x1024") ||
    imageGenDraftApiKeyEnv !== "";

  const handleSaveImageGenerate = useCallback(async () => {
    setBusyImage(true);
    setImageMessage(null);
    try {
      const next = await settingsSaveImageGenerateSettings(
        imageGenDraftProtocol,
        imageGenDraftBaseUrl,
        imageGenDraftModel,
        imageGenDraftSize,
        imageGenDraftApiKeyEnv,
      );
      setSnapshot(next);
      setImageMessage("生/改图配置已保存。");
    } catch (e) {
      setError(String(e));
      setImageMessage(String(e));
    } finally {
      setBusyImage(false);
    }
  }, [
    imageGenDraftProtocol,
    imageGenDraftBaseUrl,
    imageGenDraftModel,
    imageGenDraftSize,
    imageGenDraftApiKeyEnv,
  ]);

  const handleSaveImageGenerateKey = useCallback(async () => {
    const key = imageGenApiKey.trim();
    if (!key) return;
    setBusyImage(true);
    setImageMessage(null);
    try {
      const next = await settingsSaveImageGenerateApiKey(key);
      setSnapshot(next);
      setImageGenApiKey("");
      setImageMessage("生/改图 API key 已保存。");
    } catch (e) {
      setError(String(e));
      setImageMessage(String(e));
    } finally {
      setBusyImage(false);
    }
  }, [imageGenApiKey]);

  const handleSelectCodexChannel = useCallback(
    async (channel: "default" | "byok") => {
      const byokProfiles = snapshot
        ? selectableByokSourceProfiles(
            snapshot.codex_acp.profiles,
            byokProfileId,
          )
        : [];
      const selectedByokProfileId =
        byokProfiles.find((profile) => profile.id === byokProfileId)?.id ??
        (codexProfileId !== "default" && codexProfileId !== "byok"
          ? codexProfileId
          : undefined) ??
        byokProfiles.find((profile) => profile.configured)?.id ??
        byokProfiles[0]?.id;
      const nextProfileId = channel === "default" ? "default" : "byok";
      if (snapshot?.codex_acp.selected_profile_id === nextProfileId) return;
      setBusyCodexAcp(true);
      setError(null);
      setCodexAcpMessage(null);
      setCodexAcpMessageTarget("channel");
      try {
        const nextSnapshot = await settingsSelectAgentProviderProfile(
          "codex",
          nextProfileId,
          settingsRemoteProfileId,
        );
        setSnapshot(nextSnapshot);
        setCodexProfileId(nextProfileId);
        if (channel === "byok") {
          setByokProfileId(selectedByokProfileId ?? byokProfileId);
        }
        setCodexAcpApiKey("");
        setCodexAcpMessageTarget("channel");
        setCodexAcpMessage(
          `Codex 通道已切换到 ${channel === "default" ? "默认" : "BYOK"}`,
        );
      } catch (e) {
        setError(String(e));
      } finally {
        setBusyCodexAcp(false);
      }
    },
    [
      byokProfileId,
      codexProfileId,
      settingsRemoteProfileId,
      snapshot?.codex_acp.profiles,
      snapshot?.codex_acp.selected_profile_id,
    ],
  );

  const updateLspDraft = useCallback(
    (languageId: string, patch: Partial<LspServerConfigInput>) => {
      setLspDrafts((drafts) => ({
        ...drafts,
        [languageId]: {
          ...drafts[languageId],
          languageId,
          ...patch,
        },
      }));
    },
    [],
  );

  const handleProbeLsp = useCallback(
    async (languageId: string) => {
      const draft = lspDrafts[languageId];
      if (!draft) return;
      setBusyLsp(languageId);
      setLspError(null);
      try {
        const result = await settingsProbeLspServer(
          draft.command,
          settingsRemoteProfileId,
        );
        setProbeMessages((messages) => ({
          ...messages,
          [languageId]: result.available
            ? `已找到：${result.resolvedPath ?? draft.command}`
            : (result.message ?? "未找到命令"),
        }));
      } catch (e) {
        setLspError(String(e));
      } finally {
        setBusyLsp(null);
      }
    },
    [lspDrafts, settingsRemoteProfileId],
  );

  const handleSaveLsp = useCallback(
    async (languageId: string) => {
      const draft = lspDrafts[languageId];
      if (!draft) return;
      setBusyLsp(languageId);
      setLspError(null);
      try {
        const nextSnapshot = await settingsSaveLspServer(
          draft,
          settingsRemoteProfileId,
        );
        applyLspSnapshot(nextSnapshot);
        setProbeMessages((messages) => ({
          ...messages,
          [languageId]: "已保存",
        }));
      } catch (e) {
        setLspError(String(e));
      } finally {
        setBusyLsp(null);
      }
    },
    [applyLspSnapshot, lspDrafts, settingsRemoteProfileId],
  );

  const handleResetLsp = useCallback(
    async (languageId: string) => {
      setBusyLsp(languageId);
      setLspError(null);
      try {
        const nextSnapshot = await settingsResetLspServer(
          languageId,
          settingsRemoteProfileId,
        );
        applyLspSnapshot(nextSnapshot);
        setProbeMessages((messages) => ({
          ...messages,
          [languageId]: "已恢复默认",
        }));
      } catch (e) {
        setLspError(String(e));
      } finally {
        setBusyLsp(null);
      }
    },
    [applyLspSnapshot, settingsRemoteProfileId],
  );

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

  const updateRemoteDraft = useCallback(
    (patch: Partial<RemoteProfileDraft>) => {
      setRemoteDraft((draft) => (draft ? { ...draft, ...patch } : draft));
    },
    [],
  );

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
      setRemoteDraft((draft) => (draft?.id === profileId ? null : draft));
      setRemoteMessage("远程机器已删除");
    } catch (e) {
      setRemoteError(String(e));
    } finally {
      setBusyRemote(null);
    }
  }, []);

  const handleRestoreArchivedSession = useCallback(
    async (session: ArchivedSessionListItem) => {
      setBusyArchivedSession(session.id);
      setArchivedError(null);
      setArchivedMessage(null);
      try {
        await sessionUnarchive(session.id, session.workspace_root);
        setArchivedSessions((sessions) =>
          sessions.filter((item) => item.id !== session.id),
        );
        setArchivedMessage(`已恢复 ${session.title}`);
      } catch (e) {
        setArchivedError(String(e));
      } finally {
        setBusyArchivedSession(null);
      }
    },
    [],
  );

  const handleDeleteArchivedSession = useCallback(
    async (session: ArchivedSessionListItem) => {
      setBusyArchivedSession(session.id);
      setArchivedError(null);
      setArchivedMessage(null);
      try {
        await sessionDeleteArchived(session.id);
        setArchivedSessions((sessions) =>
          sessions.filter((item) => item.id !== session.id),
        );
        setArchivedMessage(`已删除 ${session.title}`);
      } catch (e) {
        setArchivedError(String(e));
      } finally {
        setBusyArchivedSession(null);
      }
    },
    [],
  );

  const handleDeleteAllArchivedSessions = useCallback(async () => {
    if (archivedSessions.length === 0) return;
    const accepted = await confirm("确定删除所有已归档对话？此操作不可撤销。");
    if (!accepted) return;
    setDeletingAllArchived(true);
    setArchivedError(null);
    setArchivedMessage(null);
    try {
      await sessionDeleteAllArchived();
      setArchivedSessions([]);
      setArchivedMessage("已删除所有已归档对话");
    } catch (e) {
      setArchivedError(String(e));
    } finally {
      setDeletingAllArchived(false);
    }
  }, [archivedSessions.length]);

  const handleValidateRemoteProfile = useCallback(
    async (profile: RemoteMachineProfile) => {
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
        const updated = nextSnapshot.profiles.find(
          (item) => item.id === profile.id,
        );
        setRemoteMessage(
          updated?.last_validation?.ok
            ? "远程机器验证通过"
            : "远程机器验证失败",
        );
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
    },
    [remoteValidationPasswords, remoteValidationPaths],
  );

  const renderAgentRuntime = (agentId: AgentSettingsTab) => {
    if (!snapshot) return null;
    const agent = snapshot.agents.find((item) => item.id === agentId);
    if (!agent) return null;
    return (
      <div className="settings-provider-detail settings-agent-runtime">
        <span
          className={`settings-row-badge ${agent.installed ? "is-installed" : "is-missing"}`}
        >
          {agent.installed ? "已安装" : "未安装"}
        </span>
        <div className="settings-row-actions">
          {agent.installed ? (
            <button
              type="button"
              className={`settings-btn ${agent.selected ? "is-selected" : ""}`}
              disabled={
                agent.selected ||
                busyAgent === agent.id ||
                !!snapshot.env_override
              }
              onClick={() => handleSelect(agent.id)}
            >
              {agent.selected
                ? "当前默认"
                : busyAgent === agent.id
                  ? "保存中..."
                  : "设为默认"}
            </button>
          ) : (
            <button
              type="button"
              className="settings-btn is-install"
              disabled={busyAgent === agent.id}
              onClick={() => handleInstall(agent.id)}
            >
              {busyAgent === agent.id
                ? "下载中..."
                : agent.id === "codex-acp"
                  ? "下载"
                  : "安装"}
            </button>
          )}
        </div>
      </div>
    );
  };

  const renderClaudeFastModelControl = () => {
    if (!snapshot) return null;
    const options = snapshot.claude.fast_model_options;
    const selected = snapshot.claude.fast_model ?? "";
    return (
      <label className="settings-field settings-provider-key-field">
        <span>快速模型</span>
        <select
          aria-label="claude_fast_model"
          className="settings-provider-native-select"
          value={selected}
          disabled={busyClaudeFastModel || options.length === 0}
          onChange={(event) =>
            handleSelectClaudeFastModel(event.currentTarget.value)
          }
        >
          <option value="">自动</option>
          {options.map((option) => (
            <option key={option.id} value={option.id}>
              {option.label} · {option.provider_label}
            </option>
          ))}
        </select>
        {codexAcpMessageTarget === "claude-fast" && codexAcpMessage && (
          <span className="settings-provider-config-message">
            {codexAcpMessage}
          </span>
        )}
      </label>
    );
  };

  const renderByokPool = () => {
    if (!snapshot) return null;

    const byokProfiles = selectableByokSourceProfiles(
      snapshot.codex_acp.profiles,
      byokProfileId,
    );
    const countedByokProfiles = countableByokSourceProfiles(
      snapshot.codex_acp.profiles,
    );
    const profile =
      byokProfiles.find((item) => item.id === byokProfileId) ?? byokProfiles[0];
    if (!profile) return null;
    const isCustomProfile = profile.custom;
    return (
      <section className="settings-provider-config settings-byok-config">
        <div className="settings-provider-config-head">
          <div>
            <span>BYOK 模型池</span>
            <p>保存自己的 API key。</p>
          </div>
          <span className="settings-provider-active">
            {countedByokProfiles.filter((item) => item.configured).length}/
            {countedByokProfiles.length} 已配置
          </span>
        </div>
        <div className="settings-field settings-provider-source-field">
          <span>模型来源</span>
          <div className="settings-provider-source-controls">
            <div
              ref={byokProviderMenuRef}
              className={`settings-provider-select ${byokProviderMenuOpen ? "is-open" : ""}`}
              onBlur={(event) => {
                const nextFocus = event.relatedTarget;
                if (
                  !(nextFocus instanceof Node) ||
                  !event.currentTarget.contains(nextFocus)
                ) {
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
                disabled={
                  busyCodexAcp || busyProviderModels || busyCustomProvider
                }
                onClick={() => setByokProviderMenuOpen((open) => !open)}
              >
                <span>
                  {profile.label}
                  {profile.configured ? " · 已配置" : " · 未配置"}
                </span>
                <span
                  className="settings-provider-select-chevron"
                  aria-hidden="true"
                >
                  v
                </span>
              </button>
              {byokProviderMenuOpen &&
                !(busyCodexAcp || busyProviderModels || busyCustomProvider) && (
                  <div
                    id="byok-provider-profile-listbox"
                    className="settings-provider-select-menu"
                    role="listbox"
                    aria-label="BYOK 模型来源"
                  >
                    {byokProfiles.map((item) => {
                      const selected = item.id === profile.id;
                      const optionLabel = `${item.label}${item.configured ? " · 已配置" : " · 未配置"}`;
                      const actionLabel = item.custom ? "移除" : "清除设置";
                      const actionDisabled = item.custom
                        ? busyCustomProvider || editingRemoteSettings
                        : busyProviderModels;
                      const showCustomEditAction = item.custom && item.configured;
                      return (
                        <div
                          key={item.id}
                          className={`settings-provider-select-option ${selected ? "is-selected" : ""}`}
                          role="option"
                          aria-selected={selected}
                          aria-label={optionLabel}
                        >
                          <button
                            type="button"
                            className="settings-provider-select-option-main"
                            onClick={() => handleSelectByokProfile(item.id)}
                          >
                            <span>{optionLabel}</span>
                          </button>
                          {showCustomEditAction && (
                            <button
                              type="button"
                              className="settings-provider-select-option-action"
                              disabled={actionDisabled}
                              aria-label={`编辑 ${item.label}`}
                              onClick={(event) => {
                                event.preventDefault();
                                event.stopPropagation();
                                handleOpenCustomProviderEdit(item);
                              }}
                            >
                              编辑
                            </button>
                          )}
                          {item.configured && (
                            <button
                              type="button"
                              className={`settings-provider-select-option-action ${item.custom ? "is-danger" : ""}`}
                              disabled={actionDisabled}
                              aria-label={`${actionLabel} ${item.label}`}
                              onClick={(event) => {
                                event.preventDefault();
                                event.stopPropagation();
                                if (item.custom) {
                                  void handleRemoveCustomProvider(item);
                                } else {
                                  void handleClearByokProviderConfiguration(
                                    item,
                                  );
                                }
                              }}
                            >
                              {actionLabel}
                            </button>
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}
            </div>
            <button
              type="button"
              className="settings-btn"
              disabled={busyCustomProvider || editingRemoteSettings}
              onClick={handleOpenCustomProviderEditor}
            >
              添加自定义
            </button>
          </div>
        </div>
        <div className="settings-provider-detail">
          <span
            className={`settings-row-badge ${profile.configured ? "is-installed" : "is-missing"}`}
          >
            {profile.configured ? "已配置" : "未配置"}
          </span>
          {renderModelChip(profile.models)}
          {profile.custom && profile.base_url && (
            <code>{profile.base_url}</code>
          )}
          {profile.custom && profile.protocol && (
            <span>{customProviderProtocolLabel(profile.protocol)}</span>
          )}
        </div>
        {codexAcpMessageTarget === "custom" &&
          codexAcpMessage &&
          !customProviderEditorOpen && (
            <span className="settings-provider-config-message">
              {codexAcpMessage}
            </span>
          )}
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
        <label className="settings-field settings-provider-model-url-field">
          <span>列表 URL</span>
          <div className="settings-provider-model-url-row">
            <input
              aria-label="byok_provider_model_list_url"
              type="url"
              value={modelListUrlDraft}
              disabled={busyProviderModels}
              placeholder="https://example.com/v1/models"
              onChange={(event) => {
                setModelListUrlDraft(event.currentTarget.value);
                setCodexAcpMessage(null);
                setCodexAcpMessageTarget("models");
              }}
            />
            <button
              type="button"
              className="settings-btn"
              disabled={busyProviderModels || !modelListUrlDraft.trim()}
              onClick={handleSyncProviderModelsFromUrl}
            >
              {busyProviderModels ? "同步中..." : "同步"}
            </button>
          </div>
        </label>
        <div className="settings-provider-config-actions">
          {codexAcpMessageTarget === "models" && codexAcpMessage && (
            <span className="settings-provider-config-message">
              {codexAcpMessage}
            </span>
          )}
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
        {!isCustomProfile && (
          <>
            <label className="settings-field settings-provider-key-field">
              <span>
                {profile.credential_label ?? `${profile.label} API key`}
              </span>
              <input
                aria-label="byok_api_key"
                type="password"
                autoComplete="off"
                placeholder={
                  profile.configured
                    ? `输入新的 ${profile.label} API key 以替换`
                    : `输入 ${profile.label} API key`
                }
                value={codexAcpApiKey}
                onChange={(event) =>
                  setCodexAcpApiKey(event.currentTarget.value)
                }
              />
            </label>
            <div className="settings-provider-config-actions">
              {codexAcpMessageTarget === "byok" && codexAcpMessage && (
                <span className="settings-provider-config-message">
                  {codexAcpMessage}
                </span>
              )}
              <button
                type="button"
                className="settings-btn"
                disabled={busyCodexAcp || !codexAcpApiKey.trim()}
                onClick={handleSaveByokProviderKey}
              >
                {busyCodexAcp ? "保存中..." : `保存 ${profile.label} key`}
              </button>
            </div>
          </>
        )}
      </section>
    );
  };

  const renderCustomProviderModal = () => {
    if (!customProviderEditorOpen) return null;
    const editingCustomProvider = customProviderEditorMode === "edit";
    const title = editingCustomProvider
      ? "编辑自定义 Provider"
      : "添加自定义 Provider";
    return createPortal(
      <div
        className="settings-custom-provider-modal-backdrop"
        role="presentation"
        onMouseDown={handleCloseCustomProviderEditor}
      >
        <section
          className="settings-custom-provider-modal"
          role="dialog"
          aria-modal="true"
          aria-labelledby="settings-custom-provider-title"
          onMouseDown={(event) => event.stopPropagation()}
        >
          <div className="settings-custom-provider-modal-head">
            <div>
              <h2
                id="settings-custom-provider-title"
                className="settings-custom-provider-modal-title"
              >
                {title}
              </h2>
              <p className="settings-custom-provider-modal-copy">
                {editingCustomProvider
                  ? "名称保存后保持不变，可更新 endpoint、协议和 API key。"
                  : "命名并保存后，才会出现在 BYOK 模型来源下拉框里。"}
              </p>
            </div>
            <button
              type="button"
              className="settings-custom-provider-modal-close"
              aria-label="关闭自定义 provider"
              disabled={busyCustomProvider}
              onClick={handleCloseCustomProviderEditor}
            >
              x
            </button>
          </div>
          <div className="settings-custom-provider-grid">
            <label className="settings-field">
              <span>名称</span>
              <input
                aria-label="custom_provider_label"
                value={customProviderLabel}
                disabled={busyCustomProvider || editingRemoteSettings || editingCustomProvider}
                placeholder="My Provider"
                autoFocus
                onChange={(event) => {
                  setCustomProviderLabel(event.currentTarget.value);
                  setCodexAcpMessage(null);
                  setCodexAcpMessageTarget("custom");
                }}
              />
            </label>
            <label className="settings-field">
              <span>Endpoint</span>
              <input
                aria-label="custom_provider_endpoint"
                type="url"
                value={customProviderEndpoint}
                disabled={busyCustomProvider || editingRemoteSettings}
                placeholder="https://api.example.com/v1/chat/completions"
                onChange={(event) => {
                  setCustomProviderEndpoint(event.currentTarget.value);
                  setCodexAcpMessage(null);
                  setCodexAcpMessageTarget("custom");
                }}
              />
            </label>
            <label className="settings-field">
              <span>协议</span>
              <select
                aria-label="custom_provider_protocol"
                className="settings-provider-native-select"
                value={customProviderProtocol}
                disabled={busyCustomProvider || editingRemoteSettings}
                onChange={(event) => {
                  setCustomProviderProtocol(
                    event.currentTarget.value as CustomProviderProtocol,
                  );
                  setCodexAcpMessage(null);
                  setCodexAcpMessageTarget("custom");
                }}
              >
                <option value="chat_completions">Chat Completions</option>
                <option value="responses">Responses</option>
                <option value="anthropic_messages">Anthropic Messages</option>
              </select>
            </label>

            <label className="settings-field">
              <span>API key</span>
              <input
                aria-label="custom_provider_api_key"
                type="password"
                autoComplete="off"
                value={customProviderApiKey}
                disabled={busyCustomProvider || editingRemoteSettings}
                placeholder={
                  editingCustomProvider
                    ? "输入新的 API key 以保存配置"
                    : "输入 API key"
                }
                onChange={(event) => {
                  setCustomProviderApiKey(event.currentTarget.value);
                  setCodexAcpMessage(null);
                  setCodexAcpMessageTarget("custom");
                }}
              />
            </label>
          </div>
          <div className="settings-provider-config-actions settings-custom-provider-modal-actions">
            {codexAcpMessageTarget === "custom" && codexAcpMessage && (
              <span className="settings-provider-config-message">
                {codexAcpMessage}
              </span>
            )}

            <button
              type="button"
              className="settings-btn"
              disabled={busyCustomProvider}
              onClick={handleCloseCustomProviderEditor}
            >
              取消
            </button>
            <button
              type="button"
              className="settings-btn is-install"
              disabled={
                busyCustomProvider ||
                editingRemoteSettings ||
                !customProviderLabel.trim() ||
                !customProviderEndpoint.trim() ||
                (customProviderEditorMode === "add" && !customProviderApiKey.trim())
              }
              onClick={handleSaveCustomProvider}
            >
              {busyCustomProvider
                ? "保存中..."
                : editingCustomProvider
                  ? "保存修改"
                  : "保存自定义 provider"}
            </button>
          </div>
        </section>
      </div>,
      document.body,
    );
  };
  const renderWebToolsSection = () => {
    if (!snapshot) return null;
    if (editingRemoteSettings) {
      return (
        <section className="settings-section">
          <h2 className="settings-section-title">Web 工具</h2>
          <p className="settings-section-desc">
            给 Codex 和 Claude 本机会话提供搜索与网页抓取能力。
          </p>
          <div className="settings-warning">
            远程会话暂不支持注入本地 Web 工具。
          </div>
        </section>
      );
    }
    const webTools = snapshot.web_tools;
    const providerMeta = webToolProviderMeta(webTools.provider);
    return (
      <section className="settings-section">
        <h2 className="settings-section-title">Web 工具</h2>
        <p className="settings-section-desc">
          给 Codex 和 Claude 本机会话提供搜索与网页抓取能力。
        </p>
        <div className="settings-provider-config">
          <div className="settings-provider-config-head">
            <div>
              <span>{providerMeta.label}</span>
              <p>通过本地 Kodex MCP server 注入 web_search 和 web_fetch。</p>
            </div>
            <label className="settings-switch">
              <input
                type="checkbox"
                checked={webTools.enabled}
                disabled={busyWebTools}
                onChange={(event) =>
                  handleToggleWebTools(event.currentTarget.checked)
                }
              />
              <span>{webTools.enabled ? "已启用" : "已关闭"}</span>
            </label>
          </div>
          <div className="settings-provider-detail">
            <span
              className={`settings-row-badge ${webTools.configured ? "is-installed" : "is-missing"}`}
            >
              {webTools.configured ? "已配置" : "未配置"}
            </span>
            <span className="settings-provider-config-message">
              Provider：{providerMeta.label}
            </span>
          </div>
          <label className="settings-field settings-provider-source-field">
            <span>搜索来源</span>
            <select
              aria-label="web_tools_provider"
              className="settings-provider-native-select"
              value={webTools.provider}
              disabled={busyWebTools}
              onChange={(event) =>
                handleSelectWebToolsProvider(event.currentTarget.value)
              }
            >
              {WEB_TOOL_PROVIDER_OPTIONS.map((option) => (
                <option key={option.id} value={option.id}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
          {webTools.enabled && !webTools.configured && (
            <div className="settings-warning">
              需要保存 {providerMeta.apiKeyLabel} 后，新会话才会实际获得 Web
              工具。
            </div>
          )}
          <label className="settings-field settings-provider-key-field">
            <span>API key</span>
            <input
              aria-label="web_tools_api_key"
              type="password"
              autoComplete="off"
              placeholder={
                webTools.configured
                  ? `输入新的 ${providerMeta.apiKeyLabel} 以替换`
                  : `输入 ${providerMeta.apiKeyLabel}`
              }
              value={webToolsApiKey}
              onChange={(event) => setWebToolsApiKey(event.currentTarget.value)}
            />
          </label>
          <div className="settings-provider-config-actions">
            {webToolsMessage && (
              <span className="settings-provider-config-message">
                {webToolsMessage}
              </span>
            )}
            <button
              type="button"
              className="settings-btn"
              disabled={busyWebTools || !webToolsApiKey.trim()}
              onClick={handleSaveWebToolsKey}
            >
              {busyWebTools ? "保存中..." : "保存 key"}
            </button>
          </div>
        </div>
      </section>
    );
  };

  const renderImageSection = () => {
    if (!snapshot) return null;
    if (editingRemoteSettings) {
      return (
        <section className="settings-section">
          <h2 className="settings-section-title">图像能力</h2>
          <p className="settings-section-desc">
            识图、生图、改图的降级 MCP 工具。
          </p>
          <div className="settings-warning">
            远程会话暂不支持注入本地图像 MCP 工具。
          </div>
        </section>
      );
    }
    const image = snapshot.image;
    // Image view reuses an existing BYOK provider's key, so only offer
    // providers that actually have a resolved key. Unconfigured providers are
    // hidden to avoid letting the user pick one that can never authenticate.
    const byokProfiles = selectableByokSourceProfiles(
      snapshot.codex_acp.profiles,
      byokProfileId,
    ).filter((profile) => profile.configured);
    return (
      <section className="settings-section">
        <h2 className="settings-section-title">图像能力</h2>
        <p className="settings-section-desc">
          当底层模型缺少识图 / 生图 / 改图能力时，自动注入本地
          kodex-image MCP 工具降级补齐。
        </p>
        <div className="settings-provider-config">
          <div className="settings-provider-config-head">
            <div>
              <span>识图（view_image）</span>
              <p>
                复用对话模型 catalog 里的多模态模型；text-only 主模型收到图片时，转由该模型理解并返回文字描述。
              </p>
            </div>
            <label className="settings-switch">
              <input
                type="checkbox"
                checked={image?.enabled ?? false}
                disabled={busyImage}
                onChange={(event) =>
                  handleToggleImageEnabled(event.currentTarget.checked)
                }
              />
              <span>{image?.enabled ? "已启用" : "已关闭"}</span>
            </label>
          </div>
          <div className="settings-provider-detail">
            <span
              className={`settings-row-badge ${image?.view_configured ? "is-installed" : "is-missing"}`}
            >
              {image?.view_configured ? "已配置" : "未配置"}
            </span>
          </div>
          <label className="settings-field settings-provider-source-field">
            <span>识图模型来源（BYOK provider）</span>
            <select
              aria-label="image_view_provider"
              className="settings-provider-native-select"
              value={imageViewDraftProvider}
              disabled={busyImage}
              onChange={(event) => {
                setImageViewDraftProvider(event.currentTarget.value);
                setImageViewDraftModel("");
              }}
            >
              <option value="">— 选择 provider —</option>
              {byokProfiles.map((profile) => (
                <option key={profile.id} value={profile.id}>
                  {profile.label}
                </option>
              ))}
            </select>
          </label>
          <label className="settings-field settings-provider-source-field">
            <span>识图模型</span>
            <select
              aria-label="image_view_model"
              className="settings-provider-native-select"
              value={imageViewDraftModel}
              disabled={busyImage || !imageViewDraftProvider}
              onChange={(event) =>
                setImageViewDraftModel(event.currentTarget.value)
              }
            >
              <option value="">— 选择模型 —</option>
              {imageViewModelOptions.map((model: string) => (
                <option key={model} value={model}>
                  {model}
                </option>
              ))}
            </select>
          </label>
          <div className="settings-provider-config-actions">
            {imageMessage && (
              <span className="settings-provider-config-message">
                {imageMessage}
              </span>
            )}
            <button
              type="button"
              className="settings-btn"
            disabled={
              busyImage ||
              !imageViewDraftProvider ||
              !imageViewDraftModel ||
              !imageViewDirty
            }
              onClick={handleSaveImageView}
            >
              {busyImage ? "保存中..." : "保存识图配置"}
            </button>
          </div>
        </div>

        <div className="settings-provider-config" style={{ marginTop: 16 }}>
          <div className="settings-provider-config-head">
            <div>
              <span>生图 / 改图（generate_image / edit_image）</span>
              <p>
                独立配置的生图模型，generate 与 edit 共用。可指定协议：OpenAI
                images 接口、chat/completions 或 Gemini。
              </p>
            </div>
          </div>
          <div className="settings-provider-detail">
            <span
              className={`settings-row-badge ${image?.generate_configured ? "is-installed" : "is-missing"}`}
            >
              {image?.generate_configured ? "已配置" : "未配置"}
            </span>
          </div>
          <label className="settings-field settings-provider-source-field">
            <span>协议</span>
            <select
              aria-label="image_generate_protocol"
              className="settings-provider-native-select"
              value={imageGenDraftProtocol}
              disabled={busyImage}
              onChange={(event) =>
                setImageGenDraftProtocol(
                  event.currentTarget.value as ImageGenerateProtocol,
                )
              }
            >
              <option value="openai_images">
                OpenAI images/generations + images/edits
              </option>
              <option value="chat_completions">
                OpenAI chat/completions（内联图片输出）
              </option>
              <option value="gemini">Google Gemini generateContent</option>
            </select>
          </label>
          <label className="settings-field settings-provider-source-field">
            <span>Base URL</span>
            <input
              aria-label="image_generate_base_url"
              type="text"
              autoComplete="off"
              placeholder={
                imageGenDraftProtocol === "gemini"
                  ? "https://generativelanguage.googleapis.com/v1beta"
                  : "https://api.example.com/v1"
              }
              value={imageGenDraftBaseUrl}
              disabled={busyImage}
              onChange={(event) =>
                setImageGenDraftBaseUrl(event.currentTarget.value)
              }
            />
          </label>
          <label className="settings-field settings-provider-source-field">
            <span>模型</span>
            <input
              aria-label="image_generate_model"
              type="text"
              autoComplete="off"
              placeholder="nana-banana / gemini-2.5-flash-image / ..."
              value={imageGenDraftModel}
              disabled={busyImage}
              onChange={(event) =>
                setImageGenDraftModel(event.currentTarget.value)
              }
            />
          </label>
          <label className="settings-field settings-provider-source-field">
            <span>默认尺寸</span>
            <input
              aria-label="image_generate_size"
              type="text"
              autoComplete="off"
              placeholder="1024x1024"
              value={imageGenDraftSize}
              disabled={busyImage}
              onChange={(event) =>
                setImageGenDraftSize(event.currentTarget.value)
              }
            />
          </label>
          <label className="settings-field settings-provider-source-field">
            <span>API Key 环境变量名（可选，留空则用下方 key）</span>
            <input
              aria-label="image_generate_api_key_env"
              type="text"
              autoComplete="off"
              placeholder="IMAGE_GEN_API_KEY"
              value={imageGenDraftApiKeyEnv}
              disabled={busyImage}
              onChange={(event) =>
                setImageGenDraftApiKeyEnv(event.currentTarget.value)
              }
            />
          </label>
          <div className="settings-provider-config-actions">
            <button
              type="button"
              className="settings-btn"
            disabled={
              busyImage ||
              !imageGenDraftBaseUrl ||
              !imageGenDraftModel ||
              !imageGenerateDirty
            }
              onClick={handleSaveImageGenerate}
            >
              {busyImage ? "保存中..." : "保存生/改图配置"}
            </button>
          </div>
          <label className="settings-field settings-provider-key-field">
            <span>API key</span>
            <input
              aria-label="image_generate_api_key"
              type="password"
              autoComplete="off"
              placeholder={
                image?.generate_configured
                  ? "输入新的 key 以替换"
                  : "输入生/改图模型 API key"
              }
              value={imageGenApiKey}
              disabled={busyImage}
              onChange={(event) =>
                setImageGenApiKey(event.currentTarget.value)
              }
            />
          </label>
          <div className="settings-provider-config-actions">
            <button
              type="button"
              className="settings-btn"
              disabled={busyImage || !imageGenApiKey.trim()}
              onClick={handleSaveImageGenerateKey}
            >
              {busyImage ? "保存中..." : "保存 key"}
            </button>
          </div>
        </div>
      </section>
    );
  };

  const renderArchivePane = () => {
    const workspaceOptions = archivedWorkspaceOptions(archivedSessions);
    const normalizedSearch = archivedSearch.trim().toLowerCase();
    const visibleSessions = archivedSessions.filter((session) => {
      const workspaceName = workspaceNameFromRoot(session.workspace_root);
      const matchesWorkspace =
        archivedWorkspaceFilter === "all" ||
        session.workspace_root === archivedWorkspaceFilter;
      const matchesChatFilter =
        archivedChatFilter === "all" || session.message_count > 0;
      const matchesSearch =
        !normalizedSearch ||
        session.title.toLowerCase().includes(normalizedSearch) ||
        workspaceName.toLowerCase().includes(normalizedSearch) ||
        session.workspace_root.toLowerCase().includes(normalizedSearch);
      return matchesWorkspace && matchesChatFilter && matchesSearch;
    });
    const groups = groupArchivedSessionsByWorkspace(visibleSessions);

    return (
      <section className="settings-section settings-archive-section">
        <div className="settings-section-head settings-archive-head">
          <div>
            <h2 className="settings-section-title">已归档对话</h2>
            <p className="settings-section-desc">
              恢复或永久删除已从项目列表中移除的会话。
            </p>
          </div>
          <button
            type="button"
            className="settings-btn settings-danger-btn"
            disabled={deletingAllArchived || archivedSessions.length === 0}
            onClick={handleDeleteAllArchivedSessions}
          >
            <Trash2 aria-hidden="true" size={15} />
            {deletingAllArchived ? "删除中..." : "全部删除"}
          </button>
        </div>

        <div className="settings-archive-panel">
          <div className="settings-archive-toolbar">
            <label className="settings-archive-search">
              <Search aria-hidden="true" size={16} />
              <input
                aria-label="搜索已归档聊天"
                value={archivedSearch}
                placeholder="搜索已归档聊天"
                onChange={(event) =>
                  setArchivedSearch(event.currentTarget.value)
                }
              />
              {archivedSearch && (
                <button
                  type="button"
                  aria-label="清空归档搜索"
                  onClick={() => setArchivedSearch("")}
                >
                  <X aria-hidden="true" size={14} />
                </button>
              )}
            </label>
            <label className="settings-archive-select">
              <ListFilter aria-hidden="true" size={16} />
              <select
                aria-label="归档聊天范围"
                value={archivedChatFilter}
                onChange={(event) =>
                  setArchivedChatFilter(
                    event.currentTarget.value as "all" | "with_messages",
                  )
                }
              >
                <option value="all">全部聊天</option>
                <option value="with_messages">有消息</option>
              </select>
              <ChevronDown aria-hidden="true" size={15} />
            </label>
            <label className="settings-archive-select">
              <Folder aria-hidden="true" size={16} />
              <select
                aria-label="归档项目筛选"
                value={archivedWorkspaceFilter}
                onChange={(event) =>
                  setArchivedWorkspaceFilter(event.currentTarget.value)
                }
              >
                <option value="all">All projects</option>
                {workspaceOptions.map((workspace) => (
                  <option key={workspace.root} value={workspace.root}>
                    {workspace.name}
                  </option>
                ))}
              </select>
              <ChevronDown aria-hidden="true" size={15} />
            </label>
          </div>

          {archivedError && (
            <div className="settings-error settings-archive-status">
              <span>{archivedError}</span>
              <button
                type="button"
                className="settings-link-btn"
                onClick={loadArchivedSessions}
              >
                重试
              </button>
            </div>
          )}
          {archivedMessage && (
            <div className="settings-success settings-archive-status">
              {archivedMessage}
            </div>
          )}
          {archivedLoading && (
            <div className="settings-status settings-archive-status">
              正在载入已归档对话...
            </div>
          )}

          {!archivedLoading && archivedSessions.length === 0 && (
            <div className="settings-empty-panel settings-archive-empty">
              <div className="settings-row-title">还没有已归档对话</div>
              <p>归档会话后，它们会保留在这里，可以恢复到项目列表。</p>
            </div>
          )}

          {!archivedLoading &&
            archivedSessions.length > 0 &&
            groups.length === 0 && (
              <div className="settings-empty-panel settings-archive-empty">
                <div className="settings-row-title">没有匹配的归档对话</div>
                <p>换一个搜索词或项目筛选试试。</p>
              </div>
            )}

          {groups.length > 0 && (
            <div className="settings-archive-list">
              {groups.map((group) => (
                <section key={group.root} className="settings-archive-group">
                  <div className="settings-archive-group-head">
                    <span className="settings-archive-group-title">
                      <Folder aria-hidden="true" size={16} />
                      {group.name}
                    </span>
                    <span>{group.sessions.length} 个聊天</span>
                  </div>
                  <div className="settings-archive-rows">
                    {group.sessions.map((session) => {
                      const busy = busyArchivedSession === session.id;
                      return (
                        <article
                          key={session.id}
                          className="settings-archive-row"
                        >
                          <div className="settings-archive-row-copy">
                            <div className="settings-archive-row-title">
                              {session.title}
                            </div>
                            <div className="settings-archive-row-meta">
                              {formatArchiveDate(session.archived_at)}
                              {session.message_count > 0
                                ? ` · ${session.message_count} 条消息`
                                : ""}
                            </div>
                          </div>
                          <div className="settings-archive-row-actions">
                            <button
                              type="button"
                              className="settings-icon-btn"
                              disabled={busy}
                              title="恢复对话"
                              aria-label={`恢复对话 ${session.title}`}
                              onClick={() =>
                                handleRestoreArchivedSession(session)
                              }
                            >
                              <ArchiveRestore aria-hidden="true" size={16} />
                            </button>
                            <button
                              type="button"
                              className="settings-icon-btn is-danger"
                              disabled={busy}
                              title="删除对话"
                              aria-label={`删除对话 ${session.title}`}
                              onClick={() =>
                                handleDeleteArchivedSession(session)
                              }
                            >
                              <Trash2 aria-hidden="true" size={16} />
                            </button>
                          </div>
                        </article>
                      );
                    })}
                  </div>
                </section>
              ))}
            </div>
          )}
        </div>
      </section>
    );
  };

  const renderUsagePane = () => {
    const totalTokens = usageRows.reduce(
      (sum, row) => sum + usageTokenTotal(row.tokens),
      0,
    );
    const totalEvents = usageRows.reduce(
      (sum, row) => sum + row.event_count,
      0,
    );
    const totalSessions = usageRows.reduce(
      (sum, row) => sum + row.session_count,
      0,
    );

    return (
      <section className="settings-section settings-usage-section">
        <div className="settings-section-head">
          <div>
            <h2 className="settings-section-title">用量</h2>
            <p className="settings-section-desc">
              查看当前工作区记录到的上下文和 token 用量，不包含计价。
            </p>
          </div>
          <button
            type="button"
            className="settings-btn"
            disabled={usageLoading}
            onClick={loadUsageSummary}
          >
            {usageLoading ? "刷新中..." : "刷新"}
          </button>
        </div>

        <div className="settings-usage-summary-grid" aria-label="用量汇总">
          <div className="settings-usage-summary-card">
            <span>总 tokens</span>
            <strong>{formatUsageTokens(totalTokens)}</strong>
          </div>
          <div className="settings-usage-summary-card">
            <span>事件</span>
            <strong>{totalEvents.toLocaleString("en-US")}</strong>
          </div>
          <div className="settings-usage-summary-card">
            <span>会话计数</span>
            <strong>{totalSessions.toLocaleString("en-US")}</strong>
          </div>
        </div>

        <div className="settings-usage-filter-stack">
          <div
            className="settings-usage-toolbar"
            role="radiogroup"
            aria-label="用量分组"
          >
            {(
              [
                "model",
                "agent",
                "workspace",
                "session",
              ] as UsageSummaryGroupBy[]
            ).map((group) => (
              <button
                key={group}
                type="button"
                className={`settings-btn ${usageGroupBy === group ? "is-selected" : ""}`}
                aria-pressed={usageGroupBy === group}
                onClick={() => setUsageGroupBy(group)}
              >
                {usageGroupLabel(group)}
              </button>
            ))}
          </div>

          <div
            className="settings-usage-toolbar"
            role="radiogroup"
            aria-label="用量时间范围"
          >
            {(["today", "7d", "30d", "all"] as UsageDateRange[]).map(
              (range) => (
                <button
                  key={range}
                  type="button"
                  className={`settings-btn ${usageDateRange === range ? "is-selected" : ""}`}
                  aria-pressed={usageDateRange === range}
                  onClick={() => setUsageDateRange(range)}
                >
                  {usageDateRangeLabel(range)}
                </button>
              ),
            )}
          </div>

          <div
            className="settings-usage-toolbar"
            role="group"
            aria-label="用量范围"
          >
            {(["current", "all"] as UsageWorkspaceScope[]).map((scope) => (
              <button
                key={scope}
                type="button"
                className={`settings-btn ${usageWorkspaceScope === scope ? "is-selected" : ""}`}
                aria-pressed={usageWorkspaceScope === scope}
                onClick={() => setUsageWorkspaceScope(scope)}
              >
                {usageWorkspaceScopeLabel(scope)}
              </button>
            ))}
            <label className="settings-usage-checkbox">
              <input
                type="checkbox"
                checked={usageIncludeArchived}
                onChange={(event) =>
                  setUsageIncludeArchived(event.currentTarget.checked)
                }
              />
              包含已归档
            </label>
          </div>
        </div>

        {usageError && <div className="settings-error">{usageError}</div>}
        {usageLoading && <div className="settings-status">正在加载用量...</div>}
        {!usageLoading && usageRows.length === 0 && (
          <div className="settings-empty-panel">暂无用量记录。</div>
        )}
        {!usageLoading && usageRows.length > 0 && (
          <div className="settings-usage-list">
            {usageRows.map((row) => {
              const breakdown = usageBreakdownParts(row.tokens);
              return (
                <article
                  key={`${usageGroupBy}:${row.label}:${row.model ?? ""}:${row.agent_cli ?? ""}:${row.workspace_root ?? ""}:${row.session_id ?? ""}`}
                  className="settings-usage-row"
                >
                  <div className="settings-usage-row-main">
                    <div className="settings-usage-row-title">{row.label}</div>
                    <div className="settings-usage-row-meta">
                      {usageRowMeta(row)}
                    </div>
                    {breakdown.length > 0 && (
                      <div
                        className="settings-usage-breakdown"
                        aria-label="token 分项"
                      >
                        {breakdown.map((item) => (
                          <span key={item.label}>
                            {item.label} {formatUsageTokens(item.value)}
                          </span>
                        ))}
                      </div>
                    )}
                  </div>
                  <div className="settings-usage-row-stats">
                    <strong>
                      {formatUsageTokens(usageTokenTotal(row.tokens))}
                    </strong>
                    {row.context_peak_tokens != null &&
                      row.context_peak_tokens > 0 && (
                        <span>
                          峰值 {formatUsageTokens(row.context_peak_tokens)}
                        </span>
                      )}
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </section>
    );
  };

  const renderRemotePane = () => {
    const duplicateTarget = remoteDraft
      ? remoteSnapshot.profiles.find(
          (profile) =>
            profile.id !== remoteDraft.id &&
            normalizeRemoteTarget(profile.ssh_target, profile.ssh_port) ===
              normalizeRemoteTarget(
                remoteDraft.ssh_target,
                parseRemotePort(remoteDraft.ssh_port),
              ),
        )
      : null;
    return (
      <section className="settings-section">
        <div className="settings-section-head">
          <div>
            <h2 className="settings-section-title">远程机器</h2>
            <p className="settings-section-desc">
              保存常用 Linux 开发机，默认验证用户目录；从 Workbench
              连接机器后再打开项目。
            </p>
          </div>
          <button
            type="button"
            className="settings-btn is-install"
            onClick={startNewRemoteProfile}
          >
            添加远程机器
          </button>
        </div>
        {remoteContext && (
          <div className="settings-remote-context-card">
            <div className="settings-provider-config-head">
              <div>
                <span>当前远程上下文</span>
                <p>
                  {remoteContext.workspaceName} · {remoteContext.sshTarget}
                  {remoteContext.sshPort ? `:${remoteContext.sshPort}` : ""}
                </p>
              </div>
              <code>{remoteContext.remotePath}</code>
            </div>
            <div className="settings-remote-context-grid">
              <div>
                <span className="settings-row-meta">当前运行通道</span>
                <strong>{remoteContext.agentLabel ?? "未识别"}</strong>
              </div>
              <div className="settings-row-actions">
                <button
                  type="button"
                  className="settings-btn"
                  disabled={!canUseRemoteSettings}
                  onClick={() => openRemoteAgentSettings("claude-agent-acp")}
                >
                  编辑远程 Claude
                </button>
                <button
                  type="button"
                  className="settings-btn"
                  disabled={!canUseRemoteSettings}
                  onClick={() => openRemoteAgentSettings("codex-acp")}
                >
                  编辑远程 Codex
                </button>
                <button
                  type="button"
                  className="settings-btn"
                  disabled={!canUseRemoteSettings}
                  onClick={() => openRemoteAgentSettings("codebuddy")}
                >
                  编辑远程 CodeBuddy
                </button>
              </div>
            </div>
          </div>
        )}
        {remoteError && (
          <div className="settings-error">
            <span>{remoteError}</span>
            <button type="button" className="settings-link-btn" onClick={load}>
              重试
            </button>
          </div>
        )}
        {remoteMessage && (
          <div className="settings-success">{remoteMessage}</div>
        )}
        {remoteDraft && (
          <div className="settings-remote-editor">
            <div className="settings-provider-config-head">
              <div>
                <span>{remoteDraft.id ? "编辑远程机器" : "添加远程机器"}</span>
                <p>
                  这里只保存机器名称、SSH
                  目标和端口；密码只在验证或连接机器时临时输入。
                </p>
              </div>
              <button
                type="button"
                className="settings-btn"
                onClick={() => setRemoteDraft(null)}
              >
                取消
              </button>
            </div>
            <label className="settings-field">
              <span>名称</span>
              <input
                aria-label="remote_profile_name"
                value={remoteDraft.display_name}
                onChange={(event) =>
                  updateRemoteDraft({ display_name: event.currentTarget.value })
                }
                placeholder="开发机"
              />
            </label>
            <label className="settings-field">
              <span>SSH 目标</span>
              <input
                aria-label="remote_profile_ssh_target"
                value={remoteDraft.ssh_target}
                onChange={(event) =>
                  updateRemoteDraft({ ssh_target: event.currentTarget.value })
                }
                placeholder="root@devbox 或 SSH config alias"
              />
            </label>
            <label className="settings-field">
              <span>端口</span>
              <input
                aria-label="remote_profile_ssh_port"
                inputMode="numeric"
                value={remoteDraft.ssh_port}
                onChange={(event) =>
                  updateRemoteDraft({
                    ssh_port: event.currentTarget.value.replace(/[^\d]/g, ""),
                  })
                }
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
                disabled={
                  busyRemote === "save" ||
                  !remoteDraft.display_name.trim() ||
                  !remoteDraft.ssh_target.trim()
                }
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
            <p>
              添加一台 Linux 开发机后，可以验证 SSH 和默认用户目录，再从
              Workbench 打开远程目录。
            </p>
            <button
              type="button"
              className="settings-btn is-install"
              onClick={startNewRemoteProfile}
            >
              添加远程机器
            </button>
          </div>
        ) : (
          <div className="settings-remote-list">
            {remoteSnapshot.profiles.map((profile) => (
              <article key={profile.id} className="settings-remote-card">
                <div className="settings-lsp-head">
                  <div>
                    <div className="settings-row-title">
                      {profile.display_name}
                    </div>
                    <div className="settings-row-meta">
                      <code>{formatRemoteTarget(profile)}</code>
                      <span
                        className={`settings-row-badge ${profile.last_validation?.ok ? "is-installed" : "is-missing"}`}
                      >
                        {profile.last_validation
                          ? profile.last_validation.ok
                            ? "已验证"
                            : "验证失败"
                          : "未验证"}
                      </span>
                    </div>
                  </div>
                  <div className="settings-row-actions">
                    <button
                      type="button"
                      className="settings-btn"
                      onClick={() => editRemoteProfile(profile)}
                    >
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
                <p className="settings-remote-note">
                  验证目录为空时检查远程用户目录；不填密码时使用 SSH
                  key、ssh-agent 或 SSH config。
                </p>
                {profile.last_validation && (
                  <div className="settings-remote-phases">
                    {profile.last_validation.phases.map((phase) => (
                      <span
                        key={phase.phase}
                        className={`settings-remote-phase is-${phase.status}`}
                        title={phase.message ?? undefined}
                      >
                        {remotePhaseLabel(phase.phase)} ·{" "}
                        {remotePhaseStatusLabel(phase.status)}
                      </span>
                    ))}
                    <span className="settings-row-meta">
                      {formatValidationTime(
                        profile.last_validation.checked_at_ms,
                      )}
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
            className={`settings-nav-item ${activePane === "web" ? "is-active" : ""}`}
            onClick={() => setActivePane("web")}
          >
           Web 工具
          </button>
          <button
            type="button"
            className={`settings-nav-item ${activePane === "image" ? "is-active" : ""}`}
            onClick={() => setActivePane("image")}
          >
            图像能力
          </button>
          <button
            type="button"
            className={`settings-nav-item ${activePane === "archive" ? "is-active" : ""}`}
            onClick={() => setActivePane("archive")}
          >
            已归档
          </button>
          <button
            type="button"
            className={`settings-nav-item ${activePane === "usage" ? "is-active" : ""}`}
            onClick={() => setActivePane("usage")}
          >
            用量
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
                      <span
                        className="settings-theme-swatches"
                        aria-hidden="true"
                      >
                        {theme.swatches.map((color) => (
                          <span key={color} style={{ background: color }} />
                        ))}
                      </span>
                      <span className="settings-theme-copy">
                        <span className="settings-theme-title">
                          {theme.label}
                        </span>
                        <span className="settings-theme-desc">
                          {selected ? "当前主题" : theme.description}
                        </span>
                      </span>
                      {busyTheme === theme.id && (
                        <span className="settings-theme-saving">保存中...</span>
                      )}
                    </button>
                  );
                })}
              </div>
            </section>

            <section className="settings-section">
              <h2 className="settings-section-title">应用更新</h2>
              <p className="settings-section-desc">
                检查 GitHub Release 上的 Kodex 桌面更新。
              </p>
              <div className="settings-update-panel">
                <div className="settings-update-copy">
                  <div className="settings-row-title">
                    Kodex{appVersion ? ` ${appVersion}` : ""}
                  </div>
                  <div className="settings-row-meta">
                    {updateInfo
                      ? `可更新到 ${updateInfo.version}`
                      : "通过 Tauri updater 校验签名后安装"}
                  </div>
                </div>
                <div className="settings-row-actions">
                  <button
                    type="button"
                    className="settings-btn"
                    disabled={
                      updateStatus === "checking" ||
                      updateStatus === "installing"
                    }
                    onClick={handleCheckForUpdate}
                  >
                    {updateStatus === "checking" ? "检查中..." : "检查更新"}
                  </button>
                  {updateStatus === "available" && (
                    <button
                      type="button"
                      className="settings-btn is-install"
                      onClick={handleInstallUpdate}
                    >
                      安装并重启
                    </button>
                  )}
                </div>
              </div>
              {updateMessage && (
                <div
                  className={
                    updateStatus === "error"
                      ? "settings-error"
                      : updateStatus === "available"
                        ? "settings-warning"
                        : "settings-status"
                  }
                >
                  {updateMessage}
                </div>
              )}
              {updateInfo?.body && updateStatus === "available" && (
                <div className="settings-update-notes">{updateInfo.body}</div>
              )}
              {updateStatus === "installing" &&
                updateProgress?.contentLength && (
                  <progress
                    className="settings-update-progress"
                    max={updateProgress.contentLength}
                    value={Math.min(
                      updateProgress.downloadedBytes,
                      updateProgress.contentLength,
                    )}
                    aria-label="更新下载进度"
                  />
                )}
            </section>

            <section className="settings-section">
              <h2 className="settings-section-title">智能体</h2>
              <p className="settings-section-desc">
                {editingRemoteSettings && remoteContext
                  ? `正在编辑 ${remoteContext.workspaceName} 的远程运行时设置。`
                  : "选择本机默认智能体和可用模型来源。"}
              </p>

              {loading && <div className="settings-status">加载中...</div>}
              {editingRemoteSettings && remoteContext && (
                <div className="settings-warning">
                  <span>
                    远程设置会连接 {remoteContext.sshTarget}
                    {remoteContext.sshPort ? `:${remoteContext.sshPort}` : ""}。
                  </span>
                  <button
                    type="button"
                    className="settings-link-btn"
                    onClick={returnToLocalSettings}
                  >
                    回到本机设置
                  </button>
                </div>
              )}
              {error && (
                <div className="settings-error">
                  <span>{error}</span>
                  <button
                    type="button"
                    className="settings-link-btn"
                    onClick={load}
                  >
                    重试
                  </button>
                </div>
              )}
              {snapshot?.env_override && (
                <div className="settings-warning">
                  <code>ACP_AGENT_COMMAND</code> 已设置，将覆盖此选择：
                  <code>{snapshot.env_override}</code>
                </div>
              )}
              {installResult && (
                <div
                  className={
                    installResult.success
                      ? "settings-success"
                      : "settings-error"
                  }
                >
                  <span>{installResult.message}</span>
                  {installResult.manual_instruction && (
                    <div>
                      <code>{installResult.manual_instruction}</code>
                    </div>
                  )}
                </div>
              )}

              {snapshot && (
                <div className="settings-agent-settings">
                  <div
                    className="settings-agent-tabs"
                    role="tablist"
                    aria-label="Agent settings"
                  >
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
                    {activeAgentTab === "codebuddy" &&
                      (() => {
                        return (
                          <div className="settings-provider-config">
                            <div className="settings-provider-config-head">
                              <div>
                                <span>CodeBuddy</span>
                              </div>
                              <span className="settings-provider-active">
                                {snapshot.settings.selected_agent ===
                                "codebuddy"
                                  ? "当前默认"
                                  : "可选"}
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
                              当前：
                              {snapshot.codex_acp.selected_profile_id ===
                              "default"
                                ? "默认"
                                : "BYOK"}
                            </span>
                          </div>
                          {renderAgentRuntime("codex-acp")}
                          <div
                            className="settings-provider-options"
                            role="radiogroup"
                            aria-label="Codex channel"
                          >
                            {(["default", "byok"] as const).map((channel) => {
                              const selected =
                                channel === "default"
                                  ? snapshot.codex_acp.selected_profile_id ===
                                    "default"
                                  : snapshot.codex_acp.selected_profile_id !==
                                    "default";
                              return (
                                <button
                                  key={channel}
                                  type="button"
                                  className={`settings-provider-option ${selected ? "is-selected" : ""}`}
                                  onClick={() =>
                                    handleSelectCodexChannel(channel)
                                  }
                                  disabled={busyCodexAcp}
                                  aria-pressed={selected}
                                >
                                  <span className="settings-provider-option-main">
                                    <span>
                                      {channel === "default" ? "默认" : "BYOK"}
                                    </span>
                                    <span>
                                      {channel === "default"
                                        ? "本机默认配置"
                                        : "自带 API key"}
                                    </span>
                                  </span>
                                  <span
                                    className={`settings-row-badge ${selected ? "is-installed" : "is-missing"}`}
                                  >
                                    {selected ? "当前" : "可选"}
                                  </span>
                                </button>
                              );
                            })}
                          </div>
                          {codexAcpMessageTarget === "channel" &&
                            codexAcpMessage && (
                              <div className="settings-provider-config-message">
                                {codexAcpMessage}
                              </div>
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
                          {renderClaudeFastModelControl()}
                        </div>
                        {renderByokPool()}
                      </>
                    )}
                  </div>
                </div>
              )}

              <div className="settings-detect-row">
                <button
                  type="button"
                  className="settings-link-btn"
                  onClick={handleDetect}
                  disabled={loading}
                >
                  重新检测已安装的 CLI
                </button>
              </div>
            </section>
          </>
        )}

        {activePane === "web" && renderWebToolsSection()}

        {activePane === "image" && renderImageSection()}

        {activePane === "remote" && renderRemotePane()}

        {activePane === "archive" && renderArchivePane()}

        {activePane === "usage" && renderUsagePane()}

        {activePane === "lsp" && (
          <section className="settings-section">
            <h2 className="settings-section-title">LSP 语言服务</h2>
            <p className="settings-section-desc">
              管理编辑器诊断、悬浮提示和补全使用的 language server。
            </p>
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
                  <article
                    key={server.languageId}
                    className="settings-lsp-card"
                  >
                    <div className="settings-lsp-head">
                      <div>
                        <div className="settings-row-title">
                          {server.displayName}
                        </div>
                        <div className="settings-row-meta">
                          <code>{server.languageId}</code>
                          {server.running && (
                            <span className="settings-row-badge is-installed">
                              运行中
                            </span>
                          )}
                          {!server.enabled && (
                            <span className="settings-row-badge is-missing">
                              已禁用
                            </span>
                          )}
                          {server.enabled && server.available && (
                            <span className="settings-row-badge is-installed">
                              可用
                            </span>
                          )}
                          {server.enabled && !server.available && (
                            <span className="settings-row-badge is-missing">
                              缺少命令
                            </span>
                          )}
                        </div>
                      </div>
                      <label className="settings-switch">
                        <input
                          type="checkbox"
                          checked={draft.enabled}
                          onChange={(event) =>
                            updateLspDraft(server.languageId, {
                              enabled: event.currentTarget.checked,
                            })
                          }
                        />
                        <span>启用</span>
                      </label>
                    </div>
                    <label className="settings-field">
                      <span>命令</span>
                      <input
                        value={draft.command}
                        onChange={(event) =>
                          updateLspDraft(server.languageId, {
                            command: event.currentTarget.value,
                          })
                        }
                        placeholder={server.defaultCommand}
                      />
                    </label>
                    <label className="settings-field">
                      <span>参数</span>
                      <input
                        value={argsText}
                        onChange={(event) =>
                          updateLspDraft(server.languageId, {
                            args: splitArgs(event.currentTarget.value),
                          })
                        }
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
                          disabled={
                            !server.customized || busyLsp === server.languageId
                          }
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

      {renderCustomProviderModal()}

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
              <button
                type="button"
                className="settings-btn is-install"
                autoFocus
                onClick={dismissStartupNotice}
              >
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

function usageGroupLabel(group: UsageSummaryGroupBy): string {
  if (group === "agent") return "按智能体";
  if (group === "workspace") return "按工作区";
  if (group === "session") return "按会话";
  return "按模型";
}

function usageDateRangeLabel(range: UsageDateRange): string {
  if (range === "today") return "今天";
  if (range === "7d") return "7 天";
  if (range === "30d") return "30 天";
  return "全部";
}

function usageWorkspaceScopeLabel(scope: UsageWorkspaceScope): string {
  return scope === "all" ? "全部工作区" : "当前工作区";
}

function usageDateRangeBounds(range: UsageDateRange): {
  from?: string;
  to?: string;
} {
  if (range === "all") return {};
  const now = new Date();
  const start = new Date(now);
  start.setHours(0, 0, 0, 0);
  if (range === "7d") start.setDate(start.getDate() - 6);
  if (range === "30d") start.setDate(start.getDate() - 29);
  return { from: start.toISOString(), to: now.toISOString() };
}

function usageTokenTotal(tokens: UsageSummaryRow["tokens"]): number {
  return (
    tokens.total_tokens ??
    (tokens.input_tokens ?? 0) +
      (tokens.output_tokens ?? 0) +
      (tokens.cache_read_tokens ?? 0) +
      (tokens.cache_write_tokens ?? 0) +
      (tokens.reasoning_tokens ?? 0)
  );
}

function formatUsageTokens(value: number): string {
  if (value >= 1_000_000)
    return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`;
  if (value >= 10_000) return `${Math.round(value / 1_000)}k`;
  return value.toLocaleString("en-US");
}

function usageBreakdownParts(
  tokens: UsageSummaryRow["tokens"],
): Array<{ label: string; value: number }> {
  return [
    { label: "输入", value: tokens.input_tokens },
    { label: "输出", value: tokens.output_tokens },
    { label: "缓存读", value: tokens.cache_read_tokens },
    { label: "缓存写", value: tokens.cache_write_tokens },
    { label: "推理", value: tokens.reasoning_tokens },
  ].flatMap((item) =>
    item.value != null ? [{ label: item.label, value: item.value }] : [],
  );
}

function usageRowMeta(row: UsageSummaryRow): string {
  const parts = [
    row.agent_cli,
    row.provider,
    row.workspace_root ? workspaceNameFromRoot(row.workspace_root) : null,
    row.session_count > 0 ? `${row.session_count} 会话` : null,
    `${row.event_count} 事件`,
  ].filter(Boolean);
  return parts.join(" · ");
}

function settingsPaneTitle(pane: SettingsPane): string {
  if (pane === "archive") return "已归档";
  if (pane === "remote") return "远程";
  if (pane === "web") return "Web 工具";
  if (pane === "image") return "图像能力";
  if (pane === "usage") return "用量";
  if (pane === "lsp") return "LSP";
  return "通用";
}

function settingsPaneDescription(pane: SettingsPane): string {
  if (pane === "archive") return "查看、恢复或删除保留在本地的已归档对话。";
  if (pane === "remote")
    return "管理远程 Linux 开发机，并在打开远程目录前验证 SSH。";
  if (pane === "web")
    return "配置 Codex 和 Claude 本机会话可用的搜索与网页抓取能力。";
  if (pane === "image")
    return "配置识图、生图、改图的降级 MCP 工具：识图复用对话模型，生/改图独立配置协议与模型。";
  if (pane === "usage")
    return "查看按模型、智能体、工作区或会话汇总的 token 用量。";
  if (pane === "lsp")
    return "管理编辑器诊断、悬浮提示和补全使用的 language server。";
  return "外观、默认提供者和智能体配置。";
}

function workspaceNameFromRoot(root: string): string {
  const trimmed = root.trim();
  if (!trimmed) return "未知项目";
  const pathOnly = trimmed.startsWith("ssh://")
    ? trimmed.replace(/^ssh:\/\/[^/]+/, "")
    : trimmed;
  const segments = pathOnly.split(/[\\/]+/).filter(Boolean);
  return segments.length > 0 ? segments[segments.length - 1] : trimmed;
}

function archivedWorkspaceOptions(sessions: ArchivedSessionListItem[]) {
  const options = new Map<string, string>();
  for (const session of sessions) {
    if (!options.has(session.workspace_root)) {
      options.set(
        session.workspace_root,
        workspaceNameFromRoot(session.workspace_root),
      );
    }
  }
  return [...options.entries()]
    .map(([root, name]) => ({ root, name }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

function groupArchivedSessionsByWorkspace(sessions: ArchivedSessionListItem[]) {
  const groups = new Map<
    string,
    { root: string; name: string; sessions: ArchivedSessionListItem[] }
  >();
  for (const session of sessions) {
    const root = session.workspace_root || "unknown";
    const group = groups.get(root) ?? {
      root,
      name: workspaceNameFromRoot(root),
      sessions: [],
    };
    group.sessions.push(session);
    groups.set(root, group);
  }
  return [...groups.values()].sort((a, b) => a.name.localeCompare(b.name));
}

function formatArchiveDate(value: string): string {
  const timestamp = Date.parse(
    value.includes("T") ? value : value.replace(" ", "T"),
  );
  if (!Number.isFinite(timestamp)) return value;
  return new Date(timestamp).toLocaleString("zh-CN", {
    year: "numeric",
    month: "long",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
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

function providerLabel(
  profiles: AgentProviderProfile[],
  profileId: string,
): string {
  return (
    profiles.find((profile) => profile.id === profileId)?.label ?? profileId
  );
}

function countableByokSourceProfiles(
  profiles: AgentProviderProfile[],
): AgentProviderProfile[] {
  return profiles.filter(
    (profile) =>
      profile.requires_credential &&
      !profile.hidden &&
      (!profile.custom || profile.configured),
  );
}

function selectableByokSourceProfiles(
  profiles: AgentProviderProfile[],
  selectedProfileId?: string,
): AgentProviderProfile[] {
  const sourceProfiles = profiles.filter(
    (profile) => profile.requires_credential && !profile.hidden,
  );
  const configuredCount = sourceProfiles.filter(
    (profile) => profile.configured,
  ).length;
  return sourceProfiles.filter((profile) =>
    visibleByokSourceProfile(profile, configuredCount, selectedProfileId),
  );
}

function visibleByokSourceProfile(
  profile: AgentProviderProfile,
  _configuredCount = 0,
  _selectedProfileId?: string,
): boolean {
  if (profile.hidden) return false;
  if (profile.custom) return profile.configured;
  return true;
}

function customProviderProtocolLabel(protocol: CustomProviderProtocol): string {
  if (protocol === "responses") return "Responses";
  if (protocol === "anthropic_messages") return "Anthropic Messages";
  return "Chat Completions";
}
