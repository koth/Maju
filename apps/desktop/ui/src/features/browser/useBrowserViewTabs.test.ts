import { describe, expect, it } from "vitest";
import type { BrowserViewTarget } from "../../types";
import type { ReviewPanelOpenTab } from "../review/ReviewPanel";
import { reconcileWebTabs } from "./useBrowserViewTabs";

function target(id: string, url: string, title: string): BrowserViewTarget {
  return { target_id: id, url, title, type: "page" };
}

const fileTab: ReviewPanelOpenTab = { kind: "file", path: "src/a.ts" };
const diffTab: ReviewPanelOpenTab = { kind: "diff", path: "src/b.ts", changeSetId: "cs-1" };

describe("reconcileWebTabs", () => {
  it("appends new targets after non-web tabs", () => {
    const next = reconcileWebTabs([fileTab], [target("t1", "https://a.dev", "A")]);

    expect(next).toEqual([
      fileTab,
      { kind: "web", id: "t1", url: "https://a.dev", title: "A" },
    ]);
  });

  it("refreshes url and title of kept tabs and preserves their order", () => {
    const current: ReviewPanelOpenTab[] = [
      fileTab,
      { kind: "web", id: "t1", url: "https://a.dev", title: "old" },
      { kind: "web", id: "t2", url: "https://b.dev", title: "B" },
    ];

    const next = reconcileWebTabs(current, [
      target("t2", "https://b.dev/v2", "B v2"),
      target("t1", "https://a.dev/next", "A next"),
    ]);

    expect(next).toEqual([
      fileTab,
      { kind: "web", id: "t1", url: "https://a.dev/next", title: "A next" },
      { kind: "web", id: "t2", url: "https://b.dev/v2", title: "B v2" },
    ]);
  });

  it("drops web tabs whose target vanished and keeps the rest", () => {
    const current: ReviewPanelOpenTab[] = [
      diffTab,
      { kind: "web", id: "t1", url: "https://a.dev", title: "A" },
      { kind: "web", id: "t2", url: "https://b.dev", title: "B" },
    ];

    const next = reconcileWebTabs(current, [target("t2", "https://b.dev", "B")]);

    expect(next).toEqual([
      diffTab,
      { kind: "web", id: "t2", url: "https://b.dev", title: "B" },
    ]);
  });

  it("never touches non-web tabs", () => {
    const next = reconcileWebTabs([fileTab, diffTab], []);

    expect(next).toEqual([fileTab, diffTab]);
  });

  it("keeps the last known url when a target reports an empty one", () => {
    const current: ReviewPanelOpenTab[] = [
      { kind: "web", id: "t1", url: "https://a.dev", title: "A" },
    ];

    const next = reconcileWebTabs(current, [target("t1", "", "")]);

    expect(next).toEqual([
      { kind: "web", id: "t1", url: "https://a.dev", title: "A" },
    ]);
  });
});
