// Handoff digest — turn the conversation into a briefing for the next agent.
//
// A handoff is not a fork: nothing is branched and no history is copied. The
// digest is what the next agent gets, so it has to carry what that agent cannot
// reconstruct from the repository: what was asked, what the previous agent did
// and decided, which files it touched, what is still open, and where to pick up.
// It is built locally from the snapshot — instant, deterministic, no model call,
// and it can never invent facts the transcript does not contain. The desktop
// dialog can hand the result to a model for a readable rewrite (`session_handoff_summary`);
// this file stays the source of truth and the fallback.

import type { UiSnapshot } from "../../types";

/// Per-section caps. A handoff must stay pasteable: a long session otherwise
/// produces a digest longer than the new agent's own context.
const MAX_REQUESTS = 8;
const REQUEST_CHARS = 260;
const MAX_EARLIER_CONCLUSIONS = 3;
const CONCLUSION_CHARS = 900;
const MAX_CHANGED_FILES = 20;
const MAX_PLAN_ITEMS = 20;
const MAX_ACTIVITY_KINDS = 6;

const KIND_LABELS: Record<string, string> = {
  read: "读取/搜索",
  execute: "运行命令",
  edit: "编辑文件",
  ask: "询问",
  permission: "权限确认",
};

function truncate(text: string, max: number): string {
  const collapsed = text.replace(/\s+/g, " ").trim();
  return collapsed.length > max ? `${collapsed.slice(0, max)}…` : collapsed;
}

function isToolFinal(tool: UiSnapshot["tools"][number]): boolean {
  return tool.status !== "Pending" && tool.status !== "Running";
}

/// Message ids up to and including the turn that contains `upToMessageId`.
///
/// The handoff button sits on a turn's final reply, so the digest covers the
/// conversation *through* that turn: handing off "here" must not leak later
/// turns the user did not mean to include.
function cutoffIndex(messages: UiSnapshot["messages"], upToMessageId?: string): number {
  if (!upToMessageId) return messages.length;
  const index = messages.findIndex((message) => message.id === upToMessageId);
  return index < 0 ? messages.length : index + 1;
}

function activitySummary(tools: UiSnapshot["tools"][number][]): string[] {
  const counts = new Map<string, number>();
  for (const tool of tools) {
    const label = KIND_LABELS[tool.kind] ?? null;
    if (!label) continue;
    counts.set(label, (counts.get(label) ?? 0) + 1);
  }
  return [...counts.entries()]
    .sort((left, right) => right[1] - left[1])
    .slice(0, MAX_ACTIVITY_KINDS)
    .map(([label, count]) => `${label} ×${count}`);
}

/// Every file the run touched, with its diff size when the harness reported one.
function changedFiles(tools: UiSnapshot["tools"][number][]): string[] {
  const stats = new Map<string, { added: number; removed: number }>();
  const ensure = (path: string) => {
    const entry = stats.get(path) ?? { added: 0, removed: 0 };
    stats.set(path, entry);
    return entry;
  };
  for (const tool of tools) {
    for (const path of tool.diff_paths) ensure(path);
    for (const preview of tool.diff_previews) {
      const entry = ensure(preview.path);
      for (const hunk of preview.hunks) {
        for (const line of hunk.lines) {
          if (line.kind === "Added") entry.added += 1;
          else if (line.kind === "Removed") entry.removed += 1;
        }
      }
    }
  }
  return [...stats.entries()]
    .slice(0, MAX_CHANGED_FILES)
    .map(([path, { added, removed }]) =>
      added || removed ? `${path} (+${added}/-${removed})` : path,
    );
}

/// The agent's own task list, split into what is done and what is not: the
/// single most useful thing for whoever takes over.
function planLines(snapshot: UiSnapshot): { done: string[]; open: string[] } {
  const done: string[] = [];
  const open: string[] = [];
  for (const entry of snapshot.agent_plan ?? []) {
    const label = truncate(entry.content ?? "", 160);
    if (!label) continue;
    // `cancelled` items are neither done nor actionable.
    if (entry.status === "completed") done.push(label);
    else if (entry.status !== "cancelled") open.push(label);
  }
  return { done: done.slice(0, MAX_PLAN_ITEMS), open: open.slice(0, MAX_PLAN_ITEMS) };
}

/// Build the handoff briefing for the conversation (or its prefix through
/// `upToMessageId`). Returns an empty string when there is nothing to hand off.
export function buildHandoffDigest(
  snapshot: UiSnapshot,
  upToMessageId?: string,
): string {
  const limit = cutoffIndex(snapshot.messages, upToMessageId);
  const messages = snapshot.messages.slice(0, limit);

  const requests = messages
    .filter((message) => message.role === "User" && !message.is_steer)
    .slice(-MAX_REQUESTS)
    .map((message) => truncate(message.body, REQUEST_CHARS))
    .filter(Boolean);

  const replies = messages.filter((message) => message.role === "Assistant");
  const conclusion = replies.length > 0 ? truncate(replies[replies.length - 1].body, CONCLUSION_CHARS) : "";
  const earlierConclusions = replies
    .slice(-1 - MAX_EARLIER_CONCLUSIONS, -1)
    .map((message) => truncate(message.body, 300))
    .filter(Boolean);

  const finished = snapshot.tools.filter(isToolFinal);
  const pending = snapshot.tools.filter((tool) => !isToolFinal(tool));
  const activity = activitySummary(finished);
  const files = changedFiles(finished);
  const plan = planLines(snapshot);

  if (requests.length === 0 && replies.length === 0) return "";

  const lines: string[] = ["# 交接说明（来自上一个会话）", ""];

  lines.push("## 会话信息");
  lines.push(`- 会话标题：${snapshot.session.title || "（无）"}`);
  lines.push(`- 工作区：${snapshot.workspace.root}`);
  if (snapshot.repository?.branch) lines.push(`- 分支：${snapshot.repository.branch}`);
  const agent = snapshot.session.agent_cli;
  const model = snapshot.session.model;
  if (agent || model) lines.push(`- 上一个智能体：${agent ?? "未知"}${model ? ` · ${model}` : ""}`);
  lines.push("");

  lines.push("## 目标 / 用户请求");
  if (requests.length === 0) {
    lines.push("- （本会话没有用户请求记录）");
  } else {
    for (const request of requests) lines.push(`- ${request}`);
  }
  lines.push("");

  lines.push("## 任务清单（上一个智能体的计划）");
  if (plan.done.length === 0 && plan.open.length === 0) {
    lines.push("- 记录中未体现");
  } else {
    for (const item of plan.done) lines.push(`- [x] ${item}`);
    for (const item of plan.open) lines.push(`- [ ] ${item}`);
  }
  lines.push("");

  lines.push("## 已完成的进展");
  if (activity.length === 0) {
    lines.push("- 尚无工具活动记录");
  } else {
    lines.push(`- 工具活动：${activity.join(" · ")}`);
  }
  if (files.length > 0) {
    lines.push("- 改动的文件：");
    for (const file of files) lines.push(`  - ${file}`);
  }
  if (conclusion) {
    lines.push("- 上一个智能体的最新结论：");
    lines.push(`  > ${conclusion}`);
  }
  if (earlierConclusions.length > 0) {
    lines.push("- 更早的关键结论（按时间顺序）：");
    for (const text of earlierConclusions) lines.push(`  > ${text}`);
  }
  lines.push("");

  lines.push("## 当前状态");
  lines.push(`- 会话状态：${snapshot.session.status}`);
  if (pending.length > 0) {
    lines.push(
      `- 有 ${pending.length} 个工具调用尚未结束：${pending
        .map((tool) => truncate(tool.summary || tool.name, 80))
        .slice(0, 5)
        .join(" · ")}`,
    );
  }
  lines.push("");

  lines.push("## 请接下来");
  lines.push(
    "- 先确认你对上述目标与进展的理解，再继续未完成的工作；如与仓库现状不符，以仓库为准并说明差异。",
  );

  return lines.join("\n");
}
