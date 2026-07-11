import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import {
  clearComposerDraft,
  composerDraftKey,
  resetAllComposerDrafts,
  setComposerDraftAttachments,
  setComposerDraftInput,
  useComposerDraft,
} from "./composer-draft-store";

function makeAttachment(overrides: Partial<{
  id: string;
  name: string;
  displayName: string;
  uri: string;
  path: string;
  kind: "image" | "file" | "workspace_file";
}> = {}) {
  return {
    id: overrides.id ?? "a-1",
    name: overrides.name ?? "demo.ts",
    displayName: overrides.displayName ?? "demo.ts",
    mimeType: "text/plain",
    data: null,
    text: null,
    uri: overrides.uri ?? "file:///demo.ts",
    kind: overrides.kind ?? "workspace_file",
    path: overrides.path ?? "demo.ts",
    startLine: null,
    endLine: null,
    previewUrl: null,
    thumbnailData: null,
    thumbnailMimeType: null,
  };
}

describe("composer-draft-store", () => {
  beforeEach(() => {
    resetAllComposerDrafts();
  });

  afterEach(() => {
    resetAllComposerDrafts();
    cleanup();
  });

  it("starts with an empty draft for an unseen owner", () => {
    const ownerKey = composerDraftKey("/repo", "s-other");
    const { getByTestId } = render(<Probe ownerKey={ownerKey} />);
    expect(getByTestId("input").textContent).toBe("");
    expect(getByTestId("attachment-count").textContent).toBe("0");
  });

  it("preserves typed text across unmount/remount of the consumer", () => {
    const ownerKey = composerDraftKey("/repo", "s-1");
    setComposerDraftInput(ownerKey, "typed text");

    const { getByTestId, unmount } = render(<Probe ownerKey={ownerKey} />);
    expect(getByTestId("input").textContent).toBe("typed text");
    unmount();

    const second = render(<Probe ownerKey={ownerKey} />);
    expect(second.getByTestId("input").textContent).toBe("typed text");
  });

  it("preserves attachments across unmount/remount", () => {
    const ownerKey = composerDraftKey("/repo", "s-1");
    setComposerDraftAttachments(ownerKey, [makeAttachment()]);

    const { getByTestId, unmount } = render(<Probe ownerKey={ownerKey} />);
    expect(getByTestId("attachment-count").textContent).toBe("1");
    unmount();

    const second = render(<Probe ownerKey={ownerKey} />);
    expect(second.getByTestId("attachment-count").textContent).toBe("1");
  });

  it("isolates drafts per workspace root (session id is ignored)", () => {
    // Drafts are keyed by workspace root only on purpose: see the file
    // header in composer-draft-store.ts. Two different "session ids" for
    // the same root must therefore share the draft, otherwise switching
    // workspaces (which can resync the session id) would silently wipe
    // the user's typed text.
    const keyA = composerDraftKey("/repo-a", "s-1");
    const keyAOtherSession = composerDraftKey("/repo-a", "s-2");
    const keyB = composerDraftKey("/repo-b", "s-1");
    setComposerDraftInput(keyA, "typed text");

    const a1 = render(<Probe ownerKey={keyA} />);
    expect(a1.getByTestId("input").textContent).toBe("typed text");
    a1.unmount();

    // Same workspace root, different session id — draft should still be
    // there because the key collapses to the root.
    const aAgain = render(<Probe ownerKey={keyAOtherSession} />);
    expect(aAgain.getByTestId("input").textContent).toBe("typed text");
    aAgain.unmount();

    // Different workspace — independent draft.
    const other = render(<Probe ownerKey={keyB} />);
    expect(other.getByTestId("input").textContent).toBe("");
    other.unmount();
  });

  it("survives a workspace switch round trip even when the session id changes", () => {
    // This mirrors the user-reported flow: type in workspace A, switch to
    // workspace B (where the composer mounts with B's empty draft),
    // switch back to A — the typed text should still be there, even if
    // the snapshot we receive on the way back carries a fresh session id.
    const keyA1 = composerDraftKey("/repo-a", "sess-original");
    setComposerDraftInput(keyA1, "fix the login bug");

    const aFirst = render(<Probe ownerKey={keyA1} />);
    expect(aFirst.getByTestId("input").textContent).toBe("fix the login bug");
    aFirst.unmount();

    const b = render(<Probe ownerKey={composerDraftKey("/repo-b", "sess-b")} />);
    expect(b.getByTestId("input").textContent).toBe("");
    b.unmount();

    // Back to A, but the snapshot we get carries a freshly-issued session
    // id (e.g. the backend resynced). The draft must still be restored.
    const aAgain = render(<Probe ownerKey={composerDraftKey("/repo-a", "sess-resynced")} />);
    expect(aAgain.getByTestId("input").textContent).toBe("fix the login bug");
  });

  it("survives a session switch round trip within the same workspace", () => {
    // Mirrors: in project A, type in session S1, switch to S2 (composer
    // mounts with the workspace's draft), then switch back to S1 — the
    // typed text should still be there.
    const ownerKey = composerDraftKey("/repo-a", "sess-1");
    setComposerDraftInput(ownerKey, "fix the login bug");

    const s1First = render(<Probe ownerKey={ownerKey} />);
    expect(s1First.getByTestId("input").textContent).toBe("fix the login bug");
    s1First.unmount();

    const s2 = render(<Probe ownerKey={composerDraftKey("/repo-a", "sess-2")} />);
    expect(s2.getByTestId("input").textContent).toBe("fix the login bug");
    s2.unmount();

    const s1Again = render(<Probe ownerKey={composerDraftKey("/repo-a", "sess-1")} />);
    expect(s1Again.getByTestId("input").textContent).toBe("fix the login bug");
  });

  it("supports functional updaters like useState", () => {
    const ownerKey = composerDraftKey("/repo", "s-1");
    setComposerDraftAttachments(ownerKey, [makeAttachment({ id: "seed", uri: "file:///seed.ts" })]);

    const { getByTestId, unmount } = render(<Updater ownerKey={ownerKey} />);
    expect(getByTestId("count").textContent).toBe("1");
    act(() => {
      getByTestId("append").click();
    });
    expect(getByTestId("count").textContent).toBe("2");
    unmount();
  });

  it("clearComposerDraft wipes both fields and notifies subscribers", () => {
    const ownerKey = composerDraftKey("/repo", "s-1");
    setComposerDraftInput(ownerKey, "hello");
    setComposerDraftAttachments(ownerKey, [makeAttachment()]);

    const { getByTestId } = render(<Probe ownerKey={ownerKey} />);
    expect(getByTestId("input").textContent).toBe("hello");
    expect(getByTestId("attachment-count").textContent).toBe("1");

    act(() => {
      clearComposerDraft(ownerKey);
    });

    expect(getByTestId("input").textContent).toBe("");
    expect(getByTestId("attachment-count").textContent).toBe("0");
  });

  it("re-renders subscribers when the draft changes", () => {
    // The snapshot reference must flip on each publish so that
    // useSyncExternalStore can detect a change. This test guards that
    // invariant by observing the rendered output across an external
    // mutation.
    const ownerKey = composerDraftKey("/repo", "s-1");
    const { getByTestId } = render(<Probe ownerKey={ownerKey} />);
    expect(getByTestId("input").textContent).toBe("");

    act(() => {
      setComposerDraftInput(ownerKey, "hello");
    });
    expect(getByTestId("input").textContent).toBe("hello");
  });
});

function Probe({ ownerKey }: { ownerKey: string }) {
  const { input, attachments } = useComposerDraft(ownerKey);
  return (
    <div>
      <span data-testid="input">{input}</span>
      <span data-testid="attachment-count">{attachments.length}</span>
    </div>
  );
}

function Updater({ ownerKey }: { ownerKey: string }) {
  const { attachments, setAttachments } = useComposerDraft(ownerKey);
  return (
    <div>
      <span data-testid="count">{attachments.length}</span>
      <button
        type="button"
        data-testid="append"
        onClick={() =>
          setAttachments((current) => [
            ...current,
            makeAttachment({ id: "appended", uri: "file:///appended.ts", path: "appended.ts" }),
          ])
        }
      >
        append
      </button>
    </div>
  );
}
