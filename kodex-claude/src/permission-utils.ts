import type { PermissionOption } from "@agentclientprotocol/sdk";
import type { PermissionMode, PermissionUpdate } from "@anthropic-ai/claude-agent-sdk";
import type { Logger } from "./logger.js";

const KODEX_PERMISSION_GUIDANCE_META_KEY = "kodex.ai/permissionGuidance";
const KODEX_USER_INPUT_ANSWERS_META_KEY = "kodex.ai/userInputAnswers";
const ASK_USER_QUESTION_OPTION_PREFIX = "ask_user_question";

interface NormalizedAskUserQuestionOption {
  label: string;
  description: string;
  preview?: string;
}

interface NormalizedAskUserQuestion {
  question: string;
  header: string;
  options: NormalizedAskUserQuestionOption[];
  multiSelect: boolean;
}

interface AskUserQuestionSelection {
  question: NormalizedAskUserQuestion;
  option: NormalizedAskUserQuestionOption;
}

const IS_ROOT = (process.geteuid?.() ?? process.getuid?.()) === 0;
export const ALLOW_BYPASS = !IS_ROOT || !!process.env.IS_SANDBOX;

const PERMISSION_MODE_ALIASES: Record<string, PermissionMode> = {
  auto: "auto",
  default: "default",
  acceptedits: "acceptEdits",
  dontask: "dontAsk",
  plan: "plan",
  bypasspermissions: "bypassPermissions",
  bypass: "bypassPermissions",
};

export function resolvePermissionMode(
  defaultMode?: unknown,
  logger: Logger = console,
): PermissionMode {
  if (defaultMode === undefined) {
    return "default";
  }

  if (typeof defaultMode !== "string") {
    logger.error("Ignoring permissions.defaultMode from settings: expected a string.");
    return "default";
  }

  const normalized = defaultMode.trim().toLowerCase();
  if (normalized === "") {
    logger.error("Ignoring permissions.defaultMode from settings: expected a non-empty string.");
    return "default";
  }

  const mapped = PERMISSION_MODE_ALIASES[normalized];
  if (!mapped) {
    logger.error(`Ignoring permissions.defaultMode from settings: unknown value '${defaultMode}'.`);
    return "default";
  }

  if (mapped === "bypassPermissions" && !ALLOW_BYPASS) {
    logger.error(
      "Ignoring permissions.defaultMode from settings: bypassPermissions is not available when running as root.",
    );
    return "default";
  }

  return mapped;
}

/**
 * Builds the label for the "Always Allow" permission option so the user can see
 * the exact scope they are committing to. Uses the SDK-provided suggestions
 * when available (e.g. `Bash(npm test:*)`) and falls back to naming the whole
 * tool so "Always Allow" is never a blank check without disclosure.
 */
export function describeAlwaysAllow(
  suggestions: PermissionUpdate[] | undefined,
  toolName: string,
): string {
  if (!suggestions || suggestions.length === 0) {
    return `Always Allow all ${toolName}`;
  }

  const ruleLabels: string[] = [];
  const directories: string[] = [];

  for (const update of suggestions) {
    if (update.type === "addRules" && update.behavior === "allow") {
      for (const rule of update.rules) {
        ruleLabels.push(
          rule.ruleContent ? `${rule.toolName}(${rule.ruleContent})` : `all ${rule.toolName}`,
        );
      }
    } else if (update.type === "addDirectories") {
      directories.push(...update.directories);
    }
  }

  const parts: string[] = [];
  if (ruleLabels.length > 0) {
    parts.push(ruleLabels.join(", "));
  }
  if (directories.length > 0) {
    parts.push(`access to ${directories.join(", ")}`);
  }

  if (parts.length === 0) {
    return `Always Allow all ${toolName}`;
  }

  return `Always Allow ${parts.join(" and ")}`;
}

export function normalizeAskUserQuestions(input: unknown): NormalizedAskUserQuestion[] | null {
  if (!isRecord(input) || !Array.isArray(input.questions)) return null;

  const questions: NormalizedAskUserQuestion[] = [];
  for (const rawQuestion of input.questions) {
    if (!isRecord(rawQuestion)) continue;
    const question = stringValue(rawQuestion.question);
    const header = stringValue(rawQuestion.header) || question.slice(0, 12) || "Question";
    if (!question || !Array.isArray(rawQuestion.options)) continue;

    const options: NormalizedAskUserQuestionOption[] = [];
    for (const rawOption of rawQuestion.options) {
      if (!isRecord(rawOption)) continue;
      const label = stringValue(rawOption.label);
      const description = stringValue(rawOption.description);
      if (!label) continue;
      const preview = stringValue(rawOption.preview);
      options.push({
        label,
        description,
        ...(preview ? { preview } : {}),
      });
    }

    if (options.length === 0) continue;
    questions.push({
      question,
      header,
      options,
      multiSelect: rawQuestion.multiSelect === true,
    });
  }

  return questions.length > 0 ? questions : null;
}

export function askUserQuestionPermissionOptions(questions: NormalizedAskUserQuestion[]): PermissionOption[] {
  const includeQuestionPrefix = questions.length > 1;
  const options: PermissionOption[] = [];

  questions.forEach((question, questionIndex) => {
    question.options.forEach((option, optionIndex) => {
      options.push({
        kind: "allow_once",
        name: includeQuestionPrefix ? `${question.header}: ${option.label}` : option.label,
        optionId: `${ASK_USER_QUESTION_OPTION_PREFIX}:${questionIndex}:${optionIndex}`,
      });
    });
  });

  return options;
}

export function askUserQuestionSelection(
  optionId: string | undefined,
  questions: NormalizedAskUserQuestion[],
): AskUserQuestionSelection | null {
  if (!optionId) return null;
  const [prefix, questionIndexText, optionIndexText] = optionId.split(":");
  if (prefix !== ASK_USER_QUESTION_OPTION_PREFIX) return null;
  const questionIndex = Number(questionIndexText);
  const optionIndex = Number(optionIndexText);
  if (!Number.isInteger(questionIndex) || !Number.isInteger(optionIndex)) return null;
  const question = questions[questionIndex];
  const option = question?.options[optionIndex];
  return question && option ? { question, option } : null;
}

export function withAskUserQuestionAnswer(
  input: unknown,
  selection: AskUserQuestionSelection,
  guidance?: string,
): Record<string, unknown> {
  const base = isRecord(input) ? { ...input } : {};
  const answers = stringRecord(base.answers);
  answers[selection.question.question] = selection.option.label;
  base.answers = answers;

  const notes = guidance?.trim();
  const preview = selection.option.preview?.trim();
  if (preview || notes) {
    const annotations = annotationRecord(base.annotations);
    annotations[selection.question.question] = {
      ...(preview ? { preview } : {}),
      ...(notes ? { notes } : {}),
    };
    base.annotations = annotations;
  }

  return base;
}

export function withAskUserQuestionAnswers(
  input: unknown,
  questions: NormalizedAskUserQuestion[],
  answerMap: Record<string, string[]>,
): Record<string, unknown> {
  const base = isRecord(input) ? { ...input } : {};
  const answers = stringRecord(base.answers);
  const annotations = annotationRecord(base.annotations);
  let hasAnnotations = Object.keys(annotations).length > 0;

  for (const question of questions) {
    const values = answerMap[question.question] ?? [];
    if (values.length === 0) continue;
    answers[question.question] = question.multiSelect ? values.join(", ") : values[0] ?? "";

    const previews = values
      .map((value) => question.options.find((option) => option.label === value)?.preview?.trim())
      .filter((preview): preview is string => !!preview);
    if (previews.length > 0) {
      annotations[question.question] = {
        ...(annotations[question.question] ?? {}),
        preview: previews.join("\n\n"),
      };
      hasAnnotations = true;
    }
  }

  base.answers = answers;
  if (hasAnnotations) {
    base.annotations = annotations;
  }

  return base;
}

export function permissionGuidance(response: unknown): string | undefined {
  if (!isRecord(response)) return undefined;
  const topLevelMeta = isRecord(response._meta) ? response._meta : undefined;
  const outcome = isRecord(response.outcome) ? response.outcome : undefined;
  const outcomeMeta = outcome && isRecord(outcome._meta) ? outcome._meta : undefined;
  return (
    stringValue(topLevelMeta?.[KODEX_PERMISSION_GUIDANCE_META_KEY]) ||
    stringValue(outcomeMeta?.[KODEX_PERMISSION_GUIDANCE_META_KEY]) ||
    undefined
  );
}

export function permissionInputAnswers(response: unknown): Record<string, string[]> | undefined {
  if (!isRecord(response)) return undefined;
  const topLevelMeta = isRecord(response._meta) ? response._meta : undefined;
  const outcome = isRecord(response.outcome) ? response.outcome : undefined;
  const outcomeMeta = outcome && isRecord(outcome._meta) ? outcome._meta : undefined;
  return (
    inputAnswersFromMeta(topLevelMeta?.[KODEX_USER_INPUT_ANSWERS_META_KEY]) ??
    inputAnswersFromMeta(outcomeMeta?.[KODEX_USER_INPUT_ANSWERS_META_KEY]) ??
    undefined
  );
}

function inputAnswersFromMeta(value: unknown): Record<string, string[]> | undefined {
  if (!isRecord(value)) return undefined;
  const rawAnswers = isRecord(value.answers) ? value.answers : value;
  const answers: Record<string, string[]> = {};
  for (const [key, entry] of Object.entries(rawAnswers)) {
    if (!Array.isArray(entry)) continue;
    const values = entry
      .filter((item): item is string => typeof item === "string")
      .map((item) => item.trim())
      .filter(Boolean);
    if (values.length > 0) answers[key] = values;
  }
  return Object.keys(answers).length > 0 ? answers : undefined;
}

function stringRecord(value: unknown): Record<string, string> {
  if (!isRecord(value)) return {};
  const result: Record<string, string> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (typeof entry === "string") result[key] = entry;
  }
  return result;
}

function annotationRecord(value: unknown): Record<string, Record<string, string>> {
  if (!isRecord(value)) return {};
  const result: Record<string, Record<string, string>> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (!isRecord(entry)) continue;
    const annotation: Record<string, string> = {};
    const preview = stringValue(entry.preview);
    const notes = stringValue(entry.notes);
    if (preview) annotation.preview = preview;
    if (notes) annotation.notes = notes;
    if (Object.keys(annotation).length > 0) result[key] = annotation;
  }
  return result;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}
