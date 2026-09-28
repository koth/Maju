/**
 * Right-panel browser tabs ↔ pages of the session's built-in browser.
 *
 * The backend owns the truth (the browser's CDP page targets); this hook
 * mirrors it into the review panel's open-tab list, routes link clicks into
 * new pages, and keeps navigation metadata (url / title) fresh. See
 * `docs/browser-view-subsystem.md`.
 */

import { useCallback, useEffect, useRef } from "react";
import type { Dispatch, SetStateAction } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  onBrowserViewMeta,
  onBrowserViewTargets,
  onCapabilityState,
} from "../../lib/events";
import {
  browserViewAttach,
  browserViewClosePage,
  browserViewDetach,
  browserViewOpenPage,
} from "../../lib/tauri";
import type { BrowserViewTarget } from "../../types";
import type {
  ReviewPanelActiveTab,
  ReviewPanelOpenTab,
} from "../review/ReviewPanel";
import { registerPanelOpener } from "./browserTabs";

export interface UseBrowserViewTabsParams {
  sessionId: string;
  setOpenTabs: Dispatch<SetStateAction<ReviewPanelOpenTab[]>>;
  setActiveTab: Dispatch<SetStateAction<ReviewPanelActiveTab>>;
}

/** Merge a fresh target list into the open tabs, preserving user-visible
 *  order: existing web tabs keep their slot (url/title refreshed), vanished
 *  targets drop out, new targets append after the kept ones. Non-web tabs are
 *  never touched. */
export function reconcileWebTabs(
  current: ReviewPanelOpenTab[],
  targets: BrowserViewTarget[],
): ReviewPanelOpenTab[] {
  const others = current.filter((tab) => tab.kind !== "web");
  const byId = new Map(targets.map((target) => [target.target_id, target]));
  const kept: ReviewPanelOpenTab[] = [];
  for (const tab of current) {
    if (tab.kind !== "web") continue;
    const target = byId.get(tab.id);
    if (!target) continue;
    byId.delete(tab.id);
    kept.push({
      kind: "web",
      id: tab.id,
      url: target.url || tab.url,
      title: target.title || tab.title,
    });
  }
  for (const target of byId.values()) {
    kept.push({
      kind: "web",
      id: target.target_id,
      url: target.url,
      title: target.title,
    });
  }
  return [...others, ...kept];
}

export function useBrowserViewTabs({
  sessionId,
  setOpenTabs,
  setActiveTab,
}: UseBrowserViewTabsParams): {
  openPage: (url: string) => Promise<boolean>;
  closePage: (targetId: string) => void;
} {
  // Whether this session's browser has ever shown a page. The first page
  // appearing (a link click, or the agent starting to browse) surfaces itself
  // in the panel; later ones never steal focus from what the user is reading.
  const hadPagesRef = useRef(false);

  useEffect(() => {
    hadPagesRef.current = false;
  }, [sessionId]);

  const openPage = useCallback(
    async (url: string): Promise<boolean> => {
      try {
        const { target_id } = await browserViewOpenPage(sessionId, url);
        hadPagesRef.current = true;
        setOpenTabs((current) =>
          reconcileWebTabs(current, [{ target_id, url, title: url, type: "page" }]),
        );
        setActiveTab({ kind: "web", id: target_id, url, title: url });
        return true;
      } catch {
        return false;
      }
    },
    [sessionId, setOpenTabs, setActiveTab],
  );

  const closePage = useCallback(
    (targetId: string) => {
      void browserViewClosePage(sessionId, targetId).catch(() => {});
      setOpenTabs((current) =>
        current.filter((tab) => !(tab.kind === "web" && tab.id === targetId)),
      );
      setActiveTab((current) =>
        current.kind === "web" && current.id === targetId
          ? { kind: "base", tab: "Review" }
          : current,
      );
    },
    [sessionId, setOpenTabs, setActiveTab],
  );

  // Link clicks anywhere in the app route here (module-level registration so
  // deep leaves need no props); failure returns false and the caller falls
  // back to the system browser. Without a session there is no browser to
  // route to, so no opener is registered.
  const openPageRef = useRef(openPage);
  openPageRef.current = openPage;
  useEffect(() => {
    if (!sessionId) return;
    return registerPanelOpener((url) => openPageRef.current(url));
  }, [sessionId]);

  useEffect(() => {
    if (!sessionId) return;
    const unlisteners: UnlistenFn[] = [];
    let cancelled = false;
    const track = (promise: Promise<UnlistenFn>) => {
      void promise.then((unlisten) => {
        if (cancelled) unlisten();
        else unlisteners.push(unlisten);
      });
    };

    track(
      onBrowserViewTargets((event) => {
        if (cancelled || event.session_id !== sessionId) return;
        setOpenTabs((current) => reconcileWebTabs(current, event.targets));
        const appeared = !hadPagesRef.current && event.targets.length > 0;
        hadPagesRef.current = event.targets.length > 0;
        if (appeared) {
          const newest = event.targets[event.targets.length - 1];
          setActiveTab((current) =>
            current.kind === "web"
              ? current
              : { kind: "web", id: newest.target_id, url: newest.url, title: newest.title },
          );
        }
      }),
    );

    track(
      onBrowserViewMeta((event) => {
        if (cancelled || event.session_id !== sessionId) return;
        setOpenTabs((current) =>
          current.map((tab) =>
            tab.kind === "web" && tab.id === event.target_id
              ? {
                  ...tab,
                  url: event.url ?? tab.url,
                  title: event.title ?? tab.title,
                }
              : tab,
          ),
        );
      }),
    );

    // The agent starting to browse attaches the view even when the user has
    // no browser tab open yet — the point of the built-in browser is that the
    // tools' page is the one on the right.
    track(
      onCapabilityState((event) => {
        if (cancelled) return;
        if (event.kind === "browser" && event.state.session_id === sessionId) {
          void browserViewAttach(sessionId).catch(() => {});
        } else if (event.kind === "browserClosed" && event.session_id === sessionId) {
          setOpenTabs((current) => current.filter((tab) => tab.kind !== "web"));
          setActiveTab((current) =>
            current.kind === "web" ? { kind: "base", tab: "Review" } : current,
          );
        }
      }),
    );

    return () => {
      cancelled = true;
      for (const unlisten of unlisteners) unlisten();
    };
  }, [sessionId, setOpenTabs, setActiveTab]);

  // Detach when the panel goes away or the session switches; the browser and
  // the agent's tools are untouched by a detach.
  useEffect(() => {
    if (!sessionId) return;
    return () => {
      void browserViewDetach(sessionId).catch(() => {});
    };
  }, [sessionId]);

  return { openPage, closePage };
}
