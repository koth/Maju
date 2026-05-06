import type { AppTheme } from "./types";

export const DEFAULT_APP_THEME: AppTheme = "kodex_dark";

export interface AppThemeDefinition {
  id: AppTheme;
  label: string;
  description: string;
  swatches: string[];
}

export const APP_THEMES: AppThemeDefinition[] = [
  {
    id: "kodex_dark",
    label: "墨绿低光",
    description: "默认的低眩光石板绿配色。",
    swatches: ["#0f1419", "#151d26", "#6aa89f"],
  },
  {
    id: "midnight",
    label: "午夜蓝",
    description: "深蓝背景搭配冷蓝强调色。",
    swatches: ["#080d18", "#101a2c", "#6f96ff"],
  },
  {
    id: "graphite",
    label: "石墨灰",
    description: "中性暗灰，适合长时间阅读。",
    swatches: ["#101112", "#1c1d20", "#9aa4b2"],
  },
  {
    id: "forest",
    label: "夜林绿",
    description: "深森林绿背景和柔和苔绿色。",
    swatches: ["#07120f", "#102019", "#80b97a"],
  },
];

const THEME_IDS = new Set<AppTheme>(APP_THEMES.map((theme) => theme.id));

export function resolveAppTheme(theme: string | null | undefined): AppTheme {
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
