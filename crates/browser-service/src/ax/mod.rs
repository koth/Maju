//! What the page looks like, in the form an agent can act on.
//!
//! This is the observation half of browser use: a page is captured as an
//! accessibility document ([`wire`]), every element gets a stable id
//! ([`identity`]), the document becomes terse text ([`render`]), and a re-read
//! only pays for what changed ([`diff`]). Captures are taken after the page
//! stops moving ([`settle`]) over a single CDP conversation ([`session`]).
//!
//! The pieces are separate on purpose. Rendering and diffing are pure
//! functions of a `WireDoc`, so they are unit-tested against synthetic trees;
//! only [`session`] needs a browser. When the format has to change, it changes
//! in [`render`] and the tests that matter are already there.

pub mod action;
pub mod diff;
pub mod identity;
pub mod render;
pub mod session;
pub mod settle;
pub mod wire;

pub use action::{Action, ActionError, Target, parse_action};
pub use diff::{Diff, DiffError, DiffOptions};
pub use identity::{ElementIds, Numbered};
pub use render::{Collected, Line, Pending, RenderOptions, Rendered, collect, render_text};
pub use session::{
    AxError, Capture, PageTarget, act, capture, evaluate_on_page, page_targets, viewport,
};
pub use settle::{SettleOptions, SettleOutcome};
pub use wire::{WireDoc, WireNode};

/// What a caller asked to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveMode {
    /// Diff against the previous capture when that is cheaper.
    State,
    /// Always send the whole tree. Used after a navigation, or when the model
    /// asks for the full picture on purpose.
    Full,
}

/// What the caller should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observed {
    /// The whole tree.
    Full(String),
    /// Only what changed.
    Diff(String),
    /// Nothing changed since the previous capture.
    Unchanged,
}

impl Observed {
    /// The text to hand to the model. There is always something to say: a
    /// capture that changed nothing still has to tell the model so, or the next
    /// turn looks like a failed read.
    pub fn text(&self) -> Option<&str> {
        match self {
            Observed::Full(text) | Observed::Diff(text) => Some(text),
            Observed::Unchanged => Some(diff::UNCHANGED),
        }
    }

    /// Whether this was a full state rather than a diff.
    pub fn is_full(&self) -> bool {
        matches!(self, Observed::Full(_))
    }
}

/// Per-tab observation state: the id table and the last capture.
///
/// One observer belongs to one tab. Sharing one across tabs would let a
/// background tab's capture invalidate the foreground tab's ids.
#[derive(Debug, Default)]
pub struct Observer {
    ids: ElementIds,
    last: Option<Rendered>,
    document_key: String,
    target_id: String,
}

impl Observer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The document the next capture will be compared against.
    pub fn document_key(&self) -> &str {
        &self.document_key
    }

    /// Record which page the next capture came from.
    ///
    /// Line numbers are only meaningful inside one tab, so the observer has to
    /// know which tab it numbered; an action later attaches to *this* target
    /// rather than to whatever page happens to be first.
    pub fn begin_capture(&mut self, target_id: &str) {
        self.target_id = target_id.to_string();
    }

    /// The page the last capture came from.
    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    /// The lines of the last capture, in the order the model read them.
    ///
    /// Callers use this to find an element by what it says (a label, a role)
    /// and then act on its number — the same two-step a model performs.
    pub fn lines(&self) -> &[Line] {
        self.last.as_ref().map(|rendered| rendered.lines.as_slice()).unwrap_or(&[])
    }

    /// Resolve a line number to the DOM node behind it.
    ///
    /// Three failures are kept apart on purpose, because they need different
    /// recoveries: the number is from another capture (observe again), the
    /// element has no DOM node (use something else), or no capture has
    /// happened at all (observe first).
    pub fn element(&self, index: usize) -> Result<i64, ActionError> {
        let Some(rendered) = &self.last else {
            return Err(ActionError::UnknownLine { index });
        };
        let Some(line) = rendered
            .lines
            .iter()
            .find(|line| line.index == index as u64)
        else {
            return Err(ActionError::UnknownLine { index });
        };
        line.backend.ok_or(ActionError::NoNode { index })
    }

    /// Observe `doc` and decide what the model should see.
    pub fn observe(
        &mut self,
        doc: &WireDoc,
        mode: ObserveMode,
        render_options: RenderOptions,
        diff_options: DiffOptions,
    ) -> Observed {
        let collected = render::collect(doc, render_options);
        let numbered = self.ids.assign(doc, &collected.nodes());
        let rendered = Rendered::assemble(collected, &numbered);

        // A navigation invalidates both the ids and the baseline; `assign` has
        // already reset the table, and the baseline must follow it or the diff
        // would compare two unrelated documents.
        let navigated = self.document_key != doc.document_key;
        self.document_key = doc.document_key.clone();

        // The baseline is always advanced, including on a navigation: the next
        // capture has to compare against *this* document, not the one before
        // the jump.
        let previous = if navigated { None } else { self.last.clone() };
        self.last = Some(rendered.clone());

        let Some(previous) = previous else {
            return Observed::Full(rendered.text());
        };

        if mode == ObserveMode::Full {
            return Observed::Full(rendered.text());
        }

        match diff::diff(&previous, &rendered, diff_options) {
            Diff::Unchanged => Observed::Unchanged,
            Diff::Changed(text) => Observed::Diff(text),
            // A diff that is not cheaper is not a diff; fall back to the whole
            // tree rather than sending something longer than the original.
            Diff::TooLarge(_) => Observed::Full(rendered.text()),
        }
    }

    /// Render `doc` without touching the baseline. Used when the caller wants
    /// to inspect a page without disturbing a running diff sequence.
    pub fn peek(&self, doc: &WireDoc, render_options: RenderOptions) -> String {
        render::render_text(doc, render_options)
    }
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

    fn page(names: &[&str]) -> WireDoc {
        let mut nodes = vec![node("RootWebArea", "Example", None, vec![])];
        for (index, name) in names.iter().enumerate() {
            nodes[0].children.push(index + 1);
            nodes.push(node("button", name, Some(0), vec![]));
        }
        WireDoc {
            document_key: "doc-1".to_string(),
            title: "Example".to_string(),
            url: "https://example.com/".to_string(),
            root: Some(0),
            nodes,
        }
    }

    fn observe(observer: &mut Observer, doc: &WireDoc) -> Observed {
        observer.observe(
            doc,
            ObserveMode::State,
            RenderOptions::default(),
            // Tiny synthetic pages would otherwise always fall back to a full
            // state: the saving thresholds exist for real pages, and the tests
            // here are about the decision, not the thresholds.
            DiffOptions {
                min_saved_bytes: 0,
                min_saved_ratio: 1.0,
                ..DiffOptions::default()
            },
        )
    }

    #[test]
    fn the_first_capture_is_always_the_full_state() {
        let mut observer = Observer::new();
        let observed = observe(&mut observer, &page(&["A", "B"]));
        assert!(observed.is_full());
        assert!(observed.text().unwrap().contains("button A"));
    }

    #[test]
    fn a_second_identical_capture_reports_no_change() {
        let mut observer = Observer::new();
        observe(&mut observer, &page(&["A", "B"]));
        assert_eq!(observe(&mut observer, &page(&["A", "B"])), Observed::Unchanged);
    }

    #[test]
    fn a_new_button_comes_back_as_a_diff() {
        let mut observer = Observer::new();
        observe(&mut observer, &page(&["A", "B"]));
        match observe(&mut observer, &page(&["A", "B", "C"])) {
            Observed::Diff(text) => assert!(text.contains("+\t3 button C"), "got {text:?}"),
            other => panic!("expected a diff, got {other:?}"),
        }
    }

    /// The property the whole numbering scheme exists for: an element that
    /// appears above others must not renumber them, or every capture after an
    /// insertion would have to be sent in full.
    #[test]
    fn an_insertion_above_does_not_renumber_the_rest() {
        let mut observer = Observer::new();
        observe(&mut observer, &page(&["A", "B"]));

        match observe(&mut observer, &page(&["C", "A", "B"])) {
            Observed::Diff(text) => {
                assert!(text.contains("+\t3 button C"), "got {text:?}");
                assert!(!text.contains("button A"), "A was re-sent: {text:?}");
                assert!(!text.contains("button B"), "B was re-sent: {text:?}");
            }
            other => panic!("expected a one-line diff, got {other:?}"),
        }
    }

    #[test]
    fn a_navigation_forces_the_full_state() {        let mut observer = Observer::new();
        observe(&mut observer, &page(&["A"]));
        let mut navigated = page(&["A"]);
        navigated.document_key = "doc-2".to_string();
        assert!(observe(&mut observer, &navigated).is_full());
    }

    #[test]
    fn full_mode_ignores_a_cheaper_diff() {
        let mut observer = Observer::new();
        observe(&mut observer, &page(&["A"]));
        let observed = observer.observe(
            &page(&["A", "B"]),
            ObserveMode::Full,
            RenderOptions::default(),
            DiffOptions::default(),
        );
        assert!(observed.is_full());
    }

    #[test]
    fn peeking_does_not_disturb_the_baseline() {
        let mut observer = Observer::new();
        observe(&mut observer, &page(&["A"]));
        observer.peek(&page(&["A", "B"]), RenderOptions::default());
        assert_eq!(
            observe(&mut observer, &page(&["A"])),
            Observed::Unchanged,
            "a peek must not make the next capture look like a change"
        );
    }

    fn page_with_nodes() -> WireDoc {
        let mut root = node("RootWebArea", "Example", None, vec![1]);
        root.backend_id = None;
        let mut button = node("button", "Go", Some(0), vec![]);
        button.backend_id = Some(77);
        WireDoc {
            document_key: "doc-1".to_string(),
            title: "Example".to_string(),
            url: "https://example.com/".to_string(),
            root: Some(0),
            nodes: vec![root, button],
        }
    }

    #[test]
    fn a_line_number_resolves_to_the_node_it_describes() {
        let mut observer = Observer::new();
        observe(&mut observer, &page_with_nodes());
        assert_eq!(observer.element(1), Ok(77));
    }

    #[test]
    fn an_element_without_a_dom_node_says_so() {
        let mut observer = Observer::new();
        observe(&mut observer, &page_with_nodes());
        // Line 0 is the web area, which has no backend node in this fixture.
        assert_eq!(observer.element(0), Err(ActionError::NoNode { index: 0 }));
    }

    #[test]
    fn a_number_outside_the_capture_is_refused() {
        let mut observer = Observer::new();
        observe(&mut observer, &page_with_nodes());
        assert_eq!(
            observer.element(9),
            Err(ActionError::UnknownLine { index: 9 })
        );
    }

    #[test]
    fn acting_before_observing_is_refused() {
        let observer = Observer::new();
        assert_eq!(
            observer.element(0),
            Err(ActionError::UnknownLine { index: 0 })
        );
    }

    #[test]
    fn the_observer_remembers_which_tab_it_numbered() {
        let mut observer = Observer::new();
        observer.begin_capture("target-7");
        observe(&mut observer, &page_with_nodes());
        assert_eq!(observer.target_id(), "target-7");
    }
}
