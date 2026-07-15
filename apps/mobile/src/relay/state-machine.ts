// Connection state machine: disconnected -> connecting -> authenticating ->
// paired/e2e -> connected. Mirrors the spec's mobile-relay-connection states.
export type ConnectionState =
  | "disconnected"
  | "connecting"
  | "authenticating"
  | "paired/e2e"
  | "connected";

export type ConnectionListener = (state: ConnectionState) => void;

export class ConnectionStateMachine {
  private current: ConnectionState = "disconnected";
  private listeners = new Set<ConnectionListener>();

  get state(): ConnectionState {
    return this.current;
  }

  subscribe(listener: ConnectionListener): () => void {
    this.listeners.add(listener);
    listener(this.current);
    return () => this.listeners.delete(listener);
  }

  transition(next: ConnectionState): void {
    if (next === this.current) return;
    this.current = next;
    for (const l of this.listeners) l(next);
  }

  reset(): void {
    this.transition("disconnected");
  }

// trailing newline
}
