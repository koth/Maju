type StreamingMessageEvent =
  | { type: "append"; text: string }
  | { type: "replace"; text: string };

type StreamingMessageListener = (event: StreamingMessageEvent) => void;

interface StreamingMessageState {
  body: string;
  pending: string;
  listeners: Set<StreamingMessageListener>;
  flushTimer: number | null;
}

const STREAMING_FLUSH_MS = 80;
const states = new Map<string, StreamingMessageState>();

function getOrCreateState(id: string) {
  let state = states.get(id);
  if (!state) {
    state = {
      body: "",
      pending: "",
      listeners: new Set(),
      flushTimer: null,
    };
    states.set(id, state);
  }
  return state;
}

function flushPending(state: StreamingMessageState) {
  if (state.flushTimer != null) {
    window.clearTimeout(state.flushTimer);
    state.flushTimer = null;
  }
  if (!state.pending) return;
  const text = state.pending;
  state.pending = "";
  for (const listener of state.listeners) {
    listener({ type: "append", text });
  }
}

export function getStreamingMessageBody(id: string) {
  return states.get(id)?.body ?? null;
}

export function ensureStreamingMessageBody(id: string, body: string) {
  const state = getOrCreateState(id);
  if (!state.body) {
    state.body = body;
    return state.body;
  }
  if (body.length > state.body.length && body.startsWith(state.body)) {
    state.pending += body.slice(state.body.length);
    state.body = body;
      flushPending(state);
    return state.body;
  }
  if (body !== state.body) {
    state.body = body;
    state.pending = "";
  }
  return state.body;
}

export function appendStreamingMessageDelta(id: string, text: string) {
  if (!text) return;
  const state = getOrCreateState(id);
  state.body += text;
  state.pending += text;
  if (state.flushTimer == null) {
    state.flushTimer = window.setTimeout(() => flushPending(state), STREAMING_FLUSH_MS);
  }
}

export function replaceStreamingMessageBody(id: string, body: string) {
  const state = getOrCreateState(id);
  state.body = body;
  state.pending = "";
  if (state.flushTimer != null) {
    window.clearTimeout(state.flushTimer);
    state.flushTimer = null;
  }
  for (const listener of state.listeners) {
    listener({ type: "replace", text: body });
  }
}

export function subscribeStreamingMessage(
  id: string,
  listener: StreamingMessageListener,
) {
  const state = getOrCreateState(id);
  state.listeners.add(listener);
  return () => {
    state.listeners.delete(listener);
  };
}
