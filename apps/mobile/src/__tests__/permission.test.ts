import { describe, it, expect } from "vitest";
import {
  PermissionApprovalStore,
  isDestructive,
  type PendingApproval,
} from "../session/permission";
import type { ControlClient } from "../session/control-client";
import type { PermissionInputRequest } from "../types";

// The phone is the SOLE approval gate for destructive remote operations.
// These tests cover the default-deny store: surfacing a request, approving,
// denying, and the per-request timeout watchdog (which dismisses without
// resolving — the PC aborts the tool on its own timeout).

interface RecordedResolve {
  permission_request_id: string;
  option_id?: string | null;
  guidance?: string | null;
  input_response?: unknown | null;
}

/** A minimal ControlClient stub that records resolvePermission calls. */
function makeFakeControlClient(): {
  client: Pick<ControlClient, "resolvePermission">;
  calls: RecordedResolve[];
} {
  const calls: RecordedResolve[] = [];
  const client = {
    async resolvePermission(opts: {
      permission_request_id: string;
      option_id?: string | null;
      guidance?: string | null;
      input_response?: unknown | null;
    }): Promise<{ op: "resolve_permission"; request_id: string }> {
      calls.push({ ...opts });
      return { op: "resolve_permission", request_id: "rid" };
    },
  };
  return { client, calls };
}

const REQUEST: PermissionInputRequest = {
  questions: [
    {
      id: "q1",
      header: "Allow write",
      question: "Allow editing src/foo.ts?",
      is_other: false,
      is_secret: false,
      multi_select: false,
      options: [
        { label: "Allow once", description: "allow this edit" },
        { label: "Deny", description: "block this edit" },
      ],
    },
  ],
};

function makeStore(timeoutMs = 10) {
  const fake = makeFakeControlClient();
  const store = new PermissionApprovalStore(
    fake.client as unknown as ControlClient,
    timeoutMs,
  );
  return { store, calls: fake.calls };
}

describe("isDestructive heuristic", () => {
  it("flags edit/write/delete/command/shell kinds", () => {
    expect(isDestructive({ name: "x", kind: "edit" })).toBe(true);
    expect(isDestructive({ name: "x", kind: "Write" })).toBe(true);
    expect(isDestructive({ name: "x", kind: "delete" })).toBe(true);
    expect(isDestructive({ name: "x", kind: "command" })).toBe(true);
    expect(isDestructive({ name: "x", kind: "shell" })).toBe(true);
  });

  it("flags destructive tool names regardless of kind", () => {
    expect(isDestructive({ name: "apply_patch", kind: "" })).toBe(true);
    expect(isDestructive({ name: "run_bash", kind: "" })).toBe(true);
    expect(isDestructive({ name: "edit_file", kind: "" })).toBe(true);
  });

  it("does not flag benign read-only tools", () => {
    expect(isDestructive({ name: "read_file", kind: "read" })).toBe(false);
    expect(isDestructive({ name: "list", kind: "query" })).toBe(false);
  });
});

describe("PermissionApprovalStore", () => {
  it("surfaces a pending approval and notifies subscribers", () => {
    const { store } = makeStore();
    const seen: PendingApproval[][] = [];
    store.subscribe((pending) => seen.push(pending));
    seen.length = 0; // drop the immediate empty snapshot
    store.surface("pr-1", { name: "edit_file", kind: "edit", id: "t1", call_id: "c1" }, REQUEST);
    expect(store.snapshot()).toHaveLength(1);
    expect(seen[0]).toHaveLength(1);
    expect(seen[0][0].permissionRequestId).toBe("pr-1");
    expect(seen[0][0].toolName).toBe("edit_file");
  });

  it("approve sends ResolvePermission with the option id and clears the pending request", async () => {
    const { store, calls } = makeStore();
    store.surface("pr-1", { name: "edit_file", kind: "edit", id: "t1", call_id: "c1" }, REQUEST);
    await store.approve("pr-1", "allow-once", "go ahead");
    expect(calls).toHaveLength(1);
    expect(calls[0]).toMatchObject({
      permission_request_id: "pr-1",
      option_id: "allow-once",
      guidance: "go ahead",
    });
    expect(store.snapshot()).toHaveLength(0);
  });

  it("deny sends ResolvePermission with no option and a guidance message", async () => {
    const { store, calls } = makeStore();
    store.surface("pr-1", { name: "edit_file", kind: "edit", id: "t1", call_id: "c1" }, REQUEST);
    await store.deny("pr-1");
    expect(calls).toHaveLength(1);
    expect(calls[0].option_id).toBeNull();
    expect(typeof calls[0].guidance === "string" && calls[0].guidance!.length > 0).toBe(true);
    expect(store.snapshot()).toHaveLength(0);
  });

  it("timeout watchdog dismisses without resolving (PC aborts on its own timeout)", async () => {
    const { store, calls } = makeStore(5);
    store.surface("pr-1", { name: "edit_file", kind: "edit", id: "t1", call_id: "c1" }, REQUEST);
    expect(store.snapshot()).toHaveLength(1);
    // let the watchdog fire
    await new Promise((r) => setTimeout(r, 30));
    expect(store.snapshot()).toHaveLength(0);
    expect(calls).toHaveLength(0); // no ResolvePermission sent on timeout
  });

  it("approve cancels the pending timeout watchdog", async () => {
    const { store, calls } = makeStore(5);
    store.surface("pr-1", { name: "edit_file", kind: "edit", id: "t1", call_id: "c1" }, REQUEST);
    await store.approve("pr-1", "allow-once", null);
    // wait past the watchdog; nothing extra should happen
    await new Promise((r) => setTimeout(r, 20));
    expect(calls).toHaveLength(1);
    expect(store.snapshot()).toHaveLength(0);
  });

  it("subscribe returns an unsubscribe that stops notifications", () => {
    const { store } = makeStore();
    const seen: PendingApproval[][] = [];
    const unsub = store.subscribe((p) => seen.push(p));
    seen.length = 0;
    unsub();
    store.surface("pr-1", { name: "edit_file", kind: "edit", id: "t1", call_id: "c1" }, REQUEST);
    expect(seen).toHaveLength(0);
  });
});
// end of file
