import { useCallback, useEffect, useState } from "react";
import { View, Text, ActivityIndicator } from "react-native";
import { useAppController, useSnapshot } from "../../app/AppServicesContext";
import { ConversationTimeline } from "./ConversationTimeline";
import { TurnChangesBar } from "./TurnChangesBar";
import { SessionInfoSheet } from "./SessionInfoSheet";
import { Composer } from "../composer/Composer";
import { useKeyboardAvoidance } from "../composer/use-keyboard-avoidance";
import { PermissionApprovalSheet } from "../permission/PermissionApprovalSheet";
import { styles, colors, spacing } from "../theme";

interface Props {
  sessionId: string;
  // Owning workspace root, threaded from the session list. The PC routes the
  // switch to that workspace's app — without it the switch lands on whatever
  // workspace is active on the desktop and a dsh session resume fails with
  // `session-conflict` (cwd mismatch).
  workspaceRoot?: string | null;
}

// Session view: the conversation timeline, the prompt composer (which carries
// the stop button while a turn is running), and an overlay permission sheet.
// Chrome (back navigation, session title) belongs to the native stack header —
// a second in-screen header row only repeated it. The timeline is driven by
// the snapshot reducer so it stays byte-equivalent to the desktop.
export function ConversationScreen({ sessionId, workspaceRoot }: Props) {
  const controller = useAppController();
  const snapshot = useSnapshot();
  const [sendError, setSendError] = useState<string | null>(null);

  const handleSend = useCallback(
    async (text: string) => {
      setSendError(null);
      try {
        await controller.sendPrompt(text);
      } catch (e) {
        setSendError(e instanceof Error ? e.message : String(e));
        throw e;
      }
    },
    [controller],
  );

  const handleCancel = useCallback(async () => {
    await controller.cancel();
  }, [controller]);

  const handleStopTool = useCallback(
    (toolCallId: string) => controller.stopTool(toolCallId),
    [controller],
  );

  useEffect(() => {
    let active = true;
    let fallback: ReturnType<typeof setTimeout> | null = null;
    // Entry sync: the PC pushes a Full snapshot over the event channel as
    // soon as it processes the SwitchSession (its UiUpdated broadcast wakes
    // the relay event source). The switch request itself is always sent —
    // it is idempotent when the session is already active and repairs the
    // active session when the desktop user switched away locally. Only when
    // the store was actually wiped (cross-session entry) AND the push has
    // not landed within a short window — older PC build or a dead event
    // stream — do we pay for an explicit (duplicate) full GetState.
    (async () => {
      try {
        await controller.switchSession(sessionId, workspaceRoot);
        fallback = setTimeout(() => {
          if (!active || controller.snapshot) return;
          void controller
            .getState(sessionId)
            .catch((e: unknown) => {
              if (active) setSendError(e instanceof Error ? e.message : String(e));
            });
        }, 1500);
      } catch (e) {
        if (active) setSendError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      active = false;
      if (fallback !== null) clearTimeout(fallback);
    };
  }, [controller, sessionId, workspaceRoot]);

  const streaming =
    snapshot?.session.status === "Streaming" || snapshot?.session.status === "WaitingForTool";

  // Keyboard avoidance is measured, not guessed: `pad` is whatever bottom
  // padding is still owed after the keyboard's height, the window resize the OS
  // already did for us, and the home-indicator inset are accounted for. Applied
  // to the screen root so the timeline shrinks with the composer, and it is
  // exactly the keyboard's height when nothing else has moved — i.e. the
  // composer's bottom edge lands on the keyboard's top edge with no leftover
  // strip (see features/composer/keyboard-inset.ts).
  const { pad, onLayout } = useKeyboardAvoidance();

  return (
    <View
      style={[styles.screen, pad > 0 ? { paddingBottom: pad } : null]}
      onLayout={onLayout}
    >
      {snapshot ? (
        <ConversationTimeline snapshot={snapshot} onStopTool={handleStopTool} />
      ) : sendError ? (
        <View style={styles.center}>
          <Text style={[styles.text, { color: colors.danger, textAlign: "center" }]}>
            {sendError}
          </Text>
        </View>
      ) : (
        <View style={styles.center}>
          <ActivityIndicator color={colors.accent} />
          <Text style={[styles.textDim, { marginTop: spacing.sm }]}>{"正在同步会话\u2026"}</Text>
        </View>
      )}

      {snapshot ? <TurnChangesBar snapshot={snapshot} /> : null}

      <Composer
        onSend={handleSend}
        disabled={!snapshot}
        error={sendError}
        streaming={streaming}
        onCancel={handleCancel}
      />

      <SessionInfoSheet />
      <PermissionApprovalSheet />
    </View>
  );
}
// end of file
