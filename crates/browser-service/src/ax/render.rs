//! Turning a captured tree into the text the model reads.
//!
//! The format is copied from the implementation this crate is modelled on,
//! because the model's habits are built around it:
//!
//! ```text
//! 0 AXWebArea Example Domain, URL: example.com/
//! \t1 heading Example Domain
//! \t2 link More information, Description: More information
//! ```
//!
//! Rules that carry meaning:
//!
//! - **Indentation is tabs, one per level**, and it comes *before* the number.
//!   A line always reads "this deep, this element".
//! - **The number belongs to the element, not to the line.** It is assigned when
//!   the element first appears and kept for as long as it exists, so an
//!   insertion above it does not renumber it and a stale number fails loudly
//!   instead of silently addressing a different element. The root is always 0.
//! - **Names are not quoted.** Quotes would have to be escaped in page text,
//!   and escaping costs more than the ambiguity costs.
//! - **A name that Chromium took from an attribute, and every link's name, is
//!   demoted to `Description:`.** Those names are often the URL or a tooltip
//!   rather than what the user reads, and the demotion is how the model can
//!   tell the difference.
//! - **Only what changes a decision is rendered.** `disabled`, `focused`,
//!   `expanded` and heading levels are dropped; a checked checkbox survives as
//!   `Value: 1` because "is it on" is the whole question.
//! - **One word per thing.** Chromium nests an `InlineTextBox` under every
//!   `StaticText` with the same characters, and a button's text child repeats
//!   its own label; both are dropped.

use crate::ax::identity::Numbered;
use crate::ax::wire::{WireDoc, WireNode, truncate};

/// One rendered line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The number the model uses to act on this line. Stable for as long as the
    /// element exists: it is the element's identity, not its position, so an
    /// insertion above it does not renumber it.
    pub index: u64,
    /// Nesting depth; rendered as this many tabs.
    pub depth: usize,
    /// Everything after the number: `button Sign in, Help: Opens the menu`.
    pub body: String,
    /// The DOM node this line stands for, when it has one. This is what an
    /// action addressed by number resolves to; without it a number would only
    /// be a label.
    pub backend: Option<i64>,
}

impl Line {
    /// The line exactly as the model sees it, prefix included.
    pub fn text(&self) -> String {
        format!("{}{} {}", self.indent(), self.index, self.body)
    }

    /// Tabs for this line's depth.
    pub fn indent(&self) -> String {
        "\t".repeat(self.depth)
    }
}

/// A capture, ready to be shown or diffed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rendered {
    pub lines: Vec<Line>,
    /// Elements dropped by the line or character budget.
    pub omitted: usize,
}

impl Rendered {
    /// The whole capture as text, one line per element.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.lines.len() * 48);
        for line in &self.lines {
            out.push_str(&line.text());
            out.push('\n');
        }
        if self.omitted > 0 {
            out.push_str(&format!(
                "… {} more elements omitted\n",
                self.omitted
            ));
        }
        out
    }
}

/// Budgets for one capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// Hard cap on rendered lines.
    pub max_lines: usize,
    /// Hard cap on characters, header included.
    pub max_chars: usize,
    /// Deepest indentation, in levels.
    pub max_depth: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        // The original ships no cap at all; a 400-line page is normal. These
        // are generous ceilings that only fire on genuinely huge documents,
        // because truncation costs the model the part it needed most.
        Self {
            max_lines: 1200,
            max_chars: 48_000,
            max_depth: 32,
        }
    }
}

/// A line before it has been numbered.
///
/// Numbering happens in [`ElementIds`], and it needs to know which nodes will be
/// shown before it can hand out dense numbers — so the walk that decides what to
/// show runs first, without ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    /// Index into `WireDoc::nodes`.
    pub node: usize,
    pub depth: usize,
    pub body: String,
    /// The DOM node behind this line, carried through so a number can later be
    /// turned back into something to click.
    pub backend: Option<i64>,
}

/// Everything a capture wants to show, still unnumbered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    pub pending: Vec<Pending>,
    /// Elements dropped by the line or character budget.
    pub omitted: usize,
}

impl Collected {
    /// The nodes that earned a line, in render order.
    pub fn nodes(&self) -> Vec<usize> {
        self.pending.iter().map(|line| line.node).collect()
    }
}

impl Rendered {
    /// Attach numbers to a collected capture.
    pub fn assemble(collected: Collected, identities: &Numbered) -> Rendered {
        Rendered {
            lines: collected
                .pending
                .into_iter()
                .map(|line| Line {
                    // The element's identity, not its position. The root has no
                    // identity and is always 0.
                    index: identities.ids.get(line.node).copied().flatten().unwrap_or(0),
                    depth: line.depth,
                    body: line.body,
                    backend: line.backend,
                })
                .collect(),
            omitted: collected.omitted,
        }
    }
}

/// Walk the tree and decide what to show.
pub fn collect(doc: &WireDoc, options: RenderOptions) -> Collected {
    let mut collected = Collected::default();
    let mut budget = options.max_chars;

    let mut stack: Vec<(usize, usize)> = doc
        .root
        .into_iter()
        .map(|index| (index, 0usize))
        .collect();
    let mut seen = std::collections::HashSet::new();

    while let Some((index, depth)) = stack.pop() {
        if !seen.insert(index) {
            continue;
        }
        let node = &doc.nodes[index];
        let parent = node.parent.and_then(|parent| doc.nodes.get(parent));
        // Only a node that gets its own line consumes a level: a nameless
        // wrapper is structure, and indenting its children would say something
        // about the page that is not true.
        let describes = describe(node, parent, doc);
        let child_depth = if describes.is_some() && !node.ignored {
            depth + 1
        } else {
            depth
        };

        if let Some(body) = describes {
            if collected.pending.len() < options.max_lines {
                let cost = body.len() + depth + 8;
                if cost <= budget {
                    budget -= cost;
                    collected.pending.push(Pending {
                        node: index,
                        depth: depth.min(options.max_depth),
                        body,
                        backend: node.backend_id,
                    });
                } else {
                    collected.omitted += 1;
                }
            } else {
                collected.omitted += 1;
            }
        }

        for child in node.children.iter().rev() {
            stack.push((*child, child_depth));
        }
    }

    collected
}

/// Render `doc`, numbering it from a fresh table.
///
/// For callers that only want the text (tests, debugging). A live session keeps
/// its own [`ElementIds`] so numbers survive between captures.
pub fn render_text(doc: &WireDoc, options: RenderOptions) -> String {
    let collected = collect(doc, options);
    let mut identities = crate::ax::identity::ElementIds::new();
    let numbered = identities.assign(doc, &collected.nodes());
    Rendered::assemble(collected, &numbered).text()
}

/// The body of a line, or `None` when the node does not earn one.
fn describe(node: &WireNode, parent: Option<&WireNode>, doc: &WireDoc) -> Option<String> {
    if node.ignored {
        return None;
    }
    // Chromium puts an `InlineTextBox` under every `StaticText`, carrying the
    // same characters. Rendering both doubles the page and tells the model
    // nothing; the layout leaf is the one to drop.
    if is_inline_textbox(&node.role) {
        return None;
    }

    let display = role_display(&node.role);
    let is_root = node.role.eq_ignore_ascii_case("RootWebArea")
        || node.role.eq_ignore_ascii_case("WebArea");
    let is_text = is_static_text(&node.role);
    // A root with no accessible name is common; the tab title is the best
    // label the page has, and an unlabelled first line is not worth the token
    // savings.
    let name = if is_root && node.name.is_empty() {
        doc.title.as_str()
    } else {
        node.name.as_str()
    };

    // A text node that repeats its parent's name is the label the parent
    // already carries (`button Help` → `text Help`). The first one is the
    // element; the second is the same word twice.
    if is_text {
        let parent_names_it = parent.is_some_and(|parent| parent.name.trim() == name.trim());
        if name.trim().is_empty() || parent_names_it {
            return None;
        }
    }

    // An element with nothing to say and nothing to act on is structure, not
    // content: the model learns nothing from `generic` on its own.
    if !is_root
        && !is_text
        && name.is_empty()
        && node.value.is_none()
        && node.description.is_none()
        && !node.clickable
        && node.states.is_empty()
    {
        return None;
    }

    let mut body = String::with_capacity(48);
    body.push_str(display);

    if node.states.iter().any(|state| state == "selected") {
        body.push_str(" (selectable, settable)");
    }

    // Where the name lands depends on how trustworthy it is.
    let demote = node.name_source.as_deref() == Some("attribute")
        || node.role.eq_ignore_ascii_case("link");
    if !name.is_empty() && !demote {
        body.push(' ');
        body.push_str(&truncate(name, 400));
    }
    if !name.is_empty() && demote {
        body.push_str(" Description: ");
        body.push_str(&truncate(name, 400));
    }

    if let Some(description) = &node.description {
        body.push_str(", Help: ");
        body.push_str(&truncate(description, 400));
    }
    if let Some(value) = &node.value {
        body.push_str(", Value: ");
        body.push_str(&truncate(value, 400));
    }
    // A checkbox's state is its value; "checked" elsewhere is decoration.
    if node.role.eq_ignore_ascii_case("checkbox")
        && node.states.iter().any(|state| state == "checked")
    {
        body.push_str(", Value: 1");
    }
    if is_root && !doc.url.is_empty() {
        body.push_str(", URL: ");
        body.push_str(&display_url(&doc.url));
    }
    Some(body)
}

/// The role name the model reads.
///
/// Chromium's wire roles are already close to human, but a handful differ and
/// the difference is what the model has learned to expect.
fn role_display(role: &str) -> &str {
    match role {
        "RootWebArea" | "WebArea" => "AXWebArea",
        "textbox" => "text field",
        "searchbox" => "search text field",
        "StaticText" | "InlineTextBox" => "text",
        "img" => "image",
        "generic" => "container",
        "listitem" => "list item",
        other => other,
    }
}

fn is_static_text(role: &str) -> bool {
    role.eq_ignore_ascii_case("StaticText")
}

/// The text-layout leaf under a `StaticText`. It carries the same characters as
/// its parent and is never worth a line.
fn is_inline_textbox(role: &str) -> bool {
    role.eq_ignore_ascii_case("InlineTextBox")
}

/// Drop the scheme and `www.` so the URL costs what the path is worth.
///
/// A line that reads `URL: example.com/path` tells the model where it is; the
/// `https://www.` prefix tells it what it already assumes.
pub fn display_url(url: &str) -> String {
    let without_scheme = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url);
    let without_www = without_scheme
        .strip_prefix("www.")
        .unwrap_or(without_scheme);
    truncate(without_www, 400)
}

/// Does this node earn a line? Useful to callers that need to count them
/// without rendering.
pub fn earns_line(node: &WireNode, doc: &WireDoc) -> bool {
    describe(node, None, doc).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: &str, name: &str, parent: Option<usize>, children: Vec<usize>) -> WireNode {
        WireNode {
            role: role.to_string(),
            name: name.to_string(),
            parent,
            children,
            ..WireNode::default()
        }
    }

    fn doc(nodes: Vec<WireNode>) -> WireDoc {
        WireDoc {
            document_key: "doc-1".to_string(),
            title: "Example Domain".to_string(),
            url: "https://www.example.com/path?q=1".to_string(),
            root: Some(0),
            nodes,
        }
    }

    fn text_for(doc: &WireDoc) -> String {
        render_text(doc, RenderOptions::default())
    }

    #[test]
    fn the_root_line_carries_the_title_and_a_short_url() {
        let doc = doc(vec![
            node("RootWebArea", "Example Domain", None, vec![1]),
            node("heading", "Welcome", Some(0), vec![]),
        ]);
        let text = text_for(&doc);
        assert!(
            text.starts_with("0 AXWebArea Example Domain, URL: example.com/path?q=1\n"),
            "got {text:?}"
        );
    }

    #[test]
    fn the_root_falls_back_to_the_tab_title_when_the_node_has_no_name() {
        let mut root = node("RootWebArea", "", None, vec![1]);
        root.name = String::new();
        let doc = doc(vec![root, node("heading", "Welcome", Some(0), vec![])]);
        let text = text_for(&doc);
        assert!(text.starts_with("0 AXWebArea Example Domain"), "got {text:?}");
    }

    #[test]
    fn indentation_is_one_tab_per_level_and_precedes_the_number() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            node("container", "L1", Some(0), vec![2]),
            node("container", "L2", Some(1), vec![3]),
            node("button", "deep", Some(2), vec![]),
        ]);
        let text = text_for(&doc);
        assert!(text.contains("\n\t1 container L1"), "got {text:?}");
        assert!(text.contains("\n\t\t2 container L2"), "got {text:?}");
        assert!(text.contains("\n\t\t\t3 button deep"), "got {text:?}");
    }

    #[test]
    fn numbers_are_line_addresses_starting_at_zero() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1, 2]),
            node("button", "A", Some(0), vec![]),
            node("button", "B", Some(0), vec![]),
        ]);
        let text = text_for(&doc);
        assert_eq!(
            text,
            "0 AXWebArea Example, URL: example.com/path?q=1\n\
             \t1 button A\n\
             \t2 button B\n"
        );
    }

    #[test]
    fn a_name_taken_from_an_attribute_is_demoted_to_description() {
        let mut textbox = node("textbox", "Email", Some(0), vec![]);
        textbox.name_source = Some("attribute".to_string());
        textbox.value = Some("a@b.c".to_string());
        let doc = doc(vec![node("RootWebArea", "Example", None, vec![1]), textbox]);
        let text = text_for(&doc);
        assert!(
            text.contains("\t1 text field Description: Email, Value: a@b.c"),
            "got {text:?}"
        );
    }

    #[test]
    fn a_link_name_is_always_demoted() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            node("link", "More information", Some(0), vec![]),
        ]);
        let text = text_for(&doc);
        assert!(
            text.contains("\t1 link Description: More information"),
            "got {text:?}"
        );
    }

    #[test]
    fn a_plain_name_stays_in_the_main_position() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            node("button", "Sign in", Some(0), vec![]),
        ]);
        assert!(text_for(&doc).contains("\t1 button Sign in\n"));
    }

    #[test]
    fn description_and_value_are_labelled_in_a_fixed_order() {
        let mut node_with_all = node("textbox", "Coupon", Some(0), vec![]);
        node_with_all.description = Some("D".to_string());
        node_with_all.value = Some("SAVE10".to_string());
        let doc = doc(vec![node("RootWebArea", "Example", None, vec![1]), node_with_all]);
        let text = text_for(&doc);
        assert!(
            text.contains("\t1 text field Coupon, Help: D, Value: SAVE10"),
            "got {text:?}"
        );
    }

    #[test]
    fn a_checked_checkbox_reports_its_value() {
        let mut checkbox = node("checkbox", "Remember me", Some(0), vec![]);
        checkbox.states = vec!["checked".to_string(), "focused".to_string()];
        let doc = doc(vec![node("RootWebArea", "Example", None, vec![1]), checkbox]);
        let text = text_for(&doc);
        assert!(text.contains("\t1 checkbox Remember me, Value: 1"), "got {text:?}");
    }

    #[test]
    fn selected_marks_the_line_as_settable() {
        let mut tab = node("tab", "Inbox", Some(0), vec![]);
        tab.states = vec!["selected".to_string()];
        let doc = doc(vec![node("RootWebArea", "Example", None, vec![1]), tab]);
        assert!(text_for(&doc).contains("\t1 tab (selectable, settable) Inbox"));
    }

    #[test]
    fn states_that_do_not_change_a_decision_are_not_rendered() {
        let mut button = node("button", "Save", Some(0), vec![]);
        button.states = vec![
            "disabled".to_string(),
            "focused".to_string(),
            "expanded".to_string(),
        ];
        button.level = Some(2);
        let doc = doc(vec![node("RootWebArea", "Example", None, vec![1]), button]);
        let text = text_for(&doc);
        assert!(text.contains("\t1 button Save\n"), "got {text:?}");
    }

    #[test]
    fn a_text_node_renders_its_content_after_the_role() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            node("StaticText", "one two three", Some(0), vec![]),
        ]);
        assert!(text_for(&doc).contains("\t1 text one two three"));
    }

    #[test]
    fn the_layout_leaf_under_text_is_not_a_second_line() {
        // Chromium nests `InlineTextBox` under `StaticText` with the same
        // characters; rendering both doubles the page.
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            node("StaticText", "Hello", Some(0), vec![2]),
            node("InlineTextBox", "Hello", Some(1), vec![]),
        ]);
        let text = text_for(&doc);
        assert_eq!(text.matches("Hello").count(), 1, "got {text:?}");
    }

    #[test]
    fn empty_text_nodes_earn_no_line() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1, 2]),
            node("StaticText", "   ", Some(0), vec![]),
            node("button", "Go", Some(0), vec![]),
        ]);
        let text = text_for(&doc);
        assert_eq!(text.lines().count(), 2, "the header and the button: {text:?}");
        assert!(text.contains("\t1 button Go"));
    }

    #[test]
    fn text_that_repeats_its_parents_name_is_not_repeated() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            node("button", "Help", Some(0), vec![2]),
            node("StaticText", "Help", Some(1), vec![]),
        ]);
        let text = text_for(&doc);
        assert_eq!(text.matches("Help").count(), 1, "got {text:?}");
    }

    #[test]
    fn a_nameless_generic_node_earns_no_line_and_its_children_lift() {
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            node("generic", "", Some(0), vec![2]),
            node("button", "Go", Some(1), vec![]),
        ]);
        let text = text_for(&doc);
        assert!(!text.contains("generic"), "got {text:?}");
        assert!(
            text.contains("\n\t1 button Go"),
            "the button takes the wrapper's slot: {text:?}"
        );
    }

    #[test]
    fn ignored_nodes_are_skipped_and_their_children_lift() {
        let mut wrapper = node("generic", "", Some(0), vec![2]);
        wrapper.ignored = true;
        let doc = doc(vec![
            node("RootWebArea", "Example", None, vec![1]),
            wrapper,
            node("link", "Docs", Some(1), vec![]),
        ]);
        let text = text_for(&doc);
        assert!(text.contains("\n\t1 link Description: Docs"), "got {text:?}");
    }

    #[test]
    fn the_line_budget_truncates_and_says_so() {
        let mut nodes = vec![node("RootWebArea", "Example", None, vec![])];
        for index in 0..20 {
            let child = nodes.len();
            nodes[0].children.push(child);
            nodes.push(node("button", &format!("Button {index}"), Some(0), vec![]));
        }
        let doc = doc(nodes);
        let text = render_text(
            &doc,
            RenderOptions {
                max_lines: 5,
                ..RenderOptions::default()
            },
        );
        assert_eq!(text.lines().count(), 6, "five lines and the truncation note");
        assert!(text.contains("more elements omitted"));
    }

    #[test]
    fn url_shows_the_destination_without_the_ceremony() {
        assert_eq!(display_url("https://www.example.com/a?b=1"), "example.com/a?b=1");
        assert_eq!(display_url("http://localhost:3000/"), "localhost:3000/");
        assert_eq!(display_url("about:blank"), "about:blank");
    }
}
