/**
 * Converts an OpenAI Chat Completions request into inputs for the
 * CodeBuddy SDK: a single prompt string + a list of SDK MCP tool
 * definitions to register via `createSdkMcpServer`.
 *
 * Tool-calling strategy (passthrough):
 *   - Client-declared `tools` are registered as an in-process SDK MCP server.
 *     The CodeBuddy model therefore sees real tool schemas and emits native
 *     `tool_use` content blocks (Anthropic shape) in the stream.
 *   - The tool handlers NEVER resolve; instead, the adapter aborts the query
 *     the instant the first `assistant` message containing a `tool_use`
 *     block is observed. This stops CodeBuddy's agentic loop at the tool
 *     boundary so the proxy can hand the `tool_use` back to the HTTP client,
 *     which is responsible for executing the tool and sending `role: 'tool'`
 *     results in a subsequent request.
 *   - Tool names returned by the model are namespaced as
 *     `mcp__<serverName>__<toolName>`; we strip that prefix before returning.
 */
import { z } from 'zod';
import type { ZodTypeAny } from 'zod';
import type { OAIChatRequest, OAIMessage, OAITool, OAIContentPart } from './openai-types.js';
import type { ContentBlock } from '@tencent-ai/agent-sdk';

/** A SDK-compatible user message — either a plain string or a
 *  structured `UserMessage` with `ContentBlock[]`. Tool results are now
 *  rendered as plain text strings to avoid duplicate tool_use_id in the
 *  CLI's internal history (the MCP handler already recorded a placeholder
 *  tool_result). */
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
 *  - **New session** (`isNew = true`): send the **full** conversation
 *    as a single text message. The CLI has no prior history, so we must
 *    include everything (tool_use blocks, tool_results, etc.) rendered
 *    as text so the model sees the complete context.
 *
 *  - **Existing session** (`isNew = false`): send only the **incremental**
 *    tail after the last assistant message. The CLI session maintains its
 *    own history internally, so each `session.send()` delivers only the
 *    new turn's content. Tool results are rendered as text (not structured
 *    tool_result blocks) to avoid duplicate tool_use_id in the CLI's
 *    internal history — the interrupt happened before the handler resolved,
 *    so the CLI has no tool_result for the previous tool_use. */
export function extractIncrementalMessage(
  messages: OAIMessage[],
  _sessionId: string,
  isNew = true,
): IncrementalMessage {
  if (isNew) {
    return buildFullPrompt(messages);
  }
  return buildIncrementalTail(messages);
}

/** Render the full conversation (minus system) as a single text block. */
function buildFullPrompt(messages: OAIMessage[]): IncrementalMessage {
  const convo = messages.filter((m) => m.role !== 'system');
  const blocks: string[] = [];
  for (const m of convo) {
    blocks.push(renderMessage(m));
  }
  const last = convo[convo.length - 1];
  if (!last || last.role === 'assistant') {
    blocks.push('<user>\n(continue)');
  }
  return blocks.join('\n\n') || '(continue)';
}

/** Extract only the incremental tail after the last assistant message. */
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

  if (toolResults.length > 0) {
    const parts = toolResults.map((m) => {
      const id = m.tool_call_id ?? '';
      const text = contentToText(m.content);
      return `[tool_result call_id="${id}"]\n${text}`;
    });
    if (userTexts.length > 0) {
      parts.push(userTexts.join('\n\n'));
    }
    return parts.join('\n\n') || '(continue)';
  }

  return userTexts.join('\n\n') || '(continue)';
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

function renderMessage(msg: OAIMessage): string {
  const text = contentToText(msg.content);
  switch (msg.role) {
    case 'system':
      return `<system>\n${text}`;
    case 'user':
      return `<user>\n${text}`;
    case 'assistant': {
      let body = text;
      if (msg.tool_calls && msg.tool_calls.length > 0) {
        const calls = msg.tool_calls
          .map(
            (tc) =>
              '```tool_use\n' +
              JSON.stringify({ name: tc.function.name, arguments: tc.function.arguments }) +
              '\n```',
          )
          .join('\n');
        body = (body ? body + '\n' : '') + calls;
      }
      return `<assistant>\n${body}`;
    }
    case 'tool': {
      return `<tool_result call_id="${msg.tool_call_id ?? ''}">\n${text}`;
    }
    default:
      return `<${msg.role}>\n${text}`;
  }
}

export interface BuiltPrompt {
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

export function buildPrompt(
  req: OAIChatRequest,
  defaults: { defaultModel: string; maxTurns: number },
): BuiltPrompt {
  const blocks: string[] = [];

  // Extract system messages into a separate field so they can be passed
  // via SDK `SessionOptions.systemPrompt` — the proper system-prompt channel
  // that carries higher weight than user-role text and avoids conflicting
  // with CodeBuddy CLI's own default system prompt.
  const systemParts: string[] = [];
  const convo: OAIMessage[] = [];
  for (const m of req.messages) {
    if (m.role === 'system') systemParts.push(contentToText(m.content));
    else convo.push(m);
  }
  const systemPrompt = systemParts.length > 0 ? systemParts.join('\n\n') : undefined;

  for (const m of convo) {
    blocks.push(renderMessage(m));
  }

  // If the last message is an assistant message (e.g. asking to continue),
  // nudge the model to respond.
  const last = convo[convo.length - 1];
  if (!last || last.role === 'assistant') {
    blocks.push('<user>\n(continue)');
  }

  const prompt = blocks.join('\n\n');
  const model = (req.model && req.model.trim()) || defaults.defaultModel;
  return { prompt, systemPrompt, model, maxTurns: defaults.maxTurns };
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
