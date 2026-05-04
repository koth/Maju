import { useCallback, useEffect, useState } from "react";
import type { AgentCliId, AgentInstallResult, AgentSettingsSnapshot } from "../../types";
import {
  settingsDetectAgents,
  settingsGetAgentSnapshot,
  settingsInstallAgent,
  settingsSelectAgent,
} from "../../lib/tauri";
import "./SettingsPage.css";

interface Props {
  onBack: () => void;
}

export function SettingsPage({ onBack }: Props) {
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
    <div className="settings-page">
      <aside className="settings-sidebar">
        <button type="button" className="settings-back" onClick={onBack}>
          <span className="settings-back-arrow">←</span> Back to app
        </button>

        <div className="settings-nav-group">
          <span className="settings-nav-label">App</span>
          <button type="button" className="settings-nav-item is-active">General</button>
        </div>
      </aside>

      <main className="settings-content">
        <header className="settings-content-header">
          <h1>General</h1>
          <p>Default provider and agent configuration.</p>
        </header>

        <section className="settings-section">
          <h2 className="settings-section-title">Default Provider</h2>
          <p className="settings-section-desc">Choose the ACP agent CLI used for new sessions.</p>

          {loading && <div className="settings-status">Loading...</div>}
          {error && (
            <div className="settings-error">
              <span>{error}</span>
              <button type="button" className="settings-link-btn" onClick={load}>Retry</button>
            </div>
          )}
          {snapshot?.env_override && (
            <div className="settings-warning">
              <code>ACP_AGENT_COMMAND</code> is set and overrides this selection: <code>{snapshot.env_override}</code>
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
                      {agent.installed ? "Installed" : "Not installed"}
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
                      {agent.selected ? "Selected" : busyAgent === agent.id ? "Saving..." : "Use"}
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="settings-btn is-install"
                      disabled={busyAgent === agent.id}
                      onClick={() => handleInstall(agent.id)}
                    >
                      {busyAgent === agent.id ? "Installing..." : "Install"}
                    </button>
                  )}
                </div>
              </div>
            ))}
          </div>

          <div className="settings-detect-row">
            <button type="button" className="settings-link-btn" onClick={handleDetect} disabled={loading}>
              Re-detect installed CLIs
            </button>
          </div>
        </section>
      </main>
    </div>
  );
}
