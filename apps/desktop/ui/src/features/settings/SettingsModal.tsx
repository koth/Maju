import { useCallback, useEffect, useState } from "react";
import type { AgentCliId, AgentInstallResult, AgentSettingsSnapshot } from "../../types";
import {
  settingsDetectAgents,
  settingsGetAgentSnapshot,
  settingsInstallAgent,
  settingsSelectAgent,
} from "../../lib/tauri";
import "./SettingsModal.css";

interface Props {
  onClose: () => void;
}

export function SettingsModal({ onClose }: Props) {
  const [snapshot, setSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyAgent, setBusyAgent] = useState<AgentCliId | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [installResult, setInstallResult] = useState<AgentInstallResult | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setSnapshot(await settingsGetAgentSnapshot());
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onClose]);

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

  return (
    <div className="settings-overlay" role="dialog" aria-modal="true" aria-label="设置">
      <div className="settings-modal">
        <header className="settings-header">
          <div>
            <div className="settings-kicker">Kodex</div>
            <h2>设置</h2>
          </div>
          <button type="button" className="settings-close" onClick={onClose} aria-label="关闭设置">
            ×
          </button>
        </header>

        <section className="settings-section">
          <div className="settings-section-header">
            <div>
              <h3>智能体</h3>
              <p>选择用于未来会话的 ACP 智能体 CLI。</p>
            </div>
            <button type="button" className="settings-secondary-btn" onClick={handleDetect} disabled={loading}>
              重新检测
            </button>
          </div>

          {loading && <div className="settings-status">正在加载设置...</div>}
          {error && (
            <div className="settings-error">
              <span>{error}</span>
              <button type="button" onClick={load}>重试</button>
            </div>
          )}
          {snapshot?.env_override && (
            <div className="settings-warning">
              ACP_AGENT_COMMAND 已设置，将覆盖此选择：<code>{snapshot.env_override}</code>
            </div>
          )}
          {installResult && (
            <div className={installResult.success ? "settings-success" : "settings-error"}>
              <span>{installResult.message}</span>
              {installResult.manual_instruction && <code>{installResult.manual_instruction}</code>}
            </div>
          )}

          <div className="agent-list">
            {snapshot?.agents.map((agent) => (
              <div key={agent.id} className={`agent-card ${agent.selected ? "is-selected" : ""}`}>
                <div className="agent-main">
                  <div className="agent-title-row">
                    <h4>{agent.label}</h4>
                    {agent.selected && <span className="agent-badge">已选择</span>}
                    <span className={`agent-status ${agent.installed ? "is-installed" : "is-missing"}`}>
                      {agent.installed ? "已安装" : "未安装"}
                    </span>
                  </div>
                  <p>
                    Binary: <code>{agent.binary}</code>
                    {agent.detected_path && <span className="agent-path"> · {agent.detected_path}</span>}
                  </p>
                </div>
                <div className="agent-actions">
                  <button
                    type="button"
                    className="settings-primary-btn"
                    disabled={!agent.installed || agent.selected || busyAgent === agent.id || !!snapshot.env_override}
                    onClick={() => handleSelect(agent.id)}
                  >
                    {busyAgent === agent.id ? "保存中..." : "使用"}
                  </button>
                  {!agent.installed && (
                    <button
                      type="button"
                      className="settings-secondary-btn"
                      disabled={busyAgent === agent.id}
                      onClick={() => handleInstall(agent.id)}
                    >
                      {busyAgent === agent.id ? "安装中..." : "安装"}
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>
        </section>
      </div>
    </div>
  );
}
