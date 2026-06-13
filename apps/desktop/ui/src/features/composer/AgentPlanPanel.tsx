import { useEffect, useState } from "react";
import type {
  AgentPlanEntry,
  PermissionInputQuestion,
  PermissionInputRequest,
  PermissionInputResponse,
  PermissionOption,
  UiSnapshot,
} from "../../types";
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
  input?: PermissionInputRequest | null;
  isPlanApproval?: boolean;
}

interface PermissionRequestPanelProps {
  request: PendingPermissionRequest | null;
  entries: AgentPlanEntry[];
  onPermissionSelect?: (
    requestId: string,
    optionId: string | null,
    guidance?: string | null,
    inputResponse?: PermissionInputResponse | null,
  ) => void;
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
  const [inputAnswers, setInputAnswers] = useState<Record<string, string[]>>({});
  const [customAnswers, setCustomAnswers] = useState<Record<string, string>>({});

  useEffect(() => {
    setGuidance("");
    setInputAnswers({});
    setCustomAnswers({});
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
  const hasInputQuestions = !request.isPlanApproval && (request.input?.questions.length ?? 0) > 0;
  const showGuidance =
    !hasInputQuestions &&
    (request.isPlanApproval || actionOptions.some(requiresPermissionGuidance));
  const guidanceText = guidance.trim();
  const submitOption = findInputSubmitOption(request.options);
  const cancelOption = findInputCancelOption(request.options);
  const inputResponse = hasInputQuestions
    ? buildPermissionInputResponse(request.input!.questions, inputAnswers, customAnswers)
    : null;
  const inputComplete = hasInputQuestions
    ? request.input!.questions.every((question) => (inputResponse?.answers[question.id] ?? []).length > 0)
    : false;

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

      {!request.isPlanApproval && !hasInputQuestions && details && (
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
          <span>{request.isPlanApproval ? "调整要求" : "补充说明"}</span>
          <textarea
            value={guidance}
            onChange={(event) => setGuidance(event.target.value)}
            placeholder={request.isPlanApproval ? "告诉智能体应该如何调整计划" : "告诉 Codex 应该怎么调整"}
            rows={3}
          />
        </label>
      )}

      {hasInputQuestions ? (
        <>
          <div className="permission-input-form">
            {request.input!.questions.map((question) => (
              <PermissionInputQuestionField
                key={question.id}
                question={question}
                selectedAnswers={inputAnswers[question.id] ?? []}
                customAnswer={customAnswers[question.id] ?? ""}
                disabled={!canAct}
                onAnswersChange={(answers) =>
                  setInputAnswers((current) => ({ ...current, [question.id]: answers }))
                }
                onCustomAnswerChange={(answer) =>
                  setCustomAnswers((current) => ({ ...current, [question.id]: answer }))
                }
              />
            ))}
          </div>
          <div className="permission-request-actions">
            {cancelOption && (
              <button
                type="button"
                className="permission-request-action is-danger"
                disabled={!canAct}
                onClick={() => onPermissionSelect?.(request.requestId, cancelOption.id)}
              >
                取消
              </button>
            )}
            <button
              type="button"
              className="permission-request-action is-primary"
              disabled={!canAct || !submitOption || !inputComplete || !inputResponse}
              onClick={() => {
                if (submitOption && inputResponse) {
                  onPermissionSelect?.(request.requestId, submitOption.id, null, inputResponse);
                }
              }}
            >
              提交回答
            </button>
          </div>
        </>
      ) : (
        <div className="permission-request-actions">
          {actionOptions.map((option) => (
            <button
              key={option.id}
              type="button"
              className={`permission-request-action ${permissionOptionTone(option, request.isPlanApproval)}`}
              disabled={!canAct || (requiresPermissionGuidance(option) && !guidanceText)}
              onClick={() => {
                if (request.isPlanApproval && option.id === findPlanRejectOption(request.options)?.id) {
                  onPermissionSelect?.(request.requestId, option.id, guidanceText || null);
                } else if (requiresPermissionGuidance(option)) {
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
      )}
    </section>
  );
}

export function PlanApprovalModal({
  approval,
  entries,
  onPermissionSelect,
}: PlanApprovalModalProps) {
  const [guidance, setGuidance] = useState("");

  useEffect(() => {
    setGuidance("");
  }, [approval?.requestId]);

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

        <label className="permission-request-guidance">
          <span>调整要求</span>
          <textarea
            value={guidance}
            onChange={(event) => setGuidance(event.target.value)}
            placeholder="告诉智能体应该如何调整计划"
            rows={3}
          />
        </label>

        <div className="plan-approval-actions">
          <button
            type="button"
            className="plan-approval-action"
            disabled={!canAct || !rejectOption}
            onClick={() => {
              if (rejectOption) onPermissionSelect?.(approval.requestId, rejectOption.id, guidance.trim() || null);
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

interface PermissionInputQuestionFieldProps {
  question: PermissionInputQuestion;
  selectedAnswers: string[];
  customAnswer: string;
  disabled: boolean;
  onAnswersChange: (answers: string[]) => void;
  onCustomAnswerChange: (answer: string) => void;
}

function PermissionInputQuestionField({
  question,
  selectedAnswers,
  customAnswer,
  disabled,
  onAnswersChange,
  onCustomAnswerChange,
}: PermissionInputQuestionFieldProps) {
  const showCustomAnswer = question.is_other || question.options.length === 0;

  return (
    <fieldset className="permission-input-question">
      <legend>
        <span className="permission-input-header">{question.header || "Question"}</span>
        <span className="permission-input-prompt">{question.question}</span>
      </legend>

      {question.options.length > 0 && (
        <div className="permission-input-options">
          {question.options.map((option) => {
            const selected = selectedAnswers.includes(option.label);
            return (
              <label className="permission-input-option" key={option.label}>
                <input
                  type={question.multi_select ? "checkbox" : "radio"}
                  name={`permission-input-${question.id}`}
                  checked={selected}
                  disabled={disabled}
                  onChange={(event) => {
                    if (question.multi_select) {
                      const nextAnswers = event.currentTarget.checked
                        ? [...selectedAnswers, option.label]
                        : selectedAnswers.filter((answer) => answer !== option.label);
                      onAnswersChange([...new Set(nextAnswers)]);
                      return;
                    }
                    onAnswersChange([option.label]);
                  }}
                />
                <span>
                  <span className="permission-input-option-label">{option.label}</span>
                  {option.description && (
                    <span className="permission-input-option-description">{option.description}</span>
                  )}
                </span>
              </label>
            );
          })}
        </div>
      )}

      {showCustomAnswer && (
        <label className="permission-input-custom">
          <span>{question.options.length > 0 ? "其他" : "回答"}</span>
          <input
            type={question.is_secret ? "password" : "text"}
            value={customAnswer}
            disabled={disabled}
            onChange={(event) => onCustomAnswerChange(event.currentTarget.value)}
            placeholder="输入回答"
          />
        </label>
      )}
    </fieldset>
  );
}

function buildPermissionInputResponse(
  questions: PermissionInputQuestion[],
  selectedAnswers: Record<string, string[]>,
  customAnswers: Record<string, string>,
): PermissionInputResponse {
  const answers: Record<string, string[]> = {};
  for (const question of questions) {
    const values = [...(selectedAnswers[question.id] ?? [])];
    const custom = (customAnswers[question.id] ?? "").trim();
    if (custom) {
      if (question.multi_select) {
        values.push(custom);
      } else {
        values.splice(0, values.length, custom);
      }
    }
    const uniqueValues = [...new Set(values.map((value) => value.trim()).filter(Boolean))];
    if (uniqueValues.length > 0) {
      answers[question.id] = uniqueValues;
    }
  }
  return { answers };
}

function findInputSubmitOption(options: PermissionOption[]) {
  return (
    options.find((option) => option.id === "submit") ??
    options.find((option) => option.id === "approved") ??
    options.find((option) => option.id === "allow") ??
    options.find((option) => option.kind.toLowerCase().includes("allow"))
  );
}

function findInputCancelOption(options: PermissionOption[]) {
  return (
    options.find((option) => option.id === "cancel") ??
    options.find((option) => option.id === "abort") ??
    options.find((option) => option.id === "reject") ??
    options.find((option) => option.kind.toLowerCase().includes("reject"))
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
  if (isPlanApproval) {
    return "is-neutral";
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
