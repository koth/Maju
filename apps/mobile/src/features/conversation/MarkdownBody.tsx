import Markdown from "react-native-markdown-display";
import { colors, spacing } from "../theme";

// Renders assistant/user message bodies as markdown on React Native. The
// desktop uses a custom markdown-it renderer; here we use the RN markdown
// component with a dark-first style sheet so output is legible on mobile.
const markdownStyles: Record<string, object> = {
  body: { color: colors.text, fontSize: 15, lineHeight: 22 },
  paragraph: { marginTop: 0, marginBottom: spacing.sm },
  heading1: { color: colors.text, fontSize: 20, fontWeight: "700" },
  heading2: { color: colors.text, fontSize: 17, fontWeight: "700" },
  heading3: { color: colors.text, fontSize: 15, fontWeight: "600" },
  code_inline: {
    color: colors.text,
    backgroundColor: colors.surfaceAlt,
    fontFamily: "monospace",
    fontSize: 13,
  },
  fence: {
    color: colors.text,
    backgroundColor: colors.surfaceAlt,
    fontFamily: "monospace",
    fontSize: 13,
    padding: spacing.sm,
    borderRadius: 6,
    marginTop: spacing.xs,
    marginBottom: spacing.sm,
  },
  bullet_list: { marginVertical: spacing.xs },
  list_item: { marginVertical: 2 },
  strong: { fontWeight: "700" },
  em: { fontStyle: "italic" },
  link: { color: colors.accent },
  blockquote: {
    borderLeftWidth: 3,
    borderLeftColor: colors.border,
    paddingLeft: spacing.sm,
    marginVertical: spacing.sm,
  },
};

export function MarkdownBody({ body }: { body: string }) {
  return <Markdown style={markdownStyles}>{body}</Markdown>;
}
// end of file
