/**
 * Minimal OpenAI Chat Completions request/response types we need to handle.
 * Only the subset that this proxy supports; unknown fields are ignored.
 */

export interface OAIMessage {
  role: 'system' | 'user' | 'assistant' | 'tool';
  content: string | OAIContentPart[] | null;
  /** Present when role === 'tool' (result of a previous tool call). */
  tool_call_id?: string;
  /** Present when role === 'assistant' and the model emitted tool calls. */
  tool_calls?: OAIToolCall[];
  name?: string;
}

export interface OAIContentPart {
  type: 'text' | 'image_url';
  text?: string;
  image_url?: { url: string; detail?: string };
}

export interface OAIToolCall {
  id: string;
  type: 'function';
  function: { name: string; arguments: string };
}

export interface OAITool {
  type: 'function';
  function: {
    name: string;
    description?: string;
    parameters?: Record<string, unknown>;
  };
}

export interface OAIChatRequest {
  model?: string;
  messages: OAIMessage[];
  stream?: boolean;
  temperature?: number;
  max_tokens?: number | null;
  max_completion_tokens?: number | null;
  tools?: OAITool[];
  tool_choice?: 'none' | 'auto' | 'required' | { type: 'function'; function: { name: string } };
  top_p?: number;
  stop?: string | string[];
  user?: string;
  /** Other fields passed through; ignored by the proxy. */
  [key: string]: unknown;
}

// --- Response shapes ---

export interface OAIChoiceMessage {
  role: 'assistant';
  content: string | null;
  tool_calls?: OAIToolCall[];
  refusal?: string | null;
}

export interface OAIChoice {
  index: number;
  message: OAIChoiceMessage;
  finish_reason: 'stop' | 'length' | 'tool_calls' | 'content_filter' | null;
  logprobs?: null;
}

export interface OAIChatResponse {
  id: string;
  object: 'chat.completion';
  created: number;
  model: string;
  choices: OAIChoice[];
  usage: {
    prompt_tokens: number;
    completion_tokens: number;
    total_tokens: number;
  };
  system_fingerprint?: string;
}

// --- Streaming chunk shapes ---

export interface OAIChunkDelta {
  role?: 'assistant';
  content?: string | null;
  tool_calls?: Array<{
    index: number;
    id?: string;
    type?: 'function';
    function: { name?: string; arguments?: string };
  }>;
  refusal?: string | null;
}

export interface OAIChunkChoice {
  index: number;
  delta: OAIChunkDelta;
  finish_reason: 'stop' | 'length' | 'tool_calls' | 'content_filter' | null;
}

export interface OAIChatChunk {
  id: string;
  object: 'chat.completion.chunk';
  created: number;
  model: string;
  choices: OAIChunkChoice[];
}

// --- Models list ---

export interface OAIModel {
  id: string;
  object: 'model';
  created: number;
  owned_by: string;
}

export interface OAIModelsResponse {
  object: 'list';
  data: OAIModel[];
}
