use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("invalid device_id format: {0}")]
    InvalidDeviceId(String),
    #[error("timestamp_ms out of allowed window")]
    StaleTimestamp,
    #[error("device not registered and not pairing: {0}")]
    DeviceNotRegistered(String),
    #[error("pairing code not found, expired, or already used")]
    InvalidPairingCode,
    #[error("devices are not paired together; cross-pairing route blocked")]
    NotPaired,
    #[error("active subscription required")]
    SubscriptionRequired,
    #[error("db error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ws error: {0}")]
    Ws(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = RelayError> = std::result::Result<T, E>;
