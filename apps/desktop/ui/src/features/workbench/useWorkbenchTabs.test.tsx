import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { isModelDirty } from "../editor/monaco-model-registry";
import { useWorkbenchTabs } from "./useWorkbenchTabs";

vi.mock("../../lib/tauri", () => ({
  editorSaveFile: vi.fn(),
  sessionGetChangeSetFileDiff: vi.fn(),
}));

vi.mock("../editor/monaco-model-registry", () => ({
  disposeModel: vi.fn(),
  getModelBaseVersion: vi.fn(),
  getModelValue: vi.fn(),
  isModelDirty: vi.fn(() => false),
  updateModelBase: vi.fn(),
  updateModelBaseVersion: vi.fn(),
}));

describe("useWorkbenchTabs", () => {
  beforeEach(() => {
    vi.mocked(isModelDirty).mockReturnValue(false);
  });

  it("replaces the active untouched editor tab when opening another file", () => {
    const { result } = renderHook(() => useWorkbenchTabs({ onAfterEditorSave: async () => {} }));

    act(() => result.current.handleOpenEditorTab("src/first.ts"));
    act(() => result.current.handleOpenEditorTab("src/second.ts"));

    const editorTabs = result.current.tabs.filter((tab) => tab.type === "editor");
    expect(editorTabs).toHaveLength(1);
    expect(editorTabs[0].filePath).toBe("src/second.ts");
    expect(result.current.activeTab.filePath).toBe("src/second.ts");
  });

  it("keeps an editor tab after the user interacts with it", () => {
    const { result } = renderHook(() => useWorkbenchTabs({ onAfterEditorSave: async () => {} }));

    act(() => result.current.handleOpenEditorTab("src/first.ts"));
    act(() => result.current.handleEditorUserInteraction("src/first.ts"));
    act(() => result.current.handleOpenEditorTab("src/second.ts"));

    const editorTabs = result.current.tabs.filter((tab) => tab.type === "editor");
    expect(editorTabs.map((tab) => tab.filePath)).toEqual(["src/first.ts", "src/second.ts"]);
  });

  it("keeps a dirty editor tab even if it has not been interacted with", () => {
    const { result } = renderHook(() => useWorkbenchTabs({ onAfterEditorSave: async () => {} }));

    act(() => result.current.handleOpenEditorTab("src/first.ts"));
    act(() => result.current.handleEditorDirtyChange("src/first.ts", true));
    act(() => result.current.handleOpenEditorTab("src/second.ts"));

    const editorTabs = result.current.tabs.filter((tab) => tab.type === "editor");
    expect(editorTabs.map((tab) => tab.filePath)).toEqual(["src/first.ts", "src/second.ts"]);
  });
});
