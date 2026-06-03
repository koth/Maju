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

interface PlanApprovalModalProps {
  approval: PlanApprovalRequest | null;
  entries: AgentPlanEntry[];
  onPermissionSelect?: (requestId: string, optionId: string | null) => void;
}

export function AgentPlanPanel({ entries }: Props) {
  if (entries.length === 0) return null;

  const completed = entries.filter((entry) => entry.status === "completed").length;
  const active = entries.find((entry) => entry.status === "in_progress");

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
        {entries.map((entry, index) => (
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

        {entries.length > 0 && (
          <ol className="plan-approval-list">
            {entries.map((entry, index) => (
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
