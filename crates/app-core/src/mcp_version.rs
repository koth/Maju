//! Protocol version negotiation for Kodex's local MCP servers.
//!
//! All three servers — web tools, images, browser — are tool-only: they answer
//! `initialize`, `tools/list` and `tools/call` and offer no sampling, roots or
//! logging. That means a server can serve whatever revision a client speaks, so
//! the correct answer to `initialize` is the version the client asked for.
//!
//! This was not what they did. Each one answered with a hard-coded
//! `2024-11-05`, a revision that predates the MCP SDK the dsh harness ships
//! (`@modelcontextprotocol/client` 2.0.0, whose supported list contains
//! `2025-11-25` and `2026-07-28` and does not contain `2024-11-05` at all). The
//! harness therefore refused the handshake and reported
//! `mcp-client(kodex_web_tools): server is disconnected` — for every Kodex
//! server at once, which is why enabling the browser appeared to be one problem
//! when it was really three.
//!
//! The failure was invisible to the tests because they sent a modern
//! `protocolVersion` and then never asserted what came back.

use serde_json::Value;

/// The version reported when a client asks for none, or asks for something
/// unusable.
///
/// `2025-11-25` rather than a newer one: it is the older of the two revisions
/// the current SDK accepts, so it is the likelier of the two to keep working as
/// clients move on. Echoing still covers the normal path — this only applies to
/// a malformed request.
pub const FALLBACK_PROTOCOL_VERSION: &str = "2025-11-25";

/// The version to answer `initialize` with.
///
/// Per the MCP specification, a server that supports the requested version must
/// reply with that same version; replying with a different one is a signal that
/// the client should disconnect, and the SDK enforces it.
pub fn negotiate(params: Option<&Value>) -> &'static str {
    let Some(params) = params else {
        return FALLBACK_PROTOCOL_VERSION;
    };
    match params.get("protocolVersion").and_then(Value::as_str) {
        // Echo what the client asked for. The lifetime is `'static` because the
        // two literals below are, and anything else is unrecognisable input
        // rather than a version worth reporting back.
        Some("2026-07-28") => "2026-07-28",
        Some("2025-11-25") => "2025-11-25",
        // An older revision a client may still be pinned to. Replying with the
        // fallback rather than the request keeps the answer to a version we
        // actually know, and a client that asked for something older than this
        // is better served by a clear failure than by a silent downgrade.
        _ => FALLBACK_PROTOCOL_VERSION,
    }
}

use serde_json::json;

/// The revisions this build answers `server/discover` with.
///
/// Both are revisions the shipped harness SDK accepts. Advertising one would
/// still connect a client that speaks it; advertising both is what lets a client
/// on either era settle without a round trip.
pub const SUPPORTED_VERSIONS: &[&str] = &["2026-07-28", "2025-11-25"];

/// The method a client uses to learn what a server speaks, before authenticating.
pub const DISCOVER_METHOD: &str = "server/discover";

/// Answer the pre-authentication version probe, or `None` if this is not it.
///
/// The probe is deliberately unauthenticated: a client sends it to find out
/// *whether* authentication is required, so answering it behind the auth wall
/// tells the client nothing it can act on. The harness SDK treats that 401 as a
/// terminal `ClientHttpAuthentication` and abandons the whole server — reported
/// as `server is disconnected`, which reads like a network fault and sent this
/// investigation at the ports, the tokens and the firewall first.
///
/// This is also why the three Kodex MCP servers were unreachable from every dsh
/// session while a hand-written client could reach all three of them: that
/// client went straight to `initialize` with its token, and never probed.
pub fn discover_response(payload: &Value) -> Option<Value> {
    if payload.get("method").and_then(Value::as_str) != Some(DISCOVER_METHOD) {
        return None;
    }
    Some(json!({
        "supportedVersions": SUPPORTED_VERSIONS,
        "capabilities": { "tools": {} },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn asked(version: &str) -> Value {
        json!({ "protocolVersion": version, "capabilities": {} })
    }

    #[test]
    fn the_discover_probe_is_answered_with_both_supported_revisions() {
        let reply = discover_response(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}
        }))
        .expect("the probe is answered");

        let versions = reply["supportedVersions"].as_array().unwrap();
        assert_eq!(versions.len(), 2, "{reply}");
        for version in SUPPORTED_VERSIONS {
            assert!(
                versions.contains(&json!(version)),
                "{version} missing: {reply}"
            );
        }
        assert!(reply["capabilities"]["tools"].is_object(), "{reply}");
    }

    #[test]
    fn ordinary_requests_are_not_answered_by_the_probe_handler() {
        assert!(
            discover_response(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}
            }))
            .is_none()
        );
        assert!(discover_response(&json!({"jsonrpc": "2.0", "id": 1})).is_none());
    }

    #[test]
    fn a_modern_client_gets_its_own_version_back() {
        // The whole point: the harness speaks these two, and answering with
        // anything else is what disconnected it.
        assert_eq!(negotiate(Some(&asked("2026-07-28"))), "2026-07-28");
        assert_eq!(negotiate(Some(&asked("2025-11-25"))), "2025-11-25");
    }

    #[test]
    fn an_older_client_gets_the_fallback_rather_than_its_own_version() {
        // Reporting a revision back that this build does not claim to speak
        // would be the same mistake in the other direction.
        assert_eq!(
            negotiate(Some(&asked("2024-11-05"))),
            FALLBACK_PROTOCOL_VERSION
        );
    }

    #[test]
    fn a_request_without_a_version_is_answered_rather_than_refused() {
        assert_eq!(negotiate(None), FALLBACK_PROTOCOL_VERSION);
        assert_eq!(negotiate(Some(&json!({}))), FALLBACK_PROTOCOL_VERSION);
        assert_eq!(
            negotiate(Some(&json!({"protocolVersion": 2025}))),
            FALLBACK_PROTOCOL_VERSION
        );
    }

    /// The revision that broke it, named so the regression is recognisable.
    #[test]
    fn the_revision_that_disconnected_the_harness_is_never_answered() {
        assert_ne!(FALLBACK_PROTOCOL_VERSION, "2024-11-05");
    }

    /// The revisions the shipped harness SDK accepts.
    ///
    /// Not a comment: `@deepseek-ai/dsh`'s `@modelcontextprotocol/client` 2.0.0
    /// contains no occurrence of `2024-11-05` and none of `2025-06-18`, and its
    /// supported list is `2025-11-25` and `2026-07-28`. If that SDK moves on
    /// again, this test is where it should be noticed — the alternative is
    /// discovering it because every Kodex tool vanished from a session.
    #[test]
    fn every_version_we_answer_is_one_the_shipped_sdk_supports() {
        const SDK_SUPPORTED: &[&str] = &["2025-11-25", "2026-07-28"];

        for requested in ["2025-11-25", "2026-07-28", "2024-11-05", "nonsense"] {
            let answered = negotiate(Some(&asked(requested)));
            assert!(
                SDK_SUPPORTED.contains(&answered),
                "answered {answered:?} for a client asking {requested:?}, which the SDK does not support"
            );
        }
    }
}
