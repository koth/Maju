import { describe, it, expect } from 'vitest';
import {
  extractIncrementalMessage,
  buildPrompt,
  buildProxyTools,
  demangleToolName,
  PROXY_TOOL_SERVER_NAME,
} from './prompt-builder.js';
import type { OAIMessage, OAIChatRequest } from './openai-types.js';

const SID = 'sess-1';

function user(text: string): OAIMessage {
  return { role: 'user', content: text };
}
function assistant(text: string, toolCalls?: OAIMessage['tool_calls']): OAIMessage {
  return { role: 'assistant', content: text, ...(toolCalls ? { tool_calls: toolCalls } : {}) };
}
function toolResult(callId: string, text: string): OAIMessage {
  return { role: 'tool', content: text, tool_call_id: callId };
}

describe('extractIncrementalMessage — new session (isNew=true)', () => {
  it('sends only the last user message as plain text', () => {
    const msgs: OAIMessage[] = [
      user('first question'),
      assistant('first answer'),
      user('second question'),
      assistant('second answer'),
      user('latest question'),
    ];
    expect(extractIncrementalMessage(msgs, SID, true)).toBe('latest question');
  });

  it('ignores system messages when picking the last user message', () => {
    const msgs: OAIMessage[] = [
      { role: 'system', content: 'be helpful' },
      user('hello'),
    ];
    expect(extractIncrementalMessage(msgs, SID, true)).toBe('hello');
  });

  it('falls back to (continue) when there is no user message with text', () => {
    const msgs: OAIMessage[] = [assistant('solo assistant')];
    expect(extractIncrementalMessage(msgs, SID, true)).toBe('(continue)');
  });

  it('skips empty user messages when searching for the last one', () => {
    const msgs: OAIMessage[] = [
      user(''), // empty
      user('real prompt'),
    ];
    expect(extractIncrementalMessage(msgs, SID, true)).toBe('real prompt');
  });
});

describe('extractIncrementalMessage — warm session (isNew=false)', () => {
  it('returns plain text when the tail is only user messages', () => {
    const msgs: OAIMessage[] = [
      assistant('prior answer'),
      user('follow up'),
    ];
    expect(extractIncrementalMessage(msgs, SID, false)).toBe('follow up');
  });

  it('returns (continue) when there is no tail after the assistant message', () => {
    const msgs: OAIMessage[] = [assistant('prior answer')];
    expect(extractIncrementalMessage(msgs, SID, false)).toBe('(continue)');
  });

  it('joins all user messages when there is no assistant message', () => {
    const msgs: OAIMessage[] = [user('a'), user('b')];
    expect(extractIncrementalMessage(msgs, SID, false)).toBe('a\n\nb');
  });

  it('renders tool results as text that embeds the real content', () => {
    const msgs: OAIMessage[] = [
      assistant('let me check', [{ id: 'call_1', type: 'function', function: { name: 'get_weather', arguments: '{}' } }]),
      toolResult('call_1', 'sunny, 20C'),
      user('thanks'),
    ];
    const out = extractIncrementalMessage(msgs, SID, false);
    // Plain text so the CLI passes the real result content through verbatim
    // (structured `tool_result` blocks are downgraded to a contentless
    // placeholder — see prompt-builder.ts module docstring).
    expect(out).toBe('[tool_result call_id="call_1"]\nsunny, 20C\n\nthanks');
  });

  it('renders tool results as text when there is no trailing user text', () => {
    const msgs: OAIMessage[] = [
      assistant('check', [{ id: 'call_1', type: 'function', function: { name: 'f', arguments: '{}' } }]),
      toolResult('call_1', 'result'),
    ];
    expect(extractIncrementalMessage(msgs, SID, false)).toBe('[tool_result call_id="call_1"]\nresult');
  });

  it('ignores system messages when computing the incremental tail', () => {
    const msgs: OAIMessage[] = [
      { role: 'system', content: 'sys' },
      assistant('answer'),
      user('next'),
    ];
    expect(extractIncrementalMessage(msgs, SID, false)).toBe('next');
  });
});

describe('buildPrompt', () => {
  it('extracts system messages into systemPrompt and resolves the model', () => {
    const req: OAIChatRequest = {
      model: ' claude-sonnet-5 ',
      messages: [
        { role: 'system', content: 'rule one' },
        { role: 'system', content: 'rule two' },
        user('hi'),
      ],
    };
    const built = buildPrompt(req, { defaultModel: 'fallback', maxTurns: 3 });
    expect(built.systemPrompt).toBe('rule one\n\nrule two');
    expect(built.model).toBe('claude-sonnet-5');
    expect(built.maxTurns).toBe(3);
    // prompt is no longer populated with message text (adapter uses extractIncrementalMessage).
    expect(built.prompt).toBe('');
  });

  it('uses the default model when none is supplied and omits systemPrompt', () => {
    const req: OAIChatRequest = { messages: [user('hi')] };
    const built = buildPrompt(req, { defaultModel: 'fallback', maxTurns: 1 });
    expect(built.model).toBe('fallback');
    expect(built.systemPrompt).toBeUndefined();
    expect(built.prompt).toBe('');
  });
});

describe('tool helpers', () => {
  it('builds proxy tools from OpenAI tool definitions', () => {
    const tools = [
      {
        type: 'function' as const,
        function: {
          name: 'get_weather',
          description: 'Get weather',
          parameters: {
            type: 'object',
            properties: { location: { type: 'string', description: 'City' } },
            required: ['location'],
          },
        },
      },
    ];
    const proxy = buildProxyTools(tools);
    expect(proxy).toHaveLength(1);
    expect(proxy[0].name).toBe('get_weather');
    expect(proxy[0].description).toBe('Get weather');
    expect(proxy[0].inputShape).toHaveProperty('location');
  });

  it('demangles namespaced SDK MCP tool names', () => {
    const namespaced = `mcp__${PROXY_TOOL_SERVER_NAME}__get_weather`;
    expect(demangleToolName(namespaced)).toBe('get_weather');
    expect(demangleToolName('get_weather')).toBe('get_weather');
  });

  it('returns [] for an empty/undefined tools array', () => {
    expect(buildProxyTools(undefined)).toEqual([]);
    expect(buildProxyTools([])).toEqual([]);
  });
});
