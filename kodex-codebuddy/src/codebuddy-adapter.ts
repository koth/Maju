/**
 * Adapter that runs a turn on a (possibly pooled) CodeBuddy SDK `Session`
 * and converts its `Message` stream into either:
 *   - a non-streaming OpenAI `chat.completion` response, or
 *   - an async generator of OpenAI `chat.completion.chunk` SSE frames.
 *
 * Tool-calling strategy (capture + interrupt after AssistantMessage):
 *   Client-declared `tools` are registered as an in-process SDK MCP server.
 *   When the model emits `tool_use` block(s), the MCP handlers capture the
 *   complete input but **never resolve**. The adapter waits for the
 *   consolidated `AssistantMessage` (which carries the real tool_use ids
 *   and complete inputs), extracts the last tool_use, then calls
 *   `session.interrupt()` to stop the CLI's agentic loop and breaks out
 *   of the stream.
 *
 *   Only the **last** tool_use is surfaced to the HTTP client (codex-acp)
 *   for execution. The real result is sent in the next request as a
 *   **user text message** (not a structured tool_result) — the CLI's
 *   internal history has no tool_result for this tool_use (interrupt
 *   happened before the handler resolved), so there is no duplicate
 *   tool_use_id. See `extractIncrementalMessage` in prompt-builder.ts.
 */
import { createSdkMcpServer, tool as sdkTool } from '@tencent-ai/agent-sdk';
import type { Session, Message, ContentBlock, RawMessageStreamEvent } from '@tencent-ai/agent-sdk';
import type {
  OAIChatResponse,
  OAIChatChunk,
  OAIChatRequest,
  OAIToolCall,
} from './openai-types.js';
import {
  buildPrompt,
  extractIncrementalMessage,
  buildProxyTools,
  demangleToolName,
  PROXY_TOOL_SERVER_NAME,
} from './prompt-builder.js';
import type { IncrementalMessage } from './prompt-builder.js';
import { logger } from './logger.js';

export interface AdapterOptions {
  defaultModel: string;
  maxTurns: number;
  cwd: string;
}

/// A pending MCP tool handler that has captured its input. The handler
/// **never resolves** — the adapter interrupts the session after seeing
/// the `AssistantMessage`, which cancels the CLI's wait for the result.
export interface PendingHandler {
  name: string;
  arguments: string;
}

/// Shared container for all pending MCP handlers in the current turn.
export interface PendingQueue {
  handlers: PendingHandler[];
}

/// Legacy alias kept for session-pool type compatibility.
export type PendingToolUse = { id: string; name: string; arguments: string };

function buildCapturingHandler(
  toolName: string,
  pending: PendingQueue,
): (input: Record<string, unknown>) => Promise<{ content: Array<{ type: 'text'; text: string }> }> {
  return (input: Record<string, unknown>) => {
    const args = JSON.stringify(input);
    logger.info('handler_captured tool=%s args_len=%d', toolName, args.length);
    pending.handlers.push({ name: toolName, arguments: args });
    // Never resolve — the adapter will interrupt the session, which
    // cancels the CLI's agentic loop and stops waiting for this promise.
    return new Promise<{ content: Array<{ type: 'text'; text: string }> }>(() => {});
  };
}

/** Build the SDK MCP server from client tools (or undefined if none). */
export function buildMcpServer(
  req: OAIChatRequest,
  pending: PendingQueue,
) {
  const proxyTools = buildProxyTools(req.tools);
  if (proxyTools.length === 0) return undefined;
  const defs = proxyTools.map((t) =>
    sdkTool(t.name, t.description, t.inputShape, buildCapturingHandler(t.name, pending)),
  );
  return createSdkMcpServer({ name: PROXY_TOOL_SERVER_NAME, tools: defs });
}

/** Extract all tool_use blocks from an assistant message's content.
 *  Returns only the **last** tool_use (surfaced to codex-acp); earlier
 *  ones are dropped — the model will see "no result" for them because
 *  the handlers never resolve and the session is interrupted. */
function extractToolCalls(content: ContentBlock[]): OAIToolCall[] {
  const toolUseBlocks = content.filter((b) => b.type === 'tool_use');
  if (toolUseBlocks.length === 0) return [];
  if (toolUseBlocks.length > 1) {
    logger.warn(
      'multiple_tool_use_blocks kept=1(=last) dropped=%d total=%d',
      toolUseBlocks.length - 1,
      toolUseBlocks.length,
    );
  }
  const last = toolUseBlocks[toolUseBlocks.length - 1];
  return [{
    id: last.id,
    type: 'function',
    function: {
      name: demangleToolName(last.name),
      arguments: JSON.stringify(last.input),
    },
  }];
}

/** Interrupt a session, swallowing the transport teardown race error. */
async function safeInterrupt(session: Session): Promise<void> {
  try {
    await session.interrupt();
  } catch (err) {
    logger.debug('session interrupt teardown error (ignored): %s', err instanceof Error ? err.message : String(err));
  }
}

function randomId(): string {
  return Math.random().toString(36).slice(2) + Date.now().toString(36);
}

/**
 * Shared message-loop: send the prompt and drain `session.stream()`.
 * When the `AssistantMessage` with tool_use arrives, extract the last
 * tool_call, interrupt the session, and break — handing the tool_use
 * back to the HTTP client.
 */
async function runTurn(
  session: Session,
  message: IncrementalMessage,
  pending: PendingQueue,
  cb: (state: TurnState, msg: Message) => void,
): Promise<TurnState> {
  const state: TurnState = {
    assistantText: '',
    toolCalls: [],
    promptTokens: 0,
    completionTokens: 0,
    finishReason: 'stop',
    resolvedModel: '',
    sawToolUse: false,
  };
  logger.info('send_incremental type=%s preview=%s',
    typeof message === 'string' ? 'text' : 'structured',
    typeof message === 'string'
      ? message.slice(0, 200).replace(/\n/g, '\\n')
      : JSON.stringify(message).slice(0, 200));
  await session.send(message);
  for await (const msg of session.stream() as AsyncIterable<Message>) {
    cb(state, msg);
    if (msg.type === 'assistant') {
      const calls = extractToolCalls(msg.message.content);
      if (calls.length > 0) {
        state.sawToolUse = true;
        state.toolCalls = calls;
        // Interrupt the CLI's agentic loop. The MCP handlers never
        // resolved, so the CLI has no tool_result — it stops here.
        await safeInterrupt(session);
        break;
      }
    }
  }
  if (state.sawToolUse) {
    state.finishReason = 'tool_calls';
  }
  return state;
}

interface TurnState {
  assistantText: string;
  toolCalls: OAIToolCall[];
  promptTokens: number;
  completionTokens: number;
  finishReason: OAIChatResponse['choices'][number]['finish_reason'];
  resolvedModel: string;
  sawToolUse: boolean;
}

/** Run a non-streaming turn and return a full OpenAI chat.completion object. */
export async function runNonStreaming(
  req: OAIChatRequest,
  session: Session,
  opts: AdapterOptions,
  pending: PendingQueue,
  isNew = true,
): Promise<OAIChatResponse> {
  const { model } = buildPrompt(req, {
    defaultModel: opts.defaultModel,
    maxTurns: opts.maxTurns,
  });
  const message = extractIncrementalMessage(req.messages, session.sessionId, isNew);

  const completionId = `chatcmpl-${randomId()}`;
  const created = Math.floor(Date.now() / 1000);
  let resolvedModel = model;
  pending.handlers.length = 0;

  try {
    const state = await runTurn(session, message, pending, (s, msg) => {
      if (msg.type === 'assistant') {
        s.resolvedModel = msg.message.model || resolvedModel;
        for (const block of msg.message.content) {
          if (block.type === 'text') s.assistantText += block.text;
        }
        if (msg.message.usage) {
          s.promptTokens = msg.message.usage.input_tokens ?? s.promptTokens;
          s.completionTokens = msg.message.usage.output_tokens ?? s.completionTokens;
        }
        if (msg.message.stop_reason === 'max_tokens') s.finishReason = 'length';
      } else if (msg.type === 'result') {
        if ('usage' in msg && msg.usage) {
          s.promptTokens = msg.usage.input_tokens ?? s.promptTokens;
          s.completionTokens = msg.usage.output_tokens ?? s.completionTokens;
        }
      }
    });
    resolvedModel = state.resolvedModel || resolvedModel;

    return {
      id: completionId,
      object: 'chat.completion',
      created,
      model: resolvedModel,
      choices: [
        {
          index: 0,
          message: {
            role: 'assistant',
            content: state.assistantText || null,
            ...(state.toolCalls.length > 0 ? { tool_calls: state.toolCalls } : {}),
          },
          finish_reason: state.finishReason,
          logprobs: null,
        },
      ],
      usage: {
        prompt_tokens: state.promptTokens,
        completion_tokens: state.completionTokens,
        total_tokens: state.promptTokens + state.completionTokens,
      },
    };
  } catch (err) {
    logger.error('codebuddy turn failed: %s', err instanceof Error ? err.stack ?? err.message : String(err));
    throw err;
  }
}

/**
 * Run a streaming turn and yield OpenAI `chat.completion.chunk` objects
 * serialized into SSE `data: ...` frames, ending with `data: [DONE]`.
 */
export async function* runStreaming(
  req: OAIChatRequest,
  session: Session,
  opts: AdapterOptions,
  pending: PendingQueue,
  isNew = true,
): AsyncGenerator<string, void> {
  const { model } = buildPrompt(req, {
    defaultModel: opts.defaultModel,
    maxTurns: opts.maxTurns,
  });
  const message = extractIncrementalMessage(req.messages, session.sessionId, isNew);
  logger.info('send_incremental_stream type=%s preview=%s',
    typeof message === 'string' ? 'text' : 'structured',
    typeof message === 'string'
      ? message.slice(0, 200).replace(/\n/g, '\\n')
      : JSON.stringify(message).slice(0, 200));

  const completionId = `chatcmpl-${randomId()}`;
  const created = Math.floor(Date.now() / 1000);
  const send = (obj: OAIChatChunk): string => `data: ${JSON.stringify(obj)}\n\n`;

  // Initial role chunk.
  yield send({
    id: completionId,
    object: 'chat.completion.chunk',
    created,
    model,
    choices: [{ index: 0, delta: { role: 'assistant', content: '' }, finish_reason: null }],
  });

  let finishReason: OAIChatChunk['choices'][number]['finish_reason'] = 'stop';
  let resolvedModel = model;
  let sawToolUse = false;
  let currentToolIndex = 0;

  // Clear pending handlers left over from prior turns. The MCP handlers are
  // bound to this persistent queue (entry.pending on the session pool); without
  // clearing, tool captures from a previous turn leak into this turn.
  pending.handlers.length = 0;
  logger.info('runStreaming start isNew=%s tools=%d', isNew, req.tools?.length ?? 0);

  try {
    await session.send(message);
    for await (const msg of session.stream() as AsyncIterable<Message>) {
      if (msg.type === 'stream_event') {
        const ev = msg.event as RawMessageStreamEvent;
        if (ev.type !== 'content_block_delta') {
          logger.info('stream_event ev.type=%s', ev.type);
        }
        if (ev.type === 'message_start') {
          resolvedModel = ev.message.model || resolvedModel;
        } else if (ev.type === 'content_block_delta') {
          const d = ev.delta;
          if (d.type === 'text_delta') {
            yield send({
              id: completionId,
              object: 'chat.completion.chunk',
              created,
              model: resolvedModel,
              choices: [{ index: 0, delta: { content: d.text }, finish_reason: null }],
            });
          }
        } else if (ev.type === 'content_block_start') {
          const block = ev.content_block;
          if (block.type === 'tool_use') {
            logger.info(
              'content_block_start tool_use id=%s name=%s',
              block.id, block.name,
            );
          }
        }
        continue;
      }

      if (msg.type === 'assistant') {
        resolvedModel = msg.message.model || resolvedModel;
        if (msg.message.stop_reason === 'max_tokens') finishReason = 'length';
        const blockTypes = msg.message.content.map((b: ContentBlock) => b.type).join(',');
        const toolUseCount = msg.message.content.filter((b: ContentBlock) => b.type === 'tool_use').length;
        logger.info('assistant_msg stop_reason=%s content=[%s] tool_use_count=%d',
          msg.message.stop_reason, blockTypes, toolUseCount);
        const calls = extractToolCalls(msg.message.content);
        if (calls.length > 0) {
          sawToolUse = true;
          finishReason = 'tool_calls';
          for (const call of calls) {
            logger.info(
              'emitting_tool_call id=%s name=%s args_len=%d',
              call.id, call.function.name, call.function.arguments.length,
            );
            yield send({
              id: completionId,
              object: 'chat.completion.chunk',
              created,
              model: resolvedModel,
              choices: [
                {
                  index: 0,
                  delta: {
                    tool_calls: [
                      {
                        index: currentToolIndex++,
                        id: call.id,
                        type: 'function',
                        function: { name: call.function.name, arguments: call.function.arguments },
                      },
                    ],
                  },
                  finish_reason: null,
                },
              ],
            });
          }
          // Interrupt after emitting all tool_call chunks.
          await safeInterrupt(session);
          break;
        }
      } else if (msg.type === 'result') {
        logger.info('result_msg subtype=%s is_error=%s',
          msg.subtype, (msg as { is_error?: boolean }).is_error ?? false);
      } else {
        logger.info('other_msg type=%s', msg.type);
      }
    }

    if (sawToolUse) finishReason = 'tool_calls';

    logger.info('stream_ended sawToolUse=%s finishReason=%s pending_handlers=%d',
      sawToolUse, finishReason, pending.handlers.length);
    if (!sawToolUse && pending.handlers.length > 0) {
      logger.warn(
        'anomaly: %d MCP handler(s) captured tool_use but no assistant tool_use was emitted — turn ends as %s',
        pending.handlers.length, finishReason,
      );
    }

    yield send({
      id: completionId,
      object: 'chat.completion.chunk',
      created,
      model: resolvedModel,
      choices: [{ index: 0, delta: {}, finish_reason: finishReason }],
    });
  } catch (err) {
    logger.error('codebuddy streaming turn failed: %s', err instanceof Error ? err.stack ?? err.message : String(err));
    yield send({
      id: completionId,
      object: 'chat.completion.chunk',
      created,
      model: resolvedModel,
      choices: [
        {
          index: 0,
          delta: { content: `\n[proxy error: ${err instanceof Error ? err.message : String(err)}]` },
          finish_reason: 'stop',
        },
      ],
    });
  } finally {
    yield 'data: [DONE]\n\n';
  }
}

export { runNonStreaming as chatCompletion, runStreaming as chatCompletionStream };
