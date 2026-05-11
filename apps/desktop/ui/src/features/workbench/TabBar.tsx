import type { TabDescriptor } from "../../types";
import "./TabBar.css";

interface Props {
  tabs: TabDescriptor[];
  activeTabId: string;
  onTabSelect: (id: string) => void;
  onTabClose: (id: string) => void;
  className?: string;
  showWhenSingle?: boolean;
}

export function TabBar({ tabs, activeTabId, onTabSelect, onTabClose, className, showWhenSingle = false }: Props) {
  if (tabs.length <= 1 && !showWhenSingle) return null;

  return (
    <div className={`tab-bar ${className ?? ""}`}>
      {tabs.map((tab) => (
        <div
          key={tab.id}
          className={`tab-item ${tab.id === activeTabId ? "tab-active" : ""}`}
          onClick={() => onTabSelect(tab.id)}
        >
          <span className="tab-label">{tab.label}</span>
          {tab.dirty && <span className="tab-dirty" aria-label="未保存修改" />}
          {tab.type !== "conversation" && (
            <button
              className="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                onTabClose(tab.id);
              }}
            >
              x
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
