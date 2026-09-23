import { useEffect, useState } from "react";
import { getAppliedAppTheme } from "../theme";
import type { AppTheme } from "../types";

/**
 * The theme currently applied to the document.
 *
 * Renderers we do not own — Monaco, xterm, the Shiki/pierre diff — resolve
 * their palette in JavaScript, so CSS variables cannot flip them. They read the
 * applied theme here instead of taking it as a prop: the switch lives in
 * Settings, nowhere near these trees, and the `data-theme` attribute is the one
 * place that always knows.
 */
export function useCurrentAppTheme(): AppTheme {
  const [theme, setTheme] = useState<AppTheme>(() => getAppliedAppTheme());

  useEffect(() => {
    const root = document.documentElement;
    const observer = new MutationObserver(() => setTheme(getAppliedAppTheme()));
    observer.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
    return () => observer.disconnect();
  }, []);

  return theme;
}
