/**
 * Display name for the agent a session runs with.
 *
 * Session rows store the raw `agent_cli` string (e.g. "codex-acp",
 * "deepseek-harness"); older rows may carry a display label like "Codex".
 * Returns `null` when there is nothing to show so callers can drop the label
 * entirely instead of rendering a placeholder.
 */
export function formatAgentLabel(value?: string | null): string | null {
  const raw = value?.trim();
  if (!raw) return null;
  const normalized = raw.toLowerCase();
  if (normalized.includes("deepseek")) return "DeepSeek";
  if (normalized.includes("codebuddy")) return "CodeBuddy";
  if (normalized.includes("claude")) return "Claude";
  if (normalized.includes("codex")) return "Codex";
  if (normalized.includes("goose")) return "goose";
  return raw;
}
