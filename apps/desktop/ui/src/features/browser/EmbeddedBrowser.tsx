/**
 * The right panel's browser, live.
 *
 * This component draws nothing but the toolbar. The page itself is a native
 * WebView2 webview the backend created as a child of the app window
 * (`browser_panel_*`), sitting in the hole this component's slot div measures:
 * rendering, scrolling, the caret and the input method are all the platform's
 * own, so a text field in the page behaves like a text field anywhere else.
 *
 * The slot is therefore a hole, not a frame — anything drawn inside it is
 * covered by the webview. Toolbar state comes from the URL the backend reports.
 *
 * See `docs/browser-view-subsystem.md`.
 */

import { memo, useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import "./EmbeddedBrowser.css";
import {
  browserPanelActivate,
  browserPanelBounds,
  browserPanelHide,
  browserPanelHistory,
  browserPanelNavigate,
  browserPanelReload,
} from "../../lib/tauri";
import { isBlankUrl } from "./browserTabs";

interface EmbeddedBrowserProps {
  /** The panel's own tab id — the label of the webview that shows the page. */
  tabId: string;
  url: string;
  title?: string;
  /** Changes whenever the panel moves, collapses or expands. */
  layoutSignal?: string;
}

export const EmbeddedBrowser = memo(function EmbeddedBrowser({
  tabId,
  url,
  title,
  layoutSignal,
}: EmbeddedBrowserProps) {
  const slotRef = useRef<HTMLDivElement | null>(null);
  const [address, setAddress] = useState(url);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** Tell the backend where the hole is. A zero-sized rect is how the panel
   *  says "I am not showing a page": the webview then hides itself. */
  const measure = useCallback(() => {
    const slot = slotRef.current;
    if (!slot) return;
    const rect = slot.getBoundingClientRect();
    void browserPanelBounds(rect.left, rect.top, rect.width, rect.height).catch(
      () => {},
    );
  }, []);

  // The webview is a native window over the app's DOM; it has to be told where
  // the hole is. Every path that can move or resize the hole reports again.
  useEffect(() => {
    const slot = slotRef.current;
    if (!slot) return;
    let frame = 0;
    const schedule = () => {
      // After the commit that moved the slot, not inside it: a rect read here
      // would describe the layout the browser has not laid out yet.
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    schedule();
    const observer = new ResizeObserver(schedule);
    observer.observe(slot);
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
      window.removeEventListener("resize", schedule);
      // The panel is showing another tab (or is gone): a native webview paints
      // above the app's UI, so it cannot simply be left behind.
      void browserPanelHide().catch(() => {});
    };
  }, [measure]);

  // The panel can come back exactly the size it left — collapsing it and
  // expanding it again changes no element's box — so the layout signal is the
  // only thing that reports the slot again. The page itself never left: the
  // backend keeps the webview, and showing it is all that is left to do.
  useEffect(() => {
    measure();
  }, [measure, layoutSignal]);

  // Which tab the browser shows is the panel's state, not this component's: a
  // remount (the review panel collapsing and coming back) must show the page
  // again. The backend keeps the page itself alive across all of this.
  useEffect(() => {
    void browserPanelActivate(tabId).catch(() => {});
  }, [tabId]);

  // A redirect, or a link clicked inside the page, moves the address bar. What
  // the user is in the middle of typing is never overwritten.
  useEffect(() => {
    if (!editing) setAddress(url);
  }, [url, editing]);

  // A tab the user opened themselves is empty and waiting for a URL: the
  // address bar is the only thing to do with it, so it starts there.
  const addressRef = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    if (!isBlankUrl(url)) return;
    addressRef.current?.focus();
    addressRef.current?.select();
  }, [url]);

  const run = useCallback((action: Promise<unknown>) => {
    void action.then(
      () => setError(null),
      (reason: unknown) => setError(messageOf(reason)),
    );
  }, []);

  const handleSubmit = useCallback(
    (event: FormEvent) => {
      event.preventDefault();
      const target = address.trim();
      if (!target) return;
      run(browserPanelNavigate(tabId, target));
      // What was typed stays on screen until the browser reports where it
      // actually landed — and if the address is refused, the error appears
      // beside the text that caused it, still there to fix.
      setEditing(false);
    },
    [address, run, tabId],
  );

  return (
    <div className="embedded-browser">
      <div className="embedded-browser-toolbar">
        <button
          type="button"
          className="embedded-browser-button"
          aria-label="后退"
          title="后退"
          onClick={() => run(browserPanelHistory(tabId, "back"))}
        >
          ←
        </button>
        <button
          type="button"
          className="embedded-browser-button"
          aria-label="前进"
          title="前进"
          onClick={() => run(browserPanelHistory(tabId, "forward"))}
        >
          →
        </button>
        <button
          type="button"
          className="embedded-browser-button"
          aria-label="重新加载"
          title="重新加载"
          onClick={() => run(browserPanelReload(tabId))}
        >
          ⟳
        </button>
        <form className="embedded-browser-address" onSubmit={handleSubmit}>
          <input
            ref={addressRef}
            className="embedded-browser-input"
            aria-label="网址"
            value={address}
            spellCheck={false}
            autoComplete="off"
            onChange={(event) => {
              setEditing(true);
              setAddress(event.target.value);
            }}
            onFocus={() => setEditing(true)}
            onBlur={() => {
              // Clicking away abandons what was typed: the page the panel is
              // showing is where the field goes back to.
              setEditing(false);
              setAddress(url);
            }}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                setAddress(url);
                event.currentTarget.blur();
              }
            }}
          />
        </form>
      </div>
      {error && (
        <div className="embedded-browser-error" role="alert">
          {error}
        </div>
      )}
      {/* The hole the native webview is moved onto. It must stay empty. */}
      <div
        ref={slotRef}
        className="embedded-browser-slot"
        data-tab-id={tabId}
        aria-label={title ? `浏览器页面 ${title}` : "浏览器页面"}
      />
    </div>
  );
});

/** The backend's error strings are already written for the user; anything else
 *  is a transport failure. */
function messageOf(reason: unknown): string {
  if (typeof reason === "string" && reason.trim()) return reason;
  if (reason instanceof Error && reason.message) return reason.message;
  return "浏览器没有响应";
}

export default EmbeddedBrowser;
