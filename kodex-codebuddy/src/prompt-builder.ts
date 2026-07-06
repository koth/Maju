/**
 * Converts an OpenAI Chat Completions request into inputs for the
 * CodeBuddy SDK: message content (plain text or structured content blocks)
 * + a list of SDK MCP tool definitions to register via `createSdkMcpServer`.
 *
 * Tool-calling strategy (passthrough):
 *   - Client-declared `tools` are registered as an in-process SDK MCP server.
 *     The CodeBuddy model therefore sees real tool schemas and emits native
 *     `tool_use` content blocks (Anthropic shape) in the stream.
 *   - The tool handlers NEVER resolve; instead, the adapter interrupts the
 *     session once the consolidated `AssistantMessage` (carrying the real
 *     tool_use ids and complete inputs) arrives, handing the `tool_use` back
 *     to the HTTP client which executes the tool and sends `role: 'tool'`
 *     results in a subsequent request.
 *   - Tool names returned by the model are namespaced as
 *     `mcp__<serverName>__<toolName>`; we strip that prefix before returning.
 *
 * Message-format alignment with the Python sibling (`prompt_builder.py`):
 *   - **New session**: the CLI's stream-json input only accepts `type:"user"`
 *     lines, so assistant history cannot be replayed structurally. We send
 *     only the **last user message** as plain text — a clean single-turn
 *     prompt rather than a role-tagged dump of prior history (which would
 *     leak synthetic `<user>`/`<assistant>` noise). Earlier turns are
 *     effectively dropped on a cold session; clients that need continuity
 *     should pin the session via `X-Session-Id`.
 *   - **Existing session**: only the incremental tail after the last
 *     assistant message is sent. Tool results are rendered as **plain text**
 *     that embeds the real result content (not structured `tool_result`
 *     content blocks). The bundled CodeBuddy CLI's stream-json input handler
 *     (`StreamJsonUtils.convertContentBlock`) downgrades any `tool_result`
 *     content block to the placeholder text `[Tool result: <id>]` and
 *     **discards the actual result content**, so the model would not see the
 *     tool output and would re-explore endlessly. Sending the result as a
 *     text string makes the CLI pass it through verbatim, preserving the
 *     content the model needs.
 */
import { z } from 'zod';
import type { ZodTypeAny } from 'zod';
import type { OAIChatRequest, OAIMessage, OAITool, OAIContentPart } from './openai-types.js';
import type { ContentBlock } from '@tencent-ai/agent-sdk';

/** A SDK-compatible user message — either a plain string or a structured
 *  `UserMessage`. Tool results are currently rendered as plain text (see
 *  module docstring), so the structured `UserMessage` branch is retained for
 *  forward compatibility but not exercised by the current tool-result path. */
export type IncrementalMessage =
  | string
  | {
      type: 'user';
      session_id: string;
      message: { role: 'user'; content: string | ContentBlock[] };
      parent_tool_use_id: null;
    };

/** Extract the message to send to the CLI session.
 *
 *  - **New session** (`isNew = true`): send only the **last user message** as
 *    plain text (see module docstring).
 *  - **Existing session** (`isNew = false`): send only the incremental tail
 *    after the last assistant message; tool results are rendered as plain
 *    text that embeds the real result content (see module docstring). */
export function extractIncrementalMessage(
  messages: OAIMessage[],
  sessionId: string,
  isNew = true,
): IncrementalMessage {
  if (isNew) {
    return buildFullPrompt(messages);
  }
  return buildIncrementalTail(messages);
}

/** Build the user message for a turn on a fresh session: the **last user
 *  message** as plain text. The CLI stream-json input only accepts
 *  `type:"user"` lines, so assistant history cannot be replayed as structured
 *  messages; sending a role-tagged dump of the whole conversation would leak
 *  synthetic `<user>`/`<assistant>` noise into the model context. Earlier
 *  turns are effectively dropped on a cold session; clients that need
 *  continuity should pin the session via `X-Session-Id`. */
function buildFullPrompt(messages: OAIMessage[]): string {
  const convo = messages.filter((m) => m.role !== 'system');
  for (let i = convo.length - 1; i >= 0; i--) {
    const m = convo[i];
    if (m.role === 'user') {
      const text = contentToText(m.content);
      if (text.length > 0) return text;
    }
  }
  return '(continue)';
}

/** Build the user message for a turn on a warm session. The CLI keeps its own
 *  history, so only the incremental tail after the last assistant message is
 *  sent. Tool results are rendered as **plain text** embedding the real
 *  result content (e.g. `[tool_result call_id="..."]\n<result>`). We do NOT
 *  use structured `tool_result` content blocks here: the bundled CLI's
 *  stream-json input handler downgrades a `tool_result` block to the
 *  placeholder `[Tool result: <id>]` and discards the content, so the model
 *  would never see the actual tool output and would loop. A text string is
 *  passed through verbatim by the CLI, preserving the content. */
function buildIncrementalTail(messages: OAIMessage[]): IncrementalMessage {
  const convo = messages.filter((m) => m.role !== 'system');

  let lastAssistantIdx = -1;
  for (let i = convo.length - 1; i >= 0; i--) {
    if (convo[i].role === 'assistant') {
      lastAssistantIdx = i;
      break;
    }
  }

  if (lastAssistantIdx === -1) {
    const userTexts = convo
      .filter((m) => m.role === 'user')
      .map((m) => contentToText(m.content))
      .filter((t) => t.length > 0);
    return userTexts.join('\n\n') || '(continue)';
  }

  const tail = convo.slice(lastAssistantIdx + 1);
  if (tail.length === 0) {
    return '(continue)';
  }

  const toolResults = tail.filter((m) => m.role === 'tool');
  const userTexts = tail
    .filter((m) => m.role === 'user')
    .map((m) => contentToText(m.content))
    .filter((t) => t.length > 0);

  // No tool results → plain-text user turn.
  if (toolResults.length === 0) {
    return userTexts.join('\n\n') || '(continue)';
  }

  // Tool results as text: embed the real result content so the CLI passes it
  // through verbatim (structured `tool_result` blocks would be downgraded to a
  // contentless placeholder — see module docstring). tool_result(s) first,
  // then any trailing user text.
  const parts: string[] = toolResults.map((m) => {
    const id = m.tool_call_id ?? '';
    const text = contentToText(m.content);
    return `[tool_result call_id="${id}"]\n${text}`;
  });
  if (userTexts.length > 0) {
    parts.push(userTexts.join('\n\n'));
  }
  return parts.join('\n\n') || '(continue)';
}

/** Flatten an OpenAI content array into plain text (ignores images). */
function contentToText(content: OAIMessage['content']): string {
  if (content === null || content === undefined) return '';
  if (typeof content === 'string') return content;
  return content
    .map((part) => {
      if (part.type === 'text') return part.text ?? '';
      if (part.type === 'image_url') return '[image]';
      return '';
    })
    .join('');
}

export interface BuiltPrompt {
  /** Reserved; no longer populated with message text. The adapter sends
   *  message content via `extractIncrementalMessage` and passes the model
   *  name/system prompt from the fields below. Kept for API-shape parity
   *  with the Python sibling (`BuiltPrompt.prompt == ""`). */
  prompt: string;
  /** System prompt extracted from `role: system` messages, passed via
   *  SDK `SessionOptions.systemPrompt` instead of being mixed into the
   *  user message text. `undefined` when no system role is present. */
  systemPrompt?: string;
  model: string;
  maxTurns: number;
}

/**
 * Convert a JSON Schema (OpenAI tool parameters) into a Zod raw shape suitable
 * for `createSdkMcpServer`. CodeBuddy's CLI only needs the schema for the
 * model's tool-use prompting; we keep this best-effort and fall back to a
 * permissive string when the schema is not an object.
 */
export function jsonSchemaToZodShape(schema: Record<string, unknown> | undefined): Record<string, ZodTypeAny> {
  if (!schema || schema.type !== 'object' || !schema.properties) {
    return {};
  }
  const props = schema.properties as Record<string, Record<string, unknown>>;
  const required = new Set((schema.required as string[] | undefined) ?? []);
  const shape: Record<string, ZodTypeAny> = {};
  for (const [key, def] of Object.entries(props)) {
    shape[key] = jsonSchemaPropToZod(def, required.has(key));
  }
  return shape;
}

function jsonSchemaPropToZod(def: Record<string, unknown>, required: boolean): ZodTypeAny {
  let base: ZodTypeAny;
  switch (def.type) {
    case 'string':
      base = z.string();
      break;
    case 'number':
    case 'integer':
      base = z.number();
      break;
    case 'boolean':
      base = z.boolean();
      break;
    case 'array':
      base = z.array(z.unknown());
      break;
    case 'object':
      base = z.record(z.string(), z.unknown());
      break;
    default:
      base = z.unknown();
      break;
  }
  if (typeof def.description === 'string') base = base.describe(def.description);
  if (!required) base = base.optional();
  return base;
}

/** Split an OpenAI request into (model, systemPrompt).
 *
 *  `role: system` messages are extracted into `systemPrompt` so they can be
 *  passed via SDK `SessionOptions.systemPrompt` — the proper system-prompt
 *  channel — instead of being mixed into the user text. `prompt` is not used
 *  to send content (the adapter calls `extractIncrementalMessage` for that);
 *  it is returned as an empty string only to keep the `BuiltPrompt` shape
 *  aligned with the Python sibling. */
export function buildPrompt(
  req: OAIChatRequest,
  defaults: { defaultModel: string; maxTurns: number },
): BuiltPrompt {
  const systemParts: string[] = [];
  for (const m of req.messages) {
    if (m.role === 'system') systemParts.push(contentToText(m.content));
  }
  const systemPrompt = systemParts.length > 0 ? systemParts.join('\n\n') : undefined;
  const model = (req.model && req.model.trim()) || defaults.defaultModel;
  return { prompt: '', systemPrompt, model, maxTurns: defaults.maxTurns };
}

/** Name of the SDK MCP server we register for client tools. */
export const PROXY_TOOL_SERVER_NAME = 'proxy_tools';

/**
 * Strip the `mcp__<server>__<tool>` namespace the CLI applies to SDK MCP
 * tool names, returning the original client-declared tool name.
 */
export function demangleToolName(name: string, serverName = PROXY_TOOL_SERVER_NAME): string {
  const prefix = `mcp__${serverName}__`;
  return name.startsWith(prefix) ? name.slice(prefix.length) : name;
}

/** Build the SDK MCP tool definitions from an OpenAI `tools` array. */
export interface ProxyTool {
  /** Original client tool name (un-namespaced). */
  name: string;
  description: string;
  inputShape: Record<string, ZodTypeAny>;
}

export function buildProxyTools(tools: OAITool[] | undefined): ProxyTool[] {
  if (!tools || tools.length === 0) return [];
  return tools.map((t) => ({
    name: t.function.name,
    description: t.function.description ?? '',
    inputShape: jsonSchemaToZodShape(t.function.parameters as Record<string, unknown> | undefined),
  }));
}
