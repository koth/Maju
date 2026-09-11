import { useCallback, useEffect, useRef, useState } from "react";
import {
  Dimensions,
  Keyboard,
  Platform,
  type KeyboardEvent,
  type LayoutChangeEvent,
} from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { resolveKeyboardPad } from "./keyboard-inset";

export interface KeyboardAvoidance {
  /** Bottom padding (dp) the conversation container still needs. */
  pad: number;
  /** Attach to the conversation container's `onLayout`. */
  onLayout: (event: LayoutChangeEvent) => void;
}

// A hidden iOS keyboard keeps its height and just parks its frame below the
// screen bottom, so the frame *position* — not the height — tells visible from
// gone. (`keyboardWillChangeFrame` fires for both, and for every height change
// while it is up, e.g. switching to the emoji keyboard.)
function iosVisibleHeight(event: KeyboardEvent): number {
  const { height, screenY } = event.endCoordinates;
  if (!(height > 0)) return 0;
  if (screenY >= Dimensions.get("screen").height) return 0;
  return height;
}

// Keyboard avoidance for the conversation screen.
//
// This deliberately does NOT use `KeyboardAvoidingView`: that component decides
// how far to lift by mixing the view's *parent-relative* layout frame with the
// keyboard's *window-space* top, then adds `keyboardVerticalOffset` on top —
// which is how the composer ended up floating a header-height above the
// keyboard, and why the leftover offset of 88 from the old in-screen title row
// was so hard to see.
//
// Instead we measure: the keyboard's own height, plus how much shorter the
// conversation container already became (Android's `adjustResize` shrinks the
// window and lifts the composer for free — a second, manual lift on top of that
// is the "input box pushed way too high, big empty gap" bug). The difference is
// the padding still owed. See `resolveKeyboardPad`.
export function useKeyboardAvoidance(): KeyboardAvoidance {
  const insets = useSafeAreaInsets();
  const [keyboardHeight, setKeyboardHeight] = useState(0);
  const [height, setHeight] = useState(0);
  const [baseHeight, setBaseHeight] = useState(0);
  const keyboardHeightRef = useRef(0);

  useEffect(() => {
    const apply = (next: number, animateFrom: KeyboardEvent | null) => {
      if (keyboardHeightRef.current === next) return;
      keyboardHeightRef.current = next;
      // Ride the keyboard's own animation curve so the composer travels with it
      // instead of snapping ahead of the slide-up.
      if (next > 0 && animateFrom) Keyboard.scheduleLayoutAnimation(animateFrom);
      setKeyboardHeight(next);
    };

    const subscriptions =
      Platform.OS === "ios"
        ? [
            // WillChangeFrame covers the show, the hide, and every height change
            // in between (emoji keyboard, undocking); WillShow is kept as a
            // belt-and-braces path since `apply` is idempotent.
            Keyboard.addListener("keyboardWillChangeFrame", (event) => {
              const next = iosVisibleHeight(event);
              apply(next, next > 0 ? event : null);
            }),
            Keyboard.addListener("keyboardWillShow", (event) => {
              const next = iosVisibleHeight(event);
              apply(next, next > 0 ? event : null);
            }),
            Keyboard.addListener("keyboardWillHide", () => apply(0, null)),
          ]
        : [
            Keyboard.addListener("keyboardDidShow", (event) =>
              apply(event.endCoordinates.height, null),
            ),
            Keyboard.addListener("keyboardDidHide", () => apply(0, null)),
          ];

    return () => {
      subscriptions.forEach((subscription) => subscription.remove());
    };
  }, []);

  const onLayout = useCallback((event: LayoutChangeEvent) => {
    const next = event.nativeEvent.layout.height;
    setHeight(next);
    // Freeze the keyboard-free height while the keyboard is up: that baseline is
    // what the window-resize compensation measures against.
    if (keyboardHeightRef.current === 0) setBaseHeight(next);
  }, []);

  const pad = resolveKeyboardPad({
    keyboardHeight,
    windowResize: baseHeight - height,
    bottomInset: insets.bottom,
  });

  return { pad, onLayout };
}
