import type { ReactNode } from "react";

interface Props {
  collapsed: boolean;
  children: ReactNode;
}

export function ThreadSidebarShell({ collapsed, children }: Props) {
  if (collapsed) return null;

  return <aside className="thread-sidebar-shell">{children}</aside>;
}
