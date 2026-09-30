/**
 * Right-panel browser tabs ↔ the panel's own browser.
 *
 * The panel hosts a native WebView2 webview per tab (`browser_panel_*`), one
 * profile shared by the whole app rather than one browser per session. The
 * backend owns the truth (the webviews it created); this hook mirrors that into
 * the review panel's open-tab list, routes link clicks into new tabs, and keeps
 * url / title fresh.
 *
 * It mirrors rather than decides, because the panel's browser is the browser the
 * agent's tools drive: a page the agent opens becomes a tab here without the UI
 * having asked for one, and a tab closed by the user is a page the agent loses.
 * See `docs/browser-view-subsystem.md`.
 */

import { useCallback, useEffect } from "react";
import type { Dispatch, SetStateAction } from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { onBrowserPanelNewWindow, onBrowserPanelTabs, onBrowserPanelUrl } from "../../lib/events";
import {
  browserPanelClose,
  browserPanelOpen,
  browserPanelState,
} from "../../lib/tauri";
import type { BrowserPanelTab } from "../../types";
import type {
  ReviewPanelActiveTab,
  ReviewPanelOpenTab,
  ReviewPanelWebTab,
} from "../review/ReviewPanel";
import { pageLabel, registerPanelOpener } from "./browserTabs";

export interface UsePanelBrowserTabsParams {
  setOpenTabs: Dispatch<SetStateAction<ReviewPanelOpenTab[]>>;
  setActiveTab: Dispatch<SetStateAction<ReviewPanelActiveTab>>;
}

/** The panel tab a review tab stands for, or `null` for any other kind. */
export function panelTabId(tab: ReviewPanelOpenTab | ReviewPanelActiveTab): string | null {
  return tab.kind === "web" ? tab.id : null;
}

/** A panel tab as the review panel's tab strip wants it: the page's own title
 *  when the browser has reported one (`on_document_title_changed` comes back
 *  through `browser_panel:tabs`), and its host until then. */
export function webTab(tab: BrowserPanelTab): ReviewPanelWebTab {
  return {
    kind: "web",
    id: tab.tab_id,
    url: tab.url,
    title: pageLabel(tab.url, tab.title),
  };
}

/** Merge the panel's tab list into the open tabs, preserving user-visible
 *  order: every existing tab keeps its slot (web tabs with url/title refreshed),
 *  a web tab whose page is gone drops out, and pages the strip has not seen yet
 *  append after everything else. Non-web tabs are never touched, and never
 *  moved — a file the user opened next to a page stays next to it. */
export function reconcilePanelTabs(
  current: ReviewPanelOpenTab[],
  tabs: BrowserPanelTab[],
): ReviewPanelOpenTab[] {
  const byId = new Map(tabs.map((tab) => [tab.tab_id, tab]));
  const next: ReviewPanelOpenTab[] = [];
  for (const tab of current) {
    if (tab.kind !== "web") {
      next.push(tab);
      continue;
    }
    const panel = byId.get(tab.id);
    if (!panel) continue;
    byId.delete(tab.id);
    // A tab mid-navigation can report an empty url; the last one it had is
    // still where the user is.
    const url = panel.url || tab.url;
    next.push({
      kind: "web",
      id: tab.id,
      url,
      title: pageLabel(url, panel.title),
    });
  }
  // What is left is a page the browser gained without the strip asking for it:
  // the agent's own first page, or a link that asked for a window of its own.
  for (const panel of byId.values()) next.push(webTab(panel));
  return next;
}

export function usePanelBrowserTabs({
  setOpenTabs,
  setActiveTab,
}: UsePanelBrowserTabsParams): {
  openPage: (url: string) => Promise<boolean>;
  closePage: (tabId: string) => void;
} {
  const openPage = useCallback(
    async (url: string): Promise<boolean> => {
      try {
        const tab = await browserPanelOpen(url);
        // The backend also emits the new tab list; this keeps the strip right
        // whether that event lands before or after this promise resolves.
        setOpenTabs((current) =>
          current.some((open) => open.kind === "web" && open.id === tab.tab_id)
            ? current
            : [...current, webTab(tab)],
        );
        const opened = webTab(tab);
        setActiveTab({
          kind: "web",
          id: opened.id,
          url: opened.url,
          title: opened.title,
        });
        return true;
      } catch {
        return false;
      }
    },
    [setOpenTabs, setActiveTab],
  );

  const closePage = useCallback(
    (tabId: string) => {
      void browserPanelClose(tabId).catch(() => {});
      setOpenTabs((current) =>
        current.filter((tab) => !(tab.kind === "web" && tab.id === tabId)),
      );
      setActiveTab((current) =>
        current.kind === "web" && current.id === tabId
          ? { kind: "base", tab: "Review" }
          : current,
      );
    },
    [setOpenTabs, setActiveTab],
  );

  // Link clicks anywhere in the app route here (module-level registration so
  // deep leaves need no props). A failure returns false and the caller reports
  // it — the system browser is never the fallback for a web link, because a
  // second browser window is what the panel exists to replace. The panel's
  // browser belongs to the app, not to a session, so this is always registered.
  useEffect(() => registerPanelOpener((url) => openPage(url)), [openPage]);

  // Whatever the panel already had up, from before this component mounted.
  useEffect(() => {
    let cancelled = false;
    void browserPanelState()
      .then((state) => {
        if (cancelled) return;
        setOpenTabs((current) => reconcilePanelTabs(current, state.tabs));
        if (state.tabs.length === 0) return;
        setActiveTab((current) => {
          if (current.kind === "web") return current;
          const active =
            state.tabs.find((tab) => tab.tab_id === state.active) ??
            state.tabs[state.tabs.length - 1];
          if (!active) return current;
          const opened = webTab(active);
          return { kind: "web", id: opened.id, url: opened.url, title: opened.title };
        });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [setOpenTabs, setActiveTab]);

  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    let cancelled = false;
    const track = (promise: Promise<UnlistenFn>) => {
      void promise.then((unlisten) => {
        if (cancelled) unlisten();
        else unlisteners.push(unlisten);
      });
    };

    track(
      onBrowserPanelTabs((event) => {
        if (cancelled) return;
        setOpenTabs((current) => reconcilePanelTabs(current, event.tabs));
        setActiveTab((current) => {
          if (current.kind !== "web") return current;
          const same = event.tabs.find((tab) => tab.tab_id === current.id);
          if (same) {
            const url = same.url || current.url;
            const title = pageLabel(url, same.title);
            return url === current.url && title === current.title
              ? current
              : { kind: "web", id: same.tab_id, url, title };
          }
          // This page is gone (its tab was closed, possibly from the panel
          // itself). Follow the browser to whatever it is showing now, and fall
          // back to the panel's own tabs when nothing is left.
          const next = event.tabs.find((tab) => tab.tab_id === event.active);
          if (!next) return { kind: "base", tab: "Review" };
          const opened = webTab(next);
          return { kind: "web", id: opened.id, url: opened.url, title: opened.title };
        });
      }),
    );

    track(
      onBrowserPanelUrl((event) => {
        if (cancelled) return;
        setOpenTabs((current) =>
          current.map((tab) =>
            tab.kind === "web" && tab.id === event.tab_id
              ? { ...tab, url: event.url, title: pageLabel(event.url) }
              : tab,
          ),
        );
        setActiveTab((current) =>
          current.kind === "web" && current.id === event.tab_id
            ? { ...current, url: event.url, title: pageLabel(event.url) }
            : current,
        );
      }),
    );

    // `target="_blank"` and `window.open` have no second window to go to: the
    // panel is the browser, so the page opens as a tab beside the one that
    // asked for it. This is also how the agent's own first page arrives.
    track(
      onBrowserPanelNewWindow((event) => {
        if (cancelled) return;
        void openPage(event.url);
      }),
    );

    return () => {
      cancelled = true;
      for (const unlisten of unlisteners) unlisten();
    };
  }, [openPage, setOpenTabs, setActiveTab]);

  return { openPage, closePage };
}
