import { useEffect, useState } from "react";
import type { RefObject } from "react";

/**
 * How the floating agent-plan ("环境信息") dock shares the conversation area:
 *
 *  - `none`    — the centered column does not reach the dock: show it as is.
 *  - `shift`   — the column must move left (its natural width still fits).
 *  - `squeeze` — the column must also be NARROWED to fit beside the dock
 *                (down to `minColumn`). This is what makes the dock reachable
 *                on a laptop-sized window instead of silently refusing to open.
 *  - `hidden`  — even a squeezed column has no room: the dock is not shown.
 *
 * `hidden` is the last resort. Callers are expected to free space elsewhere
 * first (the workbench auto-collapses the review panel) before accepting it.
 */
export type AgentPlanOverlapTier = "none" | "shift" | "squeeze" | "hidden";

const DOCK_WIDTH = 300;
const DOCK_GAP = 28;
const DEFAULT_PANEL_GUTTER = 14;
const DEFAULT_COLUMN_MAX = 720;
const DEFAULT_COLUMN_RATIO = 0.83333;
/** Readability floor for a squeezed conversation column. Below this the dock
 *  would leave a column too narrow to read, so it is not shown at all. */
const DEFAULT_COLUMN_MIN = 360;
/** Left band reserved for the timeline nav rail once the column is pushed left
 *  (mirrors `--timeline-nav-space`). */
const DEFAULT_NAV_SPACE = 30;

/** Everything the resolver needs, all in CSS pixels. */
export interface AgentPlanDockMetrics {
  /** Center panel width — the conversation area, excluding side panels. */
  panelWidth: number;
  gutter: number;
  columnMax: number;
  columnRatio: number;
  navSpace: number;
  dockWidth: number;
  dockGap: number;
  /** Readability floor for the squeezed column. */
  minColumn: number;
}

export interface AgentPlanDockLayout {
  tier: AgentPlanOverlapTier;
  /** Column width to apply when `tier === "squeeze"` (0 otherwise). */
  columnWidth: number;
  metrics: AgentPlanDockMetrics;
}

const IDLE_METRICS: AgentPlanDockMetrics = {
  panelWidth: 0,
  gutter: DEFAULT_PANEL_GUTTER,
  columnMax: DEFAULT_COLUMN_MAX,
  columnRatio: DEFAULT_COLUMN_RATIO,
  navSpace: DEFAULT_NAV_SPACE,
  dockWidth: DOCK_WIDTH,
  dockGap: DOCK_GAP,
  minColumn: DEFAULT_COLUMN_MIN,
};

function parseCssPx(value: string, fallback: number): number {
  const parsed = parseFloat(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function parseColumnRatio(value: string): number {
  const trimmed = value.trim();
  if (trimmed.endsWith("%")) {
    const parsed = parseFloat(trimmed);
    return Number.isFinite(parsed) ? parsed / 100 : DEFAULT_COLUMN_RATIO;
  }
  return DEFAULT_COLUMN_RATIO;
}

/**
 * Pure decision function — kept separate from the hook so the layout policy is
 * unit-testable without a DOM. Mirrors the CSS geometry:
 *  - the centered column is `min(ratio, columnMax)` wide with `margin: 0 auto`,
 *  - the dock floats in the panel's top-right corner, `dockWidth` wide plus a
 *    `dockGap` safety strip,
 *  - once pushed left the column also gives up `navSpace` on the left.
 */
export function resolveAgentPlanDockLayout(
  metrics: AgentPlanDockMetrics,
): AgentPlanDockLayout {
  const {
    panelWidth,
    gutter,
    columnMax,
    columnRatio,
    navSpace,
    dockWidth,
    dockGap,
    minColumn,
  } = metrics;
  const innerWidth = Math.max(0, panelWidth - 2 * gutter);
  if (innerWidth <= 0) {
    return { tier: "hidden", columnWidth: 0, metrics };
  }

  // The width CSS gives the column while it is centered.
  const columnMin = Math.min(innerWidth, columnMax);
  const naturalColumn = Math.min(
    columnMax,
    Math.max(Math.min(columnMin, columnMax), innerWidth * columnRatio),
  );

  // Panel-space x of the dock's safe left edge (dock minus its gap).
  const contentSafeRight = panelWidth - gutter - dockWidth - dockGap;
  const centeredColumnRight = gutter + (innerWidth + naturalColumn) / 2;
  if (centeredColumnRight <= contentSafeRight + 1) {
    return { tier: "none", columnWidth: naturalColumn, metrics };
  }

  // Space the column can occupy when pushed hard left, keeping both the dock
  // strip and the nav rail clear.
  const squeezedColumn = innerWidth - dockWidth - dockGap - navSpace;
  if (naturalColumn <= squeezedColumn + 1) {
    return { tier: "shift", columnWidth: naturalColumn, metrics };
  }
  if (squeezedColumn >= minColumn) {
    return { tier: "squeeze", columnWidth: squeezedColumn, metrics };
  }
  return { tier: "hidden", columnWidth: 0, metrics };
}

/** Read the layout metrics the resolver needs off a center-panel element. */
export function measureAgentPlanDockMetrics(
  panel: HTMLElement,
): AgentPlanDockMetrics {
  const panelStyle = getComputedStyle(panel);
  return {
    panelWidth: panel.getBoundingClientRect().width,
    gutter: parseCssPx(
      panelStyle.getPropertyValue("--center-panel-gutter"),
      DEFAULT_PANEL_GUTTER,
    ),
    columnMax: parseCssPx(
      panelStyle.getPropertyValue("--conversation-composer-max"),
      DEFAULT_COLUMN_MAX,
    ),
    columnRatio: parseColumnRatio(
      panelStyle.getPropertyValue("--conversation-column-width"),
    ),
    navSpace: parseCssPx(
      panelStyle.getPropertyValue("--timeline-nav-space"),
      DEFAULT_NAV_SPACE,
    ),
    dockWidth: DOCK_WIDTH,
    dockGap: DOCK_GAP,
    minColumn: DEFAULT_COLUMN_MIN,
  };
}

export function useAgentPlanOverlap(
  centerPanelRef: RefObject<HTMLElement | null>,
  active: boolean,
): AgentPlanDockLayout {
  const [layout, setLayout] = useState<AgentPlanDockLayout>(() => ({
    tier: "none",
    columnWidth: 0,
    metrics: IDLE_METRICS,
  }));

  useEffect(() => {
    if (!active) {
      setLayout((prev) =>
        prev.tier === "none" ? prev : { ...prev, tier: "none", columnWidth: 0 },
      );
      return;
    }
    const panel = centerPanelRef.current;
    if (!panel) {
      setLayout((prev) =>
        prev.tier === "none" ? prev : { ...prev, tier: "none", columnWidth: 0 },
      );
      return;
    }

    // Deliberately measures only the UNSHIFTED geometry (the column's own CSS
    // width, never its post-shift rect): re-measuring after the shift classes
    // apply made the tier oscillate at threshold widths.
    const measure = () => {
      const next = resolveAgentPlanDockLayout(measureAgentPlanDockMetrics(panel));
      setLayout((prev) =>
        prev.tier === next.tier &&
        Math.round(prev.columnWidth) === Math.round(next.columnWidth) &&
        Math.round(prev.metrics.panelWidth) === Math.round(next.metrics.panelWidth) &&
        Math.round(prev.metrics.columnMax) === Math.round(next.metrics.columnMax) &&
        Math.round(prev.metrics.gutter) === Math.round(next.metrics.gutter) &&
        Math.round(prev.metrics.navSpace) === Math.round(next.metrics.navSpace)
          ? prev
          : next,
      );
    };

    measure();
    const resizeObserver = new ResizeObserver(measure);
    resizeObserver.observe(panel);
    window.addEventListener("resize", measure);

    return () => {
      resizeObserver.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [active, centerPanelRef]);

  return layout;
}
