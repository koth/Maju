import { StyleSheet } from "react-native";

// Shared RN styles for the companion app. Kept minimal and legible; not a
// design system. Mirrors the desktop's dark-first chrome where practical.
export const colors = {
  bg: "#0b0d12",
  surface: "#151922",
  surfaceAlt: "#1d2430",
  border: "#2a3340",
  text: "#e6eaf2",
  textDim: "#9aa4b2",
  accent: "#3b82f6",
  accentDim: "#1e3a8a",
  success: "#16a34a",
  danger: "#dc2626",
  warn: "#d97706",
  mono: "#0b0d12",
} as const;

export const spacing = { xs: 4, sm: 8, md: 12, lg: 16, xl: 24 } as const;

export const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  center: { flex: 1, backgroundColor: colors.bg, alignItems: "center", justifyContent: "center", padding: spacing.lg },
  card: { backgroundColor: colors.surface, borderRadius: 10, padding: spacing.md, marginVertical: spacing.xs, marginHorizontal: spacing.sm, borderWidth: 1, borderColor: colors.border },
  title: { color: colors.text, fontSize: 20, fontWeight: "700", marginBottom: spacing.sm },
  subtitle: { color: colors.textDim, fontSize: 13, marginBottom: spacing.md },
  row: { flexDirection: "row", alignItems: "center" },
  rowBetween: { flexDirection: "row", alignItems: "center", justifyContent: "space-between" },
  text: { color: colors.text, fontSize: 15 },
  textDim: { color: colors.textDim, fontSize: 13 },
  mono: { color: colors.text, fontFamily: "monospace", fontSize: 12 },
  input: { color: colors.text, backgroundColor: colors.surfaceAlt, borderRadius: 8, padding: spacing.md, fontSize: 15, borderWidth: 1, borderColor: colors.border, minHeight: 44 },
  button: { backgroundColor: colors.accent, borderRadius: 8, paddingVertical: spacing.md, paddingHorizontal: spacing.lg, alignItems: "center" },
  buttonDanger: { backgroundColor: colors.danger, borderRadius: 8, paddingVertical: spacing.md, paddingHorizontal: spacing.lg, alignItems: "center" },
  buttonGhost: { borderRadius: 8, paddingVertical: spacing.md, paddingHorizontal: spacing.lg, alignItems: "center", borderWidth: 1, borderColor: colors.border },
  buttonText: { color: "#fff", fontSize: 15, fontWeight: "600" },
  status: { fontSize: 12, color: colors.textDim, marginLeft: spacing.xs },
  badge: { paddingHorizontal: spacing.sm, paddingVertical: spacing.xs, borderRadius: 8, backgroundColor: colors.surfaceAlt },
  sectionHeader: { color: colors.textDim, fontSize: 12, fontWeight: "600", textTransform: "uppercase", marginHorizontal: spacing.md, marginTop: spacing.md, marginBottom: spacing.xs },
});
// end of file
