# Working with the browser

You drive a real browser: what you click is what the user would click.

## The loop

1. `browser_observe` — read the page as a numbered tree. Every line is
   `<number> <role> <name>`, indented with tabs to show nesting.
2. `browser_act` — act on one of those numbers. It observes again by default, so
   the result already tells you what changed.
3. Decide from that: another action, or answer.

Numbers belong to elements, not to lines. An element keeps its number while it
exists, so a number you read earlier still refers to the same element if it is
still on the page — and an action on a number that is gone fails instead of
hitting something else.

## Habits that pay for themselves

- **Act once per call.** The observation that comes back is the cheapest way to
  find out whether it worked. Set `observe: false` only when you are doing
  several actions in a row and the intermediate states are noise.
- **When nothing changed, look before you retry.** A dialog, a cookie banner, or
  a validation message is usually the reason. Repeating the click does not
  dismiss it.
- **Do not navigate to the URL you are already on.** That reloads the page and
  loses anything the user typed. There is no reload call; navigate only when the
  destination is different.
- **One focused navigation beats a grid of guesses.** For a lookup, go straight
  to the obvious page or the site's own search, verify there, and stop. Do not
  iterate URL variants or rewrite the same query.
- **Read the diff, not the page.** After the first observation the result is
  usually two or three lines. That is the whole change; there is nothing else to
  find.
- **Take a screenshot when seeing matters** — layout, charts, "does this look
  right" — and not as a substitute for reading the tree.
- **Stop when the task is done.** Re-verifying through a second surface usually
  costs the user tokens and answers nothing.

## Two ways to aim

- By **number**: everything the accessibility tree exposes. This is the default
  and it is what makes an action checkable.
- By **point** (`x`, `y` from a screenshot): only for what the tree cannot see —
  a canvas, a custom-drawn control, a cross-origin frame. Prefer a number
  whenever one exists; a point is a guess that happens to be aimed.

## Reading the text

- `text field Description: Email, Value: a@b.c` — an input. `Description:` means
  the name came from an attribute (a placeholder or tooltip), not a label.
- `button Sign in, Help: Opens the sign-in page` — `Help:` is the accessible
  description.
- `checkbox Remember me, Value: 1` — checked. A checkbox that is off has no
  `Value`.
- `option (selectable, settable) Net 30` — selected.
- `Removed element IDs: 12-14, 20` in a diff: those elements are gone.
