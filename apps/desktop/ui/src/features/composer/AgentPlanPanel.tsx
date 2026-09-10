import { useEffect, useMemo, useState } from "react";
import {
  Ban,
  Check,
  ChevronDown,
  Circle,
  ClipboardList,
  FileDiff,
  FileWarning,
  Gauge,
  GitBranch,
  GitCommitHorizontal,
  Laptop,
  Loader2,
  MessageCircleQuestion,
  Plus,
  Server,
  ShieldAlert,
  Terminal,
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

/** How old a context-occupancy sample may be before an active turn marks it
 *  "更新中" instead of showing its last-refresh time. Agents that report
 *  occupancy live (dsh) stay well under this; codex-core, which samples at the
 *  next model response, crosses it as soon as a turn starts. */
const CONTEXT_SAMPLE_STALE_AFTER_MS = 15_000;

export interface AgentPlanEnvironmentInfo {
  changeCount: number;
  addedLines: number;
  removedLines: number;
  locationLabel: string;
  branchLabel: string;
  actionLabel: string;
  /** When true, the commit/push action is interactive. */
  actionEnabled?: boolean;
  usage?: SessionUsageSnapshot;
  /** True while a turn is active (Streaming / WaitingForTool). Used to mark
   *  the "已用上下文" figure as pending-refresh, since codex-core only emits
   *  a context-occupancy update at the next model response. */
  streaming?: boolean;
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
  onCommitAction,
}: {
  environment: AgentPlanEnvironmentInfo;
  onCommitAction?: () => void;
}) {
  const hasLineChanges = environment.addedLines > 0 || environment.removedLines > 0;
  const addedLines = environment.addedLines.toLocaleString("en-US");
  const removedLines = environment.removedLines.toLocaleString("en-US");
  const usage = environment.usage ?? EMPTY_USAGE_SNAPSHOT;
  const contextUsed = usage.context.used_tokens ?? null;
  const contextWindow = usage.context.window_tokens ?? null;
  const sessionTotal = totalUsageTokens(usage.session_total);
  const currentTurnTotal = totalUsageTokens(usage.current_turn);
  const hasContextUsage = contextUsed != null && contextUsed > 0;
  const contextOccupancyPercent = hasContextUsage && contextWindow != null && contextWindow > 0
    ? Math.max(0, Math.min(100, (contextUsed / contextWindow) * 100))
    : null;
  const usageBreakdown = [
    currentTurnTotal > 0 ? `本轮 ${formatTokenCount(currentTurnTotal)}` : null,
    sessionTotal > 0 ? `会话 ${formatTokenCount(sessionTotal)}` : null,
  ].filter(Boolean) as string[];
  const usageLabel = hasContextUsage && contextWindow != null && contextWindow > 0
    ? `${formatTokenCount(contextUsed)} / ${formatTokenCount(contextWindow)}`
    : sessionTotal > 0
      ? formatTokenCount(sessionTotal)
      : currentTurnTotal > 0
        ? formatTokenCount(currentTurnTotal)
        : null;
  const usageDetail = hasContextUsage && contextWindow != null && contextWindow > 0
    ? `上下文 ${usageLabel}`
    : sessionTotal > 0
      ? `总计 ${formatTokenCount(sessionTotal)}`
      : currentTurnTotal > 0
        ? `本轮 ${formatTokenCount(currentTurnTotal)}`
        : "等待用量";
  // Staleness cue. codex-core only samples context occupancy at the next model
  // response, while the dsh harness reports it live (a `contextPressure`
  // projection per advanced event). So a running turn alone does not mean the
  // figure is pending: mark it "更新中" only when the sample has actually gone
  // stale, and otherwise show when it last refreshed so the dock never looks
  // frozen — and never claims to be updating while it is.
  const contextUpdatedAt = usage.context.updated_at ?? null;
  const contextUpdatedMs = contextUpdatedAt != null ? Date.parse(contextUpdatedAt) : Number.NaN;
  const contextSampleAgeMs = Number.isFinite(contextUpdatedMs) ? Date.now() - contextUpdatedMs : null;
  const usagePendingRefresh =
    environment.streaming === true &&
    hasContextUsage &&
    (contextSampleAgeMs == null || contextSampleAgeMs > CONTEXT_SAMPLE_STALE_AFTER_MS);
  const stalenessLabel = usagePendingRefresh
    ? "更新中"
    : contextUpdatedAt != null
      ? `上次更新 ${formatRelativeTime(contextUpdatedAt)}`
      : null;

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
        {environment.actionEnabled && onCommitAction ? (
          <button
            type="button"
            className="agent-plan-env-action"
            onClick={onCommitAction}
          >
            <GitCommitHorizontal size={16} strokeWidth={2.1} aria-hidden="true" />
            <span>{environment.actionLabel}</span>
          </button>
        ) : (
          <span className={`agent-plan-env-label ${environment.actionEnabled ? "" : "is-disabled"}`.trim()}>
            <GitCommitHorizontal size={16} strokeWidth={2.1} aria-hidden="true" />
            <span>{environment.actionLabel}</span>
          </span>
        )}
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
              {stalenessLabel && (
                <span
                  className={
                    usagePendingRefresh
                      ? "agent-plan-env-usage-staleness is-pending"
                      : "agent-plan-env-usage-staleness"
                  }
                  aria-hidden="true"
                >
                  {stalenessLabel}
                </span>
              )}
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
    </div>
  );
}

function totalUsageTokens(tokens: SessionUsageSnapshot["session_total"]) {
  // cache_read is a subset of input and reasoning is a subset of output;
  // adding either would double-count. Keep them as display-only breakdown,
  // not in the total.
  return tokens.total_tokens ?? (
    (tokens.input_tokens ?? 0) +
    (tokens.output_tokens ?? 0)
  );
}

/// Format an ISO-8601 instant as a coarse relative label for the context-
/// occupancy staleness cue. Returns "" for unparseable input.
function formatRelativeTime(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const diffMs = Date.now() - then;
  if (diffMs < 5000) return "刚刚";
  const secs = Math.floor(diffMs / 1000);
  if (secs < 60) return `${secs} 秒前`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins} 分前`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} 小时前`;
  return `${Math.floor(hours / 24)} 天前`;
}

function formatTokenCount(value: number) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(value >= 10_000_000 ? 0 : 1)}M`;
  if (value >= 10_000) return `${Math.round(value / 1_000)}k`;
  return value.toLocaleString("en-US");
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

  const detailSummary = useMemo(
    () => summarizePermissionDetails(request?.details ?? null, request?.title ?? ""),
    [request?.details, request?.title],
  );

  if (!request) return null;

  const canAct = !!onPermissionSelect;
  const completed = entries.filter((entry) => entry.status === "completed").length;
  const showEntries = request.isPlanApproval && entries.length > 0;
  const visibleEntries = sortAgentPlanEntries(entries);
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
  const eyebrow = request.isPlanApproval
    ? "待确认计划"
    : hasInputQuestions
      ? "需要回答"
      : "需要权限";
  const title = request.isPlanApproval
    ? "接受计划后将切换到执行"
    : hasInputQuestions
      ? request.title || "智能体需要你的选择"
      : detailSummary.headline || request.title;
  const subtitle = request.isPlanApproval
    ? "审阅步骤后再决定是否进入执行阶段"
    : hasInputQuestions
      ? "回答后智能体才会继续"
      : detailSummary.subtitle;
  const Icon = request.isPlanApproval
    ? ClipboardList
    : hasInputQuestions
      ? MessageCircleQuestion
      : detailSummary.kind === "command"
        ? Terminal
        : detailSummary.kind === "path"
          ? FileWarning
          : ShieldAlert;
  const orderedActionOptions = request.isPlanApproval
    ? actionOptions
    : orderPermissionActionOptions(actionOptions);

  return (
    <section
      className={[
        "permission-request",
        request.isPlanApproval ? "is-plan-approval" : "",
        hasInputQuestions ? "is-input" : "",
        !request.isPlanApproval && !hasInputQuestions ? "is-permission" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      aria-label={request.isPlanApproval ? "待确认计划" : "权限请求"}
    >
      <div className="permission-request-header">
        <div className="permission-request-headline">
          <span className="permission-request-icon" aria-hidden="true">
            <Icon size={16} strokeWidth={2.1} />
          </span>
          <div className="permission-request-copy">
            <div className="permission-request-eyebrow">{eyebrow}</div>
            <div className="permission-request-title">{title}</div>
            {subtitle && <div className="permission-request-subtitle">{subtitle}</div>}
          </div>
        </div>
        {showEntries ? (
          <span className="permission-request-count">{completed}/{entries.length}</span>
        ) : (
          <span className="permission-request-badge">等待确认</span>
        )}
      </div>

      {!request.isPlanApproval && !hasInputQuestions && detailSummary.body && (
        <div className="permission-request-body">
          {detailSummary.meta.length > 0 && (
            <div className="permission-request-meta">
              {detailSummary.meta.map((item) => (
                <div className="permission-request-meta-item" key={`${item.label}:${item.value}`}>
                  <span className="permission-request-meta-label">{item.label}</span>
                  <code className="permission-request-meta-value" title={item.value}>
                    {item.value}
                  </code>
                </div>
              ))}
            </div>
          )}
          <pre className="permission-request-detail">{detailSummary.body}</pre>
        </div>
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
          <div className="permission-request-proposal-label">计划内容</div>
          <div className="permission-request-proposal-body">
            <MarkdownBody content={request.planText} />
          </div>
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
          {orderedActionOptions.map((option) => (
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
          <div className="plan-approval-headline">
            <span className="plan-approval-icon" aria-hidden="true">
              <ClipboardList size={18} strokeWidth={2.1} />
            </span>
            <div>
              <div className="plan-approval-eyebrow">待确认计划</div>
              <h2 className="plan-approval-title" id="plan-approval-title">
                接受计划后将切换到执行
              </h2>
              <p className="plan-approval-subtitle">审阅步骤后再决定是否进入执行阶段</p>
            </div>
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
              <label
                className={`permission-input-option${selected ? " is-selected" : ""}`}
                key={option.label}
              >
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
                <span className="permission-input-option-copy">
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
  return localizePermissionOptionLabel(option);
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

function localizePermissionOptionLabel(option: PermissionOption) {
  const id = option.id.toLowerCase();
  const label = option.label.trim();
  const normalized = label.toLowerCase();

  if (
    id === "allow_always" ||
    normalized === "always allow" ||
    normalized === "yes, don't ask again" ||
    normalized === "yes, don’t ask again" ||
    normalized.includes("don't ask again") ||
    normalized.includes("don’t ask again")
  ) {
    return "始终允许";
  }
  if (
    id === "allow" ||
    id === "allow_once" ||
    id === "approved" ||
    id === "default" ||
    normalized === "allow" ||
    normalized === "yes" ||
    normalized === "yes, proceed" ||
    normalized === "approve"
  ) {
    return "允许";
  }
  if (
    id === "reject" ||
    id === "deny" ||
    id === "denied" ||
    id === "abort" ||
    id === "cancel" ||
    normalized === "reject" ||
    normalized === "deny" ||
    normalized === "cancel" ||
    normalized === "no"
  ) {
    return "拒绝";
  }

  return label || option.id;
}

function orderPermissionActionOptions(options: PermissionOption[]) {
  const rank = (option: PermissionOption) => {
    const tone = permissionOptionTone(option, false);
    if (tone === "is-danger") return 0;
    if (tone === "is-neutral") return 1;
    if (tone === "is-always") return 2;
    if (tone === "is-primary") return 3;
    return 4;
  };

  return options
    .map((option, index) => ({ option, index }))
    .sort((left, right) => {
      const delta = rank(left.option) - rank(right.option);
      return delta !== 0 ? delta : left.index - right.index;
    })
    .map(({ option }) => option);
}

interface PermissionDetailMeta {
  label: string;
  value: string;
}

interface PermissionDetailSummary {
  kind: "command" | "path" | "generic";
  headline: string;
  subtitle: string | null;
  meta: PermissionDetailMeta[];
  body: string | null;
}

function summarizePermissionDetails(details: string | null, title: string): PermissionDetailSummary {
  const trimmedDetails = details?.trim() ?? "";
  const trimmedTitle = stripCodeFence(title).trim();
  if (!trimmedDetails) {
    return {
      kind: looksLikeCommand(trimmedTitle) ? "command" : "generic",
      headline: trimmedTitle || "选择权限",
      subtitle: looksLikeCommand(trimmedTitle) ? "智能体请求执行命令" : "智能体请求继续操作",
      meta: [],
      body: null,
    };
  }

  const lines = trimmedDetails
    .split(/\r?\n/)
    .map((line) => line.trimEnd())
    .filter((line, index, all) => !(line.trim() === "" && all[index - 1]?.trim() === ""));

  let command: string | null = null;
  let path: string | null = null;
  const residual: string[] = [];
  let capturingCommand = false;
  let capturingPaths = false;
  const commandLines: string[] = [];

  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (!line) {
      if (capturingCommand && commandLines.length > 0) {
        capturingCommand = false;
      }
      if (!capturingCommand) {
        residual.push("");
      }
      continue;
    }

    const inlineCommand = line.match(/^Command:\s*(.*)$/i)?.[1]?.trim() ?? null;
    if (inlineCommand !== null || /^Command:\s*$/i.test(line)) {
      capturingCommand = true;
      capturingPaths = false;
      if (inlineCommand) {
        commandLines.push(inlineCommand);
      }
      continue;
    }

    const inlinePath = line.match(/^Path:\s*(.+)$/i)?.[1]?.trim() ?? null;
    if (inlinePath) {
      capturingCommand = false;
      capturingPaths = false;
      path = path ?? inlinePath;
      continue;
    }

    if (/^Paths:\s*$/i.test(line)) {
      capturingCommand = false;
      capturingPaths = true;
      continue;
    }

    const listPath = line.match(/^Paths:\s*-\s*(.+)$/i)?.[1]?.trim()
      ?? (capturingPaths ? line.replace(/^-\s*/, "").trim() : null);
    if (listPath && (capturingPaths || /^Paths:/i.test(line))) {
      capturingCommand = false;
      capturingPaths = true;
      path = path ?? listPath;
      continue;
    }

    if (capturingCommand) {
      commandLines.push(rawLine);
      continue;
    }

    capturingPaths = false;
    residual.push(rawLine);
  }

  if (commandLines.length > 0) {
    command = commandLines.join("\n").trim();
  }

  if (!command && looksLikeCommand(trimmedDetails) && !/^Path:/im.test(trimmedDetails)) {
    command = trimmedDetails;
  }

  if (!path) {
    path = extractFirstPathLike(trimmedDetails);
  }

  const residualBody = residual
    .join("\n")
    .replace(/^\n+|\n+$/g, "")
    .trim();

  const body = command
    ? command
    : residualBody || (!path ? trimmedDetails : null);

  const kind = command ? "command" : path ? "path" : "generic";
  const headline =
    kind === "command"
      ? compactPermissionHeadline(command ?? trimmedTitle, 88)
      : kind === "path"
        ? compactPermissionHeadline(path ?? trimmedTitle, 72)
        : compactPermissionHeadline(trimmedTitle || body || "选择权限", 88);

  const subtitle =
    kind === "command"
      ? path
        ? "请求执行命令，并触及工作区路径"
        : "智能体请求执行命令"
      : kind === "path"
        ? "智能体请求访问文件"
        : "智能体请求继续操作";

  const meta: PermissionDetailMeta[] = [];
  if (path) {
    meta.push({ label: "路径", value: path });
  }
  if (command && body !== command) {
    meta.push({ label: "命令", value: compactPermissionHeadline(command, 96) });
  }

  return {
    kind,
    headline,
    subtitle,
    meta,
    body,
  };
}

function stripCodeFence(value: string) {
  return value.replace(/^`+|`+$/g, "");
}

function looksLikeCommand(value: string) {
  const text = stripCodeFence(value).trim();
  if (!text) return false;
  if (/\n/.test(text)) return true;
  return /^(sudo\s+)?(find|ls|cat|rm|mv|cp|chmod|chown|curl|wget|python|python3|node|npm|pnpm|yarn|cargo|git|rg|grep|sed|awk|bash|zsh|sh|powershell|pwsh)\b/i.test(
    text,
  ) || /[|><]/.test(text) || /\s-{1,2}[A-Za-z]/.test(text);
}

function extractFirstPathLike(value: string) {
  const patterns = [
    /(?:^|[\s"'`])((?:[A-Za-z]:)?(?:\/|\\)[^\s"'`]+)/,
    /(?:^|[\s"'`])((?:\.\.?\/)[^\s"'`]+)/,
  ];
  for (const pattern of patterns) {
    const match = value.match(pattern);
    if (match?.[1]) {
      return match[1].replace(/[),.;]+$/, "");
    }
  }
  return null;
}

function compactPermissionHeadline(value: string, maxLength: number) {
  const normalized = stripCodeFence(value).replace(/\s+/g, " ").trim();
  if (normalized.length <= maxLength) {
    return normalized;
  }
  return `${normalized.slice(0, Math.max(0, maxLength - 1)).trimEnd()}…`;
}
