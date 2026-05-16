import { useCallback, useEffect, useState } from "react";
import type {
  AgentCliId,
  AgentInstallResult,
  AgentSettingsSnapshot,
  AppTheme,
  LspSettingsSnapshot,
  LspServerConfigInput,
} from "../../types";
import {
  settingsDetectAgents,
  settingsGetAgentSnapshot,
  settingsGetLspSnapshot,
  settingsInstallAgent,
  settingsProbeLspServer,
  settingsResetLspServer,
  settingsSaveCodexAcpProviderKey,
  settingsSaveCodexAcpVenusKey,
  settingsSaveLspServer,
  settingsSelectCodexAcpProvider,
  settingsSelectCodexDefaultMode,
  settingsSelectAgent,
  settingsSelectTheme,
} from "../../lib/tauri";
import { APP_THEMES, applyAppTheme } from "../../theme";
import "./SettingsPage.css";

type CodexAcpProvider = "default" | "venus" | "deepseek";

interface Props {
  onBack: () => void;
  onThemeChange?: (theme: AppTheme) => void;
}

export function SettingsPage({ onBack, onThemeChange }: Props) {
  const [activePane, setActivePane] = useState<"general" | "lsp">("general");
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
  const [codexAcpProvider, setCodexAcpProvider] = useState<CodexAcpProvider>("venus");
  const [codexAcpApiKey, setCodexAcpApiKey] = useState("");
  const [codexAcpMessage, setCodexAcpMessage] = useState<string | null>(null);

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
    const provider = snapshot?.codex_acp.provider;
    if (provider === "default" || provider === "venus" || provider === "deepseek") {
      setCodexAcpProvider(provider);
    }
  }, [snapshot?.codex_acp.provider]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onBack();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onBack]);

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

  const handleSaveCodexAcpProviderKey = useCallback(async () => {
    const key = codexAcpApiKey.trim();
    setError(null);
    setCodexAcpMessage(null);
    if (!key) {
      setError("API key 不能为空");
      return;
    }
    setBusyCodexAcp(true);
    try {
      const save =
        codexAcpProvider === "venus"
          ? settingsSaveCodexAcpVenusKey(key)
          : settingsSaveCodexAcpProviderKey(codexAcpProvider, key);
      const nextSnapshot = await save;
      setSnapshot(nextSnapshot);
      setCodexAcpApiKey("");
      setCodexAcpMessage(`${codexAcpProvider === "venus" ? "Venus" : "DeepSeek"} API key 已保存`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCodexAcp(false);
    }
  }, [codexAcpApiKey, codexAcpProvider]);

  const handleSelectCodexDefaultMode = useCallback(async () => {
    setBusyCodexAcp(true);
    setError(null);
    setCodexAcpMessage(null);
    try {
      const nextSnapshot = await settingsSelectCodexDefaultMode();
      setSnapshot(nextSnapshot);
      setCodexAcpProvider("default");
      setCodexAcpApiKey("");
      setCodexAcpMessage("已切换为默认 Codex 配置");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyCodexAcp(false);
    }
  }, []);

  const handleSelectCodexAcpProvider = useCallback(async (provider: Exclude<CodexAcpProvider, "default">) => {
    setError(null);
    setCodexAcpMessage(null);
    setCodexAcpProvider(provider);

    const configured =
      provider === "venus"
        ? !!snapshot?.codex_acp.venus_key_configured
        : !!snapshot?.codex_acp.deepseek_key_configured;
    if (!configured || snapshot?.codex_acp.provider === provider) {
      return;
    }

    setBusyCodexAcp(true);
    try {
      const nextSnapshot = await settingsSelectCodexAcpProvider(provider);
      setSnapshot(nextSnapshot);
      setCodexAcpApiKey("");
      setCodexAcpMessage(`已切换为 ${provider === "venus" ? "Venus" : "DeepSeek"} 配置`);
    } catch (e) {
      const currentProvider = snapshot?.codex_acp.provider;
      if (currentProvider === "default" || currentProvider === "venus" || currentProvider === "deepseek") {
        setCodexAcpProvider(currentProvider);
      }
      setError(String(e));
    } finally {
      setBusyCodexAcp(false);
    }
  }, [snapshot?.codex_acp.deepseek_key_configured, snapshot?.codex_acp.provider, snapshot?.codex_acp.venus_key_configured]);

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
          <h2 className="settings-section-title">默认提供者</h2>
          <p className="settings-section-desc">选择用于新会话的 ACP 智能体 CLI。</p>

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

          <div className="settings-rows">
            {snapshot?.agents.map((agent) => (
              <div key={agent.id} className={`settings-row ${agent.selected ? "is-selected" : ""}`}>
                <div className="settings-row-info">
                  <div className="settings-row-title">{agent.label}</div>
                  <div className="settings-row-meta">
                    <code>{agent.binary}</code>
                    {agent.detected_path && <span> · {agent.detected_path}</span>}
                    <span className={`settings-row-badge ${agent.installed ? "is-installed" : "is-missing"}`}>
                      {agent.installed ? "已安装" : "未安装"}
                    </span>
                  </div>
                </div>
                <div className="settings-row-actions">
                  {agent.installed ? (
                    <button
                      type="button"
                      className={`settings-btn ${agent.selected ? "is-selected" : ""}`}
                      disabled={agent.selected || busyAgent === agent.id || !!snapshot.env_override}
                      onClick={() => handleSelect(agent.id)}
                    >
                      {agent.selected ? "已选择" : busyAgent === agent.id ? "保存中..." : "使用"}
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
                {agent.id === "codex-acp" && (
                  <div className="settings-provider-config">
                    <div className="settings-provider-config-head">
                      <div>
                        <span>Codex 连接</span>
                        <p>选择 Codex 使用的模型服务，并写入本机配置。</p>
                      </div>
                      <span className="settings-provider-active">
                        当前：{snapshot.codex_acp.provider === "default" ? "默认" : snapshot.codex_acp.provider === "deepseek" ? "DeepSeek" : "Venus"}
                      </span>
                    </div>
                    <div className="settings-provider-config-path">
                      {codexAcpProvider === "default" ? (
                        <span>启动时不设置 <code>CODEX_HOME</code>，使用用户自己的 Codex 配置。</span>
                      ) : (
                        <span>写入 <code>{snapshot.codex_acp.config_path}</code></span>
                      )}
                    </div>
                    <div className="settings-provider-options" role="radiogroup" aria-label="Codex provider">
                      <button
                        type="button"
                        className={`settings-provider-option ${codexAcpProvider === "default" ? "is-selected" : ""}`}
                        onClick={handleSelectCodexDefaultMode}
                        aria-pressed={codexAcpProvider === "default"}
                        disabled={busyCodexAcp}
                      >
                        <span className="settings-provider-option-main">
                          <span>默认</span>
                          <span>不设置 CODEX_HOME，使用用户自己的 Codex 配置</span>
                        </span>
                        <span className={`settings-row-badge ${snapshot.codex_acp.provider === "default" ? "is-installed" : "is-missing"}`}>
                          {snapshot.codex_acp.provider === "default" ? "当前" : "可用"}
                        </span>
                      </button>
                      <button
                        type="button"
                        className={`settings-provider-option ${codexAcpProvider === "venus" ? "is-selected" : ""}`}
                        onClick={() => handleSelectCodexAcpProvider("venus")}
                        aria-pressed={codexAcpProvider === "venus"}
                        disabled={busyCodexAcp}
                      >
                        <span className="settings-provider-option-main">
                          <span>Venus</span>
                          <span>内部 Venus LLM 网关</span>
                        </span>
                        <span
                          className={`settings-row-badge ${snapshot.codex_acp.venus_key_configured ? "is-installed" : "is-missing"}`}
                        >
                          {snapshot.codex_acp.venus_key_configured ? "已配置" : "未配置"}
                        </span>
                      </button>
                      <button
                        type="button"
                        className={`settings-provider-option ${codexAcpProvider === "deepseek" ? "is-selected" : ""}`}
                        onClick={() => handleSelectCodexAcpProvider("deepseek")}
                        aria-pressed={codexAcpProvider === "deepseek"}
                        disabled={busyCodexAcp}
                      >
                        <span className="settings-provider-option-main">
                          <span>DeepSeek</span>
                          <span>经 Codex API Proxy 对接 DeepSeek API</span>
                        </span>
                        <span
                          className={`settings-row-badge ${snapshot.codex_acp.deepseek_key_configured ? "is-installed" : "is-missing"}`}
                        >
                          {snapshot.codex_acp.deepseek_key_configured ? "已配置" : "未配置"}
                        </span>
                      </button>
                    </div>
                    {codexAcpProvider !== "default" && (
                      <label className="settings-field settings-provider-key-field">
                        <span>{codexAcpProvider === "venus" ? "Venus key" : "DeepSeek key"}</span>
                        <input
                          aria-label="codex_acp_api_key"
                          type="password"
                          autoComplete="off"
                          placeholder={
                            codexAcpProvider === "venus"
                              ? snapshot.codex_acp.venus_key_configured
                                ? "输入新的 Venus key 以替换"
                                : "输入 Venus key"
                              : snapshot.codex_acp.deepseek_key_configured
                                ? "输入新的 DeepSeek API key 以替换"
                                : "输入 DeepSeek API key"
                          }
                          value={codexAcpApiKey}
                          onChange={(event) => setCodexAcpApiKey(event.currentTarget.value)}
                        />
                      </label>
                    )}
                    <div className="settings-provider-config-actions">
                      {codexAcpMessage && <span className="settings-provider-config-message">{codexAcpMessage}</span>}
                      {codexAcpProvider !== "default" && (
                        <button
                          type="button"
                          className="settings-btn"
                          disabled={busyCodexAcp || !codexAcpApiKey.trim()}
                          onClick={handleSaveCodexAcpProviderKey}
                        >
                          {busyCodexAcp ? "保存中..." : `保存 ${codexAcpProvider === "venus" ? "Venus" : "DeepSeek"} key`}
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>

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
    </div>
  );
}

function splitArgs(value: string): string[] {
  return value
    .split(/\s+/)
    .map((arg) => arg.trim())
    .filter(Boolean);
}
