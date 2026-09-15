import type { AppTheme } from "./types";

export const DEFAULT_APP_THEME: AppTheme = "graphite";

export interface AppThemeDefinition {
  id: AppTheme;
  label: string;
  description: string;
  swatches: string[];
}

export const APP_THEMES: AppThemeDefinition[] = [
  {
    id: "graphite",
    label: "深色",
    description: "中性深灰三层表面 + 纯白强调，去掉一切色相装饰。",
    swatches: ["#171717", "#212121", "#ffffff"],
  },
  {
    id: "light",
    label: "浅色",
    description: "白底 + 近黑强调，同一套几何与字号。",
    swatches: ["#f9f9f9", "#ffffff", "#0d0d0d"],
  },
];

const THEME_IDS = new Set<AppTheme>(APP_THEMES.map((theme) => theme.id));
const LEGACY_DARK_THEMES = new Set(["kodex_dark", "midnight", "forest"]);

export function resolveAppTheme(theme: string | null | undefined): AppTheme {
  if (LEGACY_DARK_THEMES.has(theme ?? "")) return "graphite";
  return THEME_IDS.has(theme as AppTheme) ? (theme as AppTheme) : DEFAULT_APP_THEME;
}

export function applyAppTheme(theme: string | null | undefined): AppTheme {
  const resolved = resolveAppTheme(theme);
  document.documentElement.dataset.theme = resolved;
  return resolved;
}

export function getAppliedAppTheme(): AppTheme {
  return resolveAppTheme(document.documentElement.dataset.theme);
}
