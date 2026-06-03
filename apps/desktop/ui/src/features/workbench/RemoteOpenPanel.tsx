import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AgentCliId, RemoteMachineProfile, RemoteMachineProfilesSnapshot, RemoteOpenPhaseKind, RemoteOpenProgressEvent, UiSnapshot } from "../../types";
import {
  settingsGetAgentSnapshot,
  settingsGetRemoteProfiles,
  settingsValidateRemoteProfile,
  workspaceOpenRemoteProfile,
} from "../../lib/tauri";
import { onRemoteOpenProgress } from "../../lib/events";
import "./RemoteOpenPanel.css";

interface Props {
  onWorkspaceOpened: (snapshot: UiSnapshot) => void;
  onOpenSettings: () => void;
  onCancel?: () => void;
}

const REMOTE_OPEN_AGENT_CHOICES: Array<{ id: AgentCliId; label: string; detail: string }> = [
  { id: "claude-agent-acp", label: "Claude", detail: "kodex-claude" },
  { id: "codex-acp", label: "Codex", detail: "codex-acp" },
  { id: "codebuddy", label: "CodeBuddy", detail: "npm bootstrap" },
];

const REMOTE_OPEN_PHASES: Array<{ id: RemoteOpenPhaseKind; label: string }> = [
  { id: "ssh", label: "连接" },
  { id: "platform", label: "环境" },
  { id: "remote_path", label: "目录" },
  { id: "runtime_directory", label: "Runtime" },
  { id: "agent_install", label: "Agent" },
  { id: "agent_verify", label: "验证" },
  { id: "acp_launch", label: "启动" },
];

export function RemoteOpenPanel({ onWorkspaceOpened, onOpenSettings, onCancel }: Props) {
  const [snapshot, setSnapshot] = useState<RemoteMachineProfilesSnapshot>({ profiles: [] });
  const [selectedProfileId, setSelectedProfileId] = useState("");
  const [selectedAgent, setSelectedAgent] = useState<AgentCliId>("claude-agent-acp");
  const [remotePath, setRemotePath] = useState("");
  const [sshPassword, setSshPassword] = useState("");
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<"validate" | "open" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [currentRequestId, setCurrentRequestId] = useState<string | null>(null);
  const [progressEvents, setProgressEvents] = useState<RemoteOpenProgressEvent[]>([]);
  const currentRequestIdRef = useRef<string | null>(null);

  const selectedProfile = useMemo(
    () => snapshot.profiles.find((profile) => profile.id === selectedProfileId) ?? snapshot.profiles[0] ?? null,
    [selectedProfileId, snapshot.profiles],
  );

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [nextSnapshot, agentSnapshot] = await Promise.all([
        settingsGetRemoteProfiles(),
        settingsGetAgentSnapshot().catch(() => null),
      ]);
      setSnapshot(nextSnapshot);
      setSelectedProfileId((current) =>
        current && nextSnapshot.profiles.some((profile) => profile.id === current)
          ? current
          : nextSnapshot.profiles[0]?.id ?? "",
      );
      if (agentSnapshot?.settings.selected_agent && isRemoteOpenAgent(agentSnapshot.settings.selected_agent)) {
        setSelectedAgent(agentSnapshot.settings.selected_agent);
      }
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
    currentRequestIdRef.current = currentRequestId;
  }, [currentRequestId]);

  useEffect(() => {
    let disposed = false;
    let cleanup: (() => void) | null = null;
    onRemoteOpenProgress((progress) => {
      if (disposed || progress.request_id !== currentRequestIdRef.current) return;
      setProgressEvents((previous) => {
        const next = previous.filter((event) => event.phase !== progress.phase);
        return [...next, progress];
      });
    }).then((unlisten) => {
      if (disposed) {
        unlisten();
      } else {
        cleanup = unlisten;
      }
    });
    return () => {
      disposed = true;
      cleanup?.();
    };
  }, []);

  const handleValidate = useCallback(async () => {
    if (!selectedProfile) return;
    setBusy("validate");
    setError(null);
    setMessage(null);
    try {
      const request = {
        profile_id: selectedProfile.id,
        remote_path: remotePath.trim() || "~",
        include_acp: false,
        ...(sshPassword ? { ssh_password: sshPassword } : {}),
      };
      const nextSnapshot = await settingsValidateRemoteProfile(request);
      setSnapshot(nextSnapshot);
      const nextProfile = nextSnapshot.profiles.find((profile) => profile.id === selectedProfile.id);
      setMessage(nextProfile?.last_validation?.ok ? "验证通过" : "验证失败");
    } catch (e) {
      setError(String(e));
    } finally {
      setSshPassword("");
      setBusy(null);
    }
  }, [remotePath, selectedProfile, sshPassword]);

  const handleOpen = useCallback(async () => {
    if (!selectedProfile) return;
    const path = remotePath.trim();
    if (!path) {
      setError("远程目录不能为空");
      return;
    }
    if (!path.startsWith("/")) {
      setError("打开远程目录需要填写绝对路径");
      return;
    }
    const requestId = newRequestId();
    setCurrentRequestId(requestId);
    setProgressEvents([]);
    setBusy("open");
    setError(null);
    setMessage(null);
    try {
      const nextSnapshot = await workspaceOpenRemoteProfile({
        request_id: requestId,
        profile_id: selectedProfile.id,
        remote_path: path,
        ...(sshPassword ? { ssh_password: sshPassword } : {}),
        agent_cli: selectedAgent,
      });
      onWorkspaceOpened(nextSnapshot);
    } catch (e) {
      setError(String(e));
    } finally {
      setSshPassword("");
      setBusy(null);
    }
  }, [onWorkspaceOpened, remotePath, selectedAgent, selectedProfile, sshPassword]);

  if (loading) {
    return <div className="remote-open-status">正在加载远程机器...</div>;
  }

  if (snapshot.profiles.length === 0) {
    return (
      <div className="remote-open-empty">
        <div className="remote-open-title">还没有远程机器</div>
        <p>先在设置里添加 Linux 开发机并验证 SSH 可达性。</p>
        <div className="remote-open-actions">
          {onCancel && (
            <button type="button" className="remote-open-secondary" onClick={onCancel}>
              取消
            </button>
          )}
          <button type="button" className="remote-open-primary" onClick={onOpenSettings}>
            去设置远程机器
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="remote-open-panel">
      <div className="remote-open-heading">
        <div>
          <div className="remote-open-title">打开远程目录</div>
          <p>选择一台已保存的 Linux 开发机和远程路径，作为工作区打开。</p>
        </div>
      </div>
      {error && <div className="remote-open-error">{error}</div>}
      {message && <div className="remote-open-success">{message}</div>}
      <div className="remote-open-context">
        <section className="remote-open-step">
          <div className="remote-open-step-head">
            <span>1</span>
            <div>
              <strong>远程机器</strong>
              <small>{selectedProfile ? formatProfileTarget(selectedProfile) : "未选择"}</small>
            </div>
          </div>
          <label className="remote-open-field">
            <span>机器</span>
            <select value={selectedProfile?.id ?? ""} onChange={(event) => setSelectedProfileId(event.currentTarget.value)}>
              {snapshot.profiles.map((profile) => (
                <option key={profile.id} value={profile.id}>
                  {profile.display_name} · {formatProfileTarget(profile)}
                </option>
              ))}
            </select>
          </label>
          <label className="remote-open-field">
            <span>SSH 密码</span>
            <input
              aria-label="remote_open_password"
              type="password"
              autoComplete="off"
              value={sshPassword}
              onChange={(event) => setSshPassword(event.currentTarget.value)}
              placeholder="本次使用，不保存"
            />
          </label>
          {selectedProfile?.last_validation && (
            <div className="remote-open-validation">
              <span className={selectedProfile.last_validation.ok ? "is-ok" : "is-failed"}>
                {selectedProfile.last_validation.ok ? "上次验证通过" : "上次验证失败"}
              </span>
              {selectedProfile.last_validation.remote_path && <code>{selectedProfile.last_validation.remote_path}</code>}
            </div>
          )}
        </section>

        <section className="remote-open-step">
          <div className="remote-open-step-head">
            <span>2</span>
            <div>
              <strong>远程项目</strong>
              <small>{remotePath.trim() || "~"}</small>
            </div>
          </div>
          <label className="remote-open-field">
            <span>目录</span>
            <input
              aria-label="remote_open_path"
              value={remotePath}
              onChange={(event) => setRemotePath(event.currentTarget.value)}
              placeholder="/home/user/project"
            />
          </label>
          <div className="remote-open-field">
            <span>通道</span>
            <div
              className="remote-open-agent-options"
              role="radiogroup"
              aria-label="remote_open_agent"
            >
              {REMOTE_OPEN_AGENT_CHOICES.map((agent) => (
                <button
                  key={agent.id}
                  type="button"
                  role="radio"
                  aria-checked={selectedAgent === agent.id}
                  className={`remote-open-agent-option ${selectedAgent === agent.id ? "is-selected" : ""}`}
                  onClick={() => setSelectedAgent(agent.id)}
                >
                  <strong>{agent.label}</strong>
                  <small>{agent.detail}</small>
                </button>
              ))}
            </div>
          </div>
          <p className="remote-open-note">验证可使用默认用户目录；打开项目需要远程绝对路径。</p>
        </section>
      </div>
      {progressEvents.length > 0 && (
        <div className="remote-open-progress" aria-label="remote_open_progress">
          {REMOTE_OPEN_PHASES.map((phase) => {
            const event = progressEvents.find((item) => item.phase === phase.id);
            return (
              <div key={phase.id} className={`remote-open-progress-item ${event ? `is-${event.status}` : ""}`}>
                <span className="remote-open-progress-dot" />
                <span>{phase.label}</span>
                {event?.message && <small>{event.message}</small>}
              </div>
            );
          })}
        </div>
      )}
      <div className="remote-open-actions">
        {onCancel && (
          <button type="button" className="remote-open-secondary" onClick={onCancel}>
            取消
          </button>
        )}
        <button type="button" className="remote-open-secondary" onClick={onOpenSettings}>
          管理远程机器
        </button>
        <button type="button" className="remote-open-secondary" disabled={busy !== null} onClick={handleValidate}>
          {busy === "validate" ? "验证中..." : "验证目录"}
        </button>
        <button type="button" className="remote-open-primary" disabled={busy !== null || !remotePath.trim()} onClick={handleOpen}>
          {busy === "open" ? "打开中..." : "打开目录"}
        </button>
      </div>
    </div>
  );
}

function newRequestId(): string {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (char) => {
    const value = Math.floor(Math.random() * 16);
    const next = char === "x" ? value : (value & 0x3) | 0x8;
    return next.toString(16);
  });
}

function formatProfileTarget(profile: RemoteMachineProfile): string {
  return `${profile.ssh_target}${profile.ssh_port ? `:${profile.ssh_port}` : ""}`;
}

function isRemoteOpenAgent(agent: AgentCliId): boolean {
  return REMOTE_OPEN_AGENT_CHOICES.some((choice) => choice.id === agent);
}
