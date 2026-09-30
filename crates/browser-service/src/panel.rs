//! The panel browser's DevTools endpoint, published by the app that owns it.
//!
//! Maju's right panel is a real WebView2 browser
//! (`apps/desktop/src-tauri/src/browser_panel.rs`), started with
//! `--remote-debugging-port`, and it is the browser the agent should drive: the
//! page the user reads and the page the tools act on have to be the same page.
//! The panel publishes its endpoint here the moment its browser process exists,
//! and [`crate::BrowserFactory`] prefers it over every choice in settings — a
//! running panel browser wins over a configured attach endpoint and over
//! starting a browser of our own.
//!
//! The slot is process-wide because the panel is: one profile, one port, one
//! browser per app run. Clearing it (an app that closes its panel browser for
//! good) puts the settings back in charge.

use std::sync::{Arc, OnceLock, RwLock};

/// A writable slot holding the panel browser's endpoint.
///
/// Its own type so tests can hand a factory a private slot instead of the
/// process-wide one, which keeps the decision under test without any global
/// state leaking between tests.
pub type PanelEndpoint = Arc<RwLock<Option<String>>>;

/// The process-wide slot the app publishes into.
pub fn slot() -> PanelEndpoint {
    static SLOT: OnceLock<PanelEndpoint> = OnceLock::new();
    SLOT.get_or_init(|| Arc::new(RwLock::new(None))).clone()
}

/// Read the endpoint in `slot`.
pub fn read(slot: &PanelEndpoint) -> Option<String> {
    let guard = slot.read().ok()?;
    let endpoint = guard.as_deref()?.trim();
    (!endpoint.is_empty()).then(|| endpoint.to_string())
}

/// Write `endpoint` into `slot`. An empty endpoint clears it: "the panel
/// browser is gone" and "the panel browser has no address" are the same state
/// as far as the tools are concerned.
pub fn write(slot: &PanelEndpoint, endpoint: &str) {
    let Ok(mut guard) = slot.write() else {
        return;
    };
    let trimmed = endpoint.trim();
    *guard = (!trimmed.is_empty()).then(|| trimmed.to_string());
}

/// Publish the panel browser's endpoint for the whole process.
pub fn publish(endpoint: &str) {
    write(&slot(), endpoint);
}

/// Give the settings back their authority over which browser the tools drive.
pub fn clear() {
    if let Ok(mut guard) = slot().write() {
        *guard = None;
    }
}

/// The panel browser's endpoint, when one is running.
pub fn endpoint() -> Option<String> {
    read(&slot())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot_with(endpoint: &str) -> PanelEndpoint {
        let slot: PanelEndpoint = Arc::new(RwLock::new(None));
        write(&slot, endpoint);
        slot
    }

    #[test]
    fn a_published_endpoint_is_read_back_trimmed() {
        let slot = slot_with("  http://127.0.0.1:9333  ");
        assert_eq!(read(&slot).as_deref(), Some("http://127.0.0.1:9333"));
    }

    #[test]
    fn an_empty_endpoint_clears_the_slot() {
        let slot = slot_with("http://127.0.0.1:9333");
        write(&slot, "   ");
        assert_eq!(read(&slot), None);
    }

    #[test]
    fn an_unpublished_slot_has_no_endpoint() {
        let slot: PanelEndpoint = Arc::new(RwLock::new(None));
        assert_eq!(read(&slot), None);
    }

    #[test]
    fn the_shared_slot_is_one_slot() {
        // The app publishes into this one and the factory reads it, so it must
        // not be a fresh allocation per call.
        assert!(Arc::ptr_eq(&slot(), &slot()));
    }
}
