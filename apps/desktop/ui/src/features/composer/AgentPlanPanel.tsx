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
    <section className="agent-plan" aria-label="Agent plan">
      <div className="agent-plan-header">
        <div>
          <div className="agent-plan-eyebrow">Mission plan</div>
          <div className="agent-plan-title">
            {active ? active.content : `${completed}/${entries.length} tasks completed`}
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
  if (status === "completed") return "Done";
  if (status === "cancelled") return "Skip";
  if (status === "in_progress") return "Now";
  return "Next";
}

function statusLabel(status: AgentPlanEntry["status"]) {
  if (status === "completed") return "Completed";
  if (status === "cancelled") return "Cancelled";
  if (status === "in_progress") return "In progress";
  return "Pending";
}

function priorityLabel(priority: AgentPlanEntry["priority"]) {
  if (priority === "high") return "High";
  if (priority === "low") return "Low";
  return "Medium";
}
