import type { IGrammar, IRawGrammar, IOnigLib, StateStack } from "vscode-textmate";
import { Registry, INITIAL, parseRawGrammar } from "vscode-textmate";
import { loadWASM, createOnigScanner, createOnigString } from "vscode-oniguruma";
import { getGrammarInfo, getGrammarInfoByScope } from "./textmate-registry";

let registry: Registry | null = null;
let initPromise: Promise<boolean> | null = null;
let initFailed = false;

const grammarCache = new Map<string, IGrammar>();
const registeredLanguages = new Set<string>();

async function createOnigLib(): Promise<IOnigLib> {
  const wasmResponse = await fetch("/onig.wasm");
  const wasmBinary = await wasmResponse.arrayBuffer();
  await loadWASM({ data: wasmBinary });
  return { createOnigScanner, createOnigString };
}

export async function initTextMate(): Promise<boolean> {
  if (initFailed) return false;
  if (registry) return true;
  if (initPromise) return initPromise;

  initPromise = (async () => {
    try {
      const onigLib = createOnigLib();
      registry = new Registry({
        onigLib,
        async loadGrammar(scopeName: string): Promise<IRawGrammar | null> {
          const info = getGrammarInfoByScope(scopeName);
          if (!info) return null;
          const mod = await info.grammarModule();
          const raw = (mod as Record<string, unknown>).default ?? mod;
          return parseRawGrammar(JSON.stringify(raw), `${scopeName}.json`);
        },
      });
      return true;
    } catch (err) {
      console.warn("TextMate init failed, falling back to Monarch:", err);
      initFailed = true;
      registry = null;
      return false;
    }
  })();

  return initPromise;
}

async function loadGrammar(scopeName: string): Promise<IGrammar | null> {
  const cached = grammarCache.get(scopeName);
  if (cached) return cached;
  if (!registry) return null;

  const grammar = await registry.loadGrammar(scopeName);
  if (grammar) {
    grammarCache.set(scopeName, grammar);
  }
  return grammar;
}

// Scope-to-Monaco-token mapping using longest match
const SCOPE_TOKEN_MAP: [string, string][] = [
  // Most specific first
  ["entity.name.lifetime", "lifetime"],
  ["entity.name.macro", "macro"],
  ["entity.name.function.macro", "macro"],
  ["entity.name.type.class", "type"],
  ["entity.name.type.interface", "type"],
  ["entity.name.type.enum", "type"],
  ["entity.name.type.module", "namespace"],
  ["entity.name.type", "type"],
  ["entity.name.function", "function"],
  ["entity.name.method", "function"],
  ["entity.name.namespace", "namespace"],
  ["entity.name.scope-resolution", "namespace"],
  ["entity.name.tag", "tag"],
  ["entity.name.section", "section"],
  ["entity.other.attribute-name", "attribute"],
  ["entity.other.inherited-class", "type"],
  ["support.type.primitive", "type"],
  ["support.type.builtin", "type"],
  ["support.type", "type"],
  ["support.function", "function"],
  ["support.method", "function"],
  ["support.class", "type"],
  ["support.constant", "constant"],
  ["meta.preprocessor", "keyword"],
  ["meta.object-literal.key", "string"],
  ["storage.type.function", "keyword"],
  ["storage.type", "storage.type"],
  ["storage.modifier", "storage.modifier"],
  ["meta.import keyword", "keyword"],
  ["variable.parameter", "variable.parameter"],
  ["variable.language", "variable.language"],
  ["variable.other.constant", "variable"],
  ["variable.other", "variable"],
  ["variable", "variable"],
  ["constant.numeric", "number"],
  ["constant.language", "constant"],
  ["constant.character.escape", "string.escape"],
  ["constant.other", "constant"],
  ["constant", "constant"],
  ["string.quoted", "string"],
  ["string.template", "string"],
  ["string.regexp", "regexp"],
  ["string", "string"],
  ["comment.block.documentation", "comment.doc"],
  ["comment", "comment"],
  ["keyword.operator.assignment", "operator"],
  ["keyword.operator", "operator"],
  ["keyword.control", "keyword"],
  ["keyword", "keyword"],
  ["punctuation.definition.tag", "delimiter"],
  ["punctuation.separator", "delimiter"],
  ["punctuation", "delimiter"],
  ["markup.heading", "markup.heading"],
  ["markup.bold", "markup.bold"],
  ["markup.italic", "markup.italic"],
  ["markup.underline.link", "markup.link"],
  ["markup.inline.raw", "markup.raw"],
  ["meta.embedded", ""],
];

function scopeToMonacoToken(scopes: string[]): string {
  // Walk scopes from most specific (last) to least specific
  for (let i = scopes.length - 1; i >= 0; i--) {
    const scope = scopes[i];
    for (const [prefix, token] of SCOPE_TOKEN_MAP) {
      if (scope.startsWith(prefix)) {
        return token;
      }
    }
  }
  return "";
}

// State wrapper for Monaco
class TMState {
  constructor(public readonly ruleStack: StateStack) {}
  clone(): TMState {
    return new TMState(this.ruleStack);
  }
  equals(other: TMState): boolean {
    if (!(other instanceof TMState)) return false;
    return this.ruleStack === other.ruleStack;
  }
}

export async function registerTextMateLanguage(
  monaco: typeof import("monaco-editor"),
  languageId: string,
): Promise<void> {
  if (registeredLanguages.has(languageId)) return;
  if (initFailed) return;

  const initialized = registry ? true : await initTextMate();
  if (!initialized || !registry) return;

  const info = getGrammarInfo(languageId);
  if (!info) return;

  const grammar = await loadGrammar(info.scopeName);
  if (!grammar) return;

  const languages = typeof monaco.languages.getLanguages === "function" ? monaco.languages.getLanguages() : [];
  if (!languages.some((language) => language.id === languageId)) {
    monaco.languages.register({ id: languageId });
  }

  registeredLanguages.add(languageId);

  monaco.languages.setTokensProvider(languageId, {
    getInitialState() {
      return new TMState(INITIAL);
    },
    tokenize(line: string, state: TMState) {
      const result = grammar.tokenizeLine(line, state.ruleStack);
      const tokens: { startIndex: number; scopes: string }[] = [];
      for (const tok of result.tokens) {
        tokens.push({
          startIndex: tok.startIndex,
          scopes: scopeToMonacoToken(tok.scopes),
        });
      }
      return {
        tokens,
        endState: new TMState(result.ruleStack),
      };
    },
  } as unknown as import("monaco-editor").languages.TokensProvider);
}
