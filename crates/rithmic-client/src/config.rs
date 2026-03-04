//! Configuration for the Rithmic client.

use crate::error::RithmicError;

/// Configuration for a Rithmic WebSocket connection.
#[derive(Debug, Clone)]
pub struct RithmicConfig {
    /// WebSocket URI (e.g., "wss://rituz.rithmic.com:443")
    pub uri: String,
    /// Path to the Rithmic SSL CA certificate (PEM format).
    pub cert_path: Option<String>,
    /// Rithmic username.
    pub user: String,
    /// Rithmic password.
    pub password: String,
    /// Application name sent to Rithmic.
    pub app_name: String,
    /// Application version sent to Rithmic.
    pub app_version: String,
    /// Target symbol (e.g., "MES").
    pub symbol: String,
    /// Target exchange (e.g., "CME").
    pub exchange: String,
    /// Tick size for the instrument (e.g., 0.25 for MES).
    pub tick_size: f64,
    /// S3 bucket for raw message capture. None = disable S3 capture.
    pub s3_bucket: Option<String>,
    /// Preferred Rithmic system name (e.g., "Rithmic Paper Trading").
    /// If None, uses the first available system.
    pub system_name: Option<String>,
    /// Dev mode: panic on BBO divergence instead of logging.
    pub dev_mode: bool,
    /// Path for the structured JSON Lines health log.
    pub log_file: String,
}

impl RithmicConfig {
    /// Load configuration from environment variables.
    ///
    /// Required env vars:
    /// - `RITHMIC_URI` — WebSocket URI
    /// - `RITHMIC_USER` — username
    /// - `RITHMIC_PASSWORD` — password
    ///
    /// Optional env vars:
    /// - `RITHMIC_CERT_PATH` — path to CA cert PEM
    /// - `RITHMIC_APP_NAME` — defaults to "KenomaMBO"
    /// - `RITHMIC_APP_VERSION` — defaults to "1.0.0"
    /// - `RITHMIC_SYMBOL` — defaults to "MES"
    /// - `RITHMIC_EXCHANGE` — defaults to "CME"
    /// - `RITHMIC_TICK_SIZE` — defaults to "0.25"
    /// - `RITHMIC_S3_BUCKET` — if set, enables S3 capture
    /// - `RITHMIC_DEV_MODE` — if "1" or "true", enables dev mode
    pub fn from_env() -> Result<Self, RithmicError> {
        let uri = env_required("RITHMIC_URI")?;
        let user = env_required("RITHMIC_USER")?;
        let password = env_required("RITHMIC_PASSWORD")?;

        let cert_path = std::env::var("RITHMIC_CERT_PATH").ok();
        let app_name = std::env::var("RITHMIC_APP_NAME").unwrap_or_else(|_| "KenomaMBO".into());
        let app_version = std::env::var("RITHMIC_APP_VERSION").unwrap_or_else(|_| "1.0.0".into());
        let symbol = std::env::var("RITHMIC_SYMBOL").unwrap_or_else(|_| "MES".into());
        let exchange = std::env::var("RITHMIC_EXCHANGE").unwrap_or_else(|_| "CME".into());
        let tick_size: f64 = std::env::var("RITHMIC_TICK_SIZE")
            .unwrap_or_else(|_| "0.25".into())
            .parse()
            .map_err(|e| RithmicError::Config(format!("invalid RITHMIC_TICK_SIZE: {e}")))?;
        let s3_bucket = std::env::var("RITHMIC_S3_BUCKET").ok();
        let system_name = std::env::var("RITHMIC_SYSTEM").ok();
        let dev_mode = std::env::var("RITHMIC_DEV_MODE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        Ok(Self {
            uri,
            cert_path,
            user,
            password,
            app_name,
            app_version,
            symbol,
            exchange,
            tick_size,
            s3_bucket,
            system_name,
            dev_mode,
            log_file: String::new(), // set by caller (main.rs auto-generates)
        })
    }
}

fn env_required(key: &str) -> Result<String, RithmicError> {
    std::env::var(key).map_err(|_| RithmicError::Config(format!("{key} env var not set")))
}
