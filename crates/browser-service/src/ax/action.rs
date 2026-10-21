//! Acting on the element a line number names.
//!
//! A number is only useful if it can be turned into a node. The chain is:
//! line number → `backendDOMNodeId` (captured with the line) → a resolved
//! remote object (`DOM.resolveNode`) → a real input event. Every step is a CDP
//! round trip, and the whole point of going through the DOM rather than
//! guessing coordinates is that the click lands on the element the model read,
//! not on whatever is at those pixels now.
//!
//! Two decisions are worth knowing about:
//!
//! - **Coordinates come from the element, not from the box model.** Chromium's
//!   box model is in document space, and input events are in viewport space;
//!   converting between them means reading layout metrics and getting the
//!   scroll offsets right. Asking the element for `getBoundingClientRect()`
//!   after `scrollIntoView()` is one round trip and already in the right space.
//! - **Typing replaces the field's text.** `fill` selects what is there and
//!   inserts over it; `type` inserts at the caret and leaves the rest alone.
//!   Both go through `Input.insertText`, which is what a person's keyboard
//!   produces — a `value =` assignment would not fire the events a framework
//!   listens to, and the page would not update.

use serde_json::{Value, json};

use crate::cdp::{CdpClient, CdpError};

/// What to do to an element.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A left click in the middle of the element.
    Click,
    /// Two clicks in quick succession, which is how text gets selected and
    /// how some editors open.
    DoubleClick,
    /// Focus the element, select its contents, and insert over them.
    Fill(String),
    /// Focus the element and insert at the caret, leaving existing text.
    Type(String),
    /// Focus the element and send one key.
    Press(String),
    /// Scroll by a delta. Element-targeted, it scrolls the page at that
    /// element; point-targeted, it scrolls whatever is under the point.
    Scroll { dx: f64, dy: f64 },
}

/// What an action is aimed at.
///
/// Both are first-class because neither can replace the other: an element is
/// addressed by the number the model read, and a point is the only way to reach
/// something the accessibility tree does not expose — a canvas, a custom widget,
/// a cross-origin frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// An element from an observation.
    Element { index: usize, backend_id: i64 },
    /// A viewport coordinate, in CSS pixels from the top left.
    Point { x: f64, y: f64 },
}

/// Why an action could not be performed.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionError {
    /// The CDP conversation failed.
    Cdp(CdpError),
    /// The line number is not in the capture the caller is holding.
    UnknownLine { index: usize },
    /// The element has no DOM node, so it cannot be addressed by input events.
    NoNode { index: usize },
    /// A key name this module does not know.
    UnknownKey(String),
    /// An action name this module does not know.
    UnknownAction(String),
    /// The element did not report a usable position.
    Unpositioned { index: usize },
    /// The page could not be attached to, so nothing could be done to it.
    Session(String),
    /// The action needs an element but was given a point, or the reverse.
    WrongTarget { action: String, reason: &'static str },
    /// A coordinate outside the viewport, or a non-finite one.
    BadPoint { x: f64, y: f64 },
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Cdp(error) => write!(formatter, "the action failed: {error}"),
            ActionError::UnknownLine { index } => write!(
                formatter,
                "element {index} is not in the current page state; observe the page again"
            ),
            ActionError::NoNode { index } => write!(
                formatter,
                "element {index} has no DOM node to act on; click it by position instead, passing x and y from a screenshot"
            ),
            ActionError::UnknownKey(key) => write!(
                formatter,
                "key \"{key}\" is not one this tool sends; supported keys are Enter, Tab, Escape, Backspace, Delete, Space, arrows, Home, End, PageUp and PageDown"
            ),
            ActionError::UnknownAction(action) => write!(
                formatter,
                "action \"{action}\" is not supported; use click, double_click, fill, type, press or scroll"
            ),
            ActionError::Unpositioned { index } => write!(
                formatter,
                "element {index} reported no position, so it cannot be clicked; it may be hidden"
            ),
            ActionError::Session(detail) => {
                write!(formatter, "the page could not be reached: {detail}")
            }
            ActionError::WrongTarget { action, reason } => {
                write!(formatter, "{action} cannot be used here: {reason}")
            }
            ActionError::BadPoint { x, y } => write!(
                formatter,
                "the point ({x}, {y}) is not inside the viewport in CSS pixels"
            ),
        }
    }
}

impl std::error::Error for ActionError {}

impl From<CdpError> for ActionError {
    fn from(error: CdpError) -> Self {
        ActionError::Cdp(error)
    }
}

/// Parse the action name and value a tool call carries.
///
/// `scroll` takes its distances separately because a pointer target and a
/// distance are different things, and folding them into one string would mean
/// parsing numbers back out of prose.
pub fn parse_action(
    action: &str,
    value: Option<&str>,
    scroll: Option<(f64, f64)>,
) -> Result<Action, ActionError> {
    match action.trim().to_ascii_lowercase().as_str() {
        "click" => Ok(Action::Click),
        "double_click" | "dblclick" => Ok(Action::DoubleClick),
        "fill" | "set_value" => Ok(Action::Fill(value.unwrap_or_default().to_string())),
        "type" | "type_text" => Ok(Action::Type(value.unwrap_or_default().to_string())),
        "press" | "press_key" => Ok(Action::Press(
            value.unwrap_or_default().trim().to_string(),
        )),
        "scroll" => {
            let (dx, dy) = scroll.unwrap_or((0.0, 0.0));
            if dx == 0.0 && dy == 0.0 {
                return Err(ActionError::WrongTarget {
                    action: "scroll".to_string(),
                    reason: "give a delta_y or delta_x to scroll by",
                });
            }
            Ok(Action::Scroll { dx, dy })
        }
        other => Err(ActionError::UnknownAction(other.to_string())),
    }
}

/// Perform `action` against `target`.
pub async fn perform(
    client: &CdpClient,
    session: &str,
    target: Target,
    action: &Action,
) -> Result<(), ActionError> {
    match (target, action) {
        (Target::Element { index, backend_id }, Action::Click) => {
            click_element(client, session, index, backend_id, 1).await
        }
        (Target::Element { index, backend_id }, Action::DoubleClick) => {
            click_element(client, session, index, backend_id, 2).await
        }
        (Target::Element { index, backend_id }, Action::Fill(text)) => {
            call_on(client, session, index, backend_id, FOCUS_AND_SELECT).await?;
            insert_text(client, session, text).await
        }
        (Target::Element { index, backend_id }, Action::Type(text)) => {
            call_on(client, session, index, backend_id, FOCUS).await?;
            insert_text(client, session, text).await
        }
        (Target::Element { index, backend_id }, Action::Press(key)) => {
            let key = key_def(key).ok_or_else(|| ActionError::UnknownKey(key.clone()))?;
            call_on(client, session, index, backend_id, FOCUS).await?;
            press_key(client, session, key).await
        }
        (Target::Element { index, backend_id }, Action::Scroll { dx, dy }) => {
            // Scroll under the element, not at the viewport origin: a nested
            // scroller only moves if the wheel event lands inside it.
            let position = call_on(client, session, index, backend_id, MEASURE)
                .await?
                .ok_or(ActionError::Unpositioned { index })?;
            let x = position.get("x").and_then(Value::as_f64).unwrap_or_default();
            let y = position.get("y").and_then(Value::as_f64).unwrap_or_default();
            wheel(client, session, x, y, *dx, *dy).await
        }
        (Target::Point { x, y }, Action::Click) => click_point(client, session, x, y, 1).await,
        (Target::Point { x, y }, Action::DoubleClick) => {
            click_point(client, session, x, y, 2).await
        }
        (Target::Point { x, y }, Action::Scroll { dx, dy }) => {
            wheel(client, session, x, y, *dx, *dy).await
        }
        (Target::Point { .. }, action) => Err(ActionError::WrongTarget {
            action: action_name(action).to_string(),
            reason: "it needs an element; pass index instead of x and y",
        }),
    }
}

fn action_name(action: &Action) -> &'static str {
    match action {
        Action::Click => "click",
        Action::DoubleClick => "double_click",
        Action::Fill(_) => "fill",
        Action::Type(_) => "type",
        Action::Press(_) => "press",
        Action::Scroll { .. } => "scroll",
    }
}

/// Click at raw viewport coordinates.
async fn click_point(
    client: &CdpClient,
    session: &str,
    x: f64,
    y: f64,
    count: u8,
) -> Result<(), ActionError> {
    let (x, y) = checked_point(x, y)?;
    // A press without a move lands on whatever the last mouse position was for
    // hover-sensitive UI; the move is what makes a menu open.
    client
        .call(
            "Input.dispatchMouseEvent",
            json!({ "type": "mouseMoved", "x": x, "y": y }),
            Some(session),
        )
        .await?;
    for click_count in 1..=count {
        for event_type in ["mousePressed", "mouseReleased"] {
            client
                .call(
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": event_type,
                        "x": x,
                        "y": y,
                        "button": "left",
                        "buttons": if event_type == "mousePressed" { 1 } else { 0 },
                        "clickCount": click_count,
                    }),
                    Some(session),
                )
                .await?;
        }
    }
    Ok(())
}

/// Scroll at a point.
///
/// `Input.dispatchMouseEvent { type: "mouseWheel" }` looks like the obvious
/// call and does not reliably scroll: the event is delivered but Chromium's
/// compositor may not turn it into a scroll for a synthetic pointer. The
/// gesture API is the one that means "scroll here", and its distance arguments
/// are inverted relative to a wheel delta (positive scrolls up/left), so the
/// sign flips on the way in.
async fn wheel(
    client: &CdpClient,
    session: &str,
    x: f64,
    y: f64,
    dx: f64,
    dy: f64,
) -> Result<(), ActionError> {
    let (x, y) = checked_point(x, y)?;
    client
        .call(
            "Input.synthesizeScrollGesture",
            json!({
                "x": x,
                "y": y,
                "xDistance": -dx,
                "yDistance": -dy,
                "gestureSourceType": "mouse",
                "speed": 800,
            }),
            Some(session),
        )
        .await?;
    Ok(())
}

/// Reject a point that cannot name a place on the screen.
fn checked_point(x: f64, y: f64) -> Result<(f64, f64), ActionError> {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
        return Err(ActionError::BadPoint { x, y });
    }
    Ok((x, y))
}

/// Scroll the element into view, measure it, and click its middle.
async fn click_element(
    client: &CdpClient,
    session: &str,
    index: usize,
    backend_id: i64,
    count: u8,
) -> Result<(), ActionError> {
    let position = call_on(client, session, index, backend_id, MEASURE)
        .await?
        .ok_or(ActionError::Unpositioned { index })?;
    let x = position.get("x").and_then(Value::as_f64);
    let y = position.get("y").and_then(Value::as_f64);
    let (Some(x), Some(y)) = (x, y) else {
        return Err(ActionError::Unpositioned { index });
    };
    // A zero-sized target is usually a hidden node; clicking (0,0)-ish would
    // hit whatever sits at the page corner instead of the element asked for.
    let width = position.get("w").and_then(Value::as_f64).unwrap_or_default();
    let height = position.get("h").and_then(Value::as_f64).unwrap_or_default();
    if width <= 0.0 || height <= 0.0 {
        return Err(ActionError::Unpositioned { index });
    }

    for click_count in 1..=count {
        for event_type in ["mousePressed", "mouseReleased"] {
            client
                .call(
                    "Input.dispatchMouseEvent",
                    json!({
                        "type": event_type,
                        "x": x,
                        "y": y,
                        "button": "left",
                        "buttons": if event_type == "mousePressed" { 1 } else { 0 },
                        "clickCount": click_count,
                    }),
                    Some(session),
                )
                .await?;
        }
    }
    Ok(())
}

/// Focus the node and insert text at the caret.
async fn insert_text(client: &CdpClient, session: &str, text: &str) -> Result<(), ActionError> {
    client
        .call("Input.insertText", json!({ "text": text }), Some(session))
        .await?;
    Ok(())
}

/// Dispatch one key press and release at the focused element.
async fn press_key(client: &CdpClient, session: &str, key: KeyDef) -> Result<(), ActionError> {
    // A key that produces text uses `keyDown` so the text arrives with the
    // event; a key that only moves the caret or focus must use `rawKeyDown`,
    // which is the type the browser handles as a real key press.
    let down = if key.text.is_some() { "keyDown" } else { "rawKeyDown" };
    let mut params = json!({
        "type": down,
        "key": key.key,
        "code": key.code,
        "windowsVirtualKeyCode": key.vk,
        "nativeVirtualKeyCode": key.vk,
    });
    if let Some(text) = key.text {
        params["text"] = json!(text);
        params["unmodifiedText"] = json!(text);
    }
    client
        .call("Input.dispatchKeyEvent", params, Some(session))
        .await?;
    client
        .call(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": key.key,
                "code": key.code,
                "windowsVirtualKeyCode": key.vk,
                "nativeVirtualKeyCode": key.vk,
            }),
            Some(session),
        )
        .await?;
    Ok(())
}

/// Run a small function against the node and return its value.
///
/// The node is resolved through `DOM.resolveNode`, which is the only way to get
/// an object handle for a `backendNodeId`; `callFunctionOn` then runs against
/// that handle rather than against the document, so nothing has to be looked up
/// by selector and nothing has to be escaped.
async fn call_on(
    client: &CdpClient,
    session: &str,
    index: usize,
    backend_id: i64,
    function: &str,
) -> Result<Option<Value>, ActionError> {
    let resolved = client
        .call(
            "DOM.resolveNode",
            json!({ "backendNodeId": backend_id }),
            Some(session),
        )
        .await?;
    let Some(object_id) = resolved
        .get("object")
        .and_then(|object| object.get("objectId"))
        .and_then(Value::as_str)
    else {
        return Err(ActionError::NoNode { index });
    };
    let answer = client
        .call(
            "Runtime.callFunctionOn",
            json!({
                "objectId": object_id,
                "functionDeclaration": function,
                "returnByValue": true,
            }),
            Some(session),
        )
        .await?;
    Ok(answer
        .get("result")
        .and_then(|result| result.get("value"))
        .cloned())
}

/// Scroll the element into view and report its viewport-space middle.
const MEASURE: &str = r#"function () {
  this.scrollIntoView({ block: 'center', inline: 'center' });
  const rect = this.getBoundingClientRect();
  return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2,
           w: rect.width, h: rect.height };
}"#;

const FOCUS: &str = "function () { this.focus(); }";

/// Focus and select the field's contents so an insert replaces them.
const FOCUS_AND_SELECT: &str = r#"function () {
  this.focus();
  if (typeof this.select === 'function') this.select();
  else if (this.isContentEditable) {
    const range = document.createRange();
    range.selectNodeContents(this);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
  }
}"#;

/// One key, described the way `Input.dispatchKeyEvent` wants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyDef {
    key: &'static str,
    code: &'static str,
    vk: i32,
    text: Option<&'static str>,
}

/// The keys the tool sends.
///
/// A short list on purpose: these are the keys that move a form forward, and
/// every extra name is a name that can silently do the wrong thing. Anything
/// else is refused with the list, so the model can pick a supported one instead
/// of guessing twice.
fn key_def(name: &str) -> Option<KeyDef> {
    let key = match name {
        "Enter" | "Return" => KeyDef {
            key: "Enter",
            code: "Enter",
            vk: 13,
            text: Some("\r"),
        },
        "Tab" => KeyDef {
            key: "Tab",
            code: "Tab",
            vk: 9,
            text: None,
        },
        "Escape" | "Esc" => KeyDef {
            key: "Escape",
            code: "Escape",
            vk: 27,
            text: None,
        },
        "Backspace" => KeyDef {
            key: "Backspace",
            code: "Backspace",
            vk: 8,
            text: None,
        },
        "Delete" => KeyDef {
            key: "Delete",
            code: "Delete",
            vk: 46,
            text: None,
        },
        "Space" | " " => KeyDef {
            key: " ",
            code: "Space",
            vk: 32,
            text: Some(" "),
        },
        "ArrowUp" | "Up" => KeyDef {
            key: "ArrowUp",
            code: "ArrowUp",
            vk: 38,
            text: None,
        },
        "ArrowDown" | "Down" => KeyDef {
            key: "ArrowDown",
            code: "ArrowDown",
            vk: 40,
            text: None,
        },
        "ArrowLeft" | "Left" => KeyDef {
            key: "ArrowLeft",
            code: "ArrowLeft",
            vk: 37,
            text: None,
        },
        "ArrowRight" | "Right" => KeyDef {
            key: "ArrowRight",
            code: "ArrowRight",
            vk: 39,
            text: None,
        },
        "Home" => KeyDef {
            key: "Home",
            code: "Home",
            vk: 36,
            text: None,
        },
        "End" => KeyDef {
            key: "End",
            code: "End",
            vk: 35,
            text: None,
        },
        "PageUp" => KeyDef {
            key: "PageUp",
            code: "PageUp",
            vk: 33,
            text: None,
        },
        "PageDown" => KeyDef {
            key: "PageDown",
            code: "PageDown",
            vk: 34,
            text: None,
        },
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_five_intended_actions_parse() {
        assert_eq!(parse_action("click", None, None), Ok(Action::Click));
        assert_eq!(
            parse_action("double_click", None, None),
            Ok(Action::DoubleClick)
        );
        assert_eq!(
            parse_action("fill", Some("a@b.c"), None),
            Ok(Action::Fill("a@b.c".to_string()))
        );
        assert_eq!(
            parse_action("type", Some("hi"), None),
            Ok(Action::Type("hi".to_string()))
        );
        assert_eq!(
            parse_action("press", Some("Enter"), None),
            Ok(Action::Press("Enter".to_string()))
        );
    }

    #[test]
    fn action_names_are_case_insensitive_and_the_aliases_work() {
        assert_eq!(parse_action("CLICK", None, None), Ok(Action::Click));
        assert_eq!(
            parse_action(" dblclick ", None, None),
            Ok(Action::DoubleClick)
        );
        assert_eq!(
            parse_action("set_value", Some("x"), None),
            Ok(Action::Fill("x".into()))
        );
        assert_eq!(
            parse_action("press_key", Some("Tab"), None),
            Ok(Action::Press("Tab".into()))
        );
    }

    #[test]
    fn a_missing_value_is_an_empty_string_not_an_error() {
        assert_eq!(
            parse_action("fill", None, None),
            Ok(Action::Fill(String::new()))
        );
        assert_eq!(
            parse_action("press", None, None),
            Ok(Action::Press(String::new()))
        );
    }

    #[test]
    fn an_unknown_action_says_what_it_does_support() {
        let error = parse_action("hover", None, None).unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("click, double_click, fill, type, press or scroll"),
            "{message}"
        );
    }

    #[test]
    fn scroll_takes_a_distance_and_refuses_a_zero_one() {
        assert_eq!(
            parse_action("scroll", None, Some((0.0, 480.0))),
            Ok(Action::Scroll { dx: 0.0, dy: 480.0 })
        );
        let error = parse_action("scroll", None, None).unwrap_err();
        assert!(error.to_string().contains("delta_y"), "{error}");
    }

    #[test]
    fn a_point_cannot_carry_text_actions() {
        let error = ActionError::WrongTarget {
            action: "fill".to_string(),
            reason: "needs an element",
        };
        assert!(error.to_string().contains("fill cannot be used here"));
    }

    #[test]
    fn a_negative_or_non_finite_point_is_refused() {
        assert!(checked_point(-1.0, 10.0).is_err());
        assert!(checked_point(f64::NAN, 10.0).is_err());
        assert_eq!(checked_point(10.0, 20.0), Ok((10.0, 20.0)));
    }

    #[test]
    fn keys_that_move_a_form_are_known() {
        for name in ["Enter", "Tab", "Escape", "Backspace", "Space", "ArrowDown"] {
            assert!(key_def(name).is_some(), "{name} should be known");
        }
        assert_eq!(key_def("Enter").unwrap().text, Some("\r"));
        assert_eq!(key_def("Tab").unwrap().text, None, "Tab must not insert text");
    }

    #[test]
    fn aliases_map_to_the_same_key_definition() {
        assert_eq!(key_def("Return"), key_def("Enter"));
        assert_eq!(key_def("Esc"), key_def("Escape"));
        assert_eq!(key_def(" "), key_def("Space"));
        assert_eq!(key_def("Up"), key_def("ArrowUp"));
        assert_eq!(key_def("Down"), key_def("ArrowDown"));
    }

    #[test]
    fn an_unknown_key_is_refused_with_the_list() {
        assert!(key_def("F13").is_none());
        let message = ActionError::UnknownKey("F13".to_string()).to_string();
        assert!(message.contains("Enter, Tab, Escape"), "{message}");
    }

    #[test]
    fn an_unknown_line_explains_that_the_page_moved() {
        let message = ActionError::UnknownLine { index: 7 }.to_string();
        assert!(message.contains("element 7"), "{message}");
        assert!(message.contains("observe the page again"), "{message}");
    }
}
