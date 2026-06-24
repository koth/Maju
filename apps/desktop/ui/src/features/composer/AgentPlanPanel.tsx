import { useEffect, useState } from "react";
import {
  Ban,
  Check,
  ChevronDown,
  Circle,
  ClipboardList,
  FileDiff,
  Gauge,
  GitBranch,
  GitCommitHorizontal,
  Laptop,
  Loader2,
  Plus,
  Server,
} from "lucide-react";
import type {
  AgentPlanEntry,
  PermissionInputQuestion,
  PermissionInputRequest,
  PermissionInputResponse,
  PermissionOption,
  SessionUsageSnapshot,
  UiSnapshot,
} from "../../types";
import MarkdownBody from "../conversation/MarkdownBody";
import "./Composer.css";

interface Props {
  entries: AgentPlanEntry[];
}

const EMPTY_USAGE_SNAPSHOT: SessionUsageSnapshot = {
  context: {},
  current_turn: {},
  session_total: {},
  by_model: [],
};

export interface AgentPlanEnvironmentInfo {
  changeCount: number;
  addedLines: number;
  removedLines: number;
  locationLabel: string;
  branchLabel: string;
  actionLabel: string;
  githubLabel: string;
  usage?: SessionUsageSnapshot;
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

const AGENT_PLAN_PANEL_ENTRY_LIMIT = 5;

export function AgentPlanPanel({ entries }: Props) {
  const entrySetKey = entries.map((entry) => entry.id ?? entry.content).join("\n");
  const [expanded, setExpanded] = useState(entries.length > 0);

  useEffect(() => {
    if (entries.length > 0) {
      setExpanded(true);
    }
  }, [entries.length, entrySetKey]);

  const completed = entries.filter((entry) => entry.status === "completed").length;
  const visibleEntries = sortAgentPlanEntries(entries);
  const panelEntries = visibleEntries.slice(0, AGENT_PLAN_PANEL_ENTRY_LIMIT);
  const hiddenEntryCount = visibleEntries.length - panelEntries.length;
  const progressPercent = entries.length > 0 ? Math.round((completed / entries.length) * 100) : 0;

  return (
    <section className={`agent-plan ${expanded ? "is-expanded" : ""}`} aria-label="进度">
      <button
        type="button"
        className="agent-plan-toggle"
        aria-expanded={expanded}
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="agent-plan-toggle-label">进度</span>
        <ChevronDown className="agent-plan-toggle-chevron" size={16} strokeWidth={2.2} aria-hidden="true" />
      </button>
      <div className="agent-plan-progress-section" hidden={!expanded}>
        {entries.length === 0 ? (
          <div className="agent-plan-empty">暂无进度</div>
        ) : (
          <>
            <div className="agent-plan-header">
              <div className="agent-plan-headline">
                <span className="agent-plan-icon" aria-hidden="true">
                  <ClipboardList size={18} strokeWidth={2.2} />
                </span>
                <div className="agent-plan-heading">
                  <div className="agent-plan-eyebrow">任务</div>
                  <div className="agent-plan-title">已完成 {completed}/{entries.length} 个任务</div>
                </div>
              </div>
              <span className="agent-plan-count" aria-label={`已完成 ${completed}/${entries.length} 个任务`}>
                {completed}/{entries.length}
              </span>
            </div>
            <div className="agent-plan-progress" aria-hidden="true">
              <span style={{ width: `${progressPercent}%` }} />
            </div>
            <ol className="agent-plan-list">
              {panelEntries.map((entry, index) => (
                <li
                  className={`agent-plan-entry is-${entry.status}`}
                  key={entry.id ?? `${index}-${entry.content}`}
                >
                  <span className="agent-plan-status" aria-label={statusLabel(entry.status)}>
                    {statusIcon(entry.status)}
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
            {hiddenEntryCount > 0 && (
              <div className="agent-plan-more">还有 {hiddenEntryCount} 个任务</div>
            )}
          </>
        )}
      </div>
    </section>
  );
}

export function AgentPlanEnvironment({
  environment,
}: {
  environment: AgentPlanEnvironmentInfo;
}) {
  const hasLineChanges = environment.addedLines > 0 || environment.removedLines > 0;
  const addedLines = environment.addedLines.toLocaleString("en-US");
  const removedLines = environment.removedLines.toLocaleString("en-US");
  const usage = environment.usage ?? EMPTY_USAGE_SNAPSHOT;
  const contextUsed = usage.context.used_tokens ?? null;
  const contextWindow = usage.context.window_tokens ?? null;
  const sessionTotal = totalUsageTokens(usage.session_total);
  const currentTurnTotal = totalUsageTokens(usage.current_turn);
  const contextOccupancyPercent = contextUsed != null && contextWindow != null && contextWindow > 0
    ? Math.max(0, Math.min(100, (contextUsed / contextWindow) * 100))
    : null;
  const usageBreakdown = [
    currentTurnTotal > 0 ? `本轮 ${formatTokenCount(currentTurnTotal)}` : null,
    sessionTotal > 0 ? `会话 ${formatTokenCount(sessionTotal)}` : null,
  ].filter(Boolean) as string[];
  const usageLabel = contextUsed != null && contextWindow != null && contextWindow > 0
    ? `${formatTokenCount(contextUsed)} / ${formatTokenCount(contextWindow)}`
    : sessionTotal > 0
      ? formatTokenCount(sessionTotal)
      : currentTurnTotal > 0
        ? formatTokenCount(currentTurnTotal)
        : null;
  const usageDetail = contextUsed != null && contextWindow != null && contextWindow > 0
    ? `上下文 ${usageLabel}`
    : sessionTotal > 0
      ? `总计 ${formatTokenCount(sessionTotal)}`
      : currentTurnTotal > 0
        ? `本轮 ${formatTokenCount(currentTurnTotal)}`
        : "等待用量";

  return (
    <div className="agent-plan-environment" aria-label="环境信息">
      <div className="agent-plan-environment-header">
        <div className="agent-plan-environment-title">环境信息</div>
        <span className="agent-plan-env-add" aria-hidden="true">
          <Plus size={17} strokeWidth={1.9} />
        </span>
      </div>
      <div className="agent-plan-env-row">
        <span className="agent-plan-env-label">
          <FileDiff size={16} strokeWidth={2.1} aria-hidden="true" />
          <span>变更</span>
        </span>
        <span className="agent-plan-env-change-metrics">
          {hasLineChanges ? (
            <>
              <span className="agent-plan-env-added">+{addedLines}</span>
              <span className="agent-plan-env-removed">-{removedLines}</span>
            </>
          ) : (
            <span className="agent-plan-env-file-count">{environment.changeCount} 处</span>
          )}
        </span>
      </div>
      <div className="agent-plan-env-row">
        <span className="agent-plan-env-label has-menu">
          {environment.locationLabel === "远程" ? (
            <Server size={16} strokeWidth={2.1} aria-hidden="true" />
          ) : (
            <Laptop size={16} strokeWidth={2.1} aria-hidden="true" />
          )}
          <span>{environment.locationLabel}</span>
          <ChevronDown className="agent-plan-env-chevron" size={14} strokeWidth={2.1} aria-hidden="true" />
        </span>
      </div>
      <div className="agent-plan-env-row">
        <span className="agent-plan-env-label has-menu">
          <GitBranch size={16} strokeWidth={2.1} aria-hidden="true" />
          <span>{environment.branchLabel}</span>
          <ChevronDown className="agent-plan-env-chevron" size={14} strokeWidth={2.1} aria-hidden="true" />
        </span>
      </div>
      <div className="agent-plan-env-row">
        <span className="agent-plan-env-label">
          <GitCommitHorizontal size={16} strokeWidth={2.1} aria-hidden="true" />
          <span>{environment.actionLabel}</span>
        </span>
      </div>
      {usageLabel && (
        <div className="agent-plan-env-usage" aria-label="用量">
          <div className="agent-plan-env-row">
            <span className="agent-plan-env-label">
              <Gauge size={16} strokeWidth={2.1} aria-hidden="true" />
              <span>用量</span>
            </span>
            <span className="agent-plan-env-usage-metrics" title={usageDetail}>
              <span>{usageLabel}</span>
            </span>
          </div>
          {contextOccupancyPercent != null && (
            <div
              className="agent-plan-env-usage-bar"
              role="meter"
              aria-label="上下文占用"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={Math.round(contextOccupancyPercent)}
            >
              <span style={{ width: `${contextOccupancyPercent}%` }} />
            </div>
          )}
          {usageBreakdown.length > 0 && (
            <div className="agent-plan-env-usage-breakdown">
              {usageBreakdown.map((item) => (
                <span key={item}>{item}</span>
              ))}
            </div>
          )}
        </div>
      )}
      <div className="agent-plan-env-row is-muted">
        <span className="agent-plan-env-label">
          <GithubMark />
          <span>{environment.githubLabel}</span>
        </span>
      </div>
    </div>
  );
}

function totalUsageTokens(tokens: SessionUsageSnapshot["session_total"]) {
  return tokens.total_tokens ?? (
    (tokens.input_tokens ?? 0) +
    (tokens.output_tokens ?? 0) +
    (tokens.cache_read_tokens ?? 0) +
    (tokens.cache_write_tokens ?? 0) +
    (tokens.reasoning_tokens ?? 0)
  );
}

function formatTokenCount(value: number) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`;
  if (value >= 10_000) return `${Math.round(value / 1_000)}k`;
  return value.toLocaleString("en-US");
}

function GithubMark() {
  return (
    <svg className="agent-plan-env-github" viewBox="0 0 16 16" aria-hidden="true" focusable="false">
      <path d="M8 .9a7.1 7.1 0 0 0-2.25 13.84c.36.07.49-.16.49-.35v-1.25c-2 .44-2.42-.86-2.42-.86-.33-.84-.8-1.06-.8-1.06-.66-.45.05-.44.05-.44.73.05 1.12.75 1.12.75.65 1.1 1.7.78 2.11.6.07-.47.25-.78.46-.96-1.59-.18-3.26-.79-3.26-3.54 0-.78.28-1.42.74-1.92-.07-.18-.32-.91.07-1.9 0 0 .6-.19 1.96.74A6.85 6.85 0 0 1 8 4.36c.6 0 1.2.08 1.76.24 1.36-.93 1.96-.74 1.96-.74.39.99.14 1.72.07 1.9.46.5.74 1.14.74 1.92 0 2.75-1.68 3.36-3.28 3.54.26.22.49.66.49 1.33v1.84c0 .19.13.42.5.35A7.1 7.1 0 0 0 8 .9Z" />
    </svg>
  );
}

export function shouldShowAgentPlanForSession(
  snapshot: Pick<UiSnapshot, "agent_plan" | "session">,
) {
  return snapshot.agent_plan.length > 0;
}

export const shouldShowAgentPlanDuringTurn = shouldShowAgentPlanForSession;
export const shouldShowAgentPlanNearComposer = shouldShowAgentPlanForSession;

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
  const planApprovalActions = request.isPlanApproval ? planApprovalActionOptions(request.options) : null;
  const actionOptions = planApprovalActions ?? request.options;
  const replanOption = request.isPlanApproval ? findPlanReplanOption(request.options) : null;
  const terminateOption = request.isPlanApproval ? findPlanTerminateOption(request.options) : null;
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
                if (request.isPlanApproval && option.id === replanOption?.id) {
                  onPermissionSelect?.(request.requestId, option.id, guidanceText || null);
                } else if (request.isPlanApproval && option.id === terminateOption?.id) {
                  onPermissionSelect?.(request.requestId, option.id);
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
  const replanOption = findPlanReplanOption(approval.options);
  const terminateOption = findPlanTerminateOption(approval.options);
  const actionOptions = planApprovalActionOptions(approval.options);
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
          {actionOptions.map((option) => (
            <button
              key={option.id}
              type="button"
              className={`plan-approval-action ${planApprovalActionClass(option)}`}
              disabled={!canAct}
              onClick={() => {
                if (option.id === replanOption?.id) {
                  onPermissionSelect?.(approval.requestId, option.id, guidance.trim() || null);
                } else if (option.id === terminateOption?.id) {
                  onPermissionSelect?.(approval.requestId, option.id);
                } else if (option.id === acceptOption?.id) {
                  onPermissionSelect?.(approval.requestId, option.id);
                }
              }}
            >
              {permissionOptionLabel(option, true)}
            </button>
          ))}
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

export function findPlanReplanOption(options: PermissionOption[]) {
  return (
    options.find((option) => option.id === "reject") ??
    options.find((option) => option.id === "deny") ??
    options.find((option) => option.id === "plan")
  );
}

export function findPlanTerminateOption(options: PermissionOption[]) {
  return (
    options.find((option) => option.id === "reject_and_exit_plan") ??
    options.find((option) => option.id === "rejectAndExitPlan")
  );
}

export function findPlanRejectOption(options: PermissionOption[]) {
  return findPlanReplanOption(options);
}

export function planApprovalActionOptions(options: PermissionOption[]) {
  const seen = new Set<string>();
  return [findPlanReplanOption(options), findPlanTerminateOption(options), findPlanAcceptOption(options)].filter(
    (option): option is PermissionOption => {
      if (!option || seen.has(option.id)) return false;
      seen.add(option.id);
      return true;
    },
  );
}

function statusMark(status: AgentPlanEntry["status"]) {
  if (status === "completed") return "完成";
  if (status === "cancelled") return "跳过";
  if (status === "in_progress") return "进行中";
  return "待处理";
}

function statusIcon(status: AgentPlanEntry["status"]) {
  if (status === "completed") return <Check size={14} strokeWidth={2.4} aria-hidden="true" />;
  if (status === "cancelled") return <Ban size={14} strokeWidth={2.2} aria-hidden="true" />;
  if (status === "in_progress") return <Loader2 size={14} strokeWidth={2.2} aria-hidden="true" />;
  return <Circle size={12} strokeWidth={2.4} aria-hidden="true" />;
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
    if (["reject_and_exit_plan", "rejectandexitplan"].includes(id)) {
      return "终止";
    }
    if (["plan", "reject", "deny"].includes(id)) {
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
  if (isPlanApproval && ["reject_and_exit_plan", "rejectandexitplan"].includes(option.id.toLowerCase())) {
    return "is-danger";
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

function planApprovalActionClass(option: PermissionOption) {
  const tone = permissionOptionTone(option, true);
  if (tone === "is-primary") return "plan-approval-action-primary";
  if (tone === "is-danger") return "plan-approval-action-danger";
  return "";
}
