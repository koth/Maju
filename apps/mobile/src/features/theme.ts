import { Platform, StyleSheet } from "react-native";

// Maju mobile design system — quiet, near-monochrome, ChatGPT-leaning.
//
// Design rules this file encodes (the old system broke all of them, which is
// what made the UI read as "cards everywhere, no design"):
//
//  1. ONE accent. Blue is reserved for interactive/active state (active tab,
//     focused input, running indicator, primary action). Everything else is
//     neutral. No per-item rainbow.
//  2. No chrome by default. Lists are separated by hairlines and whitespace,
//     NOT by a bordered card per row. A bordered box is an exception, used
//     only for genuinely floating surfaces (sheets, banners).
//  3. Tinted chips are not status. Status is small text or a dot; a tinted
//     pill around every state word was the main source of visual noise.
//  4. Depth comes from luminance steps (bg → surface → surfaceAlt → raised),
//     not from shadows.
//
// Existing keys are preserved (call sites import them by name); `type` and the
// list/sheet primitives at the bottom are the intended way to style new UI.
export const colors = {
  // Canvas and the luminance ladder above it. Each step is small on purpose:
  // big jumps are what make dark UIs look like a pile of cards.
  bg: "#0d0d0d",
  surface: "#141414",
  surfaceAlt: "#1b1b1b",
  surfaceRaised: "#242424",
  // Hairlines. `border` is the default separator; `borderStrong` is for
  // focused/active outlines only.
  border: "#262626",
  borderStrong: "#3a3a3a",
  // Text ladder.
  text: "#ececec",
  textDim: "#a3a3a3",
  textFaint: "#6f6f6f",
  // The single accent. Used sparingly — see rule 1.
  accent: "#5c7cfa",
  accentBright: "#8aa2ff",
  accentDim: "#1a2340",
  accentTint: "rgba(92,124,250,0.14)",
  // Semantic colors, intended for text/dots only (never as chip fills).
  success: "#3fb950",
  successTint: "rgba(63,185,80,0.14)",
  danger: "#f0656b",
  dangerTint: "rgba(240,101,107,0.14)",
  warn: "#d29b3a",
  warnTint: "rgba(210,155,58,0.14)",
  mono: "#0d0d0d",
  // Overlay scrim for sheets/modals.
  scrim: "rgba(0,0,0,0.62)",
} as const;

export const radius = { sm: 8, md: 12, lg: 16, xl: 22, pill: 999 } as const;

export const spacing = { xs: 4, sm: 8, md: 12, lg: 16, xl: 24, xxl: 32 } as const;

// Elevation. Rows and cards get NONE (rule 4) — only surfaces that genuinely
// float above content (sheets, banners, popovers) keep a soft shadow.
export const shadows = {
  // Retained for compatibility: resting rows/cards are flat now.
  card: {},
  glow: {},
  raised: Platform.select({
    ios: { shadowColor: "#000", shadowOpacity: 0.5, shadowRadius: 24, shadowOffset: { width: 0, height: 12 } },
    android: { elevation: 12 },
    default: {},
  }) as object,
} as const;

// Type scale. Sizes/weights/line-heights are defined once so screens stop
// inventing their own (inconsistent type is a big part of "no design sense").
// Named `typeScale` (not `type`) so it can be imported without colliding with
// TypeScript's `import { type X }` modifier syntax.
export const typeScale = {
  /** Screen hero title (settings, pairing). */
  hero: { fontSize: 26, fontWeight: "700" as const, letterSpacing: -0.4 },
  /** Section/group heading. */
  section: { fontSize: 12, fontWeight: "600" as const, letterSpacing: 0.3 },
  /** List row title. */
  row: { fontSize: 15, fontWeight: "500" as const },
  /** Secondary line under a row title. */
  meta: { fontSize: 12, fontWeight: "400" as const },
  /** Dialogue/assistant body. */
  body: { fontSize: 15, fontWeight: "400" as const, lineHeight: 24 },
  /** Small emphasis (chips, buttons). */
  label: { fontSize: 13, fontWeight: "600" as const },
  /** Monospace body (diffs, ids, payloads). */
  mono: { fontSize: 12, fontWeight: "400" as const },
} as const;

// Shared styles. Kept deliberately small: prefer composing these over adding
// another one-off StyleSheet (which is how the old screens drifted apart).
export const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  center: { flex: 1, backgroundColor: colors.bg, alignItems: "center", justifyContent: "center", padding: spacing.xl },

  // Flat surfaces: no border, no shadow. Use for grouped content.
  card: { backgroundColor: colors.surfaceAlt, borderRadius: radius.lg, padding: spacing.lg, marginVertical: spacing.sm, marginHorizontal: spacing.sm },
  // The one floating surface shape (modal/sheet bodies).
  sheet: { backgroundColor: colors.surface, borderTopLeftRadius: radius.xl, borderTopRightRadius: radius.xl },

  title: { color: colors.text, fontSize: 26, fontWeight: "800", letterSpacing: -0.4, marginBottom: spacing.sm },
  subtitle: { color: colors.textDim, fontSize: 14, lineHeight: 20, marginBottom: spacing.lg },
  sectionHeader: { color: colors.textFaint, fontSize: 12, fontWeight: "700", letterSpacing: 0.6, marginHorizontal: spacing.lg, marginTop: spacing.lg, marginBottom: spacing.xs },

  row: { flexDirection: "row", alignItems: "center" },
  rowBetween: { flexDirection: "row", alignItems: "center", justifyContent: "space-between" },

  text: { color: colors.text, fontSize: 15 },
  textDim: { color: colors.textDim, fontSize: 13 },
  textFaint: { color: colors.textFaint, fontSize: 12 },
  mono: { color: colors.text, fontFamily: "monospace", fontSize: 12 },
  status: { fontSize: 12, color: colors.textDim, marginLeft: spacing.xs },

  // Inputs: filled, not outlined (an outline inside a card was double chrome).
  input: { color: colors.text, backgroundColor: colors.surfaceAlt, borderRadius: radius.md, padding: spacing.md, fontSize: 15, minHeight: 46 },

  button: { backgroundColor: colors.accent, borderRadius: radius.pill, paddingVertical: spacing.md, paddingHorizontal: spacing.xl, alignItems: "center", justifyContent: "center" },
  buttonDanger: { backgroundColor: colors.danger, borderRadius: radius.pill, paddingVertical: spacing.md, paddingHorizontal: spacing.xl, alignItems: "center", justifyContent: "center" },
  buttonGhost: { backgroundColor: colors.surfaceAlt, borderRadius: radius.pill, paddingVertical: spacing.md, paddingHorizontal: spacing.xl, alignItems: "center", justifyContent: "center" },
  buttonText: { color: "#fff", fontSize: 15, fontWeight: "600" },

  badge: { paddingHorizontal: spacing.sm + 2, paddingVertical: spacing.xs, borderRadius: radius.pill, backgroundColor: colors.surfaceAlt },
  // Small tinted pill. Kept for compatibility, but prefer plain text/dots for
  // status (rule 3): pass colors inline only when the tint carries meaning.
  chip: { flexDirection: "row", alignItems: "center", paddingHorizontal: spacing.sm + 2, paddingVertical: 3, borderRadius: radius.pill },

  hairline: { height: StyleSheet.hairlineWidth, backgroundColor: colors.border },
  /** Hairline that starts after a leading avatar/glyph (iOS list look). */
  hairlineInset: { height: StyleSheet.hairlineWidth, backgroundColor: colors.border, marginLeft: spacing.lg },

  avatar: { alignItems: "center", justifyContent: "center", borderRadius: radius.md },
  avatarText: { color: colors.textDim, fontWeight: "600", fontSize: 15 },

  pillButton: { backgroundColor: colors.accent, borderRadius: radius.pill, alignItems: "center", justifyContent: "center" },
});
// end of file
