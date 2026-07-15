use crate::config::Config;
use crate::connections::Connections;
use crate::db::Db;
use crate::ratelimit::RateLimiter;

/// Shared application state cloned into every connection task.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pub db: Db,
    pub connections: Connections,
    pub rate_limiter: RateLimiter,
}

impl AppState {
    pub fn new(config: Config, db: Db) -> Self {
        Self {
            config,
            db,
            connections: Connections::new(),
            rate_limiter: RateLimiter::new(10, 300),
        }
    }
}
