import type { TabDescriptor } from "../../types";
import "./TabBar.css";

interface Props {
  tabs: TabDescriptor[];
  activeTabId: string;
  onTabSelect: (id: string) => void;
  onTabClose: (id: string) => void;
}

export function TabBar({ tabs, activeTabId, onTabSelect, onTabClose }: Props) {
  if (tabs.length <= 1) return null;

  return (
    <div className="tab-bar">
      {tabs.map((tab) => (
        <div
          key={tab.id}
          className={`tab-item ${tab.id === activeTabId ? "tab-active" : ""}`}
          onClick={() => onTabSelect(tab.id)}
        >
          <span className="tab-label">{tab.label}</span>
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
