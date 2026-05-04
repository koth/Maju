import type * as monaco from "monaco-editor";

interface CachedModel {
  model: monaco.editor.ITextModel;
  versionId: number;
}

const models = new Map<string, CachedModel>();

function pathToUri(path: string): string {
  return `file:///${path.replace(/\\/g, "/")}`;
}

function guessLanguage(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    ts: "typescript",
    tsx: "typescriptreact",
    js: "javascript",
    jsx: "javascriptreact",
    rs: "rust",
    json: "json",
    md: "markdown",
    css: "css",
    html: "html",
    toml: "toml",
    yaml: "yaml",
    yml: "yaml",
    py: "python",
    sh: "shell",
    bash: "shell",
    sql: "sql",
    xml: "xml",
    svg: "xml",
  };
  return map[ext] ?? "plaintext";
}

export function getOrCreateModel(
  monacoInstance: typeof monaco,
  path: string,
  content: string,
): monaco.editor.ITextModel {
  const cached = models.get(path);
  if (cached && !cached.model.isDisposed()) {
    const current = cached.model.getValue();
    if (current !== content) {
      cached.model.setValue(content);
    }
    return cached.model;
  }

  const uri = monacoInstance.Uri.parse(pathToUri(path));
  const existing = monacoInstance.editor.getModel(uri);
  if (existing && !existing.isDisposed()) {
    if (existing.getValue() !== content) {
      existing.setValue(content);
    }
    models.set(path, { model: existing, versionId: existing.getVersionId() });
    return existing;
  }

  const language = guessLanguage(path);
  const model = monacoInstance.editor.createModel(content, language, uri);
  models.set(path, { model, versionId: model.getVersionId() });
  return model;
}

export function disposeModel(path: string): void {
  const cached = models.get(path);
  if (cached && !cached.model.isDisposed()) {
    cached.model.dispose();
  }
  models.delete(path);
}

export function disposeAllModels(): void {
  for (const [path, cached] of models) {
    if (!cached.model.isDisposed()) {
      cached.model.dispose();
    }
    models.delete(path);
  }
}
