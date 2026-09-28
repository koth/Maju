/**
 * Link routing for the right-panel browser tabs.
 *
 * Any component that renders a web link (markdown body, search results, …)
 * calls `openLinkInPanelOrExternal`; the workbench registers the handler that
 * opens the URL as a new page in the session's built-in browser. Registration
 * is module-level (the same pattern as `lib/confirm.tsx`) so deep leaves
 * neither prop-drill nor re-render when the panel is not mounted.
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

/**
 * Route a clicked link: http(s) links open as a tab in the right panel's
 * built-in browser when one can serve them, everything else (and every panel
 * failure — browser tools off, provider missing, remote workspace) falls back
 * to the system browser so a click never dead-ends.
 */
export async function openLinkInPanelOrExternal(url: string): Promise<void> {
  const trimmed = url.trim();
  if (!trimmed) return;
  if (isHttpUrl(trimmed) && opener) {
    try {
      if (await opener(trimmed)) return;
    } catch {
      // fall through to the system browser
    }
  }
  try {
    await openExternalUrl(trimmed);
  } catch {
    // Nothing else to do: no panel, no system handler.
  }
}
