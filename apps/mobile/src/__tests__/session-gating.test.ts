import { describe, expect, it } from "vitest";
import { SessionStore } from "../session/store";
import type { UiSnapshot } from "../types";
import type { EventFrame } from "../types/relay-protocol";

// 跨会话同步污染守卫：手机停在会话 A、桌面用户切到会话 B 后，PC 推来的都
// 是 B 的状态（resync 的 GetState 响应、事件 Full、状态帧）。这些内容一旦
// 被收编，就是用户看到的"会话 B 的消息显示进了手机的会话 A"。一律丢弃，
// 并记下 PC 的位置（pcActiveHint）供动作前切回。

function snapshotOf(sessionId: string, revision: number): UiSnapshot {
  return {
    revision,
    session: {
      id: sessionId,
      workspace_id: "w1",
      title: `会话 ${sessionId}`,
      model: "m",
      mode: null,
      agent_cli: null,
      status: "Idle",
    },
    messages: [],
    timeline: [],
    tools: [],
  } as unknown as UiSnapshot;
}

describe("跨会话同步守卫", () => {
  it("setSnapshot 丢弃别的会话的快照，且记下 PC 的位置", () => {
    const store = new SessionStore();
    store.setSnapshot(snapshotOf("A", 1));
    expect(store.state?.session.id).toBe("A");
    expect(store.pcActiveHint).toBe("A");

    // PC 切到 B 后 resync 拉回的是 B 的快照 —— 绝不能覆盖 A。
    store.setSnapshot(snapshotOf("B", 9));
    expect(store.state?.session.id).toBe("A");
    expect(store.state?.revision).toBe(1);
    expect(store.heldSessionId).toBe("A");
    // 但 PC 的位置要记下来：手机动作前据此把 PC 切回来。
    expect(store.pcActiveHint).toBe("B");
  });

  it("snapshot_full 事件同样被丢弃（别的会话）", () => {
    const store = new SessionStore();
    store.beginSession("A");
    store.setSnapshot(snapshotOf("A", 3));

    store.applyEventFrame({
      kind: "snapshot_full",
      snapshot: snapshotOf("B", 40),
    } as unknown as EventFrame);
    expect(store.state?.session.id).toBe("A");
    expect(store.state?.revision).toBe(3);
    expect(store.pcActiveHint).toBe("B");
  });

  it("session_status_changed 只作用于它声明的会话", () => {
    const store = new SessionStore();
    store.setSnapshot(snapshotOf("A", 1));

    store.applyEventFrame({
      kind: "session_status_changed",
      session_id: "B",
      status: "Streaming",
    } as unknown as EventFrame);
    expect(store.state?.session.status).toBe("Idle");
    expect(store.pcActiveHint).toBe("B");

    store.applyEventFrame({
      kind: "session_status_changed",
      session_id: "A",
      status: "Streaming",
    } as unknown as EventFrame);
    expect(store.state?.session.status).toBe("Streaming");
  });

  it("同会话的新快照/新补丁照常应用", () => {
    const store = new SessionStore();
    store.setSnapshot(snapshotOf("A", 1));
    store.setSnapshot(snapshotOf("A", 2));
    expect(store.state?.revision).toBe(2);
    // 同会话的旧快照仍然拒绝（防回退）。
    store.setSnapshot(snapshotOf("A", 1));
    expect(store.state?.revision).toBe(2);
  });

  it("beginSession 后目标会话的快照正常收编", () => {
    const store = new SessionStore();
    store.setSnapshot(snapshotOf("A", 5));
    store.beginSession("B");
    expect(store.state).toBeNull();
    store.setSnapshot(snapshotOf("B", 1));
    expect(store.state?.session.id).toBe("B");
    expect(store.heldSessionId).toBe("B");
  });
});
