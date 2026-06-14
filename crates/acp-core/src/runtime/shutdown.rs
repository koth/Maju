use super::agent_process::kill_child_handle;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

pub(super) type ShutdownCleanupHook = dyn Fn() + Send + Sync + 'static;

#[derive(Clone, Default)]
pub(crate) struct ShutdownSignal {
    inner: Arc<ShutdownState>,
}

impl std::fmt::Debug for ShutdownSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownSignal")
            .field("requested", &self.is_requested())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct ShutdownState {
    requested: AtomicBool,
    agent_children: Mutex<Vec<Weak<Mutex<Option<Child>>>>>,
    cleanup_hooks: Mutex<Vec<Weak<ShutdownCleanupHook>>>,
}

impl ShutdownSignal {
    pub(crate) fn request_shutdown(&self) {
        self.inner.requested.store(true, Ordering::Release);
        self.kill_registered_agent_children();
        self.run_cleanup_hooks();
    }

    pub(super) fn is_requested(&self) -> bool {
        self.inner.requested.load(Ordering::Acquire)
    }

    pub(super) fn register_agent_child(&self, child: &Arc<Mutex<Option<Child>>>) {
        let Ok(mut guard) = self.inner.agent_children.lock() else {
            return;
        };
        guard.retain(|entry| entry.strong_count() > 0);
        guard.push(Arc::downgrade(child));
    }

    pub(super) fn register_cleanup_hook(&self, hook: &Arc<ShutdownCleanupHook>) {
        let Ok(mut guard) = self.inner.cleanup_hooks.lock() else {
            return;
        };
        guard.retain(|entry| entry.strong_count() > 0);
        guard.push(Arc::downgrade(hook));
    }

    fn kill_registered_agent_children(&self) {
        let children = {
            let Ok(mut guard) = self.inner.agent_children.lock() else {
                return;
            };
            let children = guard.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            guard.retain(|entry| entry.strong_count() > 0);
            children
        };

        for child in children {
            let _ = kill_child_handle(&child);
        }
    }

    fn run_cleanup_hooks(&self) {
        let hooks = {
            let Ok(mut guard) = self.inner.cleanup_hooks.lock() else {
                return;
            };
            let hooks = guard.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            guard.retain(|entry| entry.strong_count() > 0);
            hooks
        };

        for hook in hooks {
            hook();
        }
    }
}
