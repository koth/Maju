// Agent (+ dsh preset) chooser shared by the handoff dialog and any other
// surface that starts a session with a specific agent.
//
// Mirrors the sidebar's new-session picker: the agent list comes from the
// settings snapshot (label / binary / installed) and the dsh agent presets are
// only fetched when the DeepSeek Harness is selected.

import { useEffect, useRef, useState } from "react";
import type { AgentCliId, AgentSettingsSnapshot } from "../../types";
import { settingsGetAgentSnapshot, settingsListDshPresets } from "../../lib/tauri";
import type { DshPresetOption } from "../../lib/tauri";
import "./AgentChoiceField.css";

interface Props {
  value: AgentCliId | null;
  onChange: (agent: AgentCliId) => void;
  preset: string | null;
  onPresetChange: (preset: string | null) => void;
  disabled?: boolean;
}

/// Empty preset = "follow the dsh default configured in settings".
const DEFAULT_PRESET = "";

export function AgentChoiceField({ value, onChange, preset, onPresetChange, disabled }: Props) {
  const [snapshot, setSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [presets, setPresets] = useState<DshPresetOption[] | null>(null);
  const [presetsLoading, setPresetsLoading] = useState(false);
  const presetsFetchedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    settingsGetAgentSnapshot()
      .then((next) => {
        if (cancelled) return;
        setSnapshot(next);
        // Preselect the settings default (or the first installed agent) so the
        // dialog never opens with nothing chosen.
        if (!value) {
          const preferred =
            next.agents.find((agent) => agent.id === next.settings.selected_agent && agent.installed) ??
            next.agents.find((agent) => agent.installed) ??
            next.agents[0];
          if (preferred) onChange(preferred.id);
        }
      })
      .catch((error) => {
        if (!cancelled) setLoadError(String(error));
      });
    return () => {
      cancelled = true;
    };
    // `value`/`onChange` are intentionally out of the dependency list: this is a
    // one-shot load, and re-running it on every selection would refetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (value !== "deepseek-harness") return;
    if (presetsFetchedRef.current) return;
    presetsFetchedRef.current = true;
    let cancelled = false;
    setPresetsLoading(true);
    settingsListDshPresets()
      .then((list) => {
        if (cancelled) return;
        setPresets(list);
        onPresetChange(preset ?? DEFAULT_PRESET);
      })
      .catch(() => {
        if (cancelled) return;
        setPresets([]);
        // Allow a retry when the agent is chosen again.
        presetsFetchedRef.current = false;
      })
      .finally(() => {
        if (!cancelled) setPresetsLoading(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);

  return (
    <div className="agent-choice">
      <div className="agent-choice-agents" role="radiogroup" aria-label="交付给哪个 Agent">
        {loadError && <div className="agent-choice-error">加载 Agent 列表失败：{loadError}</div>}
        {!loadError && snapshot === null && (
          <div className="agent-choice-loading">正在读取 Agent 列表…</div>
        )}
        {snapshot?.agents.map((agent) => (
          <label
            key={agent.id}
            className={`agent-choice-option${value === agent.id ? " is-selected" : ""}${
              !agent.installed ? " is-missing" : ""
            }`}
          >
            <input
              type="radio"
              name="handoff-agent"
              value={agent.id}
              checked={value === agent.id}
              disabled={disabled}
              onChange={() => onChange(agent.id)}
            />
            <span className="agent-choice-label">{agent.label}</span>
            <span className="agent-choice-meta">
              {agent.id === snapshot.settings.selected_agent ? "Settings 默认" : agent.binary}
              {!agent.installed ? " · 未安装" : ""}
            </span>
          </label>
        ))}
      </div>

      {value === "deepseek-harness" && (
        <label className="agent-choice-preset">
          <span className="agent-choice-preset-label">Agent 预设</span>
          {presetsLoading ? (
            <span className="agent-choice-loading">正在读取预设…</span>
          ) : (
            <select
              value={preset ?? DEFAULT_PRESET}
              disabled={disabled}
              onChange={(event) => onPresetChange(event.target.value)}
            >
              <option value={DEFAULT_PRESET}>跟随 dsh 默认</option>
              {(presets ?? []).map((option) => (
                <option key={option.id} value={option.id}>
                  {option.label || option.id}
                </option>
              ))}
            </select>
          )}
        </label>
      )}
    </div>
  );
}
