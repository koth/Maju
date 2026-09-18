import { describe, expect, it } from "vitest";
import {
  BULLET_HANG,
  BULLET_SLOT,
  ROW_PADDING,
  TRANSCRIPT_GUTTER,
  toolRowLayout,
} from "../features/tooling/row-geometry";

// The phone's tool rows hang their status bullet into the transcript's left
// gutter so the row TEXT lands on the prose column (desktop parity). RN
// components cannot be rendered in this suite, so the arithmetic that makes
// that work is pinned here instead.

describe("mobile tool-row geometry", () => {
  it("puts a row's text on the transcript's prose column", () => {
    const layout = toolRowLayout();

    expect(layout.textLeft).toBe(TRANSCRIPT_GUTTER);
    // The summary row and the tool rows share these constants, so a collapsed
    // group and the rows it stands in for cannot drift apart.
    expect(layout.textLeft).toBe(layout.rowBoxLeft + BULLET_HANG);
  });

  it("keeps the hanging bullet inside the gutter, off the screen edge", () => {
    const layout = toolRowLayout();

    expect(layout.bulletLeft).toBe(TRANSCRIPT_GUTTER - BULLET_SLOT);
    expect(layout.bulletLeft).toBeGreaterThanOrEqual(4);
    expect(layout.bulletRight).toBe(TRANSCRIPT_GUTTER);
    // The bullet must not reach into the text column.
    expect(layout.bulletRight).toBeLessThanOrEqual(layout.textLeft);
  });

  it("derives the hang from the slot and the row padding", () => {
    // If either changes alone the text drifts off the column by exactly that
    // amount — this is the invariant that keeps the three in step.
    expect(BULLET_HANG).toBe(BULLET_SLOT + ROW_PADDING);
  });

  it("leaves a visible gap between the bullet and the text", () => {
    // The `●` glyph is ~4-5px at the 7px bullet size; the slot is what the
    // reader perceives as the gap.
    const glyphWidth = 5;
    expect(BULLET_SLOT - glyphWidth).toBeGreaterThanOrEqual(6);
  });

  it("keeps the row's pressed background over the hanging bullet", () => {
    // The hit target bleeds by the full hang, so it starts left of the bullet.
    const layout = toolRowLayout();
    expect(layout.rowBoxLeft).toBeLessThanOrEqual(layout.bulletLeft);
  });
});
// end of file
