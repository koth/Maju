import type { editor } from "monaco-editor";
import type { AppTheme } from "../../types";
import { DEFAULT_APP_THEME, resolveAppTheme } from "../../theme";

interface MonacoPalette {
  base: "vs" | "vs-dark";
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

type KodexMonacoThemeData = editor.IStandaloneThemeData & {
  semanticHighlighting?: boolean;
};

const palettes: Record<AppTheme, MonacoPalette> = {
  kodex_dark: {
    base: "vs-dark",
    foreground: "d9d9d9",
    comment: "858585",
    keyword: "ff7bf0",
    string: "a6ff5f",
    number: "a6ff5f",
    type: "ff806f",
    function: "8fd7ff",
    parameter: "d8d6ff",
    operator: "d9d9d9",
    background: "030303",
    lineHighlight: "0f0f0f",
    selection: "2b3f58",
    inactiveSelection: "1d2a3a",
    cursor: "c7d3e0",
    guide: "272c32",
    activeGuide: "3a424c",
    widget: "111315",
    border: "282d33",
    hover: "191d21",
    insert: "1fc16b",
    remove: "ff4d5e",
  },
  midnight: {
    base: "vs-dark",
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
    base: "vs-dark",
    foreground: "d6d6d6",
    comment: "7a7f85",
    keyword: "f078f0",
    string: "a8ff60",
    number: "a8ff60",
    type: "ff7b5c",
    function: "8fd8ff",
    parameter: "d6d6d6",
    operator: "d6d6d6",
    background: "000000",
    lineHighlight: "080808",
    selection: "2d3f56",
    inactiveSelection: "1d2938",
    cursor: "e6e6e6",
    guide: "272727",
    activeGuide: "3a3a3a",
    widget: "0b0b0b",
    border: "242424",
    hover: "111111",
    insert: "78b887",
    remove: "d78175",
  },
  forest: {
    base: "vs-dark",
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
  light: {
    base: "vs",
    foreground: "232528",
    comment: "7a7d82",
    keyword: "6f4eb7",
    string: "23754a",
    number: "8a5f1c",
    type: "2f6d7b",
    function: "4e608c",
    parameter: "4d5158",
    operator: "5f6670",
    background: "f7f7f5",
    lineHighlight: "ececea",
    selection: "d9e1ea",
    inactiveSelection: "e6e8ea",
    cursor: "30343a",
    guide: "d6d7d4",
    activeGuide: "b8bbb7",
    widget: "ffffff",
    border: "d6d7d4",
    hover: "ececea",
    insert: "4fb36d",
    remove: "d56a62",
  },
};

function createTheme(palette: MonacoPalette): KodexMonacoThemeData {
  return {
    base: palette.base,
    inherit: true,
    semanticHighlighting: true,
    rules: [
      { token: "", foreground: palette.foreground },
      { token: "comment", foreground: palette.comment, fontStyle: "italic" },
      { token: "comment.doc", foreground: palette.comment, fontStyle: "italic" },
      { token: "keyword", foreground: palette.keyword },
      { token: "keyword.control", foreground: palette.keyword },
      { token: "keyword.operator", foreground: palette.operator },
      { token: "modifier", foreground: palette.keyword },
      { token: "storage.type", foreground: palette.keyword },
      { token: "storage.modifier", foreground: palette.keyword, fontStyle: "italic" },
      { token: "string", foreground: palette.string },
      { token: "string.escape", foreground: palette.string },
      { token: "regexp", foreground: palette.remove },
      { token: "number", foreground: palette.number },
      { token: "constant", foreground: palette.foreground },
      { token: "constant.language", foreground: palette.foreground },
      { token: "constant.other", foreground: palette.foreground },
      { token: "type", foreground: palette.type },
      { token: "class", foreground: palette.type },
      { token: "enum", foreground: palette.type },
      { token: "interface", foreground: palette.type },
      { token: "struct", foreground: palette.type },
      { token: "typeParameter", foreground: palette.type },
      { token: "namespace", foreground: palette.type },
      { token: "function", foreground: palette.function },
      { token: "method", foreground: palette.function },
      { token: "variable", foreground: palette.foreground },
      { token: "variable.readonly", foreground: palette.foreground },
      { token: "variable.static", foreground: palette.foreground },
      { token: "variable.defaultLibrary", foreground: palette.foreground },
      { token: "property", foreground: palette.foreground },
      { token: "property.readonly", foreground: palette.foreground },
      { token: "enumMember", foreground: palette.number },
      { token: "variable.parameter", foreground: palette.parameter },
      { token: "parameter", foreground: palette.parameter },
      { token: "parameter.declaration", foreground: palette.parameter },
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
  light: createTheme(palettes.light),
};

const MONACO_THEME_NAMES: Record<AppTheme, string> = {
  kodex_dark: "kodex-dark",
  midnight: "kodex-midnight",
  graphite: "kodex-graphite",
  forest: "kodex-forest",
  light: "kodex-light",
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
