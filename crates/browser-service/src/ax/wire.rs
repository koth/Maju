//! The accessibility wire document: what a page looks like to the agent.
//!
//! Everything the agent reads about a page goes through this shape. It is
//! built from three CDP answers — `Accessibility.getFullAXTree` (roles, names,
//! states), `DOM.getDocument` (tags and attributes), and a one-shot
//! `Runtime.evaluate` (title, url, readiness) — and nothing else. Keeping the
//! three sources separate until [`WireDoc::from_cdp`] is deliberate: the
//! parser stays a pure function of JSON, so the rendering and diffing layers
//! can be tested without a browser.
//!
//! Two details carry most of the weight downstream:
//!
//! - **Ignored nodes are flattened, not dropped.** Chromium marks a great many
//!   wrapper `div`s as ignored while their children are meaningful, so a tree
//!   that drops them loses content. [`WireNode::ignored`] survives into the
//!   model; the renderer decides what to do with it.
//! - **Children are resolved through `childIds`.** `getFullAXTree` returns a
//!   flat array whose `parentId`/`childIds` are CDP node ids, not array
//!   indices. Rebuilding the tree is the only place that mapping is worth
//!   trusting; everything after this module speaks indices.

use serde_json::Value;

/// One node of the accessibility tree, merged with its DOM metadata.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WireNode {
    /// The Chromium accessibility node id (`nodeId`).
    pub ax_id: String,
    /// The `backendDOMNodeId`, when Chromium reported one. This is what
    /// `DOM.*` commands accept, so it is the key back into the DOM.
    pub backend_id: Option<i64>,
    /// ARIA role, e.g. `button`, `textbox`, `heading`, `RootWebArea`.
    pub role: String,
    /// Accessible name — the label the user reads.
    pub name: String,
    /// Where Chromium got the name from (`attribute`, `contents`, `title`,
    /// `value` ...). A name taken from an attribute is often the placeholder or
    /// a tooltip, so the renderer demotes it.
    pub name_source: Option<String>,
    /// Value of an input-like node.
    pub value: Option<String>,
    /// Description, when it adds something the name does not.
    pub description: Option<String>,
    /// Chromium asked assistive tech to ignore this node but keep its
    /// children. Renderers decide whether the node itself earns a line.
    pub ignored: bool,
    /// Index of the parent in [`WireDoc::nodes`].
    pub parent: Option<usize>,
    /// Indices of the children, in document order.
    pub children: Vec<usize>,
    /// Interaction states that change behaviour: `focused`, `checked`,
    /// `disabled`, `expanded`, `selected` ...
    pub states: Vec<String>,
    /// Secondary actions the node exposes (`Expand`, `Show Menu` ...). Only
    /// these may be passed to `performSecondaryAction`.
    pub actions: Vec<String>,
    /// `heading` level, `textbox` multiline and friends.
    pub level: Option<i64>,
    /// Tag name from the DOM, lowercased. `None` when the node has no DOM
    /// counterpart (synthetic ARIA nodes).
    pub tag: Option<String>,
    /// `class` attribute, trimmed to a useful length.
    pub class: Option<String>,
    /// Whether Chromium considers the node clickable. Derived from the DOM
    /// listener/service metadata rather than guessed from the role.
    pub clickable: bool,
}

/// A page state, ready for rendering.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WireDoc {
    /// Identifies the document these nodes belong to (`frameId` + `loaderId`).
    /// A change here invalidates both cached ids and diff baselines: it is a
    /// navigation, not an update.
    pub document_key: String,
    /// Page title from the DOM.
    pub title: String,
    /// Document URL from the DOM.
    pub url: String,
    /// Nodes in accessibility-tree order.
    pub nodes: Vec<WireNode>,
    /// Index of the root node.
    pub root: Option<usize>,
}

impl WireDoc {
    /// Build a document from raw CDP payloads.
    ///
    /// `ax` is the result of `Accessibility.getFullAXTree`, `dom` the result
    /// of `DOM.getDocument { pierce: true }`, and `page` the object returned
    /// by the readiness probe (`{ title, url, readyState }`).
    pub fn from_cdp(document_key: impl Into<String>, ax: &Value, dom: &Value, page: &Value) -> Self {
        let mut doc = WireDoc {
            document_key: document_key.into(),
            title: page
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            url: page
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            nodes: Vec::new(),
            root: None,
        };

        let raw: Vec<&Value> = ax
            .get("nodes")
            .and_then(Value::as_array)
            .map(|nodes| nodes.iter().collect())
            .unwrap_or_default();
        if raw.is_empty() {
            return doc;
        }

        // Pass 1: the nodes themselves, plus the id -> index table the child
        // and parent references are resolved against.
        let mut index_of: std::collections::HashMap<String, usize> =
            std::collections::HashMap::with_capacity(raw.len());
        for node in &raw {
            let Some(ax_id) = node.get("nodeId").and_then(Value::as_str) else {
                continue;
            };
            index_of.insert(ax_id.to_string(), doc.nodes.len());
            doc.nodes.push(parse_node(node));
        }

        // Pass 2: structure. `childIds` is authoritative when present;
        // `parentId` covers the nodes whose parent listed them nowhere.
        for (position, node) in raw.iter().enumerate() {
            let Some(children) = node.get("childIds").and_then(Value::as_array) else {
                continue;
            };
            for child in children {
                let Some(key) = child.as_str() else { continue };
                let Some(&child_index) = index_of.get(key) else {
                    continue;
                };
                if child_index == position {
                    continue;
                }
                doc.nodes[child_index].parent = Some(position);
                doc.nodes[position].children.push(child_index);
            }
        }
        for (position, node) in raw.iter().enumerate() {
            if doc.nodes[position].parent.is_some() {
                continue;
            }
            let Some(parent) = node.get("parentId").and_then(Value::as_str) else {
                doc.root.get_or_insert(position);
                continue;
            };
            let Some(&parent_index) = index_of.get(parent) else {
                doc.root.get_or_insert(position);
                continue;
            };
            if parent_index == position {
                doc.root.get_or_insert(position);
                continue;
            }
            doc.nodes[position].parent = Some(parent_index);
            if !doc.nodes[parent_index].children.contains(&position) {
                doc.nodes[parent_index].children.push(position);
            }
        }

        // Pass 3: DOM metadata keyed by backend node id.
        let dom_nodes = dom_metadata(dom);
        for node in &mut doc.nodes {
            let Some(backend) = node.backend_id else { continue };
            let Some(meta) = dom_nodes.get(&backend) else {
                continue;
            };
            node.tag = Some(meta.tag.clone());
            node.class = meta.class.clone();
            node.clickable = meta.clickable;
            if let Some(role) = &meta.role
                && node.role.is_empty()
            {
                node.role = role.clone();
            }
        }

        if doc.root.is_none() {
            doc.root = (0..doc.nodes.len()).find(|index| doc.nodes[*index].parent.is_none());
        }
        doc
    }

    /// The node a caller means by "the root".
    pub fn root_node(&self) -> Option<&WireNode> {
        self.root.and_then(|index| self.nodes.get(index))
    }
}

/// Parse one accessibility node into the wire shape.
fn parse_node(node: &Value) -> WireNode {
    let mut wire = WireNode {
        ax_id: node
            .get("nodeId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        backend_id: node.get("backendDOMNodeId").and_then(Value::as_i64),
        role: node
            .get("role")
            .and_then(|role| role.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        name: node
            .get("name")
            .and_then(|name| name.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        value: node
            .get("value")
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        name_source: node
            .get("name")
            .and_then(|name| name.get("sources"))
            .and_then(Value::as_array)
            .and_then(|sources| sources.first())
            .and_then(|source| source.get("type"))
            .and_then(Value::as_str)
            .map(str::to_string),
        description: node
            .get("description")
            .and_then(|description| description.get("value"))
            .and_then(Value::as_str)
            .filter(|description| !description.is_empty())
            .map(str::to_string),
        ignored: node.get("ignored").and_then(Value::as_bool).unwrap_or(false),
        parent: None,
        children: Vec::new(),
        states: Vec::new(),
        actions: Vec::new(),
        level: None,
        tag: None,
        class: None,
        clickable: false,
    };

    for property in node
        .get("properties")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = property.get("name").and_then(Value::as_str) else {
            continue;
        };
        let value = property.get("value");
        let boolean = value
            .and_then(|value| value.get("value"))
            .and_then(Value::as_bool);
        let numeric = value
            .and_then(|value| value.get("value"))
            .and_then(Value::as_i64);
        match name {
            "level" => wire.level = numeric,
            // Only states that are *on* earn a word: "not disabled" is the
            // default and saying so on every line would be pure noise.
            "focused" | "checked" | "disabled" | "expanded" | "invalid" | "modal" | "multiline"
            | "multiselectable" | "pressed" | "readonly" | "required" | "selected" => {
                if boolean == Some(true) {
                    wire.states.push(name.to_string());
                }
            }
            "valuetext" | "valuedescription" => {}
            _ => {}
        }
    }

    // Secondary actions, when Chromium exposed them.
    if let Some(actions) = node.get("actions").and_then(Value::as_array) {
        for action in actions {
            if let Some(action) = action.as_str() {
                wire.actions.push(action.to_string());
            }
        }
    }

    wire
}

/// DOM metadata for one node, keyed by `backendNodeId`.
#[derive(Debug, Clone, Default)]
struct DomMeta {
    tag: String,
    class: Option<String>,
    role: Option<String>,
    clickable: bool,
}

/// Flatten `DOM.getDocument` into a `backendNodeId -> DomMeta` table.
///
/// The node tree is walked by hand rather than with `DOM.querySelectorAll`
/// because a page state must be captured in one round trip: every extra
/// command is another chance for the page to move underneath the capture.
fn dom_metadata(dom: &Value) -> std::collections::HashMap<i64, DomMeta> {
    let mut table = std::collections::HashMap::new();
    let Some(root) = dom.get("root") else {
        return table;
    };
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if let Some(backend) = node.get("backendNodeId").and_then(Value::as_i64) {
            let tag = node
                .get("nodeName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            let mut meta = DomMeta {
                tag: tag.clone(),
                ..DomMeta::default()
            };
            if let Some(attributes) = node.get("attributes").and_then(Value::as_array) {
                // CDP flattens attributes into [name, value, name, value, ...].
                for pair in attributes.chunks(2) {
                    let Some(name) = pair.first().and_then(Value::as_str) else {
                        continue;
                    };
                    let value = pair.get(1).and_then(Value::as_str).unwrap_or_default();
                    match name {
                        "class" => {
                            meta.class = Some(truncate(value.trim(), 120)).filter(|v| !v.is_empty())
                        }
                        "role" => {
                            meta.role = Some(value.trim().to_string()).filter(|v| !v.is_empty())
                        }
                        "onclick" => meta.clickable = true,
                        _ => {}
                    }
                }
            }
            // A tag that only exists to be clicked is worth the same signal as
            // an onclick attribute.
            if matches!(
                tag.as_str(),
                "button" | "a" | "input" | "select" | "textarea" | "summary"
            ) {
                meta.clickable = true;
            }
            table.insert(backend, meta);
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            stack.extend(children.iter());
        }
        // Same-origin iframes arrive nested under `contentDocument`.
        if let Some(content) = node.get("contentDocument") {
            stack.push(content);
        }
    }
    table
}

/// Clip a string to `limit` bytes on a character boundary.
pub(crate) fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(ax_id: &str, role: &str, name: &str, children: Vec<&str>) -> Value {
        json!({
            "nodeId": ax_id,
            "ignored": false,
            "role": { "type": "role", "value": role },
            "name": { "type": "computedString", "value": name },
            "childIds": children,
            "properties": [],
        })
    }

    fn ax_tree() -> Value {
        json!({
            "nodes": [
                node("1", "RootWebArea", "Example Domain", vec!["2", "3"]),
                node("2", "heading", "Example Domain", vec![]),
                {
                    "nodeId": "3",
                    "ignored": false,
                    "role": { "value": "button" },
                    "name": { "value": "More information" },
                    "backendDOMNodeId": 33,
                    "childIds": [],
                    "properties": [
                        { "name": "focused", "value": { "value": true } },
                        { "name": "disabled", "value": { "value": false } }
                    ]
                },
                {
                    "nodeId": "4",
                    "ignored": true,
                    "role": { "value": "generic" },
                    "name": { "value": "" },
                    "childIds": [],
                    "properties": []
                }
            ]
        })
    }

    fn dom_tree() -> Value {
        json!({
            "root": {
                "backendNodeId": 33,
                "nodeName": "BUTTON",
                "attributes": ["class", "btn  primary", "type", "button"],
                "children": []
            }
        })
    }

    fn page() -> Value {
        json!({ "title": "Example Domain", "url": "https://example.com/", "readyState": "complete" })
    }

    #[test]
    fn children_and_parents_round_trip() {
        let doc = WireDoc::from_cdp("doc-1", &ax_tree(), &dom_tree(), &page());
        assert_eq!(doc.nodes.len(), 4);
        assert_eq!(doc.root, Some(0));
        assert_eq!(doc.nodes[0].children, vec![1, 2]);
        assert_eq!(doc.nodes[2].parent, Some(0));
        assert_eq!(doc.nodes[0].parent, None);
        assert_eq!(doc.title, "Example Domain");
        assert_eq!(doc.url, "https://example.com/");
    }

    #[test]
    fn dom_metadata_attaches_to_the_node_it_belongs_to() {
        let doc = WireDoc::from_cdp("doc-1", &ax_tree(), &dom_tree(), &page());
        let button = &doc.nodes[2];
        assert_eq!(button.tag.as_deref(), Some("button"));
        assert_eq!(button.class.as_deref(), Some("btn  primary"));
        assert!(button.clickable, "a <button> is clickable even without onclick");
    }

    #[test]
    fn only_states_that_are_on_are_recorded() {
        let doc = WireDoc::from_cdp("doc-1", &ax_tree(), &dom_tree(), &page());
        assert_eq!(doc.nodes[2].states, vec!["focused".to_string()]);
    }

    #[test]
    fn ignored_nodes_survive_with_their_flag() {
        let doc = WireDoc::from_cdp("doc-1", &ax_tree(), &dom_tree(), &page());
        assert!(doc.nodes[3].ignored);
    }

    #[test]
    fn a_node_without_child_ids_keeps_the_parent_that_listed_it() {
        let mut ax = ax_tree();
        ax["nodes"][1]["childIds"] = json!([]);
        // The root still lists it, so the edge exists from the parent side.
        let doc = WireDoc::from_cdp("doc-1", &ax, &dom_tree(), &page());
        assert_eq!(doc.nodes[1].parent, Some(0));
    }

    #[test]
    fn parent_id_fills_in_children_nobody_listed() {
        let ax = json!({
            "nodes": [
                node("1", "RootWebArea", "Page", vec![]),
                {
                    "nodeId": "2",
                    "parentId": "1",
                    "role": { "value": "textbox" },
                    "name": { "value": "Email" },
                    "properties": []
                }
            ]
        });
        let doc = WireDoc::from_cdp("doc-1", &ax, &json!({}), &page());
        assert_eq!(doc.nodes[1].parent, Some(0));
        assert_eq!(doc.nodes[0].children, vec![1]);
    }

    #[test]
    fn an_empty_payload_yields_an_empty_document() {
        let doc = WireDoc::from_cdp("doc-1", &json!({}), &json!({}), &json!({}));
        assert!(doc.nodes.is_empty());
        assert_eq!(doc.root, None);
        assert!(doc.root_node().is_none());
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("短", 4), "短");
        assert_eq!(truncate("短文本", 4), "短…");
    }
}
