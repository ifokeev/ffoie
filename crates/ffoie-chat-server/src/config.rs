use std::net::SocketAddr;

/// Server configuration loaded from environment variables.
///
/// All variables have sensible defaults so the server works out-of-the-box
/// with no `.env` file. Operators override via shell env or a `.env` file
/// at the working directory (loaded by `dotenvy::dotenv()`).
#[derive(Debug, Clone)]
pub struct Config {
    /// Address and port the server binds to.
    pub bind: SocketAddr,

    /// Message-of-the-day sent to every connecting client in the Welcome envelope.
    pub motd: String,

    /// WebSocket ping interval in seconds.
    pub heartbeat_secs: u64,

    /// Maximum size of a single chat message payload in bytes.
    pub max_msg_bytes: usize,

    /// Token-bucket burst capacity for per-connection rate limiting.
    pub rate_burst: u32,

    /// Token-bucket refill rate (tokens per second) for rate limiting.
    pub rate_refill_per_sec: u32,

    /// Capacity of the internal tokio broadcast channel.
    pub broadcast_capacity: usize,

    /// Number of recent messages kept in the in-memory scrollback ring.
    pub scrollback_size: usize,

    /// Consecutive `RecvError::Lagged` events before a slow client is disconnected.
    pub max_lag_disconnects: u32,
}

impl Config {
    /// Load configuration from environment variables, falling back to defaults.
    ///
    /// Silently ignores a missing `.env` file. Returns an error only if a
    /// variable is present but cannot be parsed into the expected type.
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        // Load .env file if present; ignore errors (file may not exist in prod).
        dotenvy::dotenv().ok();

        let bind = std::env::var("FFOIE_CHAT_BIND")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
            .parse::<SocketAddr>()?;

        let motd = std::env::var("FFOIE_CHAT_MOTD")
            .unwrap_or_else(|_| "Welcome to FFOIE chat — be excellent to each other".to_string());

        let heartbeat_secs = parse_or_default("FFOIE_CHAT_HEARTBEAT_SECS", 15u64)?;
        let max_msg_bytes = parse_or_default("FFOIE_CHAT_MAX_MSG_BYTES", 500usize)?;
        let rate_burst = parse_or_default("FFOIE_CHAT_RATE_BURST", 10u32)?;
        let rate_refill_per_sec = parse_or_default("FFOIE_CHAT_RATE_REFILL_PER_SEC", 2u32)?;
        let broadcast_capacity = parse_or_default("FFOIE_CHAT_BROADCAST_CAPACITY", 1024usize)?;
        let scrollback_size = parse_or_default("FFOIE_CHAT_SCROLLBACK_SIZE", 50usize)?;
        let max_lag_disconnects = parse_or_default("FFOIE_CHAT_MAX_LAG_DISCONNECTS", 3u32)?;

        Ok(Self {
            bind,
            motd,
            heartbeat_secs,
            max_msg_bytes,
            rate_burst,
            rate_refill_per_sec,
            broadcast_capacity,
            scrollback_size,
            max_lag_disconnects,
        })
    }
}

/// Read an env var and parse it; return `default` if the var is absent.
/// Returns an error if the var is present but unparseable.
fn parse_or_default<T>(key: &str, default: T) -> Result<T, Box<dyn std::error::Error>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + 'static,
{
    match std::env::var(key) {
        Ok(val) => Ok(val.parse::<T>()?),
        Err(_) => Ok(default),
    }
}
