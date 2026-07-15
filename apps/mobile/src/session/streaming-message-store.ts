// In-memory store of streaming message bodies keyed by message id, mirroring
// the desktop `conversation/streaming-message-store.ts`. `message_deltas` are
// appended here for smooth streaming until the full body lands in a patch.

const bodies = new Map<string, string>();

export function appendStreamingMessageDelta(id: string, append: string): void {
  bodies.set(id, (bodies.get(id) ?? "") + append);
}

export function getStreamingMessageBody(id: string): string | null {
  return bodies.get(id) ?? null;
}

export function clearStreamingMessage(id: string): void {
  bodies.delete(id);
}

export function clearAllStreamingMessages(): void {
  bodies.clear();
}
// end of file
