import { describe, it, expect } from "vitest";
import { humanizePairingError } from "../pairing/pairing-flow";

describe("humanizePairingError", () => {
  it("points at the PC when the relay never saw the scanned code", () => {
    const text = humanizePairingError("invalid or expired pairing code");
    expect(text).toContain("电脑");
    expect(text).toContain("relay");
  });

  it("explains an offline target PC", () => {
    const text = humanizePairingError(
      "PC is not connected to the relay; check the PC shows 已连接 relay, then scan again",
    );
    expect(text).toContain("relay");
    expect(text).not.toBe(
      "PC is not connected to the relay; check the PC shows 已连接 relay, then scan again",
    );
  });

  it("handles legacy resume wording", () => {
    expect(humanizePairingError("paired PC is offline; scan a new code")).toContain("relay");
  });

  it("maps stale/foreign pairing tokens to a re-scan instruction", () => {
    expect(humanizePairingError("pairing token unknown; scan a new code")).toContain("二维码");
    expect(humanizePairingError("pairing token does not belong to this device")).toContain(
      "二维码",
    );
  });

  it("passes unknown errors through unchanged", () => {
    expect(humanizePairingError("some brand new relay failure")).toBe(
      "some brand new relay failure",
    );
  });
});
