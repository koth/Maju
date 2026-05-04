import type { editor } from "monaco-editor";

export const KODEX_THEME_NAME = "kodex-dark";

export const kodexDarkTheme: editor.IStandaloneThemeData = {
  base: "vs-dark",
  inherit: true,
  rules: [
    // Base
    { token: "", foreground: "e9effa" },

    // Comments
    { token: "comment", foreground: "627088", fontStyle: "italic" },
    { token: "comment.doc", foreground: "627088", fontStyle: "italic" },

    // Keywords & storage
    { token: "keyword", foreground: "6289ff" },
    { token: "storage.type", foreground: "6289ff" },
    { token: "storage.modifier", foreground: "6289ff", fontStyle: "italic" },

    // Strings & escape sequences
    { token: "string", foreground: "5ed68f" },
    { token: "string.escape", foreground: "8bc4a0" },
    { token: "regexp", foreground: "e88e6e" },

    // Numbers & constants
    { token: "number", foreground: "f2c15c" },
    { token: "constant", foreground: "f2c15c" },

    // Types (classes, interfaces, enums, primitives)
    { token: "type", foreground: "56c4cc" },
    { token: "namespace", foreground: "56c4cc" },

    // Functions
    { token: "function", foreground: "dcdcaa" },

    // Variables & parameters
    { token: "variable", foreground: "e9effa" },
    { token: "variable.parameter", foreground: "9cdcfe" },
    { token: "variable.language", foreground: "6289ff", fontStyle: "italic" },

    // Operators & delimiters
    { token: "operator", foreground: "8b99b2" },
    { token: "delimiter", foreground: "8b99b2" },

    // HTML/JSX tags & attributes
    { token: "tag", foreground: "6289ff" },
    { token: "attribute", foreground: "9cdcfe" },

    // Markup (Markdown)
    { token: "markup.heading", foreground: "6289ff", fontStyle: "bold" },
    { token: "markup.bold", fontStyle: "bold" },
    { token: "markup.italic", fontStyle: "italic" },
    { token: "markup.link", foreground: "56c4cc", fontStyle: "underline" },
    { token: "markup.raw", foreground: "5ed68f" },

    // Rust-specific
    { token: "lifetime", foreground: "e88e6e", fontStyle: "italic" },
    { token: "macro", foreground: "56c4cc" },
    { token: "section", foreground: "6289ff", fontStyle: "bold" },
  ],
  colors: {
    "editor.background": "#080c14",
    "editor.foreground": "#e9effa",
    "editor.lineHighlightBackground": "#101623",
    "editor.selectionBackground": "#233c5c",
    "editor.inactiveSelectionBackground": "#1a2840",
    "editorCursor.foreground": "#6289ff",
    "editorWhitespace.foreground": "#242e42",
    "editorIndentGuide.background": "#242e42",
    "editorIndentGuide.activeBackground": "#374868",
    "editorLineNumber.foreground": "#627088",
    "editorLineNumber.activeForeground": "#8b99b2",
    "editorGutter.background": "#080c14",
    "editor.wordHighlightBackground": "#233c5c44",
    "editorBracketMatch.background": "#233c5c44",
    "editorBracketMatch.border": "#6289ff44",
    "scrollbarSlider.background": "#242e4280",
    "scrollbarSlider.hoverBackground": "#374868",
    "scrollbarSlider.activeBackground": "#374868",
    "editorWidget.background": "#0c111c",
    "editorWidget.border": "#242e42",
    "input.background": "#101623",
    "input.border": "#242e42",
    "input.foreground": "#e9effa",
    "dropdown.background": "#0c111c",
    "dropdown.border": "#242e42",
    "list.activeSelectionBackground": "#233c5c",
    "list.hoverBackground": "#151c2b",
    "minimap.background": "#080c14",
    "diffEditor.insertedTextBackground": "#163a2933",
    "diffEditor.removedTextBackground": "#4a1d2133",
    "diffEditor.insertedLineBackground": "#163a2922",
    "diffEditor.removedLineBackground": "#4a1d2122",
  },
};
