import { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import type { SessionJobRecord } from "../../types";
import { sessionListBackgroundJobs } from "../../lib/tauri";
import "./Composer.css";

/** Management action for one background job. Both are executed by the current
 *  session's agent (the harness only exposes `job_kill` / `job_output` as
 *  agent tools — there is no external kill RPC). */
export type BackgroundJobAction = "kill" | "output";

interface Props {
  /** Visible session id — polling restarts when the conversation switches. */
  sessionId: string;
  onJobAction?: (job: SessionJobRecord, action: BackgroundJobAction) => void;
}

const POLL_INTERVAL_MS = 4000;

/**
 * "后台任务" section of the conversation's context dock: the dsh harness
 * background jobs owned by the visible session. Rendered below "进度", and
 * only while the session actually has jobs.
 */
export function BackgroundJobsPanel({ sessionId, onJobAction }: Props) {
  const [jobs, setJobs] = useState<SessionJobRecord[]>([]);
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    // Reset immediately on a conversation switch so the previous session's
    // jobs never linger during the first poll of the new one.
    setJobs([]);
    setExpanded(false);
    let disposed = false;
    const refresh = () => {
      sessionListBackgroundJobs()
        .then((list) => {
          if (!disposed) setJobs(list);
        })
        .catch(() => {});
    };
    refresh();
    const timer = window.setInterval(refresh, POLL_INTERVAL_MS);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [sessionId]);

  if (jobs.length === 0) return null;

  const runningCount = jobs.filter(
    (job) => job.status === "running" || job.status === "stopping",
  ).length;

  return (
    <section
      className={`agent-plan agent-jobs ${expanded ? "is-expanded" : ""}`}
      aria-label="后台任务"
    >
      <button
        type="button"
        className="agent-plan-toggle"
        aria-expanded={expanded}
        onClick={() => setExpanded((current) => !current)}
      >
        <span className="agent-plan-toggle-label">
          后台任务
          <span className="agent-jobs-count">
            {runningCount > 0
              ? `${runningCount} 进行中 / ${jobs.length}`
              : `${jobs.length} 个`}
          </span>
        </span>
        <ChevronDown
          className="agent-plan-toggle-chevron"
          size={16}
          strokeWidth={2.2}
          aria-hidden="true"
        />
      </button>
      <div className="agent-jobs-list" hidden={!expanded}>
        {jobs.map((job) => (
          <div key={job.id} className={`agent-job-row is-${job.status}`}>
            <div className="agent-job-head">
              <span className="agent-job-label" title={job.label}>
                {job.label}
              </span>
              <span className={`agent-job-status is-${job.status}`}>
                {jobStatusLabel(job.status)}
              </span>
            </div>
            <div className="agent-job-meta">
              <span className="agent-job-kind">{job.kind || "任务"}</span>
              <span>{jobTimeText(job)}</span>
            </div>
            {job.detail && (
              <div className="agent-job-detail" title={job.detail}>
                {job.detail}
              </div>
            )}
            {onJobAction && (
              <div className="agent-job-actions">
                {job.status === "running" && (
                  <button
                    type="button"
                    onClick={() => onJobAction(job, "kill")}
                    title="让当前会话的 Agent 调用 job_kill 终止该任务"
                  >
                    终止
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => onJobAction(job, "output")}
                  title="让当前会话的 Agent 调用 job_output 查看该任务的最近输出"
                >
                  查看输出
                </button>
              </div>
            )}
          </div>
        ))}
        {onJobAction && (
          <div className="agent-jobs-note">
            终止 / 查看输出由当前会话的 Agent 执行（job_kill / job_output）
          </div>
        )}
      </div>
    </section>
  );
}

function jobStatusLabel(status: string): string {
  switch (status) {
    case "running":
      return "运行中";
    case "stopping":
      return "停止中";
    case "completed":
      return "已完成";
    case "killed":
      return "已终止";
    case "failed":
      return "失败";
    default:
      return status;
  }
}

function jobTimeText(job: SessionJobRecord): string {
  const start =
    job.startedAt > 0
      ? new Date(job.startedAt).toLocaleTimeString("zh-CN", {
          hour: "2-digit",
          minute: "2-digit",
        })
      : "";
  if (job.finishedAt != null && job.finishedAt > job.startedAt) {
    const seconds = Math.round((job.finishedAt - job.startedAt) / 1000);
    const elapsed = `用时 ${formatDuration(seconds)}`;
    return start ? `${start} · ${elapsed}` : elapsed;
  }
  return start ? `${start} 起` : "";
}

function formatDuration(seconds: number): string {
  if (seconds < 60) return `${seconds} 秒`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} 分钟`;
  return `${Math.floor(minutes / 60)} 小时 ${minutes % 60} 分`;
}
