export interface LanguageGrammarInfo {
  monacoLanguageId: string;
  scopeName: string;
  grammarModule: () => Promise<{ default: unknown }>;
}

const REGISTRY: LanguageGrammarInfo[] = [
  { monacoLanguageId: "typescript", scopeName: "source.ts", grammarModule: () => import("tm-grammars/grammars/typescript.json") },
  { monacoLanguageId: "typescriptreact", scopeName: "source.tsx", grammarModule: () => import("tm-grammars/grammars/tsx.json") },
  { monacoLanguageId: "javascript", scopeName: "source.js", grammarModule: () => import("tm-grammars/grammars/javascript.json") },
  { monacoLanguageId: "javascriptreact", scopeName: "source.js.jsx", grammarModule: () => import("tm-grammars/grammars/jsx.json") },
  { monacoLanguageId: "rust", scopeName: "source.rust", grammarModule: () => import("tm-grammars/grammars/rust.json") },
  { monacoLanguageId: "python", scopeName: "source.python", grammarModule: () => import("tm-grammars/grammars/python.json") },
  { monacoLanguageId: "json", scopeName: "source.json", grammarModule: () => import("tm-grammars/grammars/json.json") },
  { monacoLanguageId: "yaml", scopeName: "source.yaml", grammarModule: () => import("tm-grammars/grammars/yaml.json") },
  { monacoLanguageId: "toml", scopeName: "source.toml", grammarModule: () => import("tm-grammars/grammars/toml.json") },
  { monacoLanguageId: "css", scopeName: "source.css", grammarModule: () => import("tm-grammars/grammars/css.json") },
  { monacoLanguageId: "html", scopeName: "text.html.basic", grammarModule: () => import("tm-grammars/grammars/html.json") },
  { monacoLanguageId: "markdown", scopeName: "text.html.markdown", grammarModule: () => import("tm-grammars/grammars/markdown.json") },
];

const byLanguageId = new Map<string, LanguageGrammarInfo>();
const byScopeName = new Map<string, LanguageGrammarInfo>();
for (const info of REGISTRY) {
  byLanguageId.set(info.monacoLanguageId, info);
  byScopeName.set(info.scopeName, info);
}

export function getGrammarInfo(languageId: string): LanguageGrammarInfo | null {
  return byLanguageId.get(languageId) ?? null;
}

export function getGrammarInfoByScope(scopeName: string): LanguageGrammarInfo | null {
  return byScopeName.get(scopeName) ?? null;
}
