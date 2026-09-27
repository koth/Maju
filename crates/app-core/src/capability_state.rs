//! Snapshot transitions for browser-use and computer-use state.
//!
//! Kept out of the main reducer because these events are not part of the ACP
//! conversation: they come from a capability that pushes state independently
//! of any tool call, and a desktop capture can arrive whether or not a turn is
//! running.
//!
//! The rule that matters here is that the snapshot only ever carries handles
//! and a downscaled panel rendition. Full-resolution screenshot bytes live in
//! attachment storage, because `UiSnapshot` is polled — putting a page-sized
//! image in it would multiply that cost by every poll.

use workspace_model::{BrowserSessionState, CapabilityStateEvent, ScreenshotHandle, UiSnapshot};

/// Longest edge of the rendition the panel receives.
///
/// The panel is a side dock; anything larger is wasted transfer. A rendition
/// larger than this is a bug in the capture path, not a size the UI should
/// have to cope with.
pub const PANEL_RENDITION_MAX_BYTES: usize = 512 * 1024;

/// Apply one capability state event to a snapshot.
///
/// Returns `true` when the snapshot changed, so the caller can skip an
/// unnecessary emit.
pub fn apply_capability_event(ui: &mut UiSnapshot, event: &CapabilityStateEvent) -> bool {
    match event {
        CapabilityStateEvent::Browser { state } => {
            if let Some(current) = &ui.browser
                && current.session_id == state.session_id
                && current.version > state.version
            {
                // An older event arriving after a newer snapshot.
                return false;
            }
            ui.browser = Some((**state).clone());
            true
        }
        CapabilityStateEvent::Computer { state } => {
            if let Some(current) = &ui.computer_use
                && current.session_id == state.session_id
                && current.version > state.version
            {
                return false;
            }
            ui.computer_use = Some((**state).clone());
            true
        }
        CapabilityStateEvent::BrowserClosed { session_id } => match &ui.browser {
            Some(current) if current.session_id == *session_id => {
                ui.browser = None;
                true
            }
            _ => false,
        },
        CapabilityStateEvent::ComputerClosed { session_id } => match &ui.computer_use {
            Some(current) if current.session_id == *session_id => {
                ui.computer_use = None;
                true
            }
            _ => false,
        },
        // Availability is surfaced by the panel from the event stream; the
        // snapshot has no field for it, so nothing changes here.
        CapabilityStateEvent::Unavailable { .. } => false,
    }
}

/// Clear capability state belonging to a session that is going away.
pub fn clear_session(ui: &mut UiSnapshot, session_id: &str) -> bool {
    let mut changed = false;
    if ui
        .browser
        .as_ref()
        .is_some_and(|state| state.session_id == session_id)
    {
        ui.browser = None;
        changed = true;
    }
    if ui
        .computer_use
        .as_ref()
        .is_some_and(|state| state.session_id == session_id)
    {
        ui.computer_use = None;
        changed = true;
    }
    changed
}

/// Build a browser state, refusing a rendition too large to ship in a snapshot.
///
/// Returns `None` for the rendition rather than the whole state: a panel that
/// cannot show the image is still worth showing the URL and status for, and
/// the full-resolution capture remains on disk either way.
pub fn browser_state_with_rendition(
    session_id: &str,
    status: workspace_model::CapabilityResourceStatus,
    rendition: Option<Vec<u8>>,
) -> BrowserSessionState {
    BrowserSessionState {
        session_id: session_id.to_string(),
        status,
        panel_rendition: rendition
            .filter(|bytes| !bytes.is_empty() && bytes.len() <= PANEL_RENDITION_MAX_BYTES)
            .map(|bytes| base64_encode(&bytes)),
        ..BrowserSessionState::default()
    }
}

/// Whether a screenshot handle is safe to put in a polled snapshot.
///
/// Handles are paths and numbers, so they always are. The check exists so the
/// rule is asserted somewhere rather than only in a comment.
pub fn handle_is_snapshot_safe(handle: &ScreenshotHandle) -> bool {
    !handle.path.is_empty() && handle.byte_size > 0
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64, used for the panel rendition the `<img>` element needs.
pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let bits = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for (index, value) in [
            (bits >> 18) & 0x3F,
            (bits >> 12) & 0x3F,
            (bits >> 6) & 0x3F,
            bits & 0x3F,
        ]
        .into_iter()
        .enumerate()
        {
            if index > chunk.len() {
                out.push('=');
            } else {
                out.push(BASE64[value as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use workspace_model::{CapabilityResourceStatus, ComputerSessionState};

    fn browser(session_id: &str, version: u64) -> CapabilityStateEvent {
        CapabilityStateEvent::Browser {
            state: Box::new(BrowserSessionState {
                session_id: session_id.to_string(),
                version,
                current_url: "https://example.test".to_string(),
                ..BrowserSessionState::default()
            }),
        }
    }

    /// A minimal snapshot. Only the capability fields matter here, so the
    /// required session and workspace fields are filled with placeholders
    /// rather than pulling in a full fixture.
    fn snapshot() -> UiSnapshot {
        serde_json::from_value(serde_json::json!({
            "workspace": { "id": "00000000-0000-0000-0000-000000000003", "name": "w", "root": "/w" },
            "session": {
                "id": "00000000-0000-0000-0000-000000000001",
                "workspace_id": "00000000-0000-0000-0000-000000000002",
                "title": "t",
                "model": "m",
                "status": "Idle"
            },
            "repository": { "branch": "main", "head": "abc", "changed_files": [] },
            "inspector_tab": "Files",
            "messages": [], "timeline": [], "tools": [], "inspector_sections": [],
            "session_changes": [],
            "thinking_text": "",
        }))
        .expect("minimal snapshot parses")
    }

    #[test]
    fn a_browser_event_populates_the_snapshot() {
        let mut ui = snapshot();
        assert!(apply_capability_event(&mut ui, &browser("session-1", 1)));

        let state = ui.browser.expect("browser state");
        assert_eq!(state.session_id, "session-1");
        assert_eq!(state.current_url, "https://example.test");
    }

    #[test]
    fn a_newer_snapshot_wins_over_a_late_event() {
        let mut ui = snapshot();
        apply_capability_event(&mut ui, &browser("session-1", 5));

        // An event from before the current state must not roll it back.
        assert!(!apply_capability_event(&mut ui, &browser("session-1", 3)));
        assert_eq!(ui.browser.as_ref().unwrap().version, 5);
    }

    #[test]
    fn an_older_event_is_dropped_across_sessions_too() {
        let mut ui = snapshot();
        apply_capability_event(&mut ui, &browser("session-1", 1));
        apply_capability_event(&mut ui, &browser("session-2", 1));
        assert_eq!(ui.browser.as_ref().unwrap().session_id, "session-2");
    }

    #[test]
    fn closing_clears_only_the_matching_session() {
        let mut ui = snapshot();
        apply_capability_event(&mut ui, &browser("session-1", 1));

        assert!(!apply_capability_event(
            &mut ui,
            &CapabilityStateEvent::BrowserClosed {
                session_id: "session-2".to_string()
            }
        ));
        assert!(ui.browser.is_some());

        assert!(apply_capability_event(
            &mut ui,
            &CapabilityStateEvent::BrowserClosed {
                session_id: "session-1".to_string()
            }
        ));
        assert!(ui.browser.is_none());
    }

    #[test]
    fn computer_state_and_close_follow_the_same_rules() {
        let mut ui = snapshot();
        apply_capability_event(
            &mut ui,
            &CapabilityStateEvent::Computer {
                state: Box::new(ComputerSessionState {
                    session_id: "session-1".to_string(),
                    version: 2,
                    cursor_x: Some(10),
                    cursor_y: Some(20),
                    ..ComputerSessionState::default()
                }),
            },
        );

        let state = ui.computer_use.clone().expect("computer state");
        assert_eq!(state.cursor_x, Some(10));
        assert_eq!(state.overlapping_sessions, Vec::<String>::new());

        apply_capability_event(
            &mut ui,
            &CapabilityStateEvent::ComputerClosed {
                session_id: "session-1".to_string(),
            },
        );
        assert!(ui.computer_use.is_none());
    }

    #[test]
    fn clearing_a_session_removes_both_capabilities() {
        let mut ui = snapshot();
        apply_capability_event(&mut ui, &browser("session-1", 1));
        apply_capability_event(
            &mut ui,
            &CapabilityStateEvent::Computer {
                state: Box::new(ComputerSessionState {
                    session_id: "session-1".to_string(),
                    ..ComputerSessionState::default()
                }),
            },
        );

        assert!(clear_session(&mut ui, "session-1"));
        assert!(ui.browser.is_none());
        assert!(ui.computer_use.is_none());
        assert!(!clear_session(&mut ui, "session-1"));
    }

    #[test]
    fn an_unavailable_event_leaves_the_snapshot_untouched() {
        let mut ui = snapshot();
        let changed = apply_capability_event(
            &mut ui,
            &CapabilityStateEvent::Unavailable {
                capability: "browser".to_string(),
                detail: "no node".to_string(),
                remedy: "install node".to_string(),
            },
        );
        assert!(!changed);
        assert!(ui.browser.is_none());
    }

    #[test]
    fn a_rendition_within_the_cap_is_carried() {
        let state = browser_state_with_rendition(
            "session-1",
            CapabilityResourceStatus::Active,
            Some(vec![0x89, b'P', b'N', b'G', 0xAA]),
        );
        let rendition = state.panel_rendition.expect("rendition");
        assert!(!rendition.is_empty());
        // Base64 of the PNG magic, not the raw bytes: the panel binds it
        // straight into a data URL.
        assert!(rendition.starts_with("iVBO"), "got {rendition}");
        assert!(!rendition.contains('\u{0}'));
    }

    #[test]
    fn an_oversized_rendition_is_dropped_but_the_state_survives() {
        let state = browser_state_with_rendition(
            "session-1",
            CapabilityResourceStatus::Active,
            Some(vec![0u8; PANEL_RENDITION_MAX_BYTES + 1]),
        );
        assert!(
            state.panel_rendition.is_none(),
            "a rendition this large must not ride in a polled snapshot",
        );
        assert_eq!(state.session_id, "session-1");
        assert_eq!(state.status, CapabilityResourceStatus::Active);
    }

    #[test]
    fn an_empty_rendition_is_treated_as_absent() {
        let state = browser_state_with_rendition(
            "session-1",
            CapabilityResourceStatus::Idle,
            Some(Vec::new()),
        );
        assert!(state.panel_rendition.is_none());
    }

    #[test]
    fn snapshot_safety_is_decided_on_the_handle_not_the_bytes() {
        let good = ScreenshotHandle {
            path: "/tmp/a.png".to_string(),
            width: 8,
            height: 8,
            byte_size: 128,
            media_type: "image/png".to_string(),
        };
        assert!(handle_is_snapshot_safe(&good));

        assert!(!handle_is_snapshot_safe(&ScreenshotHandle {
            path: String::new(),
            ..good.clone()
        }));
        assert!(!handle_is_snapshot_safe(&ScreenshotHandle {
            byte_size: 0,
            ..good
        }));
    }

    #[test]
    fn base64_encodes_with_correct_padding() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_snapshot_survives_a_round_trip_without_capability_state() {
        // An older persisted snapshot has no browser keys; loading it must not
        // fail and must default to no panel.
        let raw = r#"{"revision":3,"session":{"id":"00000000-0000-0000-0000-000000000001","workspace_id":"00000000-0000-0000-0000-000000000002","title":"t","model":"m","status":"Idle"},"workspace":{"id":"00000000-0000-0000-0000-000000000003","name":"w","root":"/tmp"},"repository":{"branch":"main","head":"abc","changed_files":[]},"inspector_tab":"Files","messages":[],"timeline":[],"tools":[],"inspector_sections":[],"session_changes":[],"thinking_text":""}"#;
        let parsed: UiSnapshot = serde_json::from_str(raw).expect("legacy snapshot parses");
        assert!(parsed.browser.is_none());
        assert!(parsed.computer_use.is_none());
    }
}
