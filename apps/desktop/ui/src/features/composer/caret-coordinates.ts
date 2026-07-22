// Computes the textarea caret position (relative to the textarea's border
// box) using a mirror <div> that replicates the textarea's wrapping styles.
// Used to anchor the `@` mention popover to the caret so it feels native.
//
// The approach mirrors the well-tested "textarea-caret-position" technique:
// clone every style that affects text layout into an off-screen div, place a
// marker span right after the text up to the caret, and read the marker's
// bounding rect. Because the mirror is positioned at (0,0) of the document,
// marker.getBoundingClientRect() yields viewport coordinates; we subtract the
// textarea rect to get coordinates relative to the textarea.

export interface CaretCoordinates {
  top: number;
  left: number;
  height: number;
}

const CLONABLE_STYLE_PROPERTIES: readonly string[] = [
  "boxSizing",
  "width",
  "height",
  "overflowX",
  "overflowY",
  "borderTopWidth",
  "borderRightWidth",
  "borderBottomWidth",
  "borderLeftWidth",
  "borderStyle",
  "paddingTop",
  "paddingRight",
  "paddingBottom",
  "paddingLeft",
  "fontStyle",
  "fontVariant",
  "fontWeight",
  "fontStretch",
  "fontSize",
  "fontSizeAdjust",
  "lineHeight",
  "fontFamily",
  "textAlign",
  "textTransform",
  "textIndent",
  "textDecoration",
  "letterSpacing",
  "wordSpacing",
  "tabSize",
  "whiteSpace",
  "wordWrap",
  "wordBreak",
];

export function getCaretCoordinates(
  textarea: HTMLTextAreaElement,
  position: number,
): CaretCoordinates | null {
  const doc = textarea.ownerDocument;
  if (!doc) return null;

  const computed = doc.defaultView?.getComputedStyle(textarea);
  if (!computed) return null;

  const mirror = doc.createElement("div");
  mirror.setAttribute("aria-hidden", "true");
  mirror.style.position = "absolute";
  mirror.style.top = "0";
  mirror.style.left = "0";
  mirror.style.visibility = "hidden";
  mirror.style.whiteSpace = "pre-wrap";
  mirror.style.wordWrap = "break-word";

  // Read/write through a plain string record: CSSStyleDeclaration mixes
  // readonly numeric/symbol keys with named string props, which TS refuses
  // to index generically. We only touch the named layout properties listed
  // above, so the record cast is sound.
  const source = computed as unknown as Record<string, string>;
  const target = mirror.style as unknown as Record<string, string>;
  for (const property of CLONABLE_STYLE_PROPERTIES) {
    const value = source[property];
    if (typeof value === "string" && value.length > 0) {
      target[property] = value;
    }
  }

  // Force the mirror to wrap identically to the textarea by matching its
  // content-box width (width minus borders and padding).
  const textareaRect = textarea.getBoundingClientRect();
  mirror.style.width = `${textareaRect.width}px`;

  const text = textarea.value.slice(0, Math.min(position, textarea.value.length));

  const before = doc.createTextNode(text);
  const marker = doc.createElement("span");
  // A zero-width space keeps the span measurable without adding visible
  // width or pushing subsequent text onto a new line.
  marker.textContent = "\u200b";

  mirror.appendChild(before);
  mirror.appendChild(marker);
  doc.body.appendChild(mirror);

  // Measure the marker relative to the mirror itself, not the viewport. The
  // mirror is rendered at (0,0) of the document, so subtracting the textarea's
  // viewport rect would mix two unrelated coordinate systems and produce a
  // negative offset (the mention menu then ends up far to the left of the
  // textarea). The mirror shares the textarea's layout styles, so marker
  // positions relative to the mirror are equivalent to positions inside the
  // textarea.
  const mirrorRect = mirror.getBoundingClientRect();
  const markerRect = marker.getBoundingClientRect();
  doc.body.removeChild(mirror);

  return {
    top: markerRect.top - mirrorRect.top,
    left: markerRect.left - mirrorRect.left,
    height: markerRect.height || parseFloat(computed.lineHeight) || textareaRect.height,
  };
}
