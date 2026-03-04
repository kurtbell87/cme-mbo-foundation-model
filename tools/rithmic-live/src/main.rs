//! rithmic-live: connects to Rithmic, authenticates, subscribes to /MES
//! market data, feeds through book builder with BBO validation, and prints
//! 5s bar features to stdout.
//!
//! Usage:
//!   RITHMIC_USER=... RITHMIC_PASSWORD=... RITHMIC_URI=... \
//!     cargo run --bin rithmic-live -- --symbol MES --exchange CME --dev-mode

use clap::Parser;
use rithmic_client::client::RithmicClient;
use rithmic_client::config::RithmicConfig;

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

    let client = RithmicClient::new(config);
    if let Err(e) = client.run().await {
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}
