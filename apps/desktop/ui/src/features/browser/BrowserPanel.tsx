import { memo, useCallback, useEffect, useMemo, useState } from "react";
import { onCapabilityState } from "../../lib/events";
import { browserClose, browserNavigate, browserRefresh } from "../../lib/tauri";
import type {
  BrowserSessionState,
  CapabilityStateEvent,
  WithheldReason,
} from "../../types";
import "./BrowserPanel.css";

export interface BrowserPanelProps {
  /** Current state, or null when the session has no browser. */
  state: BrowserSessionState | null;
  /** Why the tools are absent, when the capability is configured off. */
  withheldReason?: WithheldReason | null;
  /** Detail behind the withheld reason. */
  withheldDetail?: string | null;
  onClose: () => void;
}

type PanelPhase =
  | { kind: "live" }
  | { kind: "idle" }
  | { kind: "starting" }
  | { kind: "closed" }
  | { kind: "failed"; detail: string }
  | { kind: "unavailable"; reason: WithheldReason; detail: string };

/**
 * Derive what the panel should show.
 *
 * Exported for tests: the mapping from resource status and withheld reason to
 * a rendered phase is where the panel's behaviour actually lives, and it is
 * much easier to pin down here than through the DOM.
 */
export function derivePhase(
  state: BrowserSessionState | null,
  withheldReason?: WithheldReason | null,
  withheldDetail?: string | null,
): PanelPhase {
  if (withheldReason) {
    return { kind: "unavailable", reason: withheldReason, detail: withheldDetail ?? "" };
  }
  if (!state) return { kind: "idle" };
  switch (state.status) {
    case "active":
      return { kind: "live" };
    case "idle":
      return { kind: "starting" };
    case "closing":
    case "closed":
      return { kind: "closed" };
    case "failed":
      return { kind: "failed", detail: "The browser session could not be started." };
    default:
      return { kind: "idle" };
  }
}

/** Human-readable title for a withheld reason. */
export function withheldHeadline(reason: WithheldReason): string {
  switch (reason) {
    case "disabled":
      return "Browser tools are off";
    case "remote-workspace":
      return "Unavailable for remote sessions";
    case "unsupported-agent":
      return "This agent cannot use browser tools";
    case "unavailable":
      return "Browser tools are not ready";
  }
}

export const BrowserPanel = memo(function BrowserPanel({
  state,
  withheldReason = null,
  withheldDetail = null,
  onClose,
}: BrowserPanelProps) {
  const [urlDraft, setUrlDraft] = useState("");
  const [pending, setPending] = useState(false);
  const [panelState, setPanelState] = useState<BrowserSessionState | null>(state);

  // The polled snapshot and the pushed event can arrive in either order, so
  // a state that is older than what we already show is dropped rather than
  // rendered. Without this the panel flickers backwards after a fast tool
  // call.
  useEffect(() => {
    setPanelState((current) =>
      state && current && current.session_id === state.session_id && current.version > state.version
        ? current
        : state,
    );
  }, [state]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    onCapabilityState((event: CapabilityStateEvent) => {
      if (cancelled) return;
      if (event.kind === "browser") {
        setPanelState((current) =>
          current && current.version > event.state.version ? current : event.state,
        );
      } else if (event.kind === "browserClosed") {
        setPanelState((current) =>
          current && current.session_id === event.session_id ? null : current,
        );
      }
    })
      .then((dispose) => {
        if (cancelled) dispose();
        else unlisten = dispose;
      })
      .catch(() => {
        /* An unavailable event channel leaves the panel on the snapshot. */
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const phase = useMemo(
    () => derivePhase(panelState, withheldReason, withheldDetail),
    [panelState, withheldReason, withheldDetail],
  );

  const submitUrl = useCallback(
    async (raw: string) => {
      const target = raw.trim();
      if (!target || !panelState) return;
      setPending(true);
      try {
        await browserNavigate(panelState.session_id, target);
        setUrlDraft("");
      } catch {
        /* The next state event carries the failure; leave the draft for a retry. */
      } finally {
        setPending(false);
      }
    },
    [panelState],
  );

  const runAction = useCallback(
    async (action: () => Promise<unknown>) => {
      setPending(true);
      try {
        await action();
      } catch {
        /* Surfaced by the next state event. */
      } finally {
        setPending(false);
      }
    },
    [],
  );

  const currentUrl = panelState?.current_url ?? "";
  useEffect(() => {
    // Follow the agent's navigation unless the user is mid-edit, so typing a
    // URL is not overwritten by a screenshot arriving behind them.
    if (urlDraft === "") setUrlDraft(currentUrl);
  }, [currentUrl, urlDraft]);

  return (
    <section className="browser-panel" aria-label="Browser">
      <header className="browser-panel__header">
        <span className="browser-panel__title">Browser</span>
        <div className="browser-panel__url">
          <input
            aria-label="Address"
            className="browser-panel__url-input"
            disabled={phase.kind !== "live" || pending}
            onChange={(event) => setUrlDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void submitUrl(urlDraft);
            }}
            placeholder={phase.kind === "live" ? currentUrl : "No page open"}
            value={urlDraft}
          />
        </div>
        <div className="browser-panel__actions">
          <button
            aria-label="Refresh"
            disabled={phase.kind !== "live" || pending}
            onClick={() => {
              if (panelState) void runAction(() => browserRefresh(panelState.session_id));
            }}
            type="button"
          >
            ⟳
          </button>
          <button
            aria-label="Close browser"
            disabled={pending}
            onClick={() => {
              if (panelState) void runAction(() => browserClose(panelState.session_id));
            }}
            type="button"
          >
            ✕
          </button>
        </div>
      </header>

      <div className="browser-panel__surface">
        {phase.kind === "live" && panelState?.panel_rendition ? (
          <img
            alt={panelState.page_title || currentUrl || "Browser page"}
            className="browser-panel__image"
            src={`data:image/png;base64,${panelState.panel_rendition}`}
          />
        ) : null}

        {phase.kind === "live" && !panelState?.panel_rendition ? (
          <p className="browser-panel__note">
            Waiting for the first page. The agent captures a screenshot when it needs one.
          </p>
        ) : null}

        {phase.kind === "starting" ? (
          <p className="browser-panel__note">
            The browser starts when the agent first uses it.
          </p>
        ) : null}

        {phase.kind === "idle" ? (
          <p className="browser-panel__note">
            No browser yet. The agent opens one when it calls a browser tool.
          </p>
        ) : null}

        {phase.kind === "closed" ? (
          <p className="browser-panel__note">This session&apos;s browser is closed.</p>
        ) : null}

        {phase.kind === "failed" ? <p className="browser-panel__error">{phase.detail}</p> : null}

        {phase.kind === "unavailable" ? (
          <div className="browser-panel__unavailable">
            <p className="browser-panel__unavailable-headline">
              {withheldHeadline(phase.reason)}
            </p>
            {phase.detail ? <p className="browser-panel__note">{phase.detail}</p> : null}
          </div>
        ) : null}
      </div>

      <footer className="browser-panel__footer">
        {panelState?.page_title ? (
          <span className="browser-panel__page-title">{panelState.page_title}</span>
        ) : (
          <span />
        )}
        <button className="browser-panel__dismiss" onClick={onClose} type="button">
          Hide
        </button>
      </footer>
    </section>
  );
});
