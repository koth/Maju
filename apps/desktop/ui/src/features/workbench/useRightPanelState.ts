import { useCallback, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";

const RIGHT_PANEL_WIDTH_STORAGE_KEY = "kodex.rightPanelWidth";
const RIGHT_PANEL_DEFAULT_VIEWPORT_RATIO = 0.28;
const RIGHT_PANEL_DEFAULT_MIN_WIDTH = 360;
const RIGHT_PANEL_DEFAULT_MAX_WIDTH = 480;
const RIGHT_PANEL_MIN_WIDTH = 360;
const RIGHT_PANEL_MAX_WIDTH = 1280;
const RIGHT_PANEL_MAX_VIEWPORT_RATIO = 0.78;
const RIGHT_PANEL_MIN_CENTER_WIDTH = 360;

function getRightPanelMaxWidth() {
  if (typeof window === "undefined") return RIGHT_PANEL_MAX_WIDTH;
  const bodyWidth =
    document.querySelector<HTMLElement>(".workbench-body")?.getBoundingClientRect().width ??
    window.innerWidth;
  const layoutMax = Math.floor(bodyWidth - RIGHT_PANEL_MIN_CENTER_WIDTH);
  const viewportMax = Math.floor(window.innerWidth * RIGHT_PANEL_MAX_VIEWPORT_RATIO);
  return Math.min(
    RIGHT_PANEL_MAX_WIDTH,
    viewportMax,
    Math.max(RIGHT_PANEL_MIN_WIDTH, layoutMax),
  );
}

function clampRightPanelWidth(width: number) {
  return Math.min(getRightPanelMaxWidth(), Math.max(RIGHT_PANEL_MIN_WIDTH, width));
}

function getRightPanelDefaultWidth() {
  if (typeof window === "undefined") return RIGHT_PANEL_DEFAULT_MIN_WIDTH;
  const target = Math.floor(window.innerWidth * RIGHT_PANEL_DEFAULT_VIEWPORT_RATIO);
  const width = Math.min(
    RIGHT_PANEL_DEFAULT_MAX_WIDTH,
    Math.max(RIGHT_PANEL_DEFAULT_MIN_WIDTH, target),
  );
  return clampRightPanelWidth(width);
}

export function useRightPanelState() {
  const [rightPanelCollapsed, setRightPanelCollapsed] = useState(false);
  const [rightPanelResizing, setRightPanelResizing] = useState(false);
  const [rightPanelWidth, setRightPanelWidth] = useState(() => {
    const stored = Number(window.localStorage.getItem(RIGHT_PANEL_WIDTH_STORAGE_KEY));
    return Number.isFinite(stored) ? clampRightPanelWidth(stored) : getRightPanelDefaultWidth();
  });

  const clampStoredRightPanelWidth = useCallback(() => {
    setRightPanelWidth((current) => {
      const next = clampRightPanelWidth(current);
      if (next !== current) {
        window.localStorage.setItem(RIGHT_PANEL_WIDTH_STORAGE_KEY, String(next));
      }
      return next;
    });
  }, []);

  const handleRightPanelResizeStart = useCallback((event: ReactPointerEvent<HTMLButtonElement>) => {
    event.preventDefault();
    const pointerId = event.pointerId;
    event.currentTarget.setPointerCapture(pointerId);
    document.body.classList.add("is-resizing-right-panel");
    setRightPanelResizing(true);

    const updateWidth = (clientX: number) => {
      const nextWidth = clampRightPanelWidth(window.innerWidth - clientX - 10);
      setRightPanelWidth(nextWidth);
      window.localStorage.setItem(RIGHT_PANEL_WIDTH_STORAGE_KEY, String(nextWidth));
    };

    const handlePointerMove = (moveEvent: PointerEvent) => {
      updateWidth(moveEvent.clientX);
    };

    const handlePointerUp = () => {
      document.body.classList.remove("is-resizing-right-panel");
      setRightPanelResizing(false);
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
      window.removeEventListener("pointercancel", handlePointerUp);
    };

    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    window.addEventListener("pointercancel", handlePointerUp);
  }, []);

  const rightPanelStyle = {
    "--right-panel-width": `${rightPanelWidth}px`,
  } as CSSProperties;

  return {
    rightPanelCollapsed,
    setRightPanelCollapsed,
    rightPanelResizing,
    rightPanelWidth,
    rightPanelStyle,
    clampStoredRightPanelWidth,
    handleRightPanelResizeStart,
  };
}
