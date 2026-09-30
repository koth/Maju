/**
 * Link routing for the right-panel browser tabs.
 *
 * Any component that renders a web link (markdown body, search results, …)
 * calls `openLinkInPanelOrExternal`; the workbench registers the handler that
 * opens the URL as a new page in the session's built-in browser. Registration
 * is module-level (the same pattern as `lib/confirm.tsx`) so deep leaves
 * neither prop-drill nor re-render when the panel is not mounted.
 *
 * A web link is never handed to the system browser from in here. The panel *is*
 * the destination for `http(s)`, and a second browser window next to it is
 * exactly what the panel exists to replace — so a panel that is missing,
 * declines, or fails is reported instead of papered over with an external
 * window. Only a scheme the panel cannot serve at all (`mailto:`, `tel:`, …)
 * goes to the operating system.
 */

import { openExternalUrl } from "../../lib/tauri";

/** Opens `url` as a browser page in the right panel. Resolves true when the
 *  panel handled it, false when the caller should fall back. */
export type PanelOpener = (url: string) => Promise<boolean>;

let opener: PanelOpener | null = null;

export function registerPanelOpener(next: PanelOpener | null): () => void {
  if (opener === next) return () => {};
  opener = next;
  return () => {
    if (opener === next) opener = null;
  };
}

/** True when a panel opener is registered (used by tests). */
export function hasPanelOpener(): boolean {
  return opener != null;
}

function isHttpUrl(url: string): boolean {
  return /^https?:\/\//i.test(url.trim());
}

/** The host of a URL, for naming a page whose own title is not available. */
export function hostOf(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

/** Whether a page is the empty one the panel opens for the user to type into,
 *  rather than a page anyone asked for. */
export function isBlankUrl(url: string): boolean {
  const trimmed = url.trim();
  return trimmed === "about:blank" || trimmed.startsWith("about:blank#");
}

/** What a page is called in the panel's tab strip.
 *
 * A webview reports the address it arrived at, never a `<title>`, so the host
 * is the label — and the pages the panel opens itself get a name instead of
 * `about:blank`. */
export function pageLabel(url: string, title?: string): string {
  if (isBlankUrl(url)) return "新标签页";
  return title?.trim() || hostOf(url);
}

/**
 * Prose punctuation that follows a link in a sentence, never part of it.
 *
 * Written as escapes so the full-width characters can never be confused with
 * their ASCII lookalikes — an ASCII `)` in a URL is real punctuation more often
 * than not, and is handled separately below.
 */
const TRAILING_PROSE =
  /(?:%E3%80%82|%EF%BC%8C|%EF%BC%8E|%E3%80%81|%EF%BC%9B|%EF%BC%9A|%EF%BC%81|%EF%BC%9F|%E2%80%A6|[\uFF09\u3002\uFF0C\u3001\uFF1B\uFF1A\uFF01\uFF1F\u2026\u00B7\u201C\u201D\u2018\u2019\u300D\u300F\u3011\u300B\u3009])+$/iu;

/** A parenthetical group the autolink annexed together with the URL. */
const TRAILING_GROUP = /(?:%EF%BC%88|\uFF08)[^\uFF08\uFF09()]*(?:%EF%BC%89|\uFF09)$/iu;

/**
 * Drop the prose that a linkified URL swallowed.
 *
 * A bare URL inside a sentence is autolinked together with whatever follows it
 * — `https://www.baidu.com/（页面标题：百度一下，你就知道）` is one autolink, not a
 * URL whose path contains a Chinese parenthetical — and the panel then loads a
 * 404 instead of the link the author meant. The markdown pipeline percent-encodes
 * the non-ASCII characters, so both spellings of the same punctuation are
 * matched. Encoded (`%EF%BC%88 … %EF%BC%89`) or balanced (`…/Foo_(bar)`)
 * punctuation that belongs to a real URL is left alone.
 */
export function trimLinkPunctuation(url: string): string {
  let trimmed = url.trim();
  for (;;) {
    const next = trimmed.replace(TRAILING_GROUP, "").replace(TRAILING_PROSE, "");
    if (next === trimmed) break;
    trimmed = next;
  }
  const closers = (value: string, char: string) => value.split(char).length - 1;
  while (trimmed.endsWith(")") && closers(trimmed, ")") > closers(trimmed, "(")) {
    trimmed = trimmed.slice(0, -1);
  }
  return trimmed.trim();
}

/**
 * Route a clicked link to the right panel's built-in browser.
 *
 * `console.error` is the whole failure surface on purpose: the user is looking
 * at the panel, and a hidden system-browser window would both hide the failure
 * and put a second browser on screen.
 */
export async function openLinkInPanelOrExternal(url: string): Promise<void> {
  const trimmed = trimLinkPunctuation(url);
  if (!trimmed) return;
  if (isHttpUrl(trimmed)) {
    if (!opener) {
      console.error("no browser panel is available for the link", trimmed);
      return;
    }
    try {
      if (!(await opener(trimmed))) {
        console.error("the browser panel declined the link", trimmed);
      }
    } catch (error) {
      console.error("the browser panel could not open the link", trimmed, error);
    }
    return;
  }
  try {
    await openExternalUrl(trimmed, "link-scheme");
  } catch {
    // Nothing else to do: no panel can serve the scheme, no system handler.
  }
}
