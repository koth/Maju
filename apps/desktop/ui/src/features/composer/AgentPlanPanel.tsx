import { useEffect, useState } from "react";
import type { AgentPlanEntry, PermissionOption, UiSnapshot } from "../../types";
import MarkdownBody from "../conversation/MarkdownBody";
import "./Composer.css";

interface Props {
  entries: AgentPlanEntry[];
}

export interface PlanApprovalRequest {
  requestId: string;
  planText?: string | null;
  options: PermissionOption[];
}

export interface PendingPermissionRequest {
  requestId: string;
  title: string;
  details?: string | null;
  planText?: string | null;
  options: PermissionOption[];
  isPlanApproval?: boolean;
}

interface PermissionRequestPanelProps {
  request: PendingPermissionRequest | null;
  entries: AgentPlanEntry[];
  onPermissionSelect?: (requestId: string, optionId: string | null, guidance?: string | null) => void;
}

interface PlanApprovalModalProps {
  approval: PlanApprovalRequest | null;
  entries: AgentPlanEntry[];
  onPermissionSelect?: (requestId: string, optionId: string | null, guidance?: string | null) => void;
}

export function AgentPlanPanel({ entries }: Props) {
  if (entries.length === 0) return null;

  const completed = entries.filter((entry) => entry.status === "completed").length;
  const active = entries.find((entry) => entry.status === "in_progress");
  const visibleEntries = sortAgentPlanEntries(entries);

  return (
    <section className="agent-plan" aria-label="智能体计划">
      <div className="agent-plan-header">
        <div>
          <div className="agent-plan-eyebrow">任务计划</div>
          <div className="agent-plan-title">
            {active ? active.content : `已完成 ${completed}/${entries.length} 个任务`}
          </div>
        </div>
        <span className="agent-plan-count">{completed}/{entries.length}</span>
      </div>
      <ol className="agent-plan-list">
        {visibleEntries.map((entry, index) => (
          <li
            className={`agent-plan-entry is-${entry.status}`}
            key={entry.id ?? `${index}-${entry.content}`}
          >
            <span className="agent-plan-status" aria-label={statusLabel(entry.status)}>
              {statusMark(entry.status)}
            </span>
            <span className="agent-plan-content">{entry.content}</span>
            <span
              className={`agent-plan-priority is-${entry.priority}`}
              aria-label={`Priority: ${priorityLabel(entry.priority)}`}
            >
              {priorityLabel(entry.priority)}
            </span>
          </li>
        ))}
      </ol>
    </section>
  );
}

export function shouldShowAgentPlanNearComposer(
  snapshot: Pick<UiSnapshot, "agent_plan" | "session">,
) {
  return (
    snapshot.agent_plan.length > 0 &&
    (snapshot.session.status === "Streaming" || snapshot.session.status === "WaitingForTool")
  );
}

export function PermissionRequestPanel({
  request,
  entries,
  onPermissionSelect,
}: PermissionRequestPanelProps) {
  const [guidance, setGuidance] = useState("");

  useEffect(() => {
    setGuidance("");
  }, [request?.requestId]);

  if (!request) return null;

  const canAct = !!onPermissionSelect;
  const completed = entries.filter((entry) => entry.status === "completed").length;
  const showEntries = request.isPlanApproval && entries.length > 0;
  const visibleEntries = sortAgentPlanEntries(entries);
  const details = request.details?.trim();
  const planApprovalActions = request.isPlanApproval
    ? [findPlanRejectOption(request.options), findPlanAcceptOption(request.options)].filter(
        (option): option is PermissionOption => !!option,
      )
    : null;
  const actionOptions = planApprovalActions ?? request.options;
  const showGuidance = !request.isPlanApproval && actionOptions.some(requiresPermissionGuidance);
  const guidanceText = guidance.trim();

  return (
    <section
      className={`permission-request ${request.isPlanApproval ? "is-plan-approval" : ""}`}
      aria-label={request.isPlanApproval ? "待确认计划" : "权限请求"}
    >
      <div className="permission-request-header">
        <div className="permission-request-copy">
          <div className="permission-request-eyebrow">
            {request.isPlanApproval ? "待确认计划" : "需要权限"}
          </div>
          <div className="permission-request-title">
            {request.isPlanApproval ? "接受计划后将切换到执行" : request.title}
          </div>
        </div>
        {showEntries && <span className="permission-request-count">{completed}/{entries.length}</span>}
      </div>

      {!request.isPlanApproval && details && (
        <div className="permission-request-detail">{details}</div>
      )}

      {showEntries && (
        <ol className="permission-request-plan-list">
          {visibleEntries.map((entry, index) => (
            <li
              className={`permission-request-plan-entry is-${entry.status}`}
              key={entry.id ?? `${index}-${entry.content}`}
            >
              <span className="permission-request-plan-status">{statusMark(entry.status)}</span>
              <span className="permission-request-plan-content">{entry.content}</span>
            </li>
          ))}
        </ol>
      )}

      {request.planText && (
        <div className="permission-request-proposal">
          <MarkdownBody content={request.planText} />
        </div>
      )}

      {showGuidance && (
        <label className="permission-request-guidance">
          <span>补充说明</span>
          <textarea
            value={guidance}
            onChange={(event) => setGuidance(event.target.value)}
            placeholder="告诉 Codex 应该怎么调整"
            rows={3}
          />
        </label>
      )}

      <div className="permission-request-actions">
        {actionOptions.map((option) => (
          <button
            key={option.id}
            type="button"
            className={`permission-request-action ${permissionOptionTone(option, request.isPlanApproval)}`}
            disabled={!canAct || (requiresPermissionGuidance(option) && !guidanceText)}
            onClick={() => {
              if (requiresPermissionGuidance(option)) {
                onPermissionSelect?.(request.requestId, option.id, guidanceText);
              } else {
                onPermissionSelect?.(request.requestId, option.id);
              }
            }}
          >
            {permissionOptionLabel(option, request.isPlanApproval)}
          </button>
        ))}
      </div>
    </section>
  );
}

export function PlanApprovalModal({
  approval,
  entries,
  onPermissionSelect,
}: PlanApprovalModalProps) {
  if (!approval) return null;

  const acceptOption = findPlanAcceptOption(approval.options);
  const rejectOption = findPlanRejectOption(approval.options);
  const canAct = !!onPermissionSelect;
  const completed = entries.filter((entry) => entry.status === "completed").length;
  const visibleEntries = sortAgentPlanEntries(entries);

  return (
    <div className="plan-approval-backdrop" role="presentation">
      <section
        className="plan-approval-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="plan-approval-title"
      >
        <div className="plan-approval-header">
          <div>
            <div className="plan-approval-eyebrow">待确认计划</div>
            <h2 className="plan-approval-title" id="plan-approval-title">
              接受计划后将切换到执行
            </h2>
          </div>
          <span className="plan-approval-count">{completed}/{entries.length}</span>
        </div>

        {visibleEntries.length > 0 && (
          <ol className="plan-approval-list">
            {visibleEntries.map((entry, index) => (
              <li className={`plan-approval-entry is-${entry.status}`} key={entry.id ?? `${index}-${entry.content}`}>
                <span className="plan-approval-status">{statusMark(entry.status)}</span>
                <span className="plan-approval-content">{entry.content}</span>
              </li>
            ))}
          </ol>
        )}

        {approval.planText && (
          <div className="plan-approval-proposal">
            <div className="plan-approval-proposal-title">计划内容</div>
            <div className="plan-approval-proposal-body">
              <MarkdownBody content={approval.planText} />
            </div>
          </div>
        )}

        <div className="plan-approval-actions">
          <button
            type="button"
            className="plan-approval-action"
            disabled={!canAct || !rejectOption}
            onClick={() => {
              if (rejectOption) onPermissionSelect?.(approval.requestId, rejectOption.id);
            }}
          >
            继续规划
          </button>
          <button
            type="button"
            className="plan-approval-action plan-approval-action-primary"
            disabled={!canAct || !acceptOption}
            onClick={() => {
              if (acceptOption) onPermissionSelect?.(approval.requestId, acceptOption.id);
            }}
          >
            接受计划
          </button>
        </div>
      </section>
    </div>
  );
}

export function findPlanAcceptOption(options: PermissionOption[]) {
  return (
    options.find((option) => option.id === "default") ??
    options.find((option) => option.id === "allow") ??
    options.find((option) => option.id === "allow_once") ??
    options.find((option) => option.kind.toLowerCase().includes("allow"))
  );
}

export function findPlanRejectOption(options: PermissionOption[]) {
  return (
    options.find((option) => option.id === "plan") ??
    options.find((option) => option.id === "reject") ??
    options.find((option) => option.id === "deny") ??
    options.find((option) => option.id === "reject_and_exit_plan") ??
    options.find((option) => option.id === "rejectAndExitPlan") ??
    options.find((option) => option.kind.toLowerCase().includes("reject"))
  );
}

function statusMark(status: AgentPlanEntry["status"]) {
  if (status === "completed") return "完成";
  if (status === "cancelled") return "跳过";
  if (status === "in_progress") return "进行中";
  return "待处理";
}

function statusLabel(status: AgentPlanEntry["status"]) {
  if (status === "completed") return "已完成";
  if (status === "cancelled") return "已取消";
  if (status === "in_progress") return "进行中";
  return "待处理";
}

function priorityLabel(priority: AgentPlanEntry["priority"]) {
  if (priority === "high") return "高";
  if (priority === "low") return "低";
  return "中";
}

function sortAgentPlanEntries(entries: AgentPlanEntry[]) {
  return entries
    .map((entry, index) => ({ entry, index }))
    .sort((left, right) => {
      const rank = planStatusRank(left.entry.status) - planStatusRank(right.entry.status);
      return rank !== 0 ? rank : left.index - right.index;
    })
    .map(({ entry }) => entry);
}

function planStatusRank(status: AgentPlanEntry["status"]) {
  if (status === "in_progress") return 0;
  if (status === "pending") return 1;
  if (status === "cancelled") return 2;
  return 3;
}

function permissionOptionLabel(option: PermissionOption, isPlanApproval?: boolean) {
  if (isPlanApproval) {
    const id = option.id.toLowerCase();
    if (["default", "allow", "allow_once"].includes(id)) {
      return "接受计划";
    }
    if (["plan", "reject", "deny", "reject_and_exit_plan", "rejectandexitplan"].includes(id)) {
      return "继续规划";
    }
  }
  if (requiresPermissionGuidance(option)) {
    return option.id.toLowerCase() === "timed_out" ? "超时并补充说明" : "拒绝并补充说明";
  }
  return option.label || option.id;
}

function requiresPermissionGuidance(option: PermissionOption) {
  const id = option.id.toLowerCase();
  const label = option.label.toLowerCase();
  if (id === "timed_out") return true;
  if (!["abort", "reject", "deny", "denied"].includes(id)) return false;
  return (
    label.includes("tell codex") ||
    label.includes("provide feedback") ||
    label.includes("what to do differently")
  );
}

function permissionOptionTone(option: PermissionOption, isPlanApproval?: boolean) {
  const text = `${option.id} ${option.kind} ${option.label}`.toLowerCase();
  if (isPlanApproval && ["default", "allow", "allow_once"].includes(option.id.toLowerCase())) {
    return "is-primary";
  }
  if (text.includes("always")) {
    return "is-always";
  }
  if (text.includes("allow") || text.includes("default")) {
    return "is-primary";
  }
  if (text.includes("reject") || text.includes("deny") || text.includes("plan")) {
    return "is-danger";
  }
  return "is-neutral";
}
