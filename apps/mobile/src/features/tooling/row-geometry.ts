// Horizontal geometry for a tool row on the phone.
//
// A tool row's status bullet is a HANGING marker, exactly like the desktop
// `--tc-bullet-slot` / `--tc-bullet-hang` pair: it sits in the transcript's left
// gutter so the row's TEXT lands on the prose column, instead of the bullet
// consuming the text column's first ~12px and pushing every tool row out of
// line with the messages around it.
//
// Kept free of React Native imports (no `theme`, no components) so the
// invariant is unit-testable — the mobile suite runs in node and cannot render
// components, and this is pure arithmetic.

/// The transcript's side gutter. This is the only place it is defined: the list
/// uses it as its horizontal padding, and the row geometry below is expressed
/// against it.
export const TRANSCRIPT_GUTTER = 24;

/// The row's own horizontal padding. Mirrors `spacing.xs + 2`.
export const ROW_PADDING = 6;

/// Width reserved for the bullet before the row's text starts. The glyph (a
/// `●` at 7px) is drawn at the slot's left edge, so the slot is also the gap.
export const BULLET_SLOT = 12;

/// How far the row's box bleeds into the gutter: far enough that the text lands
/// on the prose column (`rowBoxLeft + BULLET_HANG === TRANSCRIPT_GUTTER`), while
/// the row's pressed background still covers the hanging bullet.
export const BULLET_HANG = BULLET_SLOT + ROW_PADDING;

/// Where a row's pieces land, measured from the screen's left edge. Used by the
/// tests; the components apply the same values as styles.
export function toolRowLayout(gutter: number = TRANSCRIPT_GUTTER) {
  const rowBoxLeft = gutter - BULLET_HANG;
  const textLeft = rowBoxLeft + BULLET_HANG;
  return {
    /// The row's own box (pressed background, hit target).
    rowBoxLeft,
    /// Where the verb/title text starts — must equal `gutter`.
    textLeft,
    bulletLeft: gutter - BULLET_SLOT,
    bulletRight: gutter,
  };
}
// end of file
