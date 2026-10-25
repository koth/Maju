//! Short-lived paint leases for the app-owned panel browser.
//!
//! WebView2 stops producing compositor frames while its controller is hidden.
//! CDP input and `Page.captureScreenshot` can then wait forever, even though
//! DOM and accessibility commands still answer. The desktop shell publishes a
//! lease factory here; the provider path holds a lease only while a tool call
//! is in flight. The shell keeps those pages presentable off screen for that
//! bounded period, then restores their hidden state.
//!
//! This is separate from [`crate::panel_driver`]: Windows still drives its
//! panel through CDP. The lease controls native paintability, not navigation
//! or input, and platforms without a native panel simply publish nothing.

use std::sync::{Arc, OnceLock, RwLock};

/// A native paint hold. Dropping it releases exactly one hold.
///
/// The release callback is supplied by the desktop shell, which owns the
/// webviews. It is refcounted there so overlapping tool calls cannot hide a
/// page while another call still needs its frames.
pub struct PanelPaintLease {
    release: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl PanelPaintLease {
    pub fn new(release: impl FnOnce() + Send + 'static) -> Self {
        Self {
            release: Some(Box::new(release)),
        }
    }
}

impl Drop for PanelPaintLease {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            release();
        }
    }
}

/// The shell's way of acquiring one native paint hold.
pub type PaintLeaseFactory = dyn Fn() -> PanelPaintLease + Send + Sync + 'static;

fn slot() -> &'static RwLock<Option<Arc<PaintLeaseFactory>>> {
    static SLOT: OnceLock<RwLock<Option<Arc<PaintLeaseFactory>>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Publish the panel's paint-lease factory for the whole process.
pub fn publish(factory: Arc<PaintLeaseFactory>) {
    if let Ok(mut guard) = slot().write() {
        *guard = Some(factory);
    }
}

/// Acquire a paint hold when the desktop shell has a native panel.
///
/// The factory is cloned before it is called, so no global lock is held while
/// the shell crosses into its GUI runtime.
pub fn acquire() -> Option<PanelPaintLease> {
    let factory = slot()
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(Arc::clone));
    factory.map(|factory| factory())
}

/// Forget the shell's factory, e.g. while tearing down the native panel.
pub fn clear() {
    if let Ok(mut guard) = slot().write() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn dropping_a_lease_releases_it_exactly_once() {
        let releases = Arc::new(AtomicUsize::new(0));
        let released = Arc::clone(&releases);
        let lease = PanelPaintLease::new(move || {
            released.fetch_add(1, Ordering::SeqCst);
        });

        drop(lease);

        assert_eq!(releases.load(Ordering::SeqCst), 1);
    }
}
