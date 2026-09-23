import { describe, expect, it } from "vitest";
import { describeSchedule, formatTimestamp, formatWallClock, nextRunLabel, parseWallClock, runStatusLabel } from "./schedule";

describe("describeSchedule", () => {
  it("describes one-shot runs with their moment", () => {
    expect(
      describeSchedule({ kind: "once", run_at_ms: 1_767_225_000_000 }),
    ).toContain("仅一次");
    expect(describeSchedule({ kind: "once" })).toBe("仅一次");
  });

  it("describes intervals in minutes or whole hours", () => {
    expect(describeSchedule({ kind: "interval", interval_minutes: 30 })).toBe(
      "每 30 分钟",
    );
    expect(describeSchedule({ kind: "interval", interval_minutes: 120 })).toBe(
      "每 2 小时",
    );
    expect(describeSchedule({ kind: "interval", interval_minutes: 0 })).toBe(
      "每 1 分钟",
    );
  });

  it("describes daily and weekly wall clocks", () => {
    expect(describeSchedule({ kind: "daily", hour: 9, minute: 5 })).toBe(
      "每天 09:05",
    );
    expect(
      describeSchedule({ kind: "weekly", hour: 8, minute: 0, weekday: 3 }),
    ).toBe("每周三 08:00");
  });
});

describe("parseWallClock", () => {
  it("parses valid times and rejects invalid ones", () => {
    expect(parseWallClock("09:30")).toEqual([9, 30]);
    expect(parseWallClock("23:59")).toEqual([23, 59]);
    expect(parseWallClock("24:00")).toBeNull();
    expect(parseWallClock("10:60")).toBeNull();
    expect(parseWallClock("")).toBeNull();
    expect(parseWallClock("abc")).toBeNull();
  });
});

describe("formatWallClock", () => {
  it("zero-pads and clamps", () => {
    expect(formatWallClock(7, 5)).toBe("07:05");
    expect(formatWallClock(99, -1)).toBe("23:00");
    expect(formatWallClock(null, null)).toBe("00:00");
  });
});

describe("formatTimestamp", () => {
  it("renders epoch millis and ISO strings, and survives bad input", () => {
    expect(formatTimestamp(null)).toBe("—");
    expect(formatTimestamp("")).toBe("—");
    expect(formatTimestamp("not-a-date")).toBe("—");
    // 2026-01-01T00:00:00Z — assert only that a date came out (local tz
    // varies by machine).
    expect(formatTimestamp(1_767_225_600_000)).not.toBe("—");
    expect(formatTimestamp("2026-01-01T09:30:00Z")).not.toBe("—");
  });
});

describe("labels", () => {
  it("maps run statuses to Chinese labels", () => {
    expect(runStatusLabel("running")).toBe("执行中");
    expect(runStatusLabel("completed")).toBe("已完成");
    expect(runStatusLabel("failed")).toBe("失败");
    expect(runStatusLabel("interrupted")).toBe("已中断");
  });

  it("summarizes the next run", () => {
    expect(nextRunLabel(null, false)).toBe("已暂停");
    expect(nextRunLabel(null, true)).toBe("无待执行计划");
    expect(nextRunLabel(1_767_225_600_000, true)).toContain("下次运行");
  });
});
