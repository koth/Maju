import type { BoundDevice } from "../../account/binding";
import type { ConnectionState } from "../../relay/state-machine";

// Pure presentation helpers shared by the machines landing screen and the
// in-app machine switcher on the project page. Deliberately free of React
// Native imports so the logic stays unit-testable (see
// `src/__tests__/machine-view.test.ts`).

/** Short, stable prefix of a PC device id for display fallbacks. */
export function shortPeerId(peerDeviceId: string): string {
  return peerDeviceId.slice(0, 10);
}

/** Friendly machine name; falls back to a short device id when unlabeled.
 * No machine carries a `label` today (the QR payload has no PC name), so the
 * fallback is the normal path — it still keeps rows stable and distinct. */
export function machineLabel(
  device: Pick<BoundDevice, "label" | "peer_device_id">,
): string {
  const label = device.label?.trim();
  if (label && label.length > 0) return label;
  return `PC ${shortPeerId(device.peer_device_id)}`;
}

/**
 * The address to display for a machine, or null when unknown.
 *
 * This is the PC's OWN address as reported in its pairing QR. It must NEVER
 * fall back to the relay host: every bound machine shares one relay, so
 * printing that produced two identical rows ("120.48.49.190" for both) and the
 * list could not be told apart. Records bound before the PC reported an
 * address (or PCs that do not report one) simply show no address.
 */
export function machineAddress(
  device: Pick<BoundDevice, "peer_ip">,
): string | null {
  const ip = device.peer_ip?.trim();
  return ip && ip.length > 0 ? ip : null;
}

/** Bound date (month/day) or null when absent/invalid. */
export function boundDate(boundAt: number | undefined): string | null {
  if (!boundAt) return null;
  const parsed = new Date(boundAt);
  if (Number.isNaN(parsed.getTime())) return null;
  return parsed.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/**
 * Green-dot phase of a machine row. Only the machine the controller is bound
 * to can be anything other than "idle" — a phone holds one connection at a
 * time — and "connected" is reserved for a live connection, so a bound-but-
 * dropped machine never shows the green dot.
 */
export type MachinePhase = "connected" | "connecting" | "idle";

export function machinePhase(
  isActive: boolean,
  state: ConnectionState,
): MachinePhase {
  if (!isActive) return "idle";
  if (state === "connected") return "connected";
  if (state === "disconnected") return "idle";
  return "connecting";
}

/** Row/chip label for a phase. */
export function machinePhaseLabel(phase: MachinePhase): string {
  switch (phase) {
    case "connected":
      return "已连接";
    case "connecting":
      return "连接中…";
    default:
      return "未连接";
  }
}
