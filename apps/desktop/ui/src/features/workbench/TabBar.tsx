import { useCallback, useEffect, useRef, useState } from "react";
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
  const scrollerRef = useRef<HTMLDivElement | null>(null);
  const [canScrollLeft, setCanScrollLeft] = useState(false);
  const [canScrollRight, setCanScrollRight] = useState(false);
  const fixedTabs = tabs.filter((tab) => tab.type === "conversation");
  const scrollTabs = tabs.filter((tab) => tab.type !== "conversation");

  const updateScrollState = useCallback(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    const maxScrollLeft = scroller.scrollWidth - scroller.clientWidth;
    setCanScrollLeft(scroller.scrollLeft > 1);
    setCanScrollRight(scroller.scrollLeft < maxScrollLeft - 1);
  }, []);

  const scrollByPage = useCallback((direction: -1 | 1) => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    const left = direction * Math.max(160, Math.floor(scroller.clientWidth * 0.72));
    if (typeof scroller.scrollBy === "function") {
      scroller.scrollBy({ left, behavior: "smooth" });
    } else {
      scroller.scrollLeft += left;
      updateScrollState();
    }
  }, [updateScrollState]);

  useEffect(() => {
    updateScrollState();
    const scroller = scrollerRef.current;
    if (!scroller) return;

    const handleScroll = () => updateScrollState();
    const resizeObserver =
      typeof ResizeObserver === "undefined"
        ? null
        : new ResizeObserver(updateScrollState);
    scroller.addEventListener("scroll", handleScroll, { passive: true });
    resizeObserver?.observe(scroller);
    requestAnimationFrame(updateScrollState);

    return () => {
      scroller.removeEventListener("scroll", handleScroll);
      resizeObserver?.disconnect();
    };
  }, [scrollTabs.length, updateScrollState]);

  useEffect(() => {
    const scroller = scrollerRef.current;
    const activeTab = scroller?.querySelector<HTMLElement>(".tab-active");
    activeTab?.scrollIntoView({ block: "nearest", inline: "nearest" });
    requestAnimationFrame(updateScrollState);
  }, [activeTabId, updateScrollState]);

  if (tabs.length <= 1 && !showWhenSingle) return null;

  const renderTab = (tab: TabDescriptor) => (
    <div
      key={tab.id}
      className={`tab-item ${tab.type === "conversation" ? "tab-conversation" : ""} ${tab.id === activeTabId ? "tab-active" : ""}`}
      onClick={() => onTabSelect(tab.id)}
      title={tab.label}
    >
      <span className="tab-label">{tab.label}</span>
      {tab.dirty && <span className="tab-dirty" aria-label="未保存修改" />}
      {tab.type !== "conversation" && (
        <button
          className="tab-close"
          type="button"
          aria-label={`关闭 ${tab.label}`}
          title={`关闭 ${tab.label}`}
          onClick={(e) => {
            e.stopPropagation();
            onTabClose(tab.id);
          }}
        >
          ×
        </button>
      )}
    </div>
  );

  return (
    <div className={`tab-bar-shell ${className ?? ""}`}>
      <div className="tab-fixed-section">
        {fixedTabs.map(renderTab)}
      </div>
      {scrollTabs.length > 0 && (
        <>
          <button
            type="button"
            className="tab-scroll-btn"
            aria-label="向左滚动标签"
            disabled={!canScrollLeft}
            onClick={() => scrollByPage(-1)}
          >
            &lt;
          </button>
          <div className="tab-bar" ref={scrollerRef}>
            {scrollTabs.map(renderTab)}
          </div>
          <button
            type="button"
            className="tab-scroll-btn"
            aria-label="向右滚动标签"
            disabled={!canScrollRight}
            onClick={() => scrollByPage(1)}
          >
            &gt;
          </button>
        </>
      )}
    </div>
  );
}
