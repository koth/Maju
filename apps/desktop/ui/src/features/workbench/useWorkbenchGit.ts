import { useCallback, useEffect, useRef, useState } from "react";
import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import type { UiSnapshot } from "../../types";
import { gitRefresh } from "../../lib/tauri";

interface UseWorkbenchGitArgs {
  snapshot: UiSnapshot | null;
  setSnapshot: Dispatch<SetStateAction<UiSnapshot | null>>;
  snapshotRef: MutableRefObject<UiSnapshot | null>;
  workspaceReady: boolean;
}

export function useWorkbenchGit({
  snapshot,
  setSnapshot,
  snapshotRef,
  workspaceReady,
}: UseWorkbenchGitArgs) {
  const [gitRefreshing, setGitRefreshing] = useState(false);
  const [gitHydrated, setGitHydrated] = useState(false);
  // Bumped by `resetGitHydration` so the hydration effect re-runs even when
  // every other dependency (session id, workspace root, connection state) is
  // unchanged — see the comment on `resetGitHydration`.
  const [gitHydrationEpoch, setGitHydrationEpoch] = useState(0);
  const gitRefreshInFlight = useRef(false);
  const gitRefreshPending = useRef(false);
  const gitHydrationKey = useRef(0);

  const resetGitHydration = useCallback(() => {
    // Clearing without re-hydrating is a trap: the effect below only re-runs
    // when its dependencies change, and callers that reset on a no-op switch
    // (creating a chat reuses the empty placeholder session in the chats
    // workspace, clicking the already active session) leave all of them
    // identical. `gitHydrated` then stayed false forever, and every
    // hydration-gated consumer — the review panel's change-set list — showed
    // "暂无文件变化" for a turn whose files the timeline still listed.
    setGitHydrated(false);
    setGitHydrationEpoch((epoch) => epoch + 1);
  }, []);

  const handleRefreshGit = useCallback(async () => {
    if (gitRefreshInFlight.current) {
      gitRefreshPending.current = true;
      return;
    }
    const currentSnapshot = snapshotRef.current;
    const workspaceRoot = currentSnapshot?.workspace.root;
    if (!workspaceRoot || currentSnapshot?.workspace_connected === false) return;
    const requestKey = ++gitHydrationKey.current;
    gitRefreshInFlight.current = true;
    gitRefreshPending.current = false;
    setGitRefreshing(true);
    try {
      const repo = await gitRefresh();
      setSnapshot((prev) => {
        if (!prev || prev.workspace.root !== workspaceRoot || requestKey !== gitHydrationKey.current) {
          return prev;
        }
        return { ...prev, repository: repo };
      });
      if (requestKey === gitHydrationKey.current) {
        setGitHydrated(true);
      }
    } catch {
      // Git status is advisory; keep the previous repository snapshot.
    } finally {
      if (requestKey === gitHydrationKey.current) {
        gitRefreshInFlight.current = false;
        setGitRefreshing(false);
      }
      if (
        gitRefreshPending.current &&
        requestKey === gitHydrationKey.current &&
        snapshotRef.current?.workspace.root === workspaceRoot
      ) {
        gitRefreshPending.current = false;
        void handleRefreshGit();
      }
    }
  }, [setSnapshot, snapshotRef]);

  useEffect(() => {
    const workspaceRoot = snapshot?.workspace.root;
    if (!workspaceReady || !workspaceRoot) return;
    if (snapshot?.workspace_connected === false) {
      setGitHydrated(true);
      setGitRefreshing(false);
      return;
    }

    const requestKey = ++gitHydrationKey.current;
    setGitHydrated(false);
    setGitRefreshing(true);

    let disposed = false;
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        if (disposed || requestKey !== gitHydrationKey.current) return;
        gitRefresh()
          .then((repo) => {
            if (disposed || requestKey !== gitHydrationKey.current) return;
            setSnapshot((prev) =>
              prev && prev.workspace.root === workspaceRoot
                ? { ...prev, repository: repo }
                : prev,
            );
            setGitHydrated(true);
          })
          .catch(() => {
            if (!disposed && requestKey === gitHydrationKey.current) {
              setGitHydrated(true);
            }
          })
          .finally(() => {
            if (!disposed && requestKey === gitHydrationKey.current) {
              setGitRefreshing(false);
            }
          });
      });
    });

    return () => {
      disposed = true;
    };
  }, [
    setSnapshot,
    gitHydrationEpoch,
    snapshot?.session.id,
    snapshot?.workspace.location?.kind,
    snapshot?.workspace.root,
    snapshot?.workspace_connected,
    workspaceReady,
  ]);

  return {
    gitRefreshing,
    gitHydrated,
    handleRefreshGit,
    resetGitHydration,
  };
}
