import { describe, expect, it } from "vitest";
import { resolveKeyboardPad } from "../features/composer/keyboard-inset";

// The composer must land exactly on the keyboard's top edge. These pin the three
// ways a naive lift drifts: an untouched keyboard (pad = full height minus the
// home-indicator strip), Android's `adjustResize` (the window already moved, so
// nothing is owed), and the two combined.

describe("resolveKeyboardPad", () => {
  it("is zero when no keyboard is up", () => {
    expect(resolveKeyboardPad({ keyboardHeight: 0, windowResize: 0, bottomInset: 34 })).toBe(0);
  });

  it("gives the full keyboard height on iOS, minus the home-indicator strip", () => {
    // iPhone: 336pt keyboard, 34pt bottom inset, window untouched. The container
    // bottom already sits 34pt above the window bottom (root SafeAreaView), and
    // the keyboard frame covers that strip too — so 302 lifts the composer onto
    // the keyboard's top edge.
    expect(resolveKeyboardPad({ keyboardHeight: 336, windowResize: 0, bottomInset: 34 })).toBe(302);
  });

  it("gives the full keyboard height on a device without a bottom inset", () => {
    expect(resolveKeyboardPad({ keyboardHeight: 291, windowResize: 0, bottomInset: 0 })).toBe(291);
  });

  it("owes nothing after Android's adjustResize already shrank the window", () => {
    // The window got the whole keyboard height shorter, so the composer is
    // already above the keyboard; padding here is what used to stack a second
    // keyboard-height of empty space under the input.
    expect(resolveKeyboardPad({ keyboardHeight: 320, windowResize: 320, bottomInset: 0 })).toBe(0);
  });

  it("owes nothing when the resize and a dropped bottom inset overlap", () => {
    // Edge-to-edge Android: the container also grew when the nav-bar inset went
    // away, so the measured "resize" overshoots the keyboard. Still zero.
    expect(resolveKeyboardPad({ keyboardHeight: 320, windowResize: 368, bottomInset: 0 })).toBe(0);
  });

  it("pads for the part of the keyboard the window did not take", () => {
    // Partial resize (or adjustNothing): only the leftover overlaps the input.
    expect(resolveKeyboardPad({ keyboardHeight: 320, windowResize: 120, bottomInset: 0 })).toBe(200);
  });

  it("owes the full keyboard when the inset vanished without a resize", () => {
    // iOS with the inset already zeroed while the keyboard is up: the container
    // now reaches the window bottom, so nothing is covered twice.
    expect(resolveKeyboardPad({ keyboardHeight: 336, windowResize: 0, bottomInset: 0 })).toBe(336);
  });

  it("never returns a negative pad", () => {
    expect(resolveKeyboardPad({ keyboardHeight: 100, windowResize: 500, bottomInset: 48 })).toBe(0);
  });
});
