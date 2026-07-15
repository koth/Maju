use std::collections::HashMap;
use std::sync::{Arc, Mutex};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Per-key failure rate limiter. A key (device_id or IP) is blocked once it
/// exceeds `max_failures` within the rolling `window_ms`.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, (u32, i64)>>>,
    max_failures: u32,
    window_ms: i64,
}

impl RateLimiter {
    pub fn new(max_failures: u32, window_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_failures,
            window_ms: (window_secs as i64) * 1000,
        }
    }

    /// True if the key is currently allowed to attempt (not rate-limited).
    pub fn allowed(&self, key: &str) -> bool {
        let mut m = self.inner.lock().expect("ratelimit mutex poisoned");
        let now = now_ms();
        let entry = m.entry(key.to_string()).or_insert((0, now));
        if now - entry.1 > self.window_ms {
            *entry = (0, now);
        }
        entry.0 < self.max_failures
    }

    pub fn record_failure(&self, key: &str) {
        let mut m = self.inner.lock().expect("ratelimit mutex poisoned");
        let now = now_ms();
        let entry = m.entry(key.to_string()).or_insert((0, now));
        if now - entry.1 > self.window_ms {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    pub fn reset(&self, key: &str) {
        self.inner
            .lock()
            .expect("ratelimit mutex poisoned")
            .remove(key);
    }
}
