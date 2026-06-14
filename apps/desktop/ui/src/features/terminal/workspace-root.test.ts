import { describe, expect, it } from "vitest";
import { sameWorkspaceRoot } from "./workspace-root";

describe("sameWorkspaceRoot", () => {
  it("compares remote workspace keys exactly", () => {
    expect(sameWorkspaceRoot("ssh://devbox/srv/Project", "ssh://devbox/srv/Project")).toBe(true);
    expect(sameWorkspaceRoot("ssh://devbox/srv/Project", "ssh://devbox/srv/project")).toBe(false);
  });

  it("keeps Windows local path compatibility", () => {
    expect(sameWorkspaceRoot("D:\\work\\Kodex", "d:/work/kodex")).toBe(true);
  });
});
