import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import { AppController } from "./services";
import { SecureSecretStore } from "./secure-store";
import type { PendingApproval } from "../session/permission";
import type { ConnectionState } from "../relay/state-machine";
import type { SubscriptionState } from "../account/subscription";
import type { UiSnapshot } from "../types";

// React glue for the AppController. Constructs a single controller backed by
// the Keychain/Keystore SecretStore; screens consume it via the hooks below.
// The controller is framework-agnostic so it stays unit-testable.

const AppServicesContext = createContext<AppController | null>(null);

export function AppServicesProvider({ children }: { children: ReactNode }) {
  const [controller] = useState<AppController>(
    () => new AppController(new SecureSecretStore()),
  );

  useEffect(() => {
    void controller.ensureIdentity();
    return () => {
      void controller.disconnect();
    };
  }, [controller]);

  return (
    <AppServicesContext.Provider value={controller}>
      {children}
    </AppServicesContext.Provider>
  );
}

export function useAppController(): AppController {
  const controller = useContext(AppServicesContext);
  if (!controller) {
    throw new Error("useAppController must be used within <AppServicesProvider>");
  }
  return controller;
}

export function useConnectionState(): ConnectionState {
  const controller = useAppController();
  const [state, setState] = useState<ConnectionState>(controller.connectionState);
  useEffect(
    () => controller.connState.subscribe(setState),
    [controller],
  );
  return state;
}

export function useSnapshot(): UiSnapshot | null {
  const controller = useAppController();
  const [snapshot, setSnapshot] = useState<UiSnapshot | null>(controller.snapshot);
  useEffect(
    () => controller.sessionStore.subscribe(setSnapshot),
    [controller],
  );
  return snapshot;
}

export function usePendingApprovals(): PendingApproval[] {
  const controller = useAppController();
  const [pending, setPending] = useState<PendingApproval[]>(
    controller.pendingApprovals,
  );
  useEffect(() => controller.permissions.subscribe(setPending), [controller]);
  return pending;
}

export function useSubscriptionState(): SubscriptionState {
  const controller = useAppController();
  const [state, setState] = useState<SubscriptionState>(controller.subscriptionState);
  useEffect(() => {
    controller.setSubscriptionListener(setState);
    return () => controller.setSubscriptionListener(() => {});
  }, [controller]);
  return state;
}
// end of file
