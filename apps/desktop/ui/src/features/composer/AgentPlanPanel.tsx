import type { AgentPlanEntry } from "../../types";
import "./Composer.css";

interface Props {
  entries: AgentPlanEntry[];
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
