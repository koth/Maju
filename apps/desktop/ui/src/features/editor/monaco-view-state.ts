import type { editor } from "monaco-editor";

const viewStates = new Map<string, editor.ICodeEditorViewState>();

export function saveViewState(
  path: string,
  editorInstance: editor.IStandaloneCodeEditor,
): void {
  const state = editorInstance.saveViewState();
  if (state) {
    viewStates.set(path, state);
  }
}

export function restoreViewState(
  path: string,
  editorInstance: editor.IStandaloneCodeEditor,
): void {
  const state = viewStates.get(path);
  if (state) {
    editorInstance.restoreViewState(state);
  }
}

export function clearViewState(path: string): void {
  viewStates.delete(path);
}

export function clearAllViewStates(): void {
  viewStates.clear();
}
