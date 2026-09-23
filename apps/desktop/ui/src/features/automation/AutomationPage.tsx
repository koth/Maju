import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type {
  AgentCliId,
  AgentSettingsSnapshot,
  AutomationInput,
  AutomationRecord,
  AutomationRunRecord,
  AutomationSchedule,
  AutomationScheduleKind,
  WorkspaceSessionList,
} from "../../types";
import {
  automationCreate,
  automationDelete,
  automationList,
  automationListRuns,
  automationRunNow,
  automationSetEnabled,
  automationUpdate,
  sessionList,
  settingsGetAgentSnapshot,
  settingsListDshPresets,
  type DshPresetOption,
} from "../../lib/tauri";
import { appConfirm } from "../../lib/confirm";
import {
  SCHEDULE_KIND_LABELS,
  WEEKDAY_LABELS,
  describeSchedule,
  formatTimestamp,
  formatWallClock,
  nextRunLabel,
  parseWallClock,
  runStatusLabel,
  runTriggerLabel,
} from "./schedule";
import "./AutomationPage.css";

interface Props {
  onBack: () => void;
}

/** Editable schedule shape for the form (friendlier than raw DTO fields). */
interface ScheduleForm {
  kind: AutomationScheduleKind;
  intervalValue: number;
  intervalUnit: "minutes" | "hours";
  /** `HH:mm` for daily / weekly. */
  time: string;
  weekday: number;
  /** `YYYY-MM-DDTHH:mm` (local) for one-shot runs. */
  dateTime: string;
}

interface EditorState {
  editingId: string | null;
  name: string;
  prompt: string;
  workspaceRoot: string;
  agentCli: AgentCliId;
  agentPreset: string;
  schedule: ScheduleForm;
}

interface RunsState {
  loading: boolean;
  items: AutomationRunRecord[];
  error: string | null;
}

const DEFAULT_AGENT: AgentCliId = "deepseek-harness";

function defaultSchedule(): ScheduleForm {
  return {
    kind: "daily",
    intervalValue: 30,
    intervalUnit: "minutes",
    time: "09:30",
    weekday: 1,
    dateTime: "",
  };
}

function scheduleFromDto(schedule: AutomationSchedule): ScheduleForm {
  const minutes = schedule.interval_minutes ?? 30;
  const form: ScheduleForm = {
    kind: schedule.kind ?? "daily",
    intervalValue:
      minutes >= 60 && minutes % 60 === 0 ? minutes / 60 : Math.max(1, minutes),
    intervalUnit: minutes >= 60 && minutes % 60 === 0 ? "hours" : "minutes",
    time: formatWallClock(schedule.hour, schedule.minute),
    weekday: Math.min(Math.max(schedule.weekday ?? 1, 1), 7),
    dateTime: "",
  };
  if (schedule.kind === "once" && schedule.run_at_ms != null) {
    const date = new Date(schedule.run_at_ms);
    if (!Number.isNaN(date.getTime())) {
      const pad = (value: number) => String(value).padStart(2, "0");
      form.dateTime = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(
        date.getDate(),
      )}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
    }
  }
  return form;
}

/** Build the wire `AutomationSchedule`; `null` when the form is incomplete. */
function scheduleFromForm(form: ScheduleForm): AutomationSchedule | null {
  switch (form.kind) {
    case "once": {
      const at = form.dateTime ? new Date(form.dateTime).getTime() : NaN;
      if (Number.isNaN(at)) return null;
      return { kind: "once", run_at_ms: at };
    }
    case "interval": {
      const value = Math.floor(form.intervalValue);
      if (!Number.isFinite(value) || value < 1) return null;
      return {
        kind: "interval",
        interval_minutes: form.intervalUnit === "hours" ? value * 60 : value,
      };
    }
    case "daily": {
      const parsed = parseWallClock(form.time);
      if (!parsed) return null;
      return { kind: "daily", hour: parsed[0], minute: parsed[1] };
    }
    case "weekly": {
      const parsed = parseWallClock(form.time);
      if (!parsed) return null;
      return {
        kind: "weekly",
        hour: parsed[0],
        minute: parsed[1],
        weekday: Math.min(Math.max(form.weekday, 1), 7),
      };
    }
    default:
      return null;
  }
}

export function AutomationPage({ onBack }: Props) {
  const [automations, setAutomations] = useState<AutomationRecord[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [workspaces, setWorkspaces] = useState<WorkspaceSessionList[]>([]);
  const [agentSnapshot, setAgentSnapshot] = useState<AgentSettingsSnapshot | null>(null);
  const [editor, setEditor] = useState<EditorState | null>(null);
  const [editorError, setEditorError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [busyIds, setBusyIds] = useState<Record<string, boolean>>({});
  const [expandedRuns, setExpandedRuns] = useState<Record<string, RunsState>>({});
  const [dshPresets, setDshPresets] = useState<DshPresetOption[] | null>(null);
  const dshPresetsFetchedRef = useRef(false);

  const refresh = useCallback(async () => {
    try {
      const list = await automationList();
      setAutomations(list);
      setListError(null);
    } catch (error) {
      setAutomations([]);
      setListError(String(error));
    }
  }, []);

  useEffect(() => {
    refresh();
    sessionList()
      .then(setWorkspaces)
      .catch(() => {});
    settingsGetAgentSnapshot()
      .then(setAgentSnapshot)
      .catch(() => {});
  }, [refresh]);

  // Lazily load the dsh preset roster the first time a DeepSeek Harness
  // automation is being edited (mirrors the new-session modal).
  useEffect(() => {
    if (!editor || editor.agentCli !== "deepseek-harness") return;
    if (dshPresetsFetchedRef.current) return;
    dshPresetsFetchedRef.current = true;
    let cancelled = false;
    settingsListDshPresets()
      .then((list) => {
        if (!cancelled) setDshPresets(list);
      })
      .catch(() => {
        if (!cancelled) setDshPresets([]);
        dshPresetsFetchedRef.current = false;
      });
    return () => {
      cancelled = true;
    };
  }, [editor]);

  const workspaceNameFor = useCallback(
    (root: string) => {
      const match = workspaces.find((item) => item.workspace.root === root);
      return match?.workspace.name ?? root;
    },
    [workspaces],
  );

  const agentLabelFor = useCallback(
    (agent?: AgentCliId | null) => {
      if (!agent) return "默认";
      const match = agentSnapshot?.agents.find((item) => item.id === agent);
      return match?.label ?? agent;
    },
    [agentSnapshot],
  );

  const openCreate = useCallback(() => {
    setEditorError(null);
    setEditor({
      editingId: null,
      name: "",
      prompt: "",
      workspaceRoot: workspaces[0]?.workspace.root ?? "",
      agentCli: DEFAULT_AGENT,
      agentPreset: "",
      schedule: defaultSchedule(),
    });
  }, [workspaces]);

  const openEdit = useCallback((record: AutomationRecord) => {
    setEditorError(null);
    setEditor({
      editingId: record.id,
      name: record.name,
      prompt: record.prompt,
      workspaceRoot: record.workspace_root,
      agentCli: record.agent_cli ?? DEFAULT_AGENT,
      agentPreset: record.agent_preset ?? "",
      schedule: scheduleFromDto(record.schedule),
    });
  }, []);

  const closeEditor = useCallback(() => {
    if (saving) return;
    setEditor(null);
    setEditorError(null);
  }, [saving]);

  const handleSave = useCallback(async () => {
    if (!editor) return;
    const name = editor.name.trim();
    const prompt = editor.prompt.trim();
    if (!name) {
      setEditorError("请填写自动化名称");
      return;
    }
    if (!prompt) {
      setEditorError("请填写要执行的提示词");
      return;
    }
    if (!editor.workspaceRoot) {
      setEditorError("请选择目标项目");
      return;
    }
    const schedule = scheduleFromForm(editor.schedule);
    if (!schedule) {
      setEditorError("执行计划无效，请检查时间设置");
      return;
    }
    const input: AutomationInput = {
      name,
      prompt,
      workspace_root: editor.workspaceRoot,
      agent_cli: editor.agentCli,
      agent_preset: editor.agentPreset || null,
      schedule,
    };
    try {
      setSaving(true);
      setEditorError(null);
      if (editor.editingId) {
        await automationUpdate(editor.editingId, input);
      } else {
        await automationCreate(input);
      }
      setEditor(null);
      await refresh();
    } catch (error) {
      setEditorError(String(error));
    } finally {
      setSaving(false);
    }
  }, [editor, refresh]);

  const handleToggleEnabled = useCallback(
    async (record: AutomationRecord) => {
      try {
        setBusyIds((current) => ({ ...current, [record.id]: true }));
        await automationSetEnabled(record.id, !record.enabled);
        await refresh();
      } catch (error) {
        setListError(String(error));
      } finally {
        setBusyIds((current) => ({ ...current, [record.id]: false }));
      }
    },
    [refresh],
  );

  const loadRuns = useCallback(async (record: AutomationRecord) => {
    try {
      const items = await automationListRuns(record.id, 20);
      setExpandedRuns((latest) => ({
        ...latest,
        [record.id]: { loading: false, items, error: null },
      }));
    } catch (error) {
      setExpandedRuns((latest) => ({
        ...latest,
        [record.id]: { loading: false, items: [], error: String(error) },
      }));
    }
  }, []);

  const handleRunNow = useCallback(
    async (record: AutomationRecord) => {
      try {
        setBusyIds((current) => ({ ...current, [record.id]: true }));
        await automationRunNow(record.id);
        await refresh();
        if (expandedRuns[record.id]) {
          await loadRuns(record);
        }
      } catch (error) {
        setListError(String(error));
      } finally {
        setBusyIds((current) => ({ ...current, [record.id]: false }));
      }
    },
    [expandedRuns, loadRuns, refresh],
  );

  const handleDelete = useCallback(
    async (record: AutomationRecord) => {
      const accepted = await appConfirm({
        title: "删除自动化",
        description: "删除后将不再按计划执行，运行历史也会一并删除。",
        detail: record.name,
        confirmLabel: "删除",
        tone: "danger",
      });
      if (!accepted) return;
      try {
        setBusyIds((current) => ({ ...current, [record.id]: true }));
        await automationDelete(record.id);
        await refresh();
      } catch (error) {
        setListError(String(error));
      } finally {
        setBusyIds((current) => ({ ...current, [record.id]: false }));
      }
    },
    [refresh],
  );

  const toggleRuns = useCallback(
    (record: AutomationRecord) => {
      if (expandedRuns[record.id]) {
        setExpandedRuns((current) => {
          const next = { ...current };
          delete next[record.id];
          return next;
        });
        return;
      }
      setExpandedRuns((current) => ({
        ...current,
        [record.id]: { loading: true, items: [], error: null },
      }));
      void loadRuns(record);
    },
    [expandedRuns, loadRuns],
  );

  const workspaceOptions = useMemo(
    () =>
      workspaces.map((item) => ({
        value: item.workspace.root,
        label: item.workspace.name,
      })),
    [workspaces],
  );

  const agentOptions = useMemo(
    () => (agentSnapshot?.agents ?? []).map((agent) => agent),
    [agentSnapshot],
  );

  return (
    <div className="automation-page">
      <div className="automation-drag-strip" data-tauri-drag-region />
      <header className="automation-header">
        <button type="button" className="automation-back" onClick={onBack}>
          <span className="automation-back-arrow">←</span> 返回应用
        </button>
        <div className="automation-title-row">
          <div>
            <h1>自动化</h1>
            <p className="automation-subtitle">
              按计划自动执行提示词，到点提醒你并自动运行
            </p>
          </div>
          <button
            type="button"
            className="automation-new-btn"
            onClick={openCreate}
          >
            + 新建自动化
          </button>
        </div>
      </header>

      <div className="automation-content">
        {listError && <div className="automation-error">{listError}</div>}
        {automations == null && (
          <div className="automation-empty" role="status">
            正在载入自动化...
          </div>
        )}
        {automations != null && automations.length === 0 && (
          <div className="automation-empty">
            <span className="automation-empty-title">还没有自动化</span>
            <span className="automation-empty-copy">
              创建一个定时任务：到点后自动把提示词发给所选项目的智能体执行，并提醒你执行情况。
            </span>
            <button
              type="button"
              className="automation-new-btn"
              onClick={openCreate}
            >
              + 新建自动化
            </button>
          </div>
        )}

        {automations?.map((record) => {
          const busy = !!busyIds[record.id];
          const runs = expandedRuns[record.id];
          return (
            <section
              key={record.id}
              className={`automation-card ${record.enabled ? "" : "is-disabled"}`}
            >
              <div className="automation-card-head">
                <div className="automation-card-title">
                  <span className="automation-name">{record.name}</span>
                  <span className="automation-chip">
                    {describeSchedule(record.schedule)}
                  </span>
                  {!record.enabled && (
                    <span className="automation-chip is-muted">已暂停</span>
                  )}
                </div>
                <label className="automation-toggle">
                  <input
                    type="checkbox"
                    checked={record.enabled}
                    disabled={busy}
                    onChange={() => handleToggleEnabled(record)}
                  />
                  <span>启用</span>
                </label>
              </div>

              <div className="automation-meta">
                <span title={record.workspace_root}>
                  目标项目 {workspaceNameFor(record.workspace_root)}
                </span>
                <span>Agent {agentLabelFor(record.agent_cli)}</span>
                <span>{nextRunLabel(record.next_run_at_ms, record.enabled)}</span>
                {record.last_run && (
                  <span>
                    上次运行 {formatTimestamp(record.last_run.started_at)} ·{" "}
                    {runStatusLabel(record.last_run.status)}
                  </span>
                )}
              </div>

              <div className="automation-prompt">{record.prompt}</div>

              <div className="automation-card-actions">
                <button
                  type="button"
                  className="automation-action-btn is-primary"
                  disabled={busy}
                  onClick={() => handleRunNow(record)}
                >
                  {busy ? "执行中..." : "立即运行"}
                </button>
                <button
                  type="button"
                  className="automation-action-btn"
                  disabled={busy}
                  onClick={() => openEdit(record)}
                >
                  编辑
                </button>
                <button
                  type="button"
                  className="automation-action-btn is-danger"
                  disabled={busy}
                  onClick={() => handleDelete(record)}
                >
                  删除
                </button>
                <button
                  type="button"
                  className="automation-action-btn"
                  onClick={() => toggleRuns(record)}
                >
                  {runs ? "收起运行历史" : `运行历史 (${record.run_count})`}
                </button>
              </div>

              {runs && (
                <div className="automation-runs">
                  {runs.loading && (
                    <div className="automation-runs-note">正在载入运行历史...</div>
                  )}
                  {runs.error && (
                    <div className="automation-error">{runs.error}</div>
                  )}
                  {!runs.loading && !runs.error && runs.items.length === 0 && (
                    <div className="automation-runs-note">还没有运行记录</div>
                  )}
                  {runs.items.map((run) => (
                    <div key={run.id} className="automation-run-row">
                      <span
                        className={`automation-run-status is-${run.status}`}
                      >
                        {runStatusLabel(run.status)}
                      </span>
                      <span className="automation-run-trigger">
                        {runTriggerLabel(run.trigger)}
                      </span>
                      <span className="automation-run-time">
                        {formatTimestamp(run.started_at)}
                      </span>
                      {run.error && (
                        <span className="automation-run-error">{run.error}</span>
                      )}
                    </div>
                  ))}
                </div>
              )}
            </section>
          );
        })}
      </div>

      {editor &&
        createPortal(
          <div
            className="automation-modal-backdrop"
            role="presentation"
            onClick={closeEditor}
          >
            <div
              className="automation-modal"
              role="dialog"
              aria-modal="true"
              aria-label={editor.editingId ? "编辑自动化" : "新建自动化"}
              onClick={(event) => event.stopPropagation()}
            >
              <div className="automation-modal-header">
                <span>{editor.editingId ? "编辑自动化" : "新建自动化"}</span>
                <button
                  type="button"
                  className="automation-modal-close"
                  onClick={closeEditor}
                  disabled={saving}
                >
                  ×
                </button>
              </div>

              <div className="automation-form">
                <label className="automation-field">
                  <span className="automation-field-label">名称</span>
                  <input
                    type="text"
                    value={editor.name}
                    disabled={saving}
                    placeholder="例如：每日晨报"
                    onChange={(event) =>
                      setEditor((current) =>
                        current
                          ? { ...current, name: event.target.value }
                          : current,
                      )
                    }
                  />
                </label>

                <div className="automation-field">
                  <span className="automation-field-label">执行计划</span>
                  <div className="automation-schedule-row">
                    <select
                      value={editor.schedule.kind}
                      disabled={saving}
                      onChange={(event) =>
                        setEditor((current) =>
                          current
                            ? {
                                ...current,
                                schedule: {
                                  ...current.schedule,
                                  kind: event.target.value as AutomationScheduleKind,
                                },
                              }
                            : current,
                        )
                      }
                    >
                      {(
                        Object.entries(SCHEDULE_KIND_LABELS) as [
                          AutomationScheduleKind,
                          string,
                        ][]
                      ).map(([value, label]) => (
                        <option key={value} value={value}>
                          {label}
                        </option>
                      ))}
                    </select>

                    {editor.schedule.kind === "once" && (
                      <input
                        type="datetime-local"
                        value={editor.schedule.dateTime}
                        disabled={saving}
                        onChange={(event) =>
                          setEditor((current) =>
                            current
                              ? {
                                  ...current,
                                  schedule: {
                                    ...current.schedule,
                                    dateTime: event.target.value,
                                  },
                                }
                              : current,
                          )
                        }
                      />
                    )}

                    {editor.schedule.kind === "interval" && (
                      <>
                        <input
                          type="number"
                          min={1}
                          value={editor.schedule.intervalValue}
                          disabled={saving}
                          onChange={(event) =>
                            setEditor((current) =>
                              current
                                ? {
                                    ...current,
                                    schedule: {
                                      ...current.schedule,
                                      intervalValue: Number(event.target.value),
                                    },
                                  }
                                : current,
                            )
                          }
                        />
                        <select
                          value={editor.schedule.intervalUnit}
                          disabled={saving}
                          onChange={(event) =>
                            setEditor((current) =>
                              current
                                ? {
                                    ...current,
                                    schedule: {
                                      ...current.schedule,
                                      intervalUnit: event.target.value as
                                        | "minutes"
                                        | "hours",
                                    },
                                  }
                                : current,
                            )
                          }
                        >
                          <option value="minutes">分钟</option>
                          <option value="hours">小时</option>
                        </select>
                      </>
                    )}

                    {(editor.schedule.kind === "daily" ||
                      editor.schedule.kind === "weekly") && (
                      <>
                        {editor.schedule.kind === "weekly" && (
                          <select
                            value={editor.schedule.weekday}
                            disabled={saving}
                            onChange={(event) =>
                              setEditor((current) =>
                                current
                                  ? {
                                      ...current,
                                      schedule: {
                                        ...current.schedule,
                                        weekday: Number(event.target.value),
                                      },
                                    }
                                  : current,
                              )
                            }
                          >
                            {WEEKDAY_LABELS.map((label, index) => (
                              <option key={label} value={index + 1}>
                                {label}
                              </option>
                            ))}
                          </select>
                        )}
                        <input
                          type="time"
                          value={editor.schedule.time}
                          disabled={saving}
                          onChange={(event) =>
                            setEditor((current) =>
                              current
                                ? {
                                    ...current,
                                    schedule: {
                                      ...current.schedule,
                                      time: event.target.value,
                                    },
                                  }
                                : current,
                            )
                          }
                        />
                      </>
                    )}
                  </div>
                </div>

                <label className="automation-field">
                  <span className="automation-field-label">目标项目</span>
                  <select
                    value={editor.workspaceRoot}
                    disabled={saving}
                    onChange={(event) =>
                      setEditor((current) =>
                        current
                          ? { ...current, workspaceRoot: event.target.value }
                          : current,
                      )
                    }
                  >
                    {workspaceOptions.length === 0 && (
                      <option value="">暂无可选项目</option>
                    )}
                    {workspaceOptions.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                </label>

                <label className="automation-field">
                  <span className="automation-field-label">Agent</span>
                  <select
                    value={editor.agentCli}
                    disabled={saving}
                    onChange={(event) =>
                      setEditor((current) =>
                        current
                          ? {
                              ...current,
                              agentCli: event.target.value as AgentCliId,
                              agentPreset: "",
                            }
                          : current,
                      )
                    }
                  >
                    {agentOptions.map((agent) => (
                      <option
                        key={agent.id}
                        value={agent.id}
                        disabled={!agent.installed}
                      >
                        {agent.label}
                        {!agent.installed ? "（未安装）" : ""}
                      </option>
                    ))}
                    {agentOptions.length === 0 && (
                      <option value={DEFAULT_AGENT}>DeepSeek Harness</option>
                    )}
                  </select>
                </label>

                {editor.agentCli === "deepseek-harness" && (
                  <label className="automation-field">
                    <span className="automation-field-label">Agent 预设</span>
                    <select
                      value={editor.agentPreset}
                      disabled={saving || dshPresets == null}
                      onChange={(event) =>
                        setEditor((current) =>
                          current
                            ? { ...current, agentPreset: event.target.value }
                            : current,
                        )
                      }
                    >
                      <option value="">跟随 dsh 默认</option>
                      {(dshPresets ?? []).map((preset) => (
                        <option key={preset.id} value={preset.id}>
                          {preset.label}
                        </option>
                      ))}
                    </select>
                  </label>
                )}

                <label className="automation-field">
                  <span className="automation-field-label">提示词</span>
                  <textarea
                    rows={5}
                    value={editor.prompt}
                    disabled={saving}
                    placeholder="到点后自动发送给智能体执行的提示词"
                    onChange={(event) =>
                      setEditor((current) =>
                        current
                          ? { ...current, prompt: event.target.value }
                          : current,
                      )
                    }
                  />
                </label>

                {editorError && (
                  <div className="automation-error">{editorError}</div>
                )}
              </div>

              <div className="automation-modal-actions">
                <button
                  type="button"
                  className="automation-action-btn"
                  onClick={closeEditor}
                  disabled={saving}
                >
                  取消
                </button>
                <button
                  type="button"
                  className="automation-action-btn is-primary"
                  onClick={handleSave}
                  disabled={saving}
                >
                  {saving
                    ? "保存中..."
                    : editor.editingId
                      ? "保存修改"
                      : "创建自动化"}
                </button>
              </div>
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}
