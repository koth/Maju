import type { AutomationSchedule, AutomationScheduleKind } from "../../types";

/** ISO weekday labels indexed by `weekday - 1` (1 = Monday … 7 = Sunday). */
export const WEEKDAY_LABELS = [
  "周一",
  "周二",
  "周三",
  "周四",
  "周五",
  "周六",
  "周日",
] as const;

export const SCHEDULE_KIND_LABELS: Record<AutomationScheduleKind, string> = {
  once: "仅一次",
  interval: "每隔",
  daily: "每天",
  weekly: "每周",
};

export function formatWallClock(
  hour?: number | null,
  minute?: number | null,
): string {
  const h = Math.min(Math.max(hour ?? 0, 0), 23);
  const m = Math.min(Math.max(minute ?? 0, 0), 59);
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

/** Parse a `HH:mm` time-input value into `[hour, minute]`, or `null`. */
export function parseWallClock(value: string): [number, number] | null {
  const match = /^(\d{1,2}):(\d{2})/.exec(value.trim());
  if (!match) return null;
  const hour = Number(match[1]);
  const minute = Number(match[2]);
  if (!Number.isFinite(hour) || !Number.isFinite(minute)) return null;
  if (hour > 23 || minute > 59) return null;
  return [hour, minute];
}

/** Human-readable one-line summary of a schedule ("每天 09:30" …). */
export function describeSchedule(schedule: AutomationSchedule): string {
  const time = formatWallClock(schedule.hour, schedule.minute);
  switch (schedule.kind) {
    case "once": {
      const at = schedule.run_at_ms;
      return at != null ? `仅一次 · ${formatTimestamp(at)}` : "仅一次";
    }
    case "interval": {
      const minutes = Math.max(1, schedule.interval_minutes ?? 1);
      return minutes >= 60 && minutes % 60 === 0
        ? `每 ${minutes / 60} 小时`
        : `每 ${minutes} 分钟`;
    }
    case "daily":
      return `每天 ${time}`;
    case "weekly": {
      const weekday = Math.min(Math.max(schedule.weekday ?? 1, 1), 7);
      return `每${WEEKDAY_LABELS[weekday - 1]} ${time}`;
    }
    default:
      return "自定义计划";
  }
}

/** Display timestamp for an ISO string or epoch-millis value. */
export function formatTimestamp(
  value: string | number | null | undefined,
): string {
  if (value == null || value === "") return "—";
  const date = typeof value === "number" ? new Date(value) : new Date(value);
  if (Number.isNaN(date.getTime())) return "—";
  return date.toLocaleString("zh-CN", {
    year: "numeric",
    month: "numeric",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function runStatusLabel(status: string): string {
  switch (status) {
    case "running":
      return "执行中";
    case "completed":
      return "已完成";
    case "failed":
      return "失败";
    case "interrupted":
      return "已中断";
    default:
      return status;
  }
}

export function runTriggerLabel(trigger: string): string {
  return trigger === "manual" ? "手动" : "计划";
}

/** "下次运行" line for an automation card. */
export function nextRunLabel(
  nextRunAtMs?: number | null,
  enabled = true,
): string {
  if (!enabled) return "已暂停";
  if (nextRunAtMs == null) return "无待执行计划";
  return `下次运行 ${formatTimestamp(nextRunAtMs)}`;
}
