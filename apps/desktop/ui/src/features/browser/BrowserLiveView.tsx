/**
 * The live surface of one browser page: a screencast stream of the session's
 * built-in browser (the one the browser-use tools drive) with pointer and
 * keyboard capture forwarded back over CDP. See `docs/browser-view-subsystem.md`.
 */

import { memo, useCallback, useEffect, useRef, useState } from "react";
import { onBrowserViewFrame, onBrowserViewStatus } from "../../lib/events";
import {
  browserViewSetSize,
  browserViewInput,
  openExternalUrl,
} from "../../lib/tauri";
import type { BrowserViewInputEvent } from "../../types";
import "./BrowserLiveView.css";

export interface BrowserLiveViewProps {
  sessionId: string;
  targetId: string;
  /** Current page URL, kept fresh by the tab metadata stream. */
  url: string;
  title?: string;
}

const SIZE_SYNC_MS = 150;
const MOVE_THROTTLE_MS = 40;

function modifiersOf(event: {
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}): number {
  return (
    (event.altKey ? 1 : 0) |
    (event.ctrlKey ? 2 : 0) |
    (event.metaKey ? 4 : 0) |
    (event.shiftKey ? 8 : 0)
  );
}

function buttonName(button: number): string {
  switch (button) {
    case 1:
      return "middle";
    case 2:
      return "right";
    default:
      return "left";
  }
}

export function hostOfUrl(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

export const BrowserLiveView = memo(function BrowserLiveView({
  sessionId,
  targetId,
  url,
  title,
}: BrowserLiveViewProps) {
  const surfaceRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  const [hasFrame, setHasFrame] = useState(false);
  const [status, setStatus] = useState<"connecting" | "live" | "closed" | "failed">(
    "connecting",
  );
  const lastMoveRef = useRef(0);

  // Frames arrive as base64 JPEG and are pushed straight onto the <img>:
  // React state per frame would re-render the panel at screencast cadence.
  useEffect(() => {
    setHasFrame(false);
    setStatus("connecting");
    let cancelled = false;
    const unlisteners: Array<Promise<() => void>> = [
      onBrowserViewFrame((event) => {
        if (cancelled) return;
        if (event.session_id !== sessionId || event.target_id !== targetId) return;
        const img = imgRef.current;
        if (img) {
          img.src = `data:image/jpeg;base64,${event.frame}`;
          setHasFrame(true);
        }
      }),
      onBrowserViewStatus((event) => {
        if (cancelled || event.session_id !== sessionId) return;
        setStatus(event.status);
      }),
    ];
    return () => {
      cancelled = true;
      for (const pending of unlisteners) {
        void pending.then((unlisten) => unlisten());
      }
    };
  }, [sessionId, targetId]);

  // The page lays out for the panel slot: every size change re-syncs the
  // device metrics override (the backend restarts the screencast to match).
  useEffect(() => {
    const surface = surfaceRef.current;
    if (!surface) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    const sync = () => {
      const rect = surface.getBoundingClientRect();
      if (rect.width < 2 || rect.height < 2) return;
      void browserViewSetSize(
        sessionId,
        targetId,
        Math.round(rect.width),
        Math.round(rect.height),
      ).catch(() => {});
    };
    const schedule = () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(sync, SIZE_SYNC_MS);
    };
    sync();
    const observer = new ResizeObserver(schedule);
    observer.observe(surface);
    return () => {
      if (timer) clearTimeout(timer);
      observer.disconnect();
    };
  }, [sessionId, targetId]);

  const send = useCallback(
    (event: BrowserViewInputEvent) => {
      void browserViewInput(sessionId, targetId, event).catch(() => {});
    },
    [sessionId, targetId],
  );

  /** Pointer coordinates are mapped from the displayed image back to page
   *  CSS pixels, so a click lands where the user aimed even when the frame
   *  was scaled. */
  const pointInPage = useCallback((event: { clientX: number; clientY: number }) => {
    const surface = surfaceRef.current;
    const img = imgRef.current;
    const rect = surface?.getBoundingClientRect();
    if (!rect || rect.width === 0 || rect.height === 0) {
      return { x: 0, y: 0 };
    }
    const scaleX = img && img.naturalWidth > 0 ? img.naturalWidth / rect.width : 1;
    const scaleY = img && img.naturalHeight > 0 ? img.naturalHeight / rect.height : 1;
    return {
      x: (event.clientX - rect.left) * scaleX,
      y: (event.clientY - rect.top) * scaleY,
    };
  }, []);

  const handlePointer = useCallback(
    (type: "mousePressed" | "mouseReleased" | "mouseMoved") =>
      (event: React.PointerEvent<HTMLDivElement>) => {
        if (type === "mousePressed") {
          surfaceRef.current?.focus();
        }
        if (type === "mouseMoved") {
          const now = performance.now();
          if (now - lastMoveRef.current < MOVE_THROTTLE_MS) return;
          lastMoveRef.current = now;
        }
        const { x, y } = pointInPage(event);
        send({
          kind: "mouse",
          type,
          x,
          y,
          button: type === "mouseMoved" ? "none" : buttonName(event.button),
          click_count: type === "mouseMoved" ? 0 : Math.max(1, event.detail),
          delta_x: 0,
          delta_y: 0,
          modifiers: modifiersOf(event),
        });
      },
    [pointInPage, send],
  );

  const handleWheel = useCallback(
    (event: React.WheelEvent<HTMLDivElement>) => {
      const scale = event.deltaMode === 1 ? 40 : event.deltaMode === 2 ? 800 : 1;
      const { x, y } = pointInPage(event);
      send({
        kind: "mouse",
        type: "mouseWheel",
        x,
        y,
        button: "none",
        click_count: 0,
        delta_x: event.deltaX * scale,
        delta_y: event.deltaY * scale,
        modifiers: modifiersOf(event),
      });
    },
    [pointInPage, send],
  );

  const handleKeyDown = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      // Every key belongs to the page, including Tab and shortcuts: the
      // surface is a viewport, not a form.
      event.preventDefault();
      if (event.nativeEvent.isComposing || event.key === "Process") return;
      send({
        kind: "key",
        type: "keyDown",
        key: event.key,
        code: event.code,
        text: event.key.length === 1 ? event.key : "",
        modifiers: modifiersOf(event),
      });
    },
    [send],
  );

  const handleKeyUp = useCallback(
    (event: React.KeyboardEvent<HTMLDivElement>) => {
      event.preventDefault();
      if (event.nativeEvent.isComposing || event.key === "Process") return;
      send({
        kind: "key",
        type: "keyUp",
        key: event.key,
        code: event.code,
        text: "",
        modifiers: modifiersOf(event),
      });
    },
    [send],
  );

  const handleCompositionEnd = useCallback(
    (event: React.CompositionEvent<HTMLDivElement>) => {
      if (event.data) send({ kind: "text", text: event.data });
    },
    [send],
  );

  const display = title?.trim() || hostOfUrl(url);

  return (
    <div className="browser-live">
      <div className="browser-live-toolbar">
        <span className="browser-live-title" title={url}>
          {display}
        </span>
        <span className="browser-live-url" title={url}>
          {url}
        </span>
        <button
          type="button"
          className="browser-live-open-external"
          title="在系统浏览器中打开"
          aria-label="在系统浏览器中打开"
          onClick={() => void openExternalUrl(url).catch(() => {})}
        >
          ↗
        </button>
      </div>
      <div
        ref={surfaceRef}
        className="browser-live-surface"
        tabIndex={0}
        role="application"
        aria-label={`浏览器页面 ${display}`}
        onPointerDown={handlePointer("mousePressed")}
        onPointerUp={handlePointer("mouseReleased")}
        onPointerMove={handlePointer("mouseMoved")}
        onWheel={handleWheel}
        onKeyDown={handleKeyDown}
        onKeyUp={handleKeyUp}
        onCompositionEnd={handleCompositionEnd}
      >
        <img ref={imgRef} className="browser-live-frame" alt="" draggable={false} />
        {!hasFrame && (
          <div className="browser-live-overlay" role="status">
            {status === "failed" || status === "closed"
              ? "浏览器连接已断开"
              : "正在连接浏览器..."}
          </div>
        )}
      </div>
    </div>
  );
});
