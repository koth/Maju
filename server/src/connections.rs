use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// In-memory registry of live device connections, keyed by `device_id`.
/// Each entry holds a channel sender for outbound WebSocket text frames.
/// Entries are connection-scoped: dropped on disconnect. Pairings and
/// device registrations persist in SQLite and are unaffected by churn.
#[derive(Clone)]
pub struct Connections {
    map: Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>,
}

impl Connections {
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn insert(&self, device_id: &str, tx: mpsc::Sender<String>) {
        self.map
            .lock()
            .expect("connections mutex poisoned")
            .insert(device_id.to_string(), tx);
    }

    pub fn remove(&self, device_id: &str) {
        self.map
            .lock()
            .expect("connections mutex poisoned")
            .remove(device_id);
    }

    pub fn get(&self, device_id: &str) -> Option<mpsc::Sender<String>> {
        self.map
            .lock()
            .expect("connections mutex poisoned")
            .get(device_id)
            .cloned()
    }
}

impl Default for Connections {
    fn default() -> Self {
        Self::new()
    }
}
