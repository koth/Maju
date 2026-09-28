//! The model-visible browser tool surface, derived from the provider's
//! advertised catalog rather than hardcoded.
//!
//! Hardcoding a tool list would mean a provider upgrade — which changes
//! nothing about the protocol — forces a Kodex release, and a tool that
//! disappears would leave a model-visible name that always errors. Deriving
//! the surface from `tools/list` means the catalog is the single source of
//! truth, and a rename or removal is picked up on the next session start.

use serde::{Deserialize, Serialize};

/// One tool as advertised by the provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema for the tool's input, forwarded to the model unchanged.
    ///
    /// The provider spells this `inputSchema` — the MCP `Tool` schema — and this
    /// field carried no rename, so the binding silently never happened and every
    /// tool reached the model with `inputSchema: null`. The harness client
    /// rejects a tool list shaped like that, which is why no browser tool was
    /// ever usable. `alias` keeps the Rust spelling working for anything already
    /// round-tripping a `ProviderTool` through serde.
    #[serde(default, rename = "inputSchema", alias = "input_schema")]
    pub input_schema: serde_json::Value,
}

/// The provider's advertised catalog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderCatalog {
    pub tools: Vec<ProviderTool>,
}

impl ProviderCatalog {
    pub fn find(&self, name: &str) -> Option<&ProviderTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// A tool as exposed to the model: provider namespaced, with its effect
/// classified for the permission broker.
#[derive(Debug, Clone, PartialEq)]
pub struct ExposedTool {
    /// Namespaced name the model calls, e.g. `mcp__playwright-mcp__browser_click`.
    pub exposed_name: String,
    /// The provider's own name, used when dispatching a call.
    pub provider_name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub effect: ToolEffect,
}

/// Whether a tool reads or writes, for permission classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolEffect {
    /// Produces state without changing the page or browser.
    Read,
    /// Navigates, clicks, types, or otherwise changes state.
    Write,
}

impl ToolEffect {
    pub fn is_read(self) -> bool {
        self == ToolEffect::Read
    }
}

/// Function names the model APIs accept. DeepSeek's alphabet and the 64
/// character ceiling are protocol constants, not preferences.
const TOOL_NAME_PATTERN: &str = "^[A-Za-z0-9_-]{1,64}$";

/// Why a catalog could not be exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// The provider advertised no tools, which usually means it failed to
    /// start its browser rather than that it has nothing to offer.
    Empty,
    /// A tool name cannot be represented as a model function name.
    InvalidToolName { name: String, exposed: String },
    /// Two provider tools collide after namespacing.
    DuplicateExposedName { exposed: String },
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CatalogError::Empty => write!(formatter, "browser provider advertised no tools"),
            CatalogError::InvalidToolName { name, exposed } => write!(
                formatter,
                "browser tool \"{name}\" cannot be exposed as \"{exposed}\": not a valid model function name"
            ),
            CatalogError::DuplicateExposedName { exposed } => {
                write!(
                    formatter,
                    "browser tool name \"{exposed}\" is exposed twice"
                )
            }
        }
    }
}

impl std::error::Error for CatalogError {}

/// Name a browser tool is exposed under.
///
/// Must stay identical to the prefixes the permission broker recognises in
/// `acp-core::runtime::permissions`, or a read tool would be classified as a
/// write and prompt in Plan mode.
pub const BROWSER_TOOL_NAMESPACE: &str = "mcp__";

/// Build the exposed name for a provider tool.
pub fn exposed_name(server_name: &str, tool_name: &str) -> String {
    format!("{BROWSER_TOOL_NAMESPACE}{server_name}__{tool_name}")
}

/// Leaf tool name, i.e. the part after the namespace and server segments.
pub fn leaf_name(exposed: &str) -> Option<&str> {
    let rest = exposed.strip_prefix(BROWSER_TOOL_NAMESPACE)?;
    let leaf = rest.rsplit_once("__").map_or(rest, |(_, leaf)| leaf);
    (!leaf.is_empty()).then_some(leaf)
}

/// Classify a tool by its leaf name.
///
/// Reads are the capture verbs. Matching the trailing verb rather than an
/// exact name keeps this correct across provider renames
/// (`browser_take_screenshot`, `browser_snapshot`) and keeps a tool that merely
/// mentions a capture verb (`screenshot_and_edit`) in the write class.
pub fn classify(leaf: &str) -> ToolEffect {
    let leaf = leaf.to_ascii_lowercase();
    if leaf.ends_with("screenshot") || leaf.ends_with("snapshot") {
        ToolEffect::Read
    } else {
        ToolEffect::Write
    }
}

fn is_valid_model_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Turn a provider catalog into the model-visible tool surface.
///
/// A catalog that cannot be exposed is an error rather than a partial
/// surface: silently dropping the one tool the agent needs is harder to
/// diagnose than a startup failure naming it.
pub fn expose(
    server_name: &str,
    catalog: &ProviderCatalog,
) -> Result<Vec<ExposedTool>, CatalogError> {
    if catalog.is_empty() {
        return Err(CatalogError::Empty);
    }

    let mut exposed = Vec::with_capacity(catalog.tools.len());
    let mut seen = std::collections::HashSet::new();

    for tool in &catalog.tools {
        let name = exposed_name(server_name, &tool.name);
        if !is_valid_model_tool_name(&name) {
            return Err(CatalogError::InvalidToolName {
                name: tool.name.clone(),
                exposed: name,
            });
        }
        if !seen.insert(name.clone()) {
            return Err(CatalogError::DuplicateExposedName { exposed: name });
        }

        exposed.push(ExposedTool {
            effect: classify(&tool.name),
            exposed_name: name,
            provider_name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: model_input_schema(&tool.input_schema),
        });
    }

    Ok(exposed)
}

/// The schema a tool carries onto the model-visible surface.
///
/// MCP requires `Tool.inputSchema` to be an object. A parameterless command
/// advertises no schema at all — which reaches [`expose`] as `null` or absent —
/// and a `null` there is not "takes no arguments", it is a malformed tool list
/// that the client refuses outright. An empty object schema is the honest
/// encoding of "takes nothing".
fn model_input_schema(schema: &serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(_) => schema.clone(),
        _ => serde_json::json!({"type": "object"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str) -> ProviderTool {
        ProviderTool {
            name: name.to_string(),
            description: format!("{name} description"),
            input_schema: json!({ "type": "object" }),
        }
    }

    fn catalog(names: &[&str]) -> ProviderCatalog {
        ProviderCatalog {
            tools: names.iter().map(|name| tool(name)).collect(),
        }
    }

    #[test]
    fn exposed_names_are_namespaced_by_server() {
        let exposed = expose("playwright-mcp", &catalog(&["browser_click"])).unwrap();
        assert_eq!(
            exposed[0].exposed_name,
            "mcp__playwright-mcp__browser_click"
        );
    }

    #[test]
    fn provider_name_is_kept_for_dispatch() {
        let exposed = expose("playwright-mcp", &catalog(&["browser_click"])).unwrap();
        assert_eq!(exposed[0].provider_name, "browser_click");
    }

    #[test]
    fn capture_tools_are_reads_and_everything_else_is_a_write() {
        let exposed = expose(
            "playwright-mcp",
            &catalog(&[
                "browser_take_screenshot",
                "browser_snapshot",
                "browser_click",
                "browser_navigate",
                "browser_type",
                "browser_press_key",
            ]),
        )
        .unwrap();

        let effect = |name: &str| {
            exposed
                .iter()
                .find(|tool| tool.provider_name == name)
                .map(|tool| tool.effect)
        };

        assert_eq!(effect("browser_take_screenshot"), Some(ToolEffect::Read));
        assert_eq!(effect("browser_snapshot"), Some(ToolEffect::Read));
        assert_eq!(effect("browser_click"), Some(ToolEffect::Write));
        assert_eq!(effect("browser_navigate"), Some(ToolEffect::Write));
        assert_eq!(effect("browser_type"), Some(ToolEffect::Write));
        assert_eq!(effect("browser_press_key"), Some(ToolEffect::Write));
    }

    #[test]
    fn a_tool_merely_mentioning_a_capture_verb_stays_a_write() {
        assert_eq!(classify("screenshot_and_edit"), ToolEffect::Write);
    }

    #[test]
    fn an_unusual_read_name_falls_back_to_write_not_the_other_way() {
        // This tool obviously captures, but its name does not end in a capture
        // verb, so the rule classifies it as a write. That asymmetry is
        // deliberate: a read misclassified as a write prompts once, while a
        // write misclassified as a read would execute without asking.
        assert_eq!(classify("take_screenshot_of_element"), ToolEffect::Write);
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(classify("browser_Take_Screenshot"), ToolEffect::Read);
        assert_eq!(classify("BROWSER_SNAPSHOT"), ToolEffect::Read);
    }

    #[test]
    fn leaf_name_strips_namespace_and_server() {
        assert_eq!(
            leaf_name("mcp__playwright-mcp__browser_take_screenshot"),
            Some("browser_take_screenshot")
        );
        assert_eq!(leaf_name("mcp__server__"), None);
        assert_eq!(leaf_name("browser_click"), None);
    }

    #[test]
    fn empty_catalog_is_an_error_not_an_empty_surface() {
        assert_eq!(
            expose("playwright-mcp", &catalog(&[])),
            Err(CatalogError::Empty)
        );
    }

    #[test]
    fn a_tool_name_that_cannot_be_a_function_name_is_rejected() {
        let bad = ProviderCatalog {
            tools: vec![ProviderTool {
                name: "bad name with spaces".to_string(),
                description: String::new(),
                input_schema: json!({}),
            }],
        };
        assert!(matches!(
            expose("playwright-mcp", &bad),
            Err(CatalogError::InvalidToolName { .. })
        ));
    }

    #[test]
    fn an_over_long_exposed_name_is_rejected() {
        let long = "x".repeat(80);
        let bad = ProviderCatalog {
            tools: vec![tool(&long)],
        };
        assert!(matches!(
            expose("playwright-mcp", &bad),
            Err(CatalogError::InvalidToolName { .. })
        ));
    }

    #[test]
    fn duplicate_exposed_names_are_rejected() {
        // Two provider names that collide once the server name is applied.
        let duplicate = ProviderCatalog {
            tools: vec![tool("a"), tool("a")],
        };
        assert!(matches!(
            expose("playwright-mcp", &duplicate),
            Err(CatalogError::DuplicateExposedName { .. })
        ));
    }

    #[test]
    fn schemas_and_descriptions_pass_through_unchanged() {
        let source = ProviderTool {
            name: "browser_click".to_string(),
            description: "Click an element".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": { "selector": { "type": "string" } },
                "required": ["selector"]
            }),
        };
        let exposed = expose(
            "playwright-mcp",
            &ProviderCatalog {
                tools: vec![source.clone()],
            },
        )
        .unwrap();

        assert_eq!(exposed[0].description, source.description);
        assert_eq!(exposed[0].input_schema, source.input_schema);
    }

    /// The provider spells the field `inputSchema`; a deserialized catalog has
    /// to carry it. Before the rename this decoded as `null` for every tool and
    /// the harness client refused the whole list — so this asserts the binding,
    /// which is the assertion the provider's own test never made.
    #[test]
    fn the_provider_camelcase_schema_field_binds() {
        let catalog: ProviderCatalog = serde_json::from_value(serde_json::json!({
            "tools": [{
                "name": "browser_click",
                "description": "Click an element",
                "inputSchema": {"type": "object", "properties": {"selector": {"type": "string"}}}
            }]
        }))
        .unwrap();

        assert_eq!(
            catalog.tools[0].input_schema["properties"]["selector"]["type"],
            "string",
            "inputSchema did not bind: {:?}",
            catalog.tools[0].input_schema
        );
    }

    /// And a tool the provider advertises without one — a parameterless command
    /// — must not reach the model as `null`.
    #[test]
    fn a_tool_without_a_schema_is_exposed_with_an_object_schema() {
        let exposed = expose(
            "playwright-mcp",
            &ProviderCatalog {
                tools: vec![ProviderTool {
                    name: "browser_close".to_string(),
                    description: "Close the page".to_string(),
                    input_schema: serde_json::Value::Null,
                }],
            },
        )
        .unwrap();

        assert_eq!(exposed[0].input_schema, json!({"type": "object"}));
    }

    #[test]
    fn a_provider_rename_changes_the_surface_without_code_changes() {
        // The point of deriving the surface: a rename is a new catalog, not a
        // new Kodex release.
        let before = expose("playwright-mcp", &catalog(&["browser_take_screenshot"])).unwrap();
        let after = expose("playwright-mcp", &catalog(&["browser_screenshot"])).unwrap();

        assert_eq!(
            before[0].exposed_name,
            "mcp__playwright-mcp__browser_take_screenshot"
        );
        assert_eq!(
            after[0].exposed_name,
            "mcp__playwright-mcp__browser_screenshot"
        );
        // Both remain reads, so the permission rule is unaffected.
        assert!(before[0].effect.is_read() && after[0].effect.is_read());
    }

    #[test]
    fn a_removed_tool_disappears_from_the_surface() {
        let exposed = expose("playwright-mcp", &catalog(&["browser_click"])).unwrap();
        assert_eq!(exposed.len(), 1);
        assert!(expose("playwright-mcp", &catalog(&[])).is_err());
    }
}
