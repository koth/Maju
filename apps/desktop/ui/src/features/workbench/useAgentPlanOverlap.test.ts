import { afterEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import type { RefObject } from "react";
import {
  resolveAgentPlanDockLayout,
  useAgentPlanOverlap,
  type AgentPlanDockMetrics,
} from "./useAgentPlanOverlap";

const BASE: AgentPlanDockMetrics = {
  panelWidth: 1440,
  gutter: 14,
  columnMax: 720,
  columnRatio: 0.83333,
  navSpace: 30,
  dockWidth: 300,
  dockGap: 28,
  minColumn: 360,
};

function layout(panelWidth: number, overrides: Partial<AgentPlanDockMetrics> = {}) {
  return resolveAgentPlanDockLayout({ ...BASE, panelWidth, ...overrides });
}

describe("resolveAgentPlanDockLayout", () => {
  it("keeps the column centered ('none') when the dock fits beside it", () => {
    // inner 1412 → centered column 720 ends at 1080; safe edge 1098.
    expect(layout(1440).tier).toBe("none");
  });

  it("shifts the column left ('shift') when its natural width still fits", () => {
    // inner 1092 → squeezed room 734 ≥ natural 720 → no narrowing needed.
    const result = layout(1120);
    expect(result.tier).toBe("shift");
    expect(result.columnWidth).toBe(720);
  });

  it("squeezes the column ('squeeze') instead of refusing to open on a laptop window", () => {
    // inner 1037 → squeezed room 679 < natural 720, but well above the 360 floor.
    const result = layout(1065);
    expect(result.tier).toBe("squeeze");
    expect(result.columnWidth).toBe(679);
  });

  it("squeezes down to, but never below, the readability floor", () => {
    // Squeezed room exactly at the floor stays available.
    const atFloor = layout(300 + 28 + 30 + 360 + 28);
    expect(atFloor.tier).toBe("squeeze");
    expect(atFloor.columnWidth).toBe(360);

    // One pixel narrower and the dock must not be shown at all.
    const belowFloor = layout(300 + 28 + 30 + 359 + 28);
    expect(belowFloor.tier).toBe("hidden");
  });

  it("still hides the dock when there is genuinely no room", () => {
    expect(layout(600).tier).toBe("hidden");
    expect(layout(600).columnWidth).toBe(0);
  });

  it("reports a wider tier once a side panel is collapsed (projection)", () => {
    // A hidden panel becomes squeeze/shift when 360px of review panel is freed.
    expect(layout(600).tier).toBe("hidden");
    expect(layout(600 + 360).tier).toBe("squeeze");
    expect(layout(600 + 800).tier).toBe("shift");
  });
});

interface FakeRect {
  left: number;
  right: number;
  width: number;
  top: number;
  bottom: number;
  height: number;
}

function rect(left: number, width: number, top = 0): FakeRect {
  return { left, right: left + width, width, top, bottom: top, height: 0 };
}

function installResizeObserverMock() {
  const observers: Array<() => void> = [];
  class MockRO {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  // We don't actually use the ResizeObserver for assertions; we trigger the
  // callback manually after mocking getBoundingClientRect.
  const orig = (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = MockRO;
  observers.push(() => {
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = orig;
  });
  return () => observers.forEach((fn) => fn());
}

function setLayout(opts: {
  panel: { left: number; width: number };
  column: { left: number; width: number };
  dock: { left: number; width: number };
}) {
  const panelRect = rect(opts.panel.left, opts.panel.width);
  const columnRect = rect(opts.column.left, opts.column.width);
  const dockRect = rect(opts.dock.left, opts.dock.width);
  const panel = document.createElement("div");
  panel.className = "center-panel";
  const column = document.createElement("div");
  column.className = "timeline-items";
  const dock = document.createElement("aside");
  dock.className = "agent-plan-dock";
  panel.appendChild(column);
  panel.appendChild(dock);
  document.body.appendChild(panel);
  vi.spyOn(panel, "getBoundingClientRect").mockReturnValue(panelRect as DOMRect);
  vi.spyOn(column, "getBoundingClientRect").mockReturnValue(columnRect as DOMRect);
  vi.spyOn(dock, "getBoundingClientRect").mockReturnValue(dockRect as DOMRect);
  vi.spyOn(window, "getComputedStyle").mockReturnValue({
    getPropertyValue: (name: string) => {
      if (name === "--center-panel-gutter") return "14px";
      if (name === "--conversation-composer-max") return "720px";
      if (name === "--conversation-column-width") return "83.333%";
      if (name === "--timeline-nav-space") return "30px";
      return "";
    },
  } as unknown as CSSStyleDeclaration);
  return { panel, column, dock };
}

afterEach(() => {
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});

describe("useAgentPlanOverlap", () => {
  it("returns 'none' when the column does not overlap a 300px dock", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 1440 },
      column: { left: 340, width: 720 },
      dock: { left: 1126, width: 300 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    expect(result.current.tier).toBe("none");
    restore();
  });

  it("returns 'shift' when shifting the column left of a 300px dock clears it", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 1120 },
      column: { left: 190, width: 720 },
      dock: { left: 806, width: 300 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    // Column right edge is 910, dock safe edge is 778 -> overlaps.
    // Shifted right edge = 14 + 720 = 734 <= 778 -> "shift".
    expect(result.current.tier).toBe("shift");
    restore();
  });

  it("returns 'squeeze' with the measured column width when shifting is not enough", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 1065 },
      column: { left: 165, width: 720 },
      dock: { left: 751, width: 300 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    // inner 1037, squeezed room 1037 - 300 - 28 - 30 = 679.
    expect(result.current.tier).toBe("squeeze");
    expect(result.current.columnWidth).toBe(679);
    restore();
  });

  it("returns 'hidden' only when even a squeezed column cannot fit", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 700 },
      column: { left: 0, width: 700 },
      dock: { left: 386, width: 300 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    expect(result.current.tier).toBe("hidden");
    expect(result.current.columnWidth).toBe(0);
    restore();
  });

  it("returns 'none' when the hook is inactive", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 600 },
      column: { left: 0, width: 600 },
      dock: { left: 400, width: 200 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, false));
    expect(result.current.tier).toBe("none");
    restore();
  });
});
