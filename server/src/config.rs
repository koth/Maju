use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: SocketAddr,
    pub db_path: String,
    pub pairing_code_ttl_secs: u64,
    pub auth_timestamp_window_secs: u64,
    pub heartbeat_timeout_secs: u64,
    pub require_tls: bool,
    pub health_addr: SocketAddr,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:8787".parse().expect("valid default listen addr"),
            db_path: "kodex-relay.sqlite".to_string(),
            pairing_code_ttl_secs: 120,
            auth_timestamp_window_secs: 300,
            heartbeat_timeout_secs: 60,
            require_tls: false,
            health_addr: "0.0.0.0:8788".parse().expect("valid default health addr"),
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut c = Config::default();
        if let Ok(v) = std::env::var("RELAY_LISTEN_ADDR") {
            if let Ok(parsed) = v.parse() {
                c.listen_addr = parsed;
            }
        }
        if let Ok(v) = std::env::var("RELAY_DB_PATH") {
            c.db_path = v;
        }
        if let Ok(v) = std::env::var("RELAY_PAIRING_CODE_TTL_SECS") {
            if let Ok(n) = v.parse() {
                c.pairing_code_ttl_secs = n;
            }
        }
        if let Ok(v) = std::env::var("RELAY_AUTH_TIMESTAMP_WINDOW_SECS") {
            if let Ok(n) = v.parse() {
                c.auth_timestamp_window_secs = n;
            }
        }
        if let Ok(v) = std::env::var("RELAY_HEARTBEAT_TIMEOUT_SECS") {
            if let Ok(n) = v.parse() {
                c.heartbeat_timeout_secs = n;
            }
        }
        if let Ok(v) = std::env::var("RELAY_REQUIRE_TLS") {
            c.require_tls = v == "1" || v.eq_ignore_ascii_case("true");
        }
        if let Ok(v) = std::env::var("RELAY_HEALTH_ADDR") {
            if let Ok(parsed) = v.parse() {
                c.health_addr = parsed;
            }
        }
        c
    }
}
