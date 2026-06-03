import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { GlobalChrome } from "./GlobalChrome";
import { fsSearch } from "../../lib/tauri";
import type { WorkspaceDescriptor } from "../../types";

vi.mock("../../lib/tauri", async () => {
  const actual = await vi.importActual<typeof import("../../lib/tauri")>("../../lib/tauri");
  return {
    ...actual,
    fsSearch: vi.fn(),
  };
});

vi.mock("./WindowControls", () => ({
  WindowControls: () => null,
}));

const localWorkspace: WorkspaceDescriptor = {
  id: "local",
  name: "kodex",
  root: "D:\\work\\kodex",
  location: { kind: "local" },
};

const remoteWorkspace: WorkspaceDescriptor = {
  id: "remote",
  name: "project",
  root: "ssh://alice@devbox/srv/project",
  location: {
    kind: "remote_linux",
    ssh_target: "alice@devbox",
    ssh_port: 2222,
    remote_path: "/srv/project",
  },
};

function renderChrome(options: {
  workspace?: WorkspaceDescriptor;
  remoteWorkspace?: boolean;
  onToggleTerminal?: () => void;
  onRefreshGit?: () => void;
  onOpenRemoteWorkspace?: () => void;
}) {
  render(
    <GlobalChrome
      workspace={options.workspace ?? localWorkspace}
      remoteWorkspace={options.remoteWorkspace ?? false}
      sidebarCollapsed={false}
      refreshing={false}
      rightPanelCollapsed={false}
      terminalDockVisible={false}
      onToggleSidebar={vi.fn()}
      onToggleTerminal={options.onToggleTerminal ?? vi.fn()}
      onRefreshGit={options.onRefreshGit ?? vi.fn()}
      onToggleRightPanel={vi.fn()}
      onOpenRemoteWorkspace={options.onOpenRemoteWorkspace ?? vi.fn()}
      onFileOpen={vi.fn()}
    />,
  );
}

describe("GlobalChrome", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("disables local-only terminal for remote workspaces but keeps search and git available", () => {
    const onToggleTerminal = vi.fn();
    const onRefreshGit = vi.fn();

    renderChrome({
      workspace: remoteWorkspace,
      remoteWorkspace: true,
      onToggleTerminal,
      onRefreshGit,
    });

    const terminal = screen.getByRole("button", { name: "远程工作区暂不支持本地终端" });
    const search = screen.getByRole("button", { name: "搜索工作区" });
    const git = screen.getByRole("button", { name: "刷新 Git 状态" });

    expect(terminal).toBeDisabled();
    expect(search).not.toBeDisabled();
    expect(git).not.toBeDisabled();

    fireEvent.click(terminal);
    fireEvent.click(search);
    fireEvent.click(git);

    expect(onToggleTerminal).not.toHaveBeenCalled();
    expect(onRefreshGit).toHaveBeenCalledOnce();
    expect(screen.getByPlaceholderText("搜索文件...")).toBeInTheDocument();
    expect(fsSearch).not.toHaveBeenCalled();
  });

  it("opens the remote directory flow from the chrome", () => {
    const onOpenRemoteWorkspace = vi.fn();

    renderChrome({ onOpenRemoteWorkspace });

    fireEvent.click(screen.getByRole("button", { name: "打开远程目录" }));

    expect(onOpenRemoteWorkspace).toHaveBeenCalled();
  });
});
