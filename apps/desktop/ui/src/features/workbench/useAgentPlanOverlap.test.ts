import { afterEach, describe, expect, it, vi } from "vitest";
import { renderHook } from "@testing-library/react";
import type { RefObject } from "react";
import { useAgentPlanOverlap } from "./useAgentPlanOverlap";

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
  it("returns 'none' when the column does not overlap a 248px dock", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 1400 },
      column: { left: 340, width: 720 },
      dock: { left: 1152, width: 248 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    expect(result.current).toBe("none");
    restore();
  });

  it("returns 'shift' when shifting the column left of a 248px dock clears it", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 1100 },
      column: { left: 190, width: 720 },
      dock: { left: 852, width: 248 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    // Column right edge is 910, dock left is 852 → overlaps at 248.
    // Shifted right edge = 14 + 720 = 734 ≤ 852 → "shift".
    expect(result.current).toBe("shift");
    restore();
  });

  it("returns 'tight' when only a 200px dock can fit beside the column", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 940 },
      column: { left: 100, width: 720 },
      dock: { left: 692, width: 248 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    expect(result.current).toBe("tight");
    restore();
  });

  it("returns 'stacked' when even a 200px dock cannot fit beside the column", () => {
    const restore = installResizeObserverMock();
    const { panel } = setLayout({
      panel: { left: 0, width: 880 },
      column: { left: 70, width: 740 },
      dock: { left: 632, width: 248 },
    });
    const ref: RefObject<HTMLElement | null> = { current: panel };
    const { result } = renderHook(() => useAgentPlanOverlap(ref, true));
    expect(result.current).toBe("stacked");
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
    expect(result.current).toBe("none");
    restore();
  });
});
