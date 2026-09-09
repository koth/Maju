import { useEffect, useRef, useState, type RefObject } from "react";

/** Shared near-viewport registry for deferred markdown hydration. One
 *  IntersectionObserver serves every row: a long-history timeline can hold
 *  thousands of mounted rows, and per-row observers (or eager parsing of
 *  every row on window expansion) is what made browsing old sessions
 *  freeze the UI. Rows hydrate ~1000px before entering the viewport so
 *  scrolling up parses content just-in-time instead of all at once. */
const pending = new Map<Element, () => void>();
let sharedObserver: IntersectionObserver | null = null;

function ensureObserver(): IntersectionObserver | null {
  if (typeof IntersectionObserver === "undefined") return null;
  if (sharedObserver) return sharedObserver;
  sharedObserver = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const hydrate = pending.get(entry.target);
        if (hydrate) {
          pending.delete(entry.target);
          sharedObserver?.unobserve(entry.target);
          hydrate();
        }
      }
    },
    { rootMargin: "1000px 0px 1000px 0px" },
  );
  return sharedObserver;
}

/** Sticky once-true flag: becomes (and stays) `true` after `ref` comes within
 *  ~1000px of the viewport. Falls back to eager `true` when IntersectionObserver
 *  is unavailable (jsdom tests, old webviews) or when `enabled` is false —
 *  callers gate on body size so one-liner rows skip the observer round-trip. */
export function useNearViewportOnce(
  ref: RefObject<HTMLElement | null>,
  enabled = true,
): boolean {
  const [near, setNear] = useState(!enabled);
  const hydrated = useRef(!enabled);

  useEffect(() => {
    if (hydrated.current || near) return;
    const element = ref.current;
    if (!element) {
      setNear(true);
      return;
    }
    const observer = ensureObserver();
    if (!observer) {
      setNear(true);
      return;
    }
    pending.set(element, () => {
      hydrated.current = true;
      setNear(true);
    });
    observer.observe(element);
    return () => {
      pending.delete(element);
      observer.unobserve(element);
    };
  }, [near, ref]);

  return near;
}
