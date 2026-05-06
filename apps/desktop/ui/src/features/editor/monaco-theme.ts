import type { editor } from "monaco-editor";
import type { AppTheme } from "../../types";
import { DEFAULT_APP_THEME, resolveAppTheme } from "../../theme";

interface MonacoPalette {
  foreground: string;
  comment: string;
  keyword: string;
  string: string;
  number: string;
  type: string;
  function: string;
  parameter: string;
  operator: string;
  background: string;
  lineHighlight: string;
  selection: string;
  inactiveSelection: string;
  cursor: string;
  guide: string;
  activeGuide: string;
  widget: string;
  border: string;
  hover: string;
  insert: string;
  remove: string;
}

const palettes: Record<AppTheme, MonacoPalette> = {
  kodex_dark: {
    foreground: "e9effa",
    comment: "627088",
    keyword: "6289ff",
    string: "5ed68f",
    number: "f2c15c",
    type: "56c4cc",
    function: "dcdcaa",
    parameter: "9cdcfe",
    operator: "8b99b2",
    background: "080c14",
    lineHighlight: "101623",
    selection: "233c5c",
    inactiveSelection: "1a2840",
    cursor: "6289ff",
    guide: "242e42",
    activeGuide: "374868",
    widget: "0c111c",
    border: "242e42",
    hover: "151c2b",
    insert: "1fc16b",
    remove: "ff4d5e",
  },
  midnight: {
    foreground: "dce7ff",
    comment: "657898",
    keyword: "7fa2ff",
    string: "73d9a3",
    number: "f0bf68",
    type: "65c9dd",
    function: "e3d99b",
    parameter: "a7d1ff",
    operator: "91a0bc",
    background: "080d18",
    lineHighlight: "101a2c",
    selection: "284570",
    inactiveSelection: "1a2a46",
    cursor: "7fa2ff",
    guide: "233554",
    activeGuide: "3a527c",
    widget: "0b1220",
    border: "233554",
    hover: "142139",
    insert: "2ac77d",
    remove: "ff6470",
  },
  graphite: {
    foreground: "e2e5e9",
    comment: "747b85",
    keyword: "b7c4d8",
    string: "9ecb9e",
    number: "d8ba75",
    type: "9fc8d0",
    function: "d6d0a3",
    parameter: "c0cad8",
    operator: "9aa4b2",
    background: "101112",
    lineHighlight: "1a1b1e",
    selection: "3a414b",
    inactiveSelection: "292c31",
    cursor: "c0c8d2",
    guide: "30343a",
    activeGuide: "4a5058",
    widget: "151619",
    border: "30343a",
    hover: "202226",
    insert: "78b887",
    remove: "d78175",
  },
  forest: {
    foreground: "dce9dc",
    comment: "6c7f70",
    keyword: "99c985",
    string: "78d39a",
    number: "d8bd69",
    type: "78c5b0",
    function: "d1d899",
    parameter: "a7d0aa",
    operator: "8fa392",
    background: "07120f",
    lineHighlight: "102019",
    selection: "294b37",
    inactiveSelection: "1c3428",
    cursor: "a2d49d",
    guide: "21392c",
    activeGuide: "365944",
    widget: "0b1712",
    border: "21392c",
    hover: "15281f",
    insert: "2fbe74",
    remove: "df796c",
  },
};

function createTheme(palette: MonacoPalette): editor.IStandaloneThemeData {
  return {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "", foreground: palette.foreground },
      { token: "comment", foreground: palette.comment, fontStyle: "italic" },
      { token: "comment.doc", foreground: palette.comment, fontStyle: "italic" },
      { token: "keyword", foreground: palette.keyword },
      { token: "storage.type", foreground: palette.keyword },
      { token: "storage.modifier", foreground: palette.keyword, fontStyle: "italic" },
      { token: "string", foreground: palette.string },
      { token: "string.escape", foreground: palette.string },
      { token: "regexp", foreground: palette.remove },
      { token: "number", foreground: palette.number },
      { token: "constant", foreground: palette.number },
      { token: "type", foreground: palette.type },
      { token: "namespace", foreground: palette.type },
      { token: "function", foreground: palette.function },
      { token: "variable", foreground: palette.foreground },
      { token: "variable.parameter", foreground: palette.parameter },
      { token: "variable.language", foreground: palette.keyword, fontStyle: "italic" },
      { token: "operator", foreground: palette.operator },
      { token: "delimiter", foreground: palette.operator },
      { token: "tag", foreground: palette.keyword },
      { token: "attribute", foreground: palette.parameter },
      { token: "markup.heading", foreground: palette.keyword, fontStyle: "bold" },
      { token: "markup.bold", fontStyle: "bold" },
      { token: "markup.italic", fontStyle: "italic" },
      { token: "markup.link", foreground: palette.type, fontStyle: "underline" },
      { token: "markup.raw", foreground: palette.string },
      { token: "lifetime", foreground: palette.remove, fontStyle: "italic" },
      { token: "macro", foreground: palette.type },
      { token: "section", foreground: palette.keyword, fontStyle: "bold" },
    ],
    colors: {
      "editor.background": `#${palette.background}`,
      "editor.foreground": `#${palette.foreground}`,
      "editor.lineHighlightBackground": `#${palette.lineHighlight}`,
      "editor.selectionBackground": `#${palette.selection}`,
      "editor.inactiveSelectionBackground": `#${palette.inactiveSelection}`,
      "editorCursor.foreground": `#${palette.cursor}`,
      "editorWhitespace.foreground": `#${palette.guide}`,
      "editorIndentGuide.background": `#${palette.guide}`,
      "editorIndentGuide.activeBackground": `#${palette.activeGuide}`,
      "editorLineNumber.foreground": `#${palette.comment}`,
      "editorLineNumber.activeForeground": `#${palette.operator}`,
      "editorGutter.background": `#${palette.background}`,
      "editor.wordHighlightBackground": `#${palette.selection}44`,
      "editorBracketMatch.background": `#${palette.selection}44`,
      "editorBracketMatch.border": `#${palette.cursor}44`,
      "scrollbarSlider.background": `#${palette.guide}80`,
      "scrollbarSlider.hoverBackground": `#${palette.activeGuide}`,
      "scrollbarSlider.activeBackground": `#${palette.activeGuide}`,
      "editorWidget.background": `#${palette.widget}`,
      "editorWidget.border": `#${palette.border}`,
      "input.background": `#${palette.lineHighlight}`,
      "input.border": `#${palette.border}`,
      "input.foreground": `#${palette.foreground}`,
      "dropdown.background": `#${palette.widget}`,
      "dropdown.border": `#${palette.border}`,
      "list.activeSelectionBackground": `#${palette.selection}`,
      "list.hoverBackground": `#${palette.hover}`,
      "minimap.background": `#${palette.background}`,
      "diffEditor.insertedTextBackground": `#${palette.insert}66`,
      "diffEditor.removedTextBackground": `#${palette.remove}66`,
      "diffEditor.insertedTextBorder": "#00000000",
      "diffEditor.removedTextBorder": "#00000000",
      "diffEditor.insertedLineBackground": `#${palette.insert}24`,
      "diffEditor.removedLineBackground": `#${palette.remove}24`,
      "diffEditorGutter.insertedLineBackground": `#${palette.insert}cc`,
      "diffEditorGutter.removedLineBackground": `#${palette.remove}cc`,
      "diffEditorOverview.insertedForeground": `#${palette.insert}`,
      "diffEditorOverview.removedForeground": `#${palette.remove}`,
    },
  };
}

export const KODEX_MONACO_THEMES: Record<AppTheme, editor.IStandaloneThemeData> = {
  kodex_dark: createTheme(palettes.kodex_dark),
  midnight: createTheme(palettes.midnight),
  graphite: createTheme(palettes.graphite),
  forest: createTheme(palettes.forest),
};

const MONACO_THEME_NAMES: Record<AppTheme, string> = {
  kodex_dark: "kodex-dark",
  midnight: "kodex-midnight",
  graphite: "kodex-graphite",
  forest: "kodex-forest",
};

let registered = false;

export function registerKodexThemes(monaco: typeof import("monaco-editor")) {
  if (registered) return;
  for (const [appTheme, theme] of Object.entries(KODEX_MONACO_THEMES)) {
    monaco.editor.defineTheme(MONACO_THEME_NAMES[appTheme as AppTheme], theme);
  }
  registered = true;
}

export function monacoThemeForAppTheme(theme: string | null | undefined): string {
  return MONACO_THEME_NAMES[resolveAppTheme(theme ?? DEFAULT_APP_THEME)];
}
