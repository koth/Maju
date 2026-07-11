// Composer draft store — keeps the prompt text and attachments outside the
// React component tree so the draft survives transient unmounts (e.g. opening
// the settings page or returning to the workspace launcher, both of which
// short-circuit the Workbench render and tear down the <Composer> subtree).
//
// Drafts are scoped per workspace root and survive every navigation that
// does not end in a successful send:
//
//   - switching the active workspace (open A, switch to B, switch back to A)
//   - switching the active session within the same workspace
//     (S1 → S2 → S1 in the same project)
//   - opening and closing the settings page
//   - returning from the workspace launcher
//
// We deliberately ignore the session id when building the owner key. The
// backend can resync and reissue the id on round trips, and pinning the
// draft to a specific id would silently drop the user's typed text.
// The Workbench never calls `clearComposerDraft` on session / workspace
// navigation; the only places that drop the draft are the existing
// `setInput("")` / `setAttachments([])` calls in the send flow.
//
// This module intentionally mirrors the pattern used by
// `features/conversation/streaming-message-store.ts`.

import { useSyncExternalStore } from "react";

export interface ComposerAttachmentDraft {
  id: string;
  name: string;
  displayName: string;
  mimeType: string;
  data: string | null;
  text: string | null;
  uri: string | null;
  kind: "image" | "file" | "workspace_file";
  path: string | null;
  startLine: number | null;
  endLine: number | null;
  previewUrl: string | null;
  thumbnailData: string | null;
  thumbnailMimeType: string | null;
}

export interface ComposerDraft {
  input: string;
  attachments: ComposerAttachmentDraft[];
}

const EMPTY_DRAFT: ComposerDraft = { input: "", attachments: [] };

interface DraftEntry {
  input: string;
  attachments: ComposerAttachmentDraft[];
  listeners: Set<() => void>;
  /**
   * The most recent snapshot reference returned to React. We hand back the
   * same object on every read of `getSnapshot` until the draft actually
   * changes — this is required by `useSyncExternalStore` so React's
   * `Object.is` check sees a stable snapshot and avoids render loops.
   */
  snapshot: ComposerDraft;
}

const store: Map<string, DraftEntry> = new Map();

type Setter<T> = T | ((previous: T) => T);

function resolveSetter<T>(setter: Setter<T>, previous: T): T {
  return typeof setter === "function"
    ? (setter as (previous: T) => T)(previous)
    : setter;
}

function createEntry(): DraftEntry {
  const draft: ComposerDraft = { input: "", attachments: [] };
  return {
    input: "",
    attachments: [],
    listeners: new Set(),
    snapshot: draft,
  };
}

function getOrCreateEntry(ownerKey: string): DraftEntry {
  let entry = store.get(ownerKey);
  if (!entry) {
    entry = createEntry();
    store.set(ownerKey, entry);
  }
  return entry;
}

function publish(entry: DraftEntry, input: string, attachments: ComposerAttachmentDraft[]) {
  if (
    entry.snapshot.input === input &&
    entry.snapshot.attachments === attachments
  ) {
    return;
  }
  entry.input = input;
  entry.attachments = attachments;
  entry.snapshot = { input, attachments };
  for (const listener of entry.listeners) {
    listener();
  }
}

function subscribe(ownerKey: string, listener: () => void) {
  const entry = getOrCreateEntry(ownerKey);
  entry.listeners.add(listener);
  return () => {
    entry.listeners.delete(listener);
  };
}

function getSnapshot(ownerKey: string): ComposerDraft {
  const entry = store.get(ownerKey);
  if (!entry) return EMPTY_DRAFT;
  return entry.snapshot;
}

export function composerDraftKey(workspaceRoot: string, _sessionId: string): string {
  // See the file header for why we drop the session id from the key.
  void _sessionId;
  return workspaceRoot;
}

export function setComposerDraftInput(ownerKey: string, input: string) {
  const entry = getOrCreateEntry(ownerKey);
  publish(entry, input, entry.attachments);
}

export function setComposerDraftAttachments(
  ownerKey: string,
  attachments: ComposerAttachmentDraft[],
) {
  const entry = getOrCreateEntry(ownerKey);
  publish(entry, entry.input, attachments);
}

export function updateComposerDraftInput(ownerKey: string, input: Setter<string>) {
  const entry = getOrCreateEntry(ownerKey);
  publish(entry, resolveSetter(input, entry.input), entry.attachments);
}

export function updateComposerDraftAttachments(
  ownerKey: string,
  attachments: Setter<ComposerAttachmentDraft[]>,
) {
  const entry = getOrCreateEntry(ownerKey);
  publish(entry, entry.input, resolveSetter(attachments, entry.attachments));
}

export function clearComposerDraft(ownerKey: string) {
  const entry = store.get(ownerKey);
  if (!entry) return;
  if (entry.input === "" && entry.attachments.length === 0 && entry.snapshot === EMPTY_DRAFT) {
    return;
  }
  publish(entry, "", []);
}

export function resetAllComposerDrafts() {
  for (const entry of store.values()) {
    publish(entry, "", []);
  }
}

/**
 * React hook returning the current draft plus setters. The returned
 * `input` and `attachments` come from the module store so they persist
 * across the <Composer> being unmounted and remounted (e.g. when the
 * user opens the settings page).
 */
export function useComposerDraft(ownerKey: string | null): {
  input: string;
  attachments: ComposerAttachmentDraft[];
  setInput: (next: Setter<string>) => void;
  setAttachments: (next: Setter<ComposerAttachmentDraft[]>) => void;
  reset: () => void;
} {
  const draft = useSyncExternalStore(
    (listener) => (ownerKey ? subscribe(ownerKey, listener) : () => undefined),
    () => (ownerKey ? getSnapshot(ownerKey) : EMPTY_DRAFT),
    () => EMPTY_DRAFT,
  );

  return {
    input: draft.input,
    attachments: draft.attachments,
    setInput: (next: Setter<string>) => {
      if (ownerKey) updateComposerDraftInput(ownerKey, next);
    },
    setAttachments: (next: Setter<ComposerAttachmentDraft[]>) => {
      if (ownerKey) updateComposerDraftAttachments(ownerKey, next);
    },
    reset: () => {
      if (ownerKey) clearComposerDraft(ownerKey);
    },
  };
}
