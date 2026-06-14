import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { AgentCliId, RemoteLinuxWorkspace, RemoteMachineProfile, RemoteMachineProfilesSnapshot, RemoteOpenPhaseKind, RemoteOpenProgressEvent, UiSnapshot } from "../../types";
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
  initialRemote?: RemoteLinuxWorkspace | null;
  headingTitle?: string;
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

export function RemoteOpenPanel({ onWorkspaceOpened, onOpenSettings, onCancel, initialRemote, headingTitle = "打开远程目录" }: Props) {
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
  const isOpening = busy === "open";
  const progressByPhase = useMemo(() => {
    return new Map(progressEvents.map((event) => [event.phase, event]));
  }, [progressEvents]);
  const latestProgress = progressEvents.length > 0 ? progressEvents[progressEvents.length - 1] : null;
  const showOpeningProgress = isOpening || progressEvents.length > 0;
  const openingStatusMessage =
    latestProgress?.message || "正在建立连接并准备远程工作区";

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [nextSnapshot, agentSnapshot] = await Promise.all([
        settingsGetRemoteProfiles(),
        settingsGetAgentSnapshot().catch(() => null),
      ]);
      setSnapshot(nextSnapshot);
      setSelectedProfileId((current) => {
        if (initialRemote?.profile_id && nextSnapshot.profiles.some((profile) => profile.id === initialRemote.profile_id)) {
          return initialRemote.profile_id;
        }
        const matchingProfile = initialRemote
          ? nextSnapshot.profiles.find(
              (profile) =>
                profile.ssh_target === initialRemote.ssh_target &&
                (profile.ssh_port ?? null) === (initialRemote.ssh_port ?? null),
            )
          : null;
        if (matchingProfile) return matchingProfile.id;
        return current && nextSnapshot.profiles.some((profile) => profile.id === current)
          ? current
          : nextSnapshot.profiles[0]?.id ?? "";
      });
      if (initialRemote?.remote_path) {
        setRemotePath(initialRemote.remote_path);
      }
      if (initialRemote?.agent_cli && isRemoteOpenAgent(initialRemote.agent_cli)) {
        setSelectedAgent(initialRemote.agent_cli);
      } else if (agentSnapshot?.settings.selected_agent && isRemoteOpenAgent(agentSnapshot.settings.selected_agent)) {
        setSelectedAgent(agentSnapshot.settings.selected_agent);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [initialRemote]);

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
          <div className="remote-open-title">{headingTitle}</div>
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
          <div className="remote-open-field remote-open-machine-field">
            <span>机器</span>
            <div className="remote-open-machine-options" role="radiogroup" aria-label="remote_open_profile">
              {snapshot.profiles.map((profile) => {
                const selected = profile.id === selectedProfile?.id;
                return (
                  <button
                    key={profile.id}
                    type="button"
                    role="radio"
                    aria-checked={selected}
                    className={`remote-open-machine-option ${selected ? "is-selected" : ""}`}
                    disabled={isOpening}
                    onClick={() => setSelectedProfileId(profile.id)}
                  >
                    <strong>{profile.display_name}</strong>
                    <small>{formatProfileTarget(profile)}</small>
                  </button>
                );
              })}
            </div>
          </div>
          <label className="remote-open-field">
            <span>SSH 密码</span>
            <input
              aria-label="remote_open_password"
              type="password"
              autoComplete="off"
              value={sshPassword}
              disabled={isOpening}
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
              disabled={isOpening}
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
                  disabled={isOpening}
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
      {showOpeningProgress && (
        <div
          className={`remote-open-wait ${isOpening ? "is-active" : ""}`}
          role="status"
          aria-live="polite"
          aria-label="远程工作区准备状态"
        >
          <span className="remote-open-wait-spinner" aria-hidden="true" />
          <div className="remote-open-wait-copy">
            <strong>{isOpening ? "正在准备远程工作区" : "远程准备已结束"}</strong>
            <span>{openingStatusMessage}</span>
          </div>
        </div>
      )}
      {showOpeningProgress && (
        <div className="remote-open-progress" aria-label="remote_open_progress">
          {REMOTE_OPEN_PHASES.map((phase) => {
            const event = progressByPhase.get(phase.id);
            const statusClass = event ? `is-${event.status}` : "is-pending";
            return (
              <div key={phase.id} className={`remote-open-progress-item ${statusClass}`}>
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
          <button type="button" className="remote-open-secondary" disabled={isOpening} onClick={onCancel}>
            取消
          </button>
        )}
        <button type="button" className="remote-open-secondary" disabled={isOpening} onClick={onOpenSettings}>
          管理远程机器
        </button>
        <button type="button" className="remote-open-secondary" disabled={busy !== null} onClick={handleValidate}>
          {busy === "validate" ? "验证中..." : "验证目录"}
        </button>
        <button type="button" className="remote-open-primary" disabled={busy !== null || !remotePath.trim()} onClick={handleOpen}>
          <span className="remote-open-button-content">
            {isOpening && <span className="remote-open-button-spinner" aria-hidden="true" />}
            {isOpening ? "打开中..." : "打开目录"}
          </span>
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
