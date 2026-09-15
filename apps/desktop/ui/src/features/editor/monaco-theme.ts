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
  variable: string;
  parameter: string;
  property: string;
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
    variable: "d9d9d9",
    parameter: "d8d6ff",
    property: "8fd7ff",
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
    variable: "dce7ff",
    parameter: "a7d1ff",
    property: "65c9dd",
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
  // Neutral #212121 canvas + a restrained One Dark syntax ramp. Editor chrome
  // (background, guides, selection, widget) stays chroma-free so the code
  // surface matches the rest of the "Quiet Neutral" shell.
  graphite: {
    base: "vs-dark",
    foreground: "f3f3f3",
    comment: "8f8f8f",
    keyword: "c678dd",
    string: "98c379",
    number: "d19a66",
    type: "e5c07b",
    function: "61afef",
    variable: "f3f3f3",
    parameter: "d19a66",
    property: "61afef",
    operator: "abb2bf",
    background: "212121",
    lineHighlight: "2a2a2a",
    selection: "3d3d3d",
    inactiveSelection: "2f2f2f",
    cursor: "f3f3f3",
    guide: "323232",
    activeGuide: "4a4a4a",
    widget: "2f2f2f",
    border: "383838",
    hover: "2a2a2a",
    insert: "57c28a",
    remove: "ef7266",
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
    variable: "dce9dc",
    parameter: "a7d0aa",
    property: "78c5b0",
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
  // White canvas + One Light syntax ramp, with chroma-free editor chrome.
  light: {
    base: "vs",
    foreground: "383a42",
    comment: "a0a1a7",
    keyword: "a626a4",
    string: "50a14f",
    number: "986801",
    type: "c18401",
    function: "4078f2",
    variable: "383a42",
    parameter: "986801",
    property: "4078f2",
    operator: "383a42",
    background: "ffffff",
    lineHighlight: "f2f2f2",
    selection: "d9d9d9",
    inactiveSelection: "ececec",
    cursor: "0d0d0d",
    guide: "ececec",
    activeGuide: "d9d9d9",
    widget: "ffffff",
    border: "e0e0e0",
    hover: "f4f4f4",
    insert: "1a7f4b",
    remove: "c4342b",
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
      { token: "variable", foreground: palette.variable },
      { token: "variable.readonly", foreground: palette.foreground },
      { token: "variable.static", foreground: palette.foreground },
      { token: "variable.defaultLibrary", foreground: palette.foreground },
      { token: "property", foreground: palette.variable },
      { token: "property.readonly", foreground: palette.variable },
      { token: "enumMember", foreground: palette.type },
      // Monaco TypeScript / JSON / LSP semantic-token variants. Without
      // these, semantic tokens (e.g. `keyword.ts`, `type.ts`,
      // `variable.readwrite`) fall through to the built-in theme defaults,
      // which on `vs` (light) is a low-contrast pale grey — the code looks
      // washed out. Map every common variant to the same palette colour as
      // its base token so light mode stays readable.
      //
      // Monarch tokenizers (Monaco's built-in TS/JS/CSS/HTML/etc.) emit
      // language-suffixed tokens such as `keyword.tsx`, `identifier.css`,
      // `string.tsx`, `number.css`, `delimiter.tsx`, `comment.tsx`. Any
      // variant not listed here falls back to the base `vs` theme colour,
      // which is pale blue/red on white — unreadable. Enumerate the common
      // suffixes for every token family so light mode stays high-contrast.
      { token: "keyword.css", foreground: palette.keyword },
      { token: "keyword.ts", foreground: palette.keyword },
      { token: "keyword.tsx", foreground: palette.keyword },
      { token: "keyword.js", foreground: palette.keyword },
      { token: "keyword.jsx", foreground: palette.keyword },
      { token: "keyword.json", foreground: palette.keyword },
      { token: "keyword.flow", foreground: palette.keyword },
      { token: "identifier", foreground: palette.variable },
      { token: "identifier.css", foreground: palette.variable },
      { token: "identifier.ts", foreground: palette.foreground },
      { token: "identifier.tsx", foreground: palette.foreground },
      { token: "identifier.js", foreground: palette.foreground },
      { token: "identifier.jsx", foreground: palette.foreground },
      { token: "type.css", foreground: palette.type },
      { token: "type.ts", foreground: palette.type },
      { token: "type.tsx", foreground: palette.type },
      { token: "type.identifier", foreground: palette.type },
      { token: "typeParameter.ts", foreground: palette.type },
      { token: "class.ts", foreground: palette.type },
      { token: "interface.ts", foreground: palette.type },
      { token: "enum.ts", foreground: palette.type },
      { token: "enumMember.ts", foreground: palette.number },
      { token: "namespace.ts", foreground: palette.type },
      { token: "function.css", foreground: palette.function },
      { token: "function.ts", foreground: palette.function },
      { token: "function.tsx", foreground: palette.function },
      { token: "method.ts", foreground: palette.function },
      { token: "string.css", foreground: palette.string },
      { token: "string.ts", foreground: palette.string },
      { token: "string.tsx", foreground: palette.string },
      { token: "string.js", foreground: palette.string },
      { token: "string.jsx", foreground: palette.string },
      { token: "string.html", foreground: palette.string },
      { token: "number.css", foreground: palette.variable },
      { token: "number.hex", foreground: palette.variable },
      { token: "number.ts", foreground: palette.number },
      { token: "number.tsx", foreground: palette.number },
      { token: "number.js", foreground: palette.number },
      { token: "number.jsx", foreground: palette.number },
      { token: "comment.css", foreground: palette.comment, fontStyle: "italic" },
      { token: "comment.ts", foreground: palette.comment, fontStyle: "italic" },
      { token: "comment.tsx", foreground: palette.comment, fontStyle: "italic" },
      { token: "comment.js", foreground: palette.comment, fontStyle: "italic" },
      { token: "comment.jsx", foreground: palette.comment, fontStyle: "italic" },
      { token: "comment.html", foreground: palette.comment, fontStyle: "italic" },
      { token: "delimiter", foreground: palette.operator },
      { token: "delimiter.css", foreground: palette.operator },
      { token: "delimiter.ts", foreground: palette.operator },
      { token: "delimiter.tsx", foreground: palette.operator },
      { token: "delimiter.js", foreground: palette.operator },
      { token: "delimiter.jsx", foreground: palette.operator },
      { token: "delimiter.html", foreground: palette.operator },
      { token: "delimiter.bracket", foreground: palette.operator },
      { token: "delimiter.parenthesis", foreground: palette.operator },
      { token: "delimiter.angle", foreground: palette.operator },
      { token: "delimiter.square", foreground: palette.operator },
      { token: "delimiter.curly", foreground: palette.operator },
      { token: "tag.css", foreground: palette.variable },
      { token: "tag.html", foreground: palette.keyword },
      { token: "metatag", foreground: palette.keyword },
      { token: "metatag.html", foreground: palette.keyword },
      { token: "attribute.name", foreground: palette.property },
      { token: "attribute.name.css", foreground: palette.property },
      { token: "attribute.name.html", foreground: palette.property },
      { token: "attribute.value", foreground: palette.string },
      { token: "attribute.value.css", foreground: palette.string },
      { token: "attribute.value.html", foreground: palette.string },
      { token: "attribute.value.number", foreground: palette.variable },
      { token: "attribute.value.unit", foreground: palette.variable },
      { token: "attribute.value.hex", foreground: palette.string },
      { token: "variable.ts", foreground: palette.variable },
      { token: "variable.tsx", foreground: palette.variable },
      { token: "variable.readwrite", foreground: palette.foreground },
      { token: "variable.writeonly", foreground: palette.foreground },
      { token: "variable.readonly", foreground: palette.foreground },
      { token: "variable.declaration", foreground: palette.foreground },
      { token: "variable.static", foreground: palette.foreground },
      { token: "variable.defaultLibrary", foreground: palette.keyword, fontStyle: "italic" },
      { token: "variable.parameter", foreground: palette.parameter },
      { token: "invalid", foreground: palette.remove },
      { token: "invalid.deprecated", foreground: palette.remove, fontStyle: "italic" },
      { token: "support.function", foreground: palette.function },
      { token: "support.type", foreground: palette.type },
      { token: "support.class", foreground: palette.type },
      { token: "support.constant", foreground: palette.foreground },
      { token: "support.variable", foreground: palette.foreground },
      { token: "escape", foreground: palette.string },
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
      "editorGlyphMargin.background": `#${palette.background}`,
      "editor.lineHighlightBorder": "#00000000",
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
