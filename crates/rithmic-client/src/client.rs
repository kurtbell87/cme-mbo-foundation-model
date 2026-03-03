//! Client orchestrator: supervisor that manages the full pipeline lifecycle.
//!
//! RithmicClient::run() handles:
//! 1. Config loading
//! 2. Two-phase auth (system info → disconnect → reconnect → login)
//! 3. Market data + DBO subscription
//! 4. Spawning: WebSocket read/write, Dispatcher, Pipeline tasks
//! 5. Monitoring task health via JoinHandles
//! 6. Graceful shutdown on Ctrl+C

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::auth;
use crate::config::RithmicConfig;
use crate::connection::encode_ws_message;
use crate::counters::MessageCounters;
use crate::dispatcher::{self, CaptureRecord, PipelineCommand};
use crate::error::RithmicError;
use crate::health_log::HealthLogger;
use crate::heartbeat::{self, LivenessTracker};
use crate::pipeline::{self, FeatureOutput};
use crate::subscription;
use crate::InfraType;

/// Channel buffer sizes (per plan).
const RAW_MSG_BUF: usize = 4096;
const PIPELINE_CMD_BUF: usize = 8192;
const BBO_BUF: usize = 1024;
const RAW_CAPTURE_BUF: usize = 4096;
const OUTPUT_BUF: usize = 256;
const WS_CMD_BUF: usize = 64;
/// Recovery signal channel: pipeline → client. Buffer=1; try_send drops
/// redundant signals when a recovery is already in flight.
const RECOVERY_BUF: usize = 1;

/// The top-level Rithmic client.
pub struct RithmicClient {
    config: RithmicConfig,
}

impl RithmicClient {
    pub fn new(config: RithmicConfig) -> Self {
        Self { config }
    }

    /// Run the full pipeline: connect, authenticate, subscribe, stream.
    ///
    /// This is the main entry point. It blocks until the pipeline shuts down
    /// (via error, forced logout, or Ctrl+C).
    pub async fn run(&self) -> Result<(), RithmicError> {
        let counters = MessageCounters::new();
        let liveness = LivenessTracker::new();

        let health = match HealthLogger::open(&self.config.log_file) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[client] WARNING: could not open health log {}: {e}", self.config.log_file);
                // Fall back to /dev/null — non-fatal, don't abort the session
                HealthLogger::open("/dev/null")
                    .expect("/dev/null always openable")
            }
        };
        health.log("startup", serde_json::json!({
            "symbol": self.config.symbol,
            "exchange": self.config.exchange,
            "tick_size": self.config.tick_size,
            "dev_mode": self.config.dev_mode,
            "log_file": self.config.log_file,
        }));

        eprintln!("[client] authenticating to {}...", self.config.uri);

        // Two-phase auth
        let auth_result = auth::authenticate(
            &self.config.uri,
            self.config.cert_path.as_deref(),
            &self.config.user,
            &self.config.password,
            &self.config.app_name,
            &self.config.app_version,
            self.config.system_name.as_deref(),
            InfraType::TickerPlant,
        )
        .await?;

        eprintln!(
            "[client] authenticated: system={}, heartbeat_interval={}s",
            auth_result.system_name, auth_result.heartbeat_interval
        );

        let (mut ws_sink, mut ws_stream) = auth_result.ws_stream.split();
        let heartbeat_interval = Duration::from_secs(auth_result.heartbeat_interval);

        // Subscribe to market data
        eprintln!(
            "[client] subscribing to {} on {}...",
            self.config.symbol, self.config.exchange
        );

        let mdu = subscription::subscribe_market_data(&self.config.symbol, &self.config.exchange);
        ws_sink
            .send(encode_ws_message(&mdu))
            .await
            .map_err(|e| RithmicError::WebSocket(format!("send market data sub: {e}")))?;

        let dbo = subscription::subscribe_depth_by_order(&self.config.symbol, &self.config.exchange);
        ws_sink
            .send(encode_ws_message(&dbo))
            .await
            .map_err(|e| RithmicError::WebSocket(format!("send DBO sub: {e}")))?;

        // Request initial DBO snapshot to populate the book before incrementals arrive.
        // The server responds with 116 (ResponseDepthByOrderSnapshot) + 161 (end marker).
        let snap = subscription::request_dbo_snapshot(&self.config.symbol, &self.config.exchange);
        ws_sink
            .send(encode_ws_message(&snap))
            .await
            .map_err(|e| RithmicError::WebSocket(format!("send DBO snapshot req: {e}")))?;

        eprintln!("[client] subscribed + snapshot requested, starting pipeline...");

        // Create channels
        let (raw_msg_tx, raw_msg_rx) = mpsc::channel::<Vec<u8>>(RAW_MSG_BUF);
        let (pipeline_cmd_tx, pipeline_cmd_rx) = mpsc::channel::<PipelineCommand>(PIPELINE_CMD_BUF);
        let (bbo_tx, bbo_rx) = mpsc::channel(BBO_BUF);
        let (output_tx, mut output_rx) = mpsc::channel::<FeatureOutput>(OUTPUT_BUF);
        let (ws_cmd_tx, mut ws_cmd_rx) = mpsc::channel::<Vec<u8>>(WS_CMD_BUF);
        let (recovery_tx, mut recovery_rx) = mpsc::channel::<()>(RECOVERY_BUF);

        // Optional S3 capture channel
        let raw_capture_tx: Option<mpsc::Sender<CaptureRecord>> = if self.config.s3_bucket.is_some()
        {
            let (tx, mut rx) = mpsc::channel::<CaptureRecord>(RAW_CAPTURE_BUF);
            // Spawn a simple capture task that just counts (S3 upload deferred)
            let cap_counters = counters.clone();
            tokio::spawn(async move {
                let mut count = 0u64;
                while let Some(_record) = rx.recv().await {
                    count += 1;
                    if count % 10000 == 0 {
                        eprintln!("[capture] {} records buffered (S3 upload not yet implemented)", count);
                    }
                }
                eprintln!("[capture] channel closed after {} records", count);
                let _ = cap_counters; // keep alive
            });
            Some(tx)
        } else {
            None
        };

        // Spawn WebSocket read task
        let read_liveness = liveness.clone();
        let ws_read_handle = tokio::spawn(async move {
            while let Some(msg_result) = ws_stream.next().await {
                match msg_result {
                    Ok(msg) => {
                        read_liveness.record_inbound();
                        if let tokio_tungstenite::tungstenite::protocol::Message::Binary(data) = msg {
                            if raw_msg_tx.send(data.to_vec()).await.is_err() {
                                eprintln!("[ws_read] raw_msg_tx closed");
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[ws_read] error: {e}");
                        break;
                    }
                }
            }
            eprintln!("[ws_read] stream ended");
        });

        // Spawn write task: multiplexes heartbeat sends and dispatcher commands
        let hb_liveness = liveness.clone();
        let ws_write_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(heartbeat_interval);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        // Check liveness
                        if hb_liveness.is_dead(heartbeat_interval) {
                            eprintln!("[write] connection appears dead");
                            break;
                        }

                        // Send heartbeat
                        let hb = heartbeat::make_heartbeat_request();
                        if ws_sink.send(encode_ws_message(&hb)).await.is_err() {
                            eprintln!("[write] heartbeat send failed");
                            break;
                        }
                    }

                    cmd = ws_cmd_rx.recv() => {
                        match cmd {
                            Some(data) => {
                                let msg = tokio_tungstenite::tungstenite::protocol::Message::Binary(data.into());
                                if ws_sink.send(msg).await.is_err() {
                                    eprintln!("[write] command send failed");
                                    break;
                                }
                            }
                            None => {
                                // ws_cmd_tx dropped — dispatcher exited
                                eprintln!("[write] command channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        });

        // Spawn dispatcher task
        let disp_counters = counters.clone();
        let disp_liveness = liveness.clone();
        let instrument_id = 1u32; // /MES instrument ID
        let disp_symbol = self.config.symbol.clone();
        let disp_exchange = self.config.exchange.clone();
        // Clone ws_cmd_tx before moving into dispatcher — recovery listener needs it too.
        let recovery_ws_cmd_tx = ws_cmd_tx.clone();
        let dispatcher_handle = tokio::spawn(async move {
            dispatcher::run_dispatcher(
                raw_msg_rx,
                pipeline_cmd_tx,
                bbo_tx,
                ws_cmd_tx,
                raw_capture_tx,
                disp_counters,
                disp_liveness,
                instrument_id,
                disp_symbol,
                disp_exchange,
            )
            .await
        });

        // Spawn recovery listener: when pipeline signals a divergence, re-request snapshot.
        let recovery_symbol = self.config.symbol.clone();
        let recovery_exchange = self.config.exchange.clone();
        let recovery_handle = tokio::spawn(async move {
            while recovery_rx.recv().await.is_some() {
                eprintln!("[recovery] divergence signal received — requesting fresh DBO snapshot");
                let snap = subscription::request_dbo_snapshot(&recovery_symbol, &recovery_exchange);
                let encoded = encode_ws_message(&snap);
                if let tokio_tungstenite::tungstenite::protocol::Message::Binary(data) = encoded {
                    if recovery_ws_cmd_tx.send(data.to_vec()).await.is_err() {
                        eprintln!("[recovery] ws_cmd_tx closed, cannot request snapshot");
                        break;
                    }
                }
            }
            eprintln!("[recovery] channel closed");
        });

        // Spawn pipeline task
        let pipe_counters = counters.clone();
        let pipe_health = health.clone();
        let tick_size = self.config.tick_size;
        let pipeline_handle = tokio::spawn(async move {
            pipeline::run_pipeline(
                pipeline_cmd_rx,
                bbo_rx,
                output_tx,
                recovery_tx,
                pipe_health,
                pipe_counters,
                instrument_id,
                tick_size,
            )
            .await
        });

        // Output task: print features to stdout
        let output_handle = tokio::spawn(async move {
            while let Some(output) = output_rx.recv().await {
                println!(
                    "ts={} features={:?}",
                    output.timestamp, output.features
                );
            }
        });

        // Stats reporting task
        let stats_counters = counters.clone();
        let stats_health = health.clone();
        let stats_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;
                let s = stats_counters.snapshot();
                eprintln!("[stats] {}", stats_counters.summary());
                stats_health.log("stats", serde_json::json!({
                    "recv": s.received,
                    "proc": s.processed,
                    "dbo": s.dbo,
                    "bbo": s.bbo,
                    "trade": s.trade,
                    "gaps": s.sequence_gaps,
                    "validations": s.bbo_validations,
                    "divergences": s.bbo_divergences,
                    "recoveries": s.snapshot_recoveries,
                    "drops": s.capture_drops,
                }));
            }
        });

        // Wait for any task to complete (or Ctrl+C)
        tokio::select! {
            _ = ws_read_handle => {
                eprintln!("[client] WebSocket read task ended");
            }
            _ = ws_write_handle => {
                eprintln!("[client] write task ended");
            }
            r = dispatcher_handle => {
                match r {
                    Ok(Ok(())) => eprintln!("[client] dispatcher ended normally"),
                    Ok(Err(e)) => eprintln!("[client] dispatcher error: {e}"),
                    Err(e) => eprintln!("[client] dispatcher panicked: {e}"),
                }
            }
            r = pipeline_handle => {
                match r {
                    Ok(Ok(())) => eprintln!("[client] pipeline ended normally"),
                    Ok(Err(e)) => eprintln!("[client] pipeline error: {e}"),
                    Err(e) => eprintln!("[client] pipeline panicked: {e}"),
                }
            }
            _ = recovery_handle => {
                eprintln!("[client] recovery task ended");
            }
            _ = output_handle => {
                eprintln!("[client] output task ended");
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[client] Ctrl+C received, shutting down...");
            }
        }

        stats_handle.abort();
        let final_stats = counters.summary();
        eprintln!("[client] final stats: {final_stats}");
        let s = counters.snapshot();
        health.log("shutdown", serde_json::json!({
            "recv": s.received,
            "proc": s.processed,
            "dbo": s.dbo,
            "bbo": s.bbo,
            "trade": s.trade,
            "gaps": s.sequence_gaps,
            "validations": s.bbo_validations,
            "divergences": s.bbo_divergences,
            "recoveries": s.snapshot_recoveries,
        }));

        Ok(())
    }
}
