//! Element identity, so two captures can be compared element by element.
//!
//! Line numbers cannot do this job alone — see [`crate::ax::render`] for why the
//! number *is* the identity — but the identity itself has to come from somewhere
//! the page cannot reshuffle: **`backendDOMNodeId`**. It names the DOM node, so
//! it survives renames, re-ordering, and siblings appearing above it, and it is
//! only reused when the node is really gone.
//!
//! A structural path (the chain of role/name/ordinal from the root) is the
//! fallback for nodes with no DOM counterpart — synthetic ARIA nodes. That path
//! is fragile: one ancestor changing its ordinal rewrites every descendant's
//! identity at once, which on a real page reads as "thirty elements were
//! replaced" after a single insertion. It is acceptable only because the nodes
//! that need it are rare and cannot be acted on anyway.
//!
//! Identity is interned per document: a navigation replaces the tree wholesale,
//! and backend ids from another document mean nothing here.

use std::collections::HashMap;

use crate::ax::wire::WireDoc;

/// Assigns and remembers element ids for one document at a time.
#[derive(Debug, Default)]
pub struct ElementIds {
    document_key: String,
    next_id: u64,
    /// Identity -> the number that identity was first given.
    assigned: HashMap<String, u64>,
}

/// The ids for one capture, parallel to `WireDoc::nodes`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Numbered {
    /// Id per node index. `None` for nodes that were pruned before numbering.
    pub ids: Vec<Option<u64>>,
}

impl ElementIds {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number the nodes in `keep`, reusing ids for identities seen before.
    ///
    /// `keep` is the elements that earned a line, in render order. Nodes that
    /// did not earn a line get no number: a number nobody can see is a hole in
    /// the sequence the model reads, and it would make the first button 2
    /// instead of 1 for no reason.
    ///
    /// Callers are expected to keep one `ElementIds` per tab: two tabs share
    /// nothing, and a doc change inside one tab must not reset the other.
    pub fn assign(&mut self, doc: &WireDoc, keep: &[usize]) -> Numbered {
        if self.document_key != doc.document_key {
            self.document_key = doc.document_key.clone();
            self.assigned.clear();
            // `next_id` deliberately keeps counting across a navigation. The
            // model may still be holding a number from the page before; if that
            // number were handed out again it would silently name a different
            // element, and the error the model should get is "that element is
            // gone", not a click on the wrong thing.
        }

        let identities = identities(doc);
        let mut ids = vec![None; doc.nodes.len()];
        for index in keep {
            // The web area is the coordinate system, not an element: it is
            // always number 0 and never consumes one.
            if Some(*index) == doc.root {
                continue;
            }
            let Some(identity) = identities.get(*index).and_then(Option::as_ref) else {
                continue;
            };
            let next = self.next_id;
            let id = *self.assigned.entry(identity.clone()).or_insert_with(|| {
                // Ids start at 1 so that 0 can never be mistaken for "keep the
                // current focus" in a model-authored call, and 0 is the web
                // area itself.
                let id = next + 1;
                self.next_id = id;
                id
            });
            ids[*index] = Some(id);
        }
        Numbered { ids }
    }

    /// How many ids this document has handed out so far. Used by the renderer
    /// to explain a truncated state without exposing the table itself.
    pub fn issued(&self) -> u64 {
        self.next_id
    }
}

/// The identity of every node, or `None` for the root.
///
/// The root has no identity because it is the coordinate system everything else
/// is measured in, and it always renders as number 0.
fn identities(doc: &WireDoc) -> Vec<Option<String>> {
    let mut identities: Vec<Option<String>> = vec![None; doc.nodes.len()];
    let mut order = traversal_order(doc);
    // Parents before children, so a child can be keyed by its parent's key.
    order.reverse();
    for index in order {
        let node = &doc.nodes[index];
        // The DOM node is the identity when there is one: an insertion
        // somewhere else in the tree cannot reshuffle it.
        if let Some(backend) = node.backend_id {
            identities[index] = Some(format!("d{backend}"));
            continue;
        }
        // A parent outside the array is a malformed tree, not a reason to
        // panic: the node becomes the root of its own chain and stays
        // addressable.
        let parent = node.parent.filter(|parent| *parent < doc.nodes.len());
        let Some(parent) = parent else {
            if doc.root != Some(index) {
                identities[index] = Some(format!("/orphan{index}/{}:{}#0", node.role, node.name));
            }
            continue;
        };
        let parent_key = match &identities[parent] {
            Some(key) => key.clone(),
            // A node whose parent is unkeyed (pruned, or the root) starts a
            // fresh chain; it is still uniquely addressable through its own
            // position in the tree.
            None => format!("/n{parent}"),
        };
        let ordinal = doc.nodes[parent]
            .children
            .iter()
            .take_while(|sibling| **sibling != index)
            .filter(|sibling| {
                doc.nodes[**sibling].role == node.role && doc.nodes[**sibling].name == node.name
            })
            .count();
        identities[index] = Some(format!(
            "{parent_key}/{}:{}#{ordinal}",
            node.role, node.name
        ));
    }
    identities
}

/// Depth-first order over the accessibility tree, root first.
fn traversal_order(doc: &WireDoc) -> Vec<usize> {
    let mut order = Vec::with_capacity(doc.nodes.len());
    let mut stack: Vec<usize> = doc.root.into_iter().collect();
    let mut seen = vec![false; doc.nodes.len()];
    while let Some(index) = stack.pop() {
        if index >= doc.nodes.len() || seen[index] {
            continue;
        }
        seen[index] = true;
        order.push(index);
        // Reversed so the first child is visited first once popped.
        for child in doc.nodes[index].children.iter().rev() {
            stack.push(*child);
        }
    }
    // Nodes orphaned by a malformed tree still deserve ids rather than being
    // numbered as `None`; append the unreached ones in document order.
    for index in 0..doc.nodes.len() {
        if !seen[index] {
            order.push(index);
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::wire::WireNode;

    fn node(role: &str, name: &str, parent: Option<usize>, children: Vec<usize>) -> WireNode {
        WireNode {
            role: role.to_string(),
            name: name.to_string(),
            parent,
            children,
            ..WireNode::default()
        }
    }

    fn doc_with(nodes: Vec<WireNode>) -> WireDoc {
        WireDoc {
            document_key: "doc-1".to_string(),
            title: "T".to_string(),
            url: "https://example.com/".to_string(),
            root: Some(0),
            nodes,
        }
    }

    fn three_nodes() -> WireDoc {
        doc_with(vec![
            node("RootWebArea", "Example", None, vec![1, 2]),
            node("heading", "Example", Some(0), vec![]),
            node("button", "More information", Some(0), vec![]),
        ])
    }

    #[test]
    fn ids_are_stable_across_an_unrelated_insertion() {
        let mut ids = ElementIds::new();
        let keep = [1, 2];
        let first = ids.assign(&three_nodes(), &keep);

        // A new paragraph is inserted before the button.
        let mut grown = three_nodes();
        grown.nodes[0].children = vec![1, 3, 2];
        grown
            .nodes
            .push(node("paragraph", "New text", Some(0), vec![]));
        grown.nodes[2].parent = Some(0);
        let second = ids.assign(&grown, &[1, 3, 2]);

        assert_eq!(first.ids[1], second.ids[1], "the heading keeps its id");
        assert_eq!(
            first.ids[2], second.ids[2],
            "the button keeps its id even though its line moved"
        );
        assert_ne!(second.ids[3], None, "the new paragraph gets a fresh id");
    }

    #[test]
    fn the_same_element_is_not_renumbered_between_identical_captures() {
        let mut ids = ElementIds::new();
        let first = ids.assign(&three_nodes(), &[1, 2]);
        let second = ids.assign(&three_nodes(), &[1, 2]);
        assert_eq!(first, second);
    }

    #[test]
    fn only_the_elements_that_earn_a_line_are_numbered() {
        let mut ids = ElementIds::new();
        let numbered = ids.assign(&three_nodes(), &[2]);
        assert_eq!(
            numbered.ids[1], None,
            "a heading with no line has no number"
        );
        assert_eq!(numbered.ids[2], Some(1), "the first numbered element is 1");
        assert_eq!(ids.issued(), 1);
    }

    #[test]
    fn a_navigation_never_reuses_a_number() {
        let mut ids = ElementIds::new();
        let first = ids.assign(&three_nodes(), &[1, 2]);

        let mut navigated = three_nodes();
        navigated.document_key = "doc-2".to_string();
        let second = ids.assign(&navigated, &[1, 2]);

        assert_ne!(
            first, second,
            "numbers from the previous page must not name the new one's elements"
        );
        assert_eq!(second.ids[1], Some(3), "numbering continues after a navigation");
        assert_eq!(ids.issued(), 4);
    }

    #[test]
    fn duplicate_sibling_labels_stay_distinguishable() {
        let doc = doc_with(vec![
            node("RootWebArea", "Example", None, vec![1, 2]),
            node("button", "OK", Some(0), vec![]),
            node("button", "OK", Some(0), vec![]),
        ]);
        let mut ids = ElementIds::new();
        let numbered = ids.assign(&doc, &[1, 2]);
        assert_ne!(
            numbered.ids[1], numbered.ids[2],
            "two buttons with the same label are still two elements"
        );
    }

    #[test]
    fn the_root_gets_no_id() {
        let mut ids = ElementIds::new();
        let numbered = ids.assign(&three_nodes(), &[1, 2]);
        assert_eq!(numbered.ids[0], None);
    }

    #[test]
    fn a_dom_backed_identity_survives_a_structural_shuffle() {
        // Two identical buttons, told apart only by their DOM node. On a later
        // capture a sibling appears above them, so the structural path (role,
        // name, ordinal) of both changes — the DOM id must still win.
        let buttons = |extra: bool| {
            let mut nodes = vec![node("RootWebArea", "Example", None, vec![])];
            if extra {
                let child = nodes.len();
                nodes[0].children.push(child);
                let mut paragraph = node("paragraph", "New", Some(0), vec![]);
                paragraph.backend_id = Some(9);
                nodes.push(paragraph);
            }
            for backend in [7, 8] {
                let child = nodes.len();
                nodes[0].children.push(child);
                let mut button = node("button", "OK", Some(0), vec![]);
                button.backend_id = Some(backend);
                nodes.push(button);
            }
            doc_with(nodes)
        };

        let mut ids = ElementIds::new();
        let before = ids.assign(&buttons(false), &[1, 2]);
        let after = ids.assign(&buttons(true), &[1, 2, 3]);

        assert_eq!(before.ids[1], after.ids[2], "the first button keeps its id");
        assert_eq!(before.ids[2], after.ids[3], "the second button keeps its id");
        assert_eq!(after.ids[1], Some(3), "the new paragraph gets a fresh id");
    }

    #[test]
    fn the_root_is_never_numbered_even_when_it_has_a_dom_node() {
        let mut root = node("RootWebArea", "Example", None, vec![1]);
        root.backend_id = Some(3);
        let mut button = node("button", "Go", Some(0), vec![]);
        button.backend_id = Some(4);
        let doc = doc_with(vec![root, button]);

        let mut ids = ElementIds::new();
        let numbered = ids.assign(&doc, &[0, 1]);

        assert_eq!(numbered.ids[0], None, "the web area is 0, never an id");
        assert_eq!(numbered.ids[1], Some(1), "the first element is 1");
    }

    #[test]
    fn orphaned_nodes_still_get_numbered() {
        let doc = doc_with(vec![
            node("RootWebArea", "Example", None, vec![]),
            node("button", "Detached", Some(9), vec![]),
        ]);
        let mut ids = ElementIds::new();
        let numbered = ids.assign(&doc, &[1, 2]);
        assert!(
            numbered.ids[1].is_some(),
            "an unreachable node is not invisible"
        );
    }
}
