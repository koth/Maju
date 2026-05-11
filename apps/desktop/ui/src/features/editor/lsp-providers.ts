import type * as monaco from "monaco-editor";
import { editorLspRequest } from "../../lib/tauri";

const registeredLanguages = new Set<string>();

type LspPosition = { line: number; character: number };
type LspRange = { start: LspPosition; end: LspPosition };
type LspLocation = { uri: string; range: LspRange; targetUri?: string; targetSelectionRange?: LspRange };
type LspTextEdit = { range: LspRange; newText: string };
type LspCompletionItem = {
  label: string | { label: string };
  kind?: number;
  detail?: string;
  documentation?: string | { value?: string };
  insertText?: string;
  textEdit?: LspTextEdit;
};
type LspDocumentSymbol = {
  name: string;
  kind: number;
  range: LspRange;
  selectionRange?: LspRange;
  children?: LspDocumentSymbol[];
};
type LspSymbolInformation = {
  name: string;
  kind: number;
  location: LspLocation;
};

export function registerLspProviders(monacoInstance: typeof monaco, languageId: string): void {
  if (languageId === "plaintext" || registeredLanguages.has(languageId)) return;
  registeredLanguages.add(languageId);

  monacoInstance.languages.registerHoverProvider(languageId, {
    provideHover(model, position) {
      return editorLspRequest<unknown>(languageId, "textDocument/hover", positionParams(model, position))
        .then((result) => hoverResult(result))
        .catch(() => null);
    },
  });

  monacoInstance.languages.registerCompletionItemProvider(languageId, {
    triggerCharacters: [".", ":", "<", "\"", "'", "/"],
    provideCompletionItems(model, position) {
      return editorLspRequest<unknown>(languageId, "textDocument/completion", positionParams(model, position))
        .then((result) => ({ suggestions: completionItems(result, model, position, monacoInstance) }))
        .catch(() => ({ suggestions: [] }));
    },
  });

  monacoInstance.languages.registerDefinitionProvider(languageId, {
    provideDefinition(model, position) {
      return editorLspRequest<unknown>(languageId, "textDocument/definition", positionParams(model, position))
        .then((result) => locations(result, monacoInstance))
        .catch(() => []);
    },
  });

  monacoInstance.languages.registerReferenceProvider(languageId, {
    provideReferences(model, position) {
      return editorLspRequest<unknown>(languageId, "textDocument/references", {
        ...positionParams(model, position),
        context: { includeDeclaration: true },
      })
        .then((result) => locations(result, monacoInstance))
        .catch(() => []);
    },
  });

  monacoInstance.languages.registerDocumentSymbolProvider(languageId, {
    provideDocumentSymbols(model) {
      return editorLspRequest<unknown>(languageId, "textDocument/documentSymbol", textDocumentParams(model))
        .then((result) => documentSymbols(result, monacoInstance))
        .catch(() => []);
    },
  });

  monacoInstance.languages.registerDocumentFormattingEditProvider(languageId, {
    provideDocumentFormattingEdits(model, options) {
      return editorLspRequest<LspTextEdit[]>(languageId, "textDocument/formatting", {
        ...textDocumentParams(model),
        options: {
          tabSize: options.tabSize,
          insertSpaces: options.insertSpaces,
        },
      })
        .then((result) => textEdits(result ?? [], monacoInstance))
        .catch(() => []);
    },
  });

  monacoInstance.languages.registerDocumentSemanticTokensProvider(languageId, {
    getLegend() {
      return SEMANTIC_TOKEN_LEGEND;
    },
    provideDocumentSemanticTokens(model) {
      return editorLspRequest<{ data?: number[]; resultId?: string }>(
        languageId,
        "textDocument/semanticTokens/full",
        textDocumentParams(model),
      )
        .then((result) => {
          if (!result?.data) return null;
          return {
            data: new Uint32Array(result.data),
            resultId: result.resultId,
          };
        })
        .catch(() => null);
    },
    releaseDocumentSemanticTokens() {},
  });
}

const SEMANTIC_TOKEN_LEGEND = {
  tokenTypes: [
    "namespace",
    "type",
    "class",
    "enum",
    "interface",
    "struct",
    "typeParameter",
    "parameter",
    "variable",
    "property",
    "enumMember",
    "event",
    "function",
    "method",
    "macro",
    "keyword",
    "modifier",
    "comment",
    "string",
    "number",
    "regexp",
    "operator",
  ],
  tokenModifiers: [
    "declaration",
    "definition",
    "readonly",
    "static",
    "deprecated",
    "abstract",
    "async",
    "modification",
    "documentation",
    "defaultLibrary",
  ],
};

function textDocumentParams(model: monaco.editor.ITextModel) {
  return {
    textDocument: {
      uri: model.uri.toString(),
    },
  };
}

function positionParams(model: monaco.editor.ITextModel, position: monaco.Position) {
  return {
    ...textDocumentParams(model),
    position: {
      line: position.lineNumber - 1,
      character: position.column - 1,
    },
  };
}

function hoverResult(result: unknown): monaco.languages.Hover | null {
  const value = markdownContent((result as { contents?: unknown } | null)?.contents);
  if (!value) return null;
  return { contents: [{ value }] };
}

function completionItems(
  result: unknown,
  model: monaco.editor.ITextModel,
  position: monaco.Position,
  monacoInstance: typeof monaco,
): monaco.languages.CompletionItem[] {
  const rawItems = Array.isArray(result)
    ? result
    : Array.isArray((result as { items?: unknown[] } | null)?.items)
      ? (result as { items: unknown[] }).items
      : [];
  const word = model.getWordUntilPosition(position);
  const defaultRange = new monacoInstance.Range(
    position.lineNumber,
    word.startColumn,
    position.lineNumber,
    word.endColumn,
  );
  return rawItems.map((item) => completionItem(item as LspCompletionItem, defaultRange, monacoInstance));
}

function completionItem(
  item: LspCompletionItem,
  defaultRange: monaco.Range,
  monacoInstance: typeof monaco,
): monaco.languages.CompletionItem {
  const label = typeof item.label === "string" ? item.label : item.label.label;
  const editRange = item.textEdit?.range ? range(item.textEdit.range, monacoInstance) : undefined;
  return {
    label,
    kind: completionKind(item.kind, monacoInstance),
    detail: item.detail,
    documentation: markdownContent(item.documentation),
    insertText: item.textEdit?.newText ?? item.insertText ?? label,
    range: editRange ?? defaultRange,
  };
}

function locations(result: unknown, monacoInstance: typeof monaco): monaco.languages.Location[] {
  const values = Array.isArray(result) ? result : result ? [result] : [];
  const parsed: monaco.languages.Location[] = [];
  for (const value of values) {
    const location = value as LspLocation;
    const uri = location.targetUri ?? location.uri;
    const locationRange = location.targetSelectionRange ?? location.range;
    if (!uri || !locationRange) continue;
    parsed.push({
      uri: monacoInstance.Uri.parse(uri),
      range: range(locationRange, monacoInstance),
    });
  }
  return parsed;
}

function documentSymbols(result: unknown, monacoInstance: typeof monaco): monaco.languages.DocumentSymbol[] {
  const values = Array.isArray(result) ? result : [];
  return values
    .map((value) => {
      const symbol = value as LspDocumentSymbol | LspSymbolInformation;
      if ("location" in symbol) {
        return {
          name: symbol.name,
          detail: "",
          kind: symbolKind(symbol.kind, monacoInstance),
          range: range(symbol.location.range, monacoInstance),
          selectionRange: range(symbol.location.range, monacoInstance),
          tags: [],
          children: [],
        };
      }
      return {
        name: symbol.name,
        detail: "",
        kind: symbolKind(symbol.kind, monacoInstance),
        range: range(symbol.range, monacoInstance),
        selectionRange: range(symbol.selectionRange ?? symbol.range, monacoInstance),
        tags: [],
        children: symbol.children ? documentSymbols(symbol.children, monacoInstance) : [],
      };
    });
}

function textEdits(edits: LspTextEdit[], monacoInstance: typeof monaco): monaco.languages.TextEdit[] {
  return edits.map((edit) => ({
    range: range(edit.range, monacoInstance),
    text: edit.newText,
  }));
}

function range(value: LspRange, monacoInstance: typeof monaco): monaco.Range {
  return new monacoInstance.Range(
    value.start.line + 1,
    value.start.character + 1,
    value.end.line + 1,
    value.end.character + 1,
  );
}

function markdownContent(value: unknown): string | undefined {
  if (!value) return undefined;
  if (typeof value === "string") return value;
  if (Array.isArray(value)) return value.map(markdownContent).filter(Boolean).join("\n\n");
  if (typeof value === "object") {
    const objectValue = value as { value?: unknown; language?: unknown };
    if (typeof objectValue.value === "string") return objectValue.value;
  }
  return undefined;
}

function completionKind(kind: number | undefined, monacoInstance: typeof monaco): monaco.languages.CompletionItemKind {
  const map: Record<number, monaco.languages.CompletionItemKind> = {
    1: monacoInstance.languages.CompletionItemKind.Text,
    2: monacoInstance.languages.CompletionItemKind.Method,
    3: monacoInstance.languages.CompletionItemKind.Function,
    4: monacoInstance.languages.CompletionItemKind.Constructor,
    5: monacoInstance.languages.CompletionItemKind.Field,
    6: monacoInstance.languages.CompletionItemKind.Variable,
    7: monacoInstance.languages.CompletionItemKind.Class,
    8: monacoInstance.languages.CompletionItemKind.Interface,
    9: monacoInstance.languages.CompletionItemKind.Module,
    10: monacoInstance.languages.CompletionItemKind.Property,
    12: monacoInstance.languages.CompletionItemKind.Value,
    13: monacoInstance.languages.CompletionItemKind.Enum,
    14: monacoInstance.languages.CompletionItemKind.Keyword,
    15: monacoInstance.languages.CompletionItemKind.Snippet,
    16: monacoInstance.languages.CompletionItemKind.Color,
    17: monacoInstance.languages.CompletionItemKind.File,
    18: monacoInstance.languages.CompletionItemKind.Reference,
  };
  return map[kind ?? 0] ?? monacoInstance.languages.CompletionItemKind.Text;
}

function symbolKind(kind: number | undefined, monacoInstance: typeof monaco): monaco.languages.SymbolKind {
  const map: Record<number, monaco.languages.SymbolKind> = {
    1: monacoInstance.languages.SymbolKind.File,
    2: monacoInstance.languages.SymbolKind.Module,
    3: monacoInstance.languages.SymbolKind.Namespace,
    4: monacoInstance.languages.SymbolKind.Package,
    5: monacoInstance.languages.SymbolKind.Class,
    6: monacoInstance.languages.SymbolKind.Method,
    7: monacoInstance.languages.SymbolKind.Property,
    8: monacoInstance.languages.SymbolKind.Field,
    9: monacoInstance.languages.SymbolKind.Constructor,
    10: monacoInstance.languages.SymbolKind.Enum,
    11: monacoInstance.languages.SymbolKind.Interface,
    12: monacoInstance.languages.SymbolKind.Function,
    13: monacoInstance.languages.SymbolKind.Variable,
    14: monacoInstance.languages.SymbolKind.Constant,
    15: monacoInstance.languages.SymbolKind.String,
    16: monacoInstance.languages.SymbolKind.Number,
    17: monacoInstance.languages.SymbolKind.Boolean,
    18: monacoInstance.languages.SymbolKind.Array,
    23: monacoInstance.languages.SymbolKind.Struct,
  };
  return map[kind ?? 0] ?? monacoInstance.languages.SymbolKind.Variable;
}
