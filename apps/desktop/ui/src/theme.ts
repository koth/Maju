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
    description: "中性石墨灰，适合长时间阅读。",
    swatches: ["#101112", "#1c1d20", "#9aa4b2"],
  },
  {
    id: "light",
    label: "浅色",
    description: "明亮低噪的白灰界面。",
    swatches: ["#f7f7f5", "#ececea", "#5f6670"],
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
