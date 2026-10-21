# When the browser does not cooperate

Read this after two attempts at the same thing have failed, not before the first.

## The action reported no usable target

- `element N has no DOM node` — the tree has it but there is nothing to send
  input to. It is drawn, not built. Take a screenshot and click the point.
- `element N reported no position` — usually hidden or zero-sized. Observe
  again; if it is still there with no size, it is probably behind a collapsed
  section or a closed menu. Open that first.
- `element N is not in the current page state` — the element is gone. The page
  changed since you read it. Observe again and pick the new number.

## The action ran but nothing happened

Check in this order, and stop when one of them explains it:

1. **A blocker is on top** — a modal, a cookie banner, an overlay. It will be in
   the observation; act on it first.
2. **The field was not the one you thought** — the values in the observation
   show what is actually in each field. Compare.
3. **The click needs a different gesture** — some controls open on
   `double_click`, or accept `press` with `Enter` or `Space` once focused.
4. **The page is waiting on something slow** — one more observation after a
   short wait is reasonable; a third is not.

Do not descend to raw coordinates while a number still exists. A point that
lands on the wrong element produces a real click on a real page.

## The observation looks wrong

- A diff with a long `Removed element IDs` list after a small action means the
  page re-rendered. Ask for `mode: "full"` and read it once; the numbers are
  still the same elements.
- A tree that is mostly `container` lines is a page that builds its controls out
  of generic elements. The names are still there; look at the `text` lines
  around them for what is clickable.
- An empty tree means the page has not loaded or the target is not a document.
  Observe again; if it stays empty, say so instead of guessing.
