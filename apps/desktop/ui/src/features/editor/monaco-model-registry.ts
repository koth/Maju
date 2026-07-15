import type * as monaco from "monaco-editor";
import type { EditorFileVersion } from "../../types";
import { languageForPath } from "./languages";

interface CachedModel {
  model: monaco.editor.ITextModel;
  baseContent: string;
  baseVersionId: number;
  baseVersion?: EditorFileVersion;
}

const models = new Map<string, CachedModel>();

function pathToUri(path: string): string {
  return `file:///${path.replace(/\\/g, "/")}`;
}

export function getOrCreateModel(
  monacoInstance: typeof monaco,
  path: string,
  content: string,
): monaco.editor.ITextModel {
  const language = languageForPath(path);
  const cached = models.get(path);
  if (cached && !cached.model.isDisposed()) {
    ensureModelLanguage(monacoInstance, cached.model, language);
    const current = cached.model.getValue();
    // Clean models always track the latest disk/base content. Dirty models keep
    // local edits so reopen/switch can restore unsaved work.
    if (current === cached.baseContent) {
      // Update baseline before setValue so sync onChange handlers don't see a
      // temporary dirty state.
      cached.baseContent = content;
      if (current !== content) {
        cached.model.setValue(content);
      }
      cached.baseVersionId = cached.model.getVersionId();
    }
    return cached.model;
  }

  const uri = monacoInstance.Uri.parse(pathToUri(path));
  const existing = monacoInstance.editor.getModel(uri);
  if (existing && !existing.isDisposed()) {
    ensureModelLanguage(monacoInstance, existing, language);
    const current = existing.getValue();
    if (current !== content) {
      existing.setValue(content);
    }
    models.set(path, {
      model: existing,
      baseContent: content,
      baseVersionId: existing.getVersionId(),
    });
    return existing;
  }

  const model = monacoInstance.editor.createModel(content, language, uri);
  models.set(path, {
    model,
    baseContent: content,
    baseVersionId: model.getVersionId(),
  });
  return model;
}

/** Force a clean model onto new disk content and reset the dirty baseline. */
export function replaceCleanModelContent(
  path: string,
  content: string,
  baseVersion?: EditorFileVersion,
): boolean {
  const cached = models.get(path);
  if (!cached || cached.model.isDisposed()) {
    models.delete(path);
    return false;
  }
  if (cached.model.getValue() !== cached.baseContent) {
    return false;
  }
  // Baseline first so sync onChange handlers don't mark the model dirty.
  cached.baseContent = content;
  if (baseVersion) {
    cached.baseVersion = baseVersion;
  }
  if (cached.model.getValue() !== content) {
    cached.model.setValue(content);
  }
  cached.baseVersionId = cached.model.getVersionId();
  return true;
}

function ensureModelLanguage(
  monacoInstance: typeof monaco,
  model: monaco.editor.ITextModel,
  language: string,
): void {
  if (typeof model.getLanguageId === "function" && model.getLanguageId() !== language) {
    monacoInstance.editor.setModelLanguage(model, language);
  }
}

export function getCachedModel(path: string): monaco.editor.ITextModel | null {
  const cached = models.get(path);
  if (!cached || cached.model.isDisposed()) {
    models.delete(path);
    return null;
  }
  return cached.model;
}

export function getModelValue(path: string): string | null {
  return getCachedModel(path)?.getValue() ?? null;
}

export function setModelContent(path: string, content: string): void {
  const cached = models.get(path);
  if (!cached || cached.model.isDisposed()) return;
  if (cached.model.getValue() !== content) {
    cached.model.setValue(content);
  }
}

export function updateModelBase(path: string, content?: string): void {
  const cached = models.get(path);
  if (!cached || cached.model.isDisposed()) return;
  cached.baseContent = content ?? cached.model.getValue();
  cached.baseVersionId = cached.model.getVersionId();
}

export function updateModelBaseVersion(path: string, baseVersion: EditorFileVersion | undefined): void {
  const cached = models.get(path);
  if (!cached || cached.model.isDisposed()) return;
  cached.baseVersion = baseVersion;
}

export function getModelBaseVersion(path: string): EditorFileVersion | undefined {
  const cached = models.get(path);
  if (!cached || cached.model.isDisposed()) {
    models.delete(path);
    return undefined;
  }
  return cached.baseVersion;
}

export function isModelDirty(path: string): boolean {
  const cached = models.get(path);
  if (!cached || cached.model.isDisposed()) {
    models.delete(path);
    return false;
  }
  return cached.model.getValue() !== cached.baseContent;
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
