import { SessionConfigOption, SessionModelState, SessionModeState } from "@agentclientprotocol/sdk";
import { ModelInfo, ModelUsage, Query } from "@anthropic-ai/claude-agent-sdk";
import { SettingsManager } from "./settings.js";

type Logger = {
  error: (...args: any[]) => void;
};

export const KODEX_MODEL_PROVIDER_MAP_ENV = "KODEX_MODEL_PROVIDER_MAP";
const KODEX_PROVIDER_VALUE_PREFIX = "kodex-provider:";

type KodexModelProviderEntry = {
  model: string;
  displayName: string;
  provider: string;
};

export type KodexModelInfo = ModelInfo & {
  kodexProvider?: string;
  kodexRouteModel?: string;
};

export function buildConfigOptions(
  modes: SessionModeState,
  models: SessionModelState,
  modelInfos: ModelInfo[],
  currentEffortLevel?: string,
): SessionConfigOption[] {
  const options: SessionConfigOption[] = [
    {
      id: "mode",
      name: "Mode",
      description: "Session permission mode",
      category: "mode",
      type: "select",
      currentValue: modes.currentModeId,
      options: modes.availableModes.map((m) => ({
        value: m.id,
        name: m.name,
        description: m.description,
      })),
    },
    {
      id: "model",
      name: "Model",
      description: "AI model to use",
      category: "model",
      type: "select",
      currentValue: models.currentModelId,
      options: models.availableModels.map((m) => modelConfigOption(m)),
    },
  ];

  // Add effort level option based on the currently selected model
  const currentModelInfo = modelInfos.find((m) => m.value === models.currentModelId);
  const supportedLevels = currentModelInfo?.supportsEffort
    ? (currentModelInfo.supportedEffortLevels ?? [])
    : [];

  if (supportedLevels.length > 0) {
    const effortOptions = supportedLevels.map((level) => ({
      value: level,
      name: level
        .split(/[_-]/)
        .map((part) => (part ? part.charAt(0).toUpperCase() + part.slice(1) : part))
        .join(" "),
    }));

    // Keep the current level if valid, otherwise prefer xhigh (Claude Code's
    // recommended default for capable models), then high (the API default).
    const includes = (l: string) => (supportedLevels as string[]).includes(l);
    const validEffort =
      currentEffortLevel && includes(currentEffortLevel)
        ? currentEffortLevel
        : includes("xhigh")
          ? "xhigh"
          : includes("high")
            ? "high"
            : supportedLevels[0];

    options.push({
      id: "effort",
      name: "Effort",
      description: "Available effort levels for this model",
      category: "thought_level",
      type: "select",
      currentValue: validEffort,
      options: effortOptions,
    });
  }

  return options;
}

// Claude Code CLI persists display strings like "opus[1m]" in settings,
// but the SDK model list uses IDs like "claude-opus-4-6-1m".
const MODEL_CONTEXT_HINT_PATTERN = /\[(\d+m)\]$/i;

function tokenizeModelPreference(model: string): { tokens: string[]; contextHint?: string } {
  const lower = model.trim().toLowerCase();
  const contextHint = lower.match(MODEL_CONTEXT_HINT_PATTERN)?.[1]?.toLowerCase();

  const normalized = lower.replace(MODEL_CONTEXT_HINT_PATTERN, " $1 ");
  const rawTokens = normalized.split(/[^a-z0-9]+/).filter(Boolean);
  const tokens = rawTokens
    .map((token) => {
      if (token === "opusplan") return "opus";
      if (token === "best" || token === "default") return "";
      return token;
    })
    .filter((token) => token && token !== "claude")
    .filter((token) => /[a-z]/.test(token) || token.endsWith("m"));

  return { tokens, contextHint };
}

function scoreModelMatch(model: ModelInfo, tokens: string[], contextHint?: string): number {
  const haystack = `${model.value} ${model.displayName}`.toLowerCase();
  let score = 0;
  for (const token of tokens) {
    if (haystack.includes(token)) {
      score += token === contextHint ? 3 : 1;
    }
  }
  return score;
}

function resolveModelPreference(models: ModelInfo[], preference: string): ModelInfo | null {
  const trimmed = preference.trim();
  if (!trimmed) return null;

  const lower = trimmed.toLowerCase();

  // Exact match on value or display name
  const directMatch = models.find(
    (model) =>
      model.value === trimmed ||
      model.value.toLowerCase() === lower ||
      model.displayName.toLowerCase() === lower,
  );
  if (directMatch) return directMatch;

  // Substring match
  const includesMatch = models.find((model) => {
    const value = model.value.toLowerCase();
    const display = model.displayName.toLowerCase();
    return value.includes(lower) || display.includes(lower) || lower.includes(value);
  });
  if (includesMatch) return includesMatch;

  // Tokenized matching for aliases like "opus[1m]"
  const { tokens, contextHint } = tokenizeModelPreference(trimmed);
  if (tokens.length === 0) return null;

  let bestMatch: ModelInfo | null = null;
  let bestScore = 0;
  for (const model of models) {
    const score = scoreModelMatch(model, tokens, contextHint);
    if (0 < score && (!bestMatch || bestScore < score)) {
      bestMatch = model;
      bestScore = score;
    }
  }

  return bestMatch;
}

function resolveSettingsModel(
  models: ModelInfo[],
  settingsModel: unknown,
  logger: Logger,
): ModelInfo | null {
  if (settingsModel === undefined) {
    return null;
  }
  if (typeof settingsModel !== "string") {
    const typeLabel = settingsModel === null ? "null" : typeof settingsModel;
    logger.error(`Ignoring model from settings: expected a string, got ${typeLabel}.`);
    return null;
  }
  return resolveModelPreference(models, settingsModel);
}

/**
 * Restrict the SDK's model list to the user's `availableModels` allowlist
 * (already merged-and-deduped across settings sources by `SettingsManager`).
 * The user's exact entries become the model IDs surfaced via configOptions
 * and passed to `setModel`, which prevents Claude Code from silently
 * substituting a date-pinned variant (e.g. `haiku` →
 * `claude-haiku-4-5-20251001`) that the user may not have access to.
 *
 * For regular Claude allowlists, display info and capability flags are copied
 * from the closest SDK match so the UI still renders sensible names and effort
 * levels. External model pools opt out of the Default model; in that mode the
 * allowlist's exact entries are also used as display names so SDK alias
 * matching cannot make different BYOK models appear under the same label.
 *
 * Semantics from https://code.claude.com/docs/en/model-config#restrict-model-selection:
 * - `undefined` is handled by the caller (no allowlist applied).
 * - The Default option is unaffected by `availableModels` — it always remains
 *   available, even when the allowlist is `[]`.
 */
export function applyAvailableModelsAllowlist(
  sdkModels: ModelInfo[],
  allowlist: string[],
  preserveDefaultModel = true,
): ModelInfo[] {
  // Default is always preserved per the docs. Synthesize one if the SDK
  // didn't surface it so downstream code (e.g. `getAvailableModels` picking
  // `models[0]` as a fallback) still has something to work with.
  const defaultModel = sdkModels.find((m) => m.value === "default") ?? {
    value: "default",
    displayName: "Default",
    description: "",
  };
  const result: ModelInfo[] = preserveDefaultModel ? [defaultModel] : [];
  const seen = new Set<string>(preserveDefaultModel ? [defaultModel.value] : []);

  const sdkModelsWithoutDefault = sdkModels.filter((m) => m.value !== "default");

  for (const entry of allowlist) {
    const trimmed = entry.trim();
    if (!trimmed || seen.has(trimmed)) continue;

    const sdkMatch = resolveModelPreference(sdkModelsWithoutDefault, trimmed);
    if (sdkMatch && preserveDefaultModel) {
      result.push({ ...sdkMatch, value: trimmed });
    } else {
      result.push({ value: trimmed, displayName: trimmed, description: "" });
    }
    seen.add(trimmed);
  }

  return result.length > 0 ? result : [defaultModel];
}

export function mergeAvailableModelLists(...sources: unknown[]): string[] | undefined {
  let hasList = false;
  const result: string[] = [];

  for (const source of sources) {
    if (!Array.isArray(source)) continue;
    hasList = true;
    for (const entry of source) {
      if (typeof entry !== "string") continue;
      const trimmed = entry.trim();
      if (trimmed && !result.includes(trimmed)) {
        result.push(trimmed);
      }
    }
  }

  return hasList ? result : undefined;
}

export function parseModelProviderMap(raw: string | undefined): KodexModelProviderEntry[] {
  if (!raw) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];

  const entries: KodexModelProviderEntry[] = [];
  for (const entry of parsed) {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) continue;
    const record = entry as Record<string, unknown>;
    const model = stringField(record.model);
    const displayName = stringField(record.display_name) ?? stringField(record.displayName);
    const provider = stringField(record.provider);
    if (!model || !displayName || !provider) continue;
    entries.push({ model, displayName, provider });
  }
  return entries;
}

function stringField(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

export function applyModelProviderMap(
  models: ModelInfo[],
  providerEntries: KodexModelProviderEntry[],
): ModelInfo[] {
  if (providerEntries.length === 0) return models;

  const allowed = new Set<string>();
  for (const model of models) {
    allowed.add(model.value);
    allowed.add(model.displayName);
  }

  const routedModels: KodexModelInfo[] = [];
  const seen = new Set<string>();
  for (const entry of providerEntries) {
    if (!allowed.has(entry.model) && !allowed.has(entry.displayName)) {
      continue;
    }
    const key = `${entry.provider}:${entry.model}`;
    if (seen.has(key)) continue;
    seen.add(key);

    const sdkMatch =
      resolveModelPreference(models, entry.displayName) ?? resolveModelPreference(models, entry.model);
    routedModels.push({
      ...(sdkMatch ?? {
        value: entry.model,
        displayName: entry.displayName,
        description: "",
      }),
      value: entry.model,
      displayName: entry.displayName,
      description: sdkMatch?.description ?? "",
      kodexProvider: entry.provider,
      kodexRouteModel: entry.model,
    });
  }

  return routedModels.length > 0 ? routedModels : models;
}

function sessionModelOption(model: ModelInfo) {
  const option: Record<string, unknown> = {
    modelId: model.value,
    name: model.displayName,
    description: model.description,
  };
  const meta = modelProviderMeta(model);
  if (meta) option._meta = meta;
  return option as SessionModelState["availableModels"][number];
}

function modelConfigOption(model: SessionModelState["availableModels"][number]): any {
  const option: Record<string, unknown> = {
    value: model.modelId,
    name: model.name,
    description: model.description ?? undefined,
  };
  const meta = modelMeta(model);
  if (meta) option._meta = meta;
  return option;
}

function modelProviderMeta(model: ModelInfo): Record<string, string> | undefined {
  const provider = (model as KodexModelInfo).kodexProvider;
  if (!provider) return undefined;
  const routeModel = (model as KodexModelInfo).kodexRouteModel ?? model.value;
  return {
    provider,
    route_model: routeModel,
  };
}

function modelMeta(model: unknown): Record<string, unknown> | undefined {
  if (typeof model !== "object" || model === null) return undefined;
  const meta = (model as { _meta?: unknown; meta?: unknown })._meta ?? (model as { meta?: unknown }).meta;
  if (typeof meta !== "object" || meta === null || Array.isArray(meta)) return undefined;
  return meta as Record<string, unknown>;
}

export function optionProvider(option: unknown): string | undefined {
  const provider = modelMeta(option)?.provider;
  return typeof provider === "string" && provider.trim() ? provider.trim() : undefined;
}

export function optionRouteModel(option: unknown): string | undefined {
  const routeModel = modelMeta(option)?.route_model;
  return typeof routeModel === "string" && routeModel.trim() ? routeModel.trim() : undefined;
}

export function decodeProviderValue(value: string): { value: string; provider?: string } {
  if (!value.startsWith(KODEX_PROVIDER_VALUE_PREFIX)) {
    return { value };
  }
  const rest = value.slice(KODEX_PROVIDER_VALUE_PREFIX.length);
  const separator = rest.indexOf(":");
  if (separator <= 0) {
    return { value };
  }
  const provider = rest.slice(0, separator).trim();
  const decodedValue = rest.slice(separator + 1);
  if (!provider || !decodedValue) {
    return { value };
  }
  return { value: decodedValue, provider };
}

export function resolveModelPreferenceForProvider(
  models: ModelInfo[],
  preference: string,
  provider?: string,
): ModelInfo | null {
  if (!provider) return resolveModelPreference(models, preference);
  const providerModels = models.filter((model) => (model as KodexModelInfo).kodexProvider === provider);
  return resolveModelPreference(providerModels.length > 0 ? providerModels : models, preference);
}

export async function getAvailableModels(
  query: Query,
  models: ModelInfo[],
  settingsManager: SettingsManager,
  logger: Logger,
): Promise<SessionModelState> {
  const settings = settingsManager.getSettings();

  let currentModel = models[0];

  // Model priority (highest to lowest):
  // 1. ANTHROPIC_MODEL environment variable
  // 2. settings.model (user configuration)
  // 3. models[0] (default first model)
  if (process.env.ANTHROPIC_MODEL) {
    const match = resolveModelPreference(models, process.env.ANTHROPIC_MODEL);
    if (match) {
      currentModel = match;
    }
  } else {
    const match = resolveSettingsModel(models, settings.model, logger);
    if (match) {
      currentModel = match;
    }
  }

  await query.setModel(currentModel.value);

  return {
    availableModels: models.map((model) => sessionModelOption(model)),
    currentModelId: currentModel.value,
  };
}

function commonPrefixLength(a: string, b: string) {
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) {
    i++;
  }
  return i;
}

/** Best-effort first guess of a model's context window from its ID, used only
 *  until a `result` message arrives with the authoritative `modelUsage` value.
 *  Anthropic 1M-context variants encode "1m" as a distinct token in the SDK
 *  model ID (e.g., "claude-opus-4-6-1m"), which `\b1m\b` catches without also
 *  matching things like "10m" or embedded substrings. */
export function inferContextWindowFromModel(model: string): number | null {
  if (/\b1m\b/i.test(model)) return 1_000_000;
  return null;
}

export function parseModelConfig(raw: string | undefined):
  | {
      modelOverrides?: Record<string, string>;
      availableModels?: string[];
      preserveDefaultModel?: boolean;
    }
  | undefined {
  if (!raw) return undefined;
  const parsed = JSON.parse(raw);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("CLAUDE_MODEL_CONFIG must be a JSON object");
  }
  const result: {
    modelOverrides?: Record<string, string>;
    availableModels?: string[];
    preserveDefaultModel?: boolean;
  } = {};
  if (parsed.modelOverrides !== undefined) result.modelOverrides = parsed.modelOverrides;
  if (parsed.availableModels !== undefined) result.availableModels = parsed.availableModels;
  if (parsed.preserveDefaultModel !== undefined)
    result.preserveDefaultModel = Boolean(parsed.preserveDefaultModel);
  return Object.keys(result).length > 0 ? result : undefined;
}

export function getMatchingModelUsage(modelUsage: Record<string, ModelUsage>, currentModel: string) {
  let bestKey: string | null = null;
  let bestLen = 0;

  for (const key of Object.keys(modelUsage)) {
    const len = commonPrefixLength(key, currentModel);
    if (len > bestLen) {
      bestLen = len;
      bestKey = key;
    }
  }

  if (bestKey) {
    return modelUsage[bestKey];
  }
}
