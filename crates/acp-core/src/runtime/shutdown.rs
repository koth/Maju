use super::agent_process::kill_child_handle;
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

#[derive(Clone, Debug, Default)]
pub(crate) struct ShutdownSignal {
    inner: Arc<ShutdownState>,
}

#[derive(Debug, Default)]
struct ShutdownState {
    requested: AtomicBool,
    agent_children: Mutex<Vec<Weak<Mutex<Option<Child>>>>>,
}

impl ShutdownSignal {
    pub(crate) fn request_shutdown(&self) {
        self.inner.requested.store(true, Ordering::Release);
        self.kill_registered_agent_children();
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
}
