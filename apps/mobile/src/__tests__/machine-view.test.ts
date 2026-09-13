import { describe, it, expect } from "vitest";
import {
  boundDate,
  machineAddress,
  machineLabel,
  machinePhase,
  machinePhaseLabel,
  shortPeerId,
} from "../features/machines/machine-view";
import type { ConnectionState } from "../relay/state-machine";

describe("machineLabel", () => {
  it("prefers a trimmed label when present", () => {
    expect(machineLabel({ label: "  Studio Mac  ", peer_device_id: "abcdef012345" })).toBe(
      "Studio Mac",
    );
  });

  it("falls back to the short peer id when unlabeled", () => {
    expect(machineLabel({ peer_device_id: "abcdef0123456789" })).toBe("PC abcdef0123");
  });

  it("treats a whitespace-only label as missing", () => {
    expect(machineLabel({ label: "   ", peer_device_id: "1234567890" })).toBe("PC 1234567890");
  });
});

describe("shortPeerId", () => {
  it("clips long ids and leaves short ones intact", () => {
    expect(shortPeerId("0123456789abcdef")).toBe("0123456789");
    expect(shortPeerId("short")).toBe("short");
  });
});

describe("machineAddress", () => {
  it("shows the PC's own address, trimmed", () => {
    expect(machineAddress({ peer_ip: "192.168.1.24" })).toBe("192.168.1.24");
    expect(machineAddress({ peer_ip: "  10.0.0.7 " })).toBe("10.0.0.7");
  });

  it("shows nothing when the PC never reported an address", () => {
    // Never the relay host: it is identical for every machine, which is what
    // made two bound PCs render as indistinguishable rows.
    expect(machineAddress({})).toBeNull();
    expect(machineAddress({ peer_ip: "" })).toBeNull();
    expect(machineAddress({ peer_ip: "   " })).toBeNull();
  });
});

describe("boundDate", () => {
  it("returns null for missing or invalid timestamps", () => {
    expect(boundDate(undefined)).toBeNull();
    expect(boundDate(0)).toBeNull();
    expect(boundDate(Number.NaN)).toBeNull();
  });

  it("formats a valid timestamp", () => {
    const formatted = boundDate(Date.parse("2026-07-20T10:00:00Z"));
    expect(formatted).toBeTruthy();
    expect(formatted).not.toBe("Invalid Date");
  });
});

describe("machinePhase", () => {
  const states: ConnectionState[] = [
    "disconnected",
    "connecting",
    "authenticating",
    "paired/e2e",
    "connected",
  ];

  it("only the active + connected machine is green", () => {
    expect(machinePhase(true, "connected")).toBe("connected");
    expect(machinePhase(false, "connected")).toBe("idle");
  });

  it("maps the handshake states to connecting for the active machine", () => {
    for (const state of ["connecting", "authenticating", "paired/e2e"] as ConnectionState[]) {
      expect(machinePhase(true, state)).toBe("connecting");
    }
  });

  it("never reports a bound-but-dropped machine as connected", () => {
    for (const state of states.filter((s) => s !== "connected")) {
      expect(machinePhase(false, state)).toBe("idle");
      expect(machinePhase(true, state)).not.toBe("connected");
    }
    expect(machinePhase(true, "disconnected")).toBe("idle");
  });
});

describe("machinePhaseLabel", () => {
  it("labels every phase", () => {
    expect(machinePhaseLabel("connected")).toBe("已连接");
    expect(machinePhaseLabel("connecting")).toBe("连接中…");
    expect(machinePhaseLabel("idle")).toBe("未连接");
  });
});
