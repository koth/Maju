import { useEffect, useState } from "react";
import type { RefObject } from "react";

export type AgentPlanOverlapTier = "none" | "shift" | "tight" | "stacked";

const FULL_DOCK_WIDTH = 248;
const TIGHT_DOCK_WIDTH = 200;
const DEFAULT_PANEL_GUTTER = 14;
const DEFAULT_COLUMN_MAX = 720;
const DEFAULT_COLUMN_RATIO = 0.83333;
const DEFAULT_COLUMN_MIN = 640;

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

export function useAgentPlanOverlap(
  centerPanelRef: RefObject<HTMLElement | null>,
  active: boolean,
): AgentPlanOverlapTier {
  const [tier, setTier] = useState<AgentPlanOverlapTier>("none");

  useEffect(() => {
    if (!active) {
      setTier("none");
      return;
    }
    const panel = centerPanelRef.current;
    if (!panel) {
      setTier("none");
      return;
    }

    const measure = () => {
      const panelRect = panel.getBoundingClientRect();
      const panelWidth = panelRect.width;
      const panelStyle = getComputedStyle(panel);
      const panelGutter =
        parseCssPx(panelStyle.getPropertyValue("--center-panel-gutter"), DEFAULT_PANEL_GUTTER);
      const columnMax =
        parseCssPx(panelStyle.getPropertyValue("--conversation-composer-max"), DEFAULT_COLUMN_MAX);
      const columnRatio =
        parseColumnRatio(panelStyle.getPropertyValue("--conversation-column-width"));
      const innerWidth = Math.max(0, panelWidth - 2 * panelGutter);
      const columnMin = Math.min(innerWidth, DEFAULT_COLUMN_MIN);
      const columnWidth = Math.min(
        columnMax,
        Math.max(columnMin, innerWidth * columnRatio),
      );

      const centeredColumnRight = panelGutter + (innerWidth + columnWidth) / 2;
      const overlapWithFull = centeredColumnRight > panelWidth - FULL_DOCK_WIDTH + 1;
      const columnShiftedRight = panelGutter + columnWidth;
      const fullDockLeft = panelWidth - FULL_DOCK_WIDTH;
      const tightDockLeft = panelWidth - TIGHT_DOCK_WIDTH;

      let next: AgentPlanOverlapTier;
      if (!overlapWithFull) {
        next = "none";
      } else if (columnShiftedRight <= fullDockLeft + 1) {
        next = "shift";
      } else if (columnShiftedRight <= tightDockLeft + 1) {
        next = "tight";
      } else {
        next = "stacked";
      }

      if (next !== "stacked" && columnWidth < 360 && panelWidth - 2 * panelGutter >= 360) {
        next = "stacked";
      }
      setTier((prev) => (prev === next ? prev : next));
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

  return tier;
}
