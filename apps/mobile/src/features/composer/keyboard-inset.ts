// Pure keyboard geometry for the conversation screen — no React Native imports
// so the mobile test suite (node) can pin the arithmetic.
//
// The composer must end up flush with the keyboard's top edge. Three numbers
// decide how much bottom padding the conversation container still needs:
//
//   keyboardHeight — the raw IME frame height (`endCoordinates.height`).
//   windowResize   — how much shorter the conversation container already got
//                    while the keyboard is up. Android's `adjustResize` shrinks
//                    the whole window, which lifts the composer for free; adding
//                    the full keyboard height on top of that stacks two lifts
//                    and floats the input a keyboard away from the keyboard.
//   bottomInset    — the home-indicator strip the container bottom no longer
//                    covers. It is already spent by the root SafeAreaView and
//                    is part of the keyboard's own height, so it double-counts
//                    if we pad it again.
//
// So: pad = keyboardHeight − windowResize − bottomInset, never negative.
export function resolveKeyboardPad(input: {
  keyboardHeight: number;
  windowResize: number;
  bottomInset: number;
}): number {
  const { keyboardHeight, windowResize, bottomInset } = input;
  if (!(keyboardHeight > 0)) return 0;
  const resize = windowResize > 0 ? windowResize : 0;
  const inset = bottomInset > 0 ? bottomInset : 0;
  return Math.max(0, keyboardHeight - resize - inset);
}
