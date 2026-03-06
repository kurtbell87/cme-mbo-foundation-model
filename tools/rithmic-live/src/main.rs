//! rithmic-live: connects to Rithmic, authenticates, subscribes to /MES
//! market data, feeds through book builder with BBO validation, and prints
//! 5s bar features to stdout.
//!
//! The watchdog loop handles automatic reconnection with exponential backoff.
//! BookDegraded errors exit immediately (code 2). Connection drops retry.
//! Ctrl+C during a session triggers graceful shutdown. Ctrl+C during backoff
//! exits immediately.
//!
//! Usage:
//!   RITHMIC_USER=... RITHMIC_PASSWORD=... RITHMIC_URI=... \
//!     cargo run --bin rithmic-live -- --symbol MES --exchange CME --dev-mode

use std::time::Duration;

use clap::Parser;
use rithmic_client::client::RithmicClient;
use rithmic_client::config::RithmicConfig;
use rithmic_client::error::RithmicError;

#[derive(Parser)]
#[command(name = "rithmic-live", about = "Live Rithmic /MES pipeline")]
struct Args {
    /// Symbol to subscribe to (e.g., MES)
    #[arg(long, default_value = "MES")]
    symbol: String,

    /// Exchange (e.g., CME)
    #[arg(long, default_value = "CME")]
    exchange: String,

    /// Tick size for the instrument
    #[arg(long, default_value = "0.25")]
    tick_size: f64,

    /// Enable dev mode (additional diagnostics)
    #[arg(long)]
    dev_mode: bool,

    /// Path for the structured JSON Lines health log.
    /// Defaults to rithmic-health-{SYMBOL}-{UNIX_SECS}.jsonl in the current directory.
    #[arg(long)]
    log_file: Option<String>,

    /// S3 bucket for raw message capture (optional)
    #[arg(long)]
    s3_bucket: Option<String>,

    /// Path to Rithmic SSL CA certificate (PEM)
    #[arg(long)]
    cert_path: Option<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Load base config from env, override with CLI args
    let config = match RithmicConfig::from_env() {
        Ok(mut cfg) => {
            cfg.symbol = args.symbol.clone();
            cfg.exchange = args.exchange;
            cfg.tick_size = args.tick_size;
            cfg.dev_mode = args.dev_mode;
            if args.s3_bucket.is_some() {
                cfg.s3_bucket = args.s3_bucket;
            }
            if args.cert_path.is_some() {
                cfg.cert_path = args.cert_path;
            }
            // Auto-generate log file path if not specified
            cfg.log_file = args.log_file.unwrap_or_else(|| {
                let secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                format!("rithmic-health-{}-{secs}.jsonl", args.symbol)
            });
            cfg
        }
        Err(e) => {
            eprintln!("Configuration error: {e}");
            eprintln!();
            eprintln!("Required environment variables:");
            eprintln!("  RITHMIC_URI       WebSocket URI (e.g., wss://rituz.rithmic.com:443)");
            eprintln!("  RITHMIC_USER      Rithmic username");
            eprintln!("  RITHMIC_PASSWORD  Rithmic password");
            eprintln!();
            eprintln!("Optional:");
            eprintln!("  RITHMIC_CERT_PATH   Path to CA cert PEM");
            eprintln!("  RITHMIC_APP_NAME    Application name (default: KenomaMBO)");
            eprintln!("  RITHMIC_APP_VERSION Application version (default: 1.0.0)");
            std::process::exit(1);
        }
    };

    eprintln!("rithmic-live: {} on {} (tick_size={}, dev_mode={})",
        config.symbol, config.exchange, config.tick_size, config.dev_mode);

    // Watchdog retry loop with exponential backoff
    let mut attempt = 0u32;
    loop {
        let client = RithmicClient::new(config.clone());

        // Log gap_end if this is a retry (attempt > 0 means we just finished a backoff)
        if attempt > 0 {
            log_health_event(&config.log_file, "gap_end", &serde_json::json!({
                "attempt": attempt,
            }));
        }

        match client.run().await {
            Ok(result) => {
                eprintln!("[watchdog] clean shutdown: {} (ran {}s)",
                    result.exit_reason, result.ran_duration.as_secs());
                break;
            }
            Err(RithmicError::BookDegraded(msg)) => {
                eprintln!("[watchdog] DEGRADED: {msg} — exiting (not retryable)");
                upload_health_log_to_s3(&config).await;
                std::process::exit(2);
            }
            Err(e) => {
                attempt += 1;
                let delay = std::cmp::min(1u64 << attempt.min(5), 30);
                eprintln!("[watchdog] attempt {attempt} failed: {e} — retrying in {delay}s");

                log_health_event(&config.log_file, "gap_start", &serde_json::json!({
                    "attempt": attempt,
                    "error": e.to_string(),
                    "backoff_s": delay,
                }));

                // Ctrl+C during backoff → exit immediately
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
                    _ = tokio::signal::ctrl_c() => {
                        eprintln!("[watchdog] Ctrl+C during backoff, exiting");
                        break;
                    }
                }
            }
        }
    }

    upload_health_log_to_s3(&config).await;
}

/// Write a health log event directly (used by watchdog when no HealthLogger is available).
fn log_health_event(log_file: &str, event: &str, fields: &serde_json::Value) {
    use std::io::Write;

    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut map = match fields {
        serde_json::Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    map.insert("ts_ms".into(), serde_json::json!(ts_ms));
    map.insert("event".into(), serde_json::json!(event));

    if let Ok(line) = serde_json::to_string(&serde_json::Value::Object(map)) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_file) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Upload the health log file to S3 on shutdown (if s3_bucket is configured).
async fn upload_health_log_to_s3(config: &RithmicConfig) {
    let bucket = match &config.s3_bucket {
        Some(b) => b,
        None => return,
    };

    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let filename = std::path::Path::new(&config.log_file)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("health.jsonl");
    let key = format!("health/{}/{}/{}", config.symbol, date, filename);

    eprintln!("[watchdog] uploading health log to s3://{bucket}/{key}");

    let sdk_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let s3 = aws_sdk_s3::Client::new(&sdk_config);

    match tokio::fs::read(&config.log_file).await {
        Ok(body) => {
            match s3.put_object()
                .bucket(bucket)
                .key(&key)
                .body(body.into())
                .content_type("application/x-ndjson")
                .send()
                .await
            {
                Ok(_) => eprintln!("[watchdog] health log uploaded to s3://{bucket}/{key}"),
                Err(e) => eprintln!("[watchdog] health log upload failed: {e}"),
            }
        }
        Err(e) => eprintln!("[watchdog] could not read health log for upload: {e}"),
    }
}
