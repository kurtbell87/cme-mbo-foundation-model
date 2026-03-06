//! Client orchestrator: supervisor that manages the full pipeline lifecycle.
//!
//! ## Split-routing architecture (single socket)
//!
//! A single TickerPlant WebSocket carries all messages. A lightweight router
//! task extracts the template_id from each raw message and immediately routes
//! it to one of two channels:
//!
//! | Channel | Messages | Consumer |
//! |---|---|---|
//! | `dbo_raw` | 160, 116, 161, 118, 19, 75, 77, 13 | Dispatcher |
//! | `bbo_raw` | 151, 150, 101, 19, 75, 77, 13 | BBO Reader |
//!
//! Heartbeat (19), reject (75), and logout (77/13) go to BOTH channels so
//! each consumer can independently react to connection-level events.
//!
//! This eliminates processing serialization: snapshot loading (1731 entries)
//! no longer blocks BBO/trade handling, and vice versa.
//!
//! ## Future: two-connection upgrade
//!
//! When the Rithmic account supports 2 TickerPlant sessions, replace the
//! single socket + router with two independent connections. The dispatcher
//! and BBO reader modules are already separated for this purpose.
//! At current /MES throughput (~500 DBO msgs/sec, ~3 BBO msgs/sec) the
//! single-socket approach is adequate. Two connections would eliminate the
//! BBO-before-DBO arrival ordering inherent to a shared socket.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use crate::auth;
use crate::bbo_reader;
use crate::config::RithmicConfig;
use crate::connection::{decode_ws_payload, encode_ws_message};
use crate::counters::MessageCounters;
use crate::dispatcher::{self, CaptureRecord, PipelineCommand};
use crate::error::RithmicError;
use crate::extract_template_id;
use crate::health_log::HealthLogger;
use crate::heartbeat::{self, LivenessTracker};
use crate::pipeline::{self, FeatureOutput};
use crate::subscription;
use crate::InfraType;

/// Channel buffer sizes.
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
    /// Uses a single TickerPlant connection with split routing: a lightweight
    /// router task dispatches raw messages by template_id to the dispatcher
    /// (DBO) and BBO reader (BBO/trades) concurrently.
    pub async fn run(&self) -> Result<(), RithmicError> {
        let counters = MessageCounters::new();
        let liveness = LivenessTracker::new();

        let health = match HealthLogger::open(&self.config.log_file) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[client] WARNING: could not open health log {}: {e}", self.config.log_file);
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
            "architecture": "split-routing",
        }));

        eprintln!("[client] authenticating to {}...", self.config.uri);

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

        // Subscribe to market data + DBO + initial snapshot
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

        let snap = subscription::request_dbo_snapshot(&self.config.symbol, &self.config.exchange);
        ws_sink
            .send(encode_ws_message(&snap))
            .await
            .map_err(|e| RithmicError::WebSocket(format!("send DBO snapshot req: {e}")))?;

        eprintln!("[client] subscribed + snapshot requested, starting pipeline...");

        // ---------------------------------------------------------------
        // Create channels
        // ---------------------------------------------------------------
        // Router → dispatcher (DBO messages)
        let (dbo_raw_tx, dbo_raw_rx) = mpsc::channel::<Vec<u8>>(RAW_MSG_BUF);
        // Router → BBO reader (BBO + trade messages)
        let (bbo_raw_tx, bbo_raw_rx) = mpsc::channel::<Vec<u8>>(RAW_MSG_BUF);
        // Dispatcher → pipeline (order events + control)
        let (pipeline_cmd_tx, pipeline_cmd_rx) = mpsc::channel::<PipelineCommand>(PIPELINE_CMD_BUF);
        // BBO reader also sends trade events to pipeline (two producers, one consumer)
        let trade_cmd_tx = pipeline_cmd_tx.clone();
        // BBO reader → pipeline (BBO updates for validation)
        let (bbo_tx, bbo_rx) = mpsc::channel(BBO_BUF);
        let (output_tx, mut output_rx) = mpsc::channel::<FeatureOutput>(OUTPUT_BUF);
        let (ws_cmd_tx, mut ws_cmd_rx) = mpsc::channel::<Vec<u8>>(WS_CMD_BUF);
        let (recovery_tx, mut recovery_rx) = mpsc::channel::<()>(RECOVERY_BUF);

        // Optional S3 capture channel (shared between dispatcher and BBO reader)
        let raw_capture_tx: Option<mpsc::Sender<CaptureRecord>> = if self.config.s3_bucket.is_some()
        {
            let (tx, mut rx) = mpsc::channel::<CaptureRecord>(RAW_CAPTURE_BUF);
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
                let _ = cap_counters;
            });
            Some(tx)
        } else {
            None
        };

        // ---------------------------------------------------------------
        // Spawn WebSocket read + router task
        //
        // Reads raw binary messages, extracts template_id, and routes to
        // the appropriate downstream channel. This is the only task that
        // touches the WebSocket read half.
        // ---------------------------------------------------------------
        let read_liveness = liveness.clone();
        let ws_read_handle = tokio::spawn(async move {
            while let Some(msg_result) = ws_stream.next().await {
                match msg_result {
                    Ok(msg) => {
                        read_liveness.record_inbound();
                        if let tokio_tungstenite::tungstenite::protocol::Message::Binary(data) = msg {
                            let raw = data.to_vec();

                            // Extract template_id for routing (cheap — only reads one varint field)
                            let ws_msg = tokio_tungstenite::tungstenite::protocol::Message::Binary(
                                raw.clone().into(),
                            );
                            let tid = match decode_ws_payload(&ws_msg) {
                                Some(payload) => extract_template_id(payload).unwrap_or(-1),
                                None => continue,
                            };

                            match tid {
                                // DBO messages → dispatcher
                                160 | 116 | 161 | 118 => {
                                    if dbo_raw_tx.send(raw).await.is_err() {
                                        eprintln!("[router] dbo_raw_tx closed");
                                        break;
                                    }
                                }
                                // BBO + trade messages → BBO reader
                                151 | 150 | 101 => {
                                    if bbo_raw_tx.send(raw).await.is_err() {
                                        eprintln!("[router] bbo_raw_tx closed");
                                        break;
                                    }
                                }
                                // Connection-level messages → both channels
                                19 | 75 | 77 | 13 => {
                                    let raw2 = raw.clone();
                                    if dbo_raw_tx.send(raw).await.is_err() {
                                        eprintln!("[router] dbo_raw_tx closed");
                                        break;
                                    }
                                    if bbo_raw_tx.send(raw2).await.is_err() {
                                        eprintln!("[router] bbo_raw_tx closed");
                                        break;
                                    }
                                }
                                // Unknown → dispatcher (for logging)
                                _ => {
                                    if dbo_raw_tx.send(raw).await.is_err() {
                                        eprintln!("[router] dbo_raw_tx closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[router] WebSocket error: {e}");
                        break;
                    }
                }
            }
            eprintln!("[router] stream ended");
        });

        // ---------------------------------------------------------------
        // Spawn write task: heartbeat + outbound commands (snapshot requests)
        // ---------------------------------------------------------------
        let hb_liveness = liveness.clone();
        let ws_write_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(heartbeat_interval);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if hb_liveness.is_dead(heartbeat_interval) {
                            eprintln!("[write] connection appears dead");
                            break;
                        }
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
                                eprintln!("[write] command channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        });

        // ---------------------------------------------------------------
        // Spawn dispatcher (DBO channel → pipeline)
        // ---------------------------------------------------------------
        let disp_counters = counters.clone();
        let disp_liveness = liveness.clone();
        let instrument_id = 1u32;
        let disp_symbol = self.config.symbol.clone();
        let disp_exchange = self.config.exchange.clone();
        let recovery_ws_cmd_tx = ws_cmd_tx.clone();
        let disp_capture_tx = raw_capture_tx.clone();
        let dispatcher_handle = tokio::spawn(async move {
            dispatcher::run_dispatcher(
                dbo_raw_rx,
                pipeline_cmd_tx,
                ws_cmd_tx,
                disp_capture_tx,
                disp_counters,
                disp_liveness,
                instrument_id,
                disp_symbol,
                disp_exchange,
            )
            .await
        });

        // ---------------------------------------------------------------
        // Spawn BBO reader (BBO channel → pipeline + bbo validation)
        // ---------------------------------------------------------------
        let bbo_reader_counters = counters.clone();
        let bbo_reader_liveness = liveness.clone();
        let bbo_reader_capture_tx = raw_capture_tx.clone();
        let bbo_reader_handle = tokio::spawn(async move {
            bbo_reader::run_bbo_reader(
                bbo_raw_rx,
                bbo_tx,
                trade_cmd_tx,
                bbo_reader_capture_tx,
                bbo_reader_counters,
                bbo_reader_liveness,
                instrument_id,
            )
            .await
        });

        // ---------------------------------------------------------------
        // Spawn recovery listener
        // ---------------------------------------------------------------
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

        // ---------------------------------------------------------------
        // Spawn pipeline task
        // ---------------------------------------------------------------
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

        // Output task
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

        // ---------------------------------------------------------------
        // Monitor all tasks
        // ---------------------------------------------------------------
        let (exit_reason, degraded) = tokio::select! {
            _ = ws_read_handle => {
                eprintln!("[client] router/read task ended");
                ("ws_read_ended".to_string(), false)
            }
            _ = ws_write_handle => {
                eprintln!("[client] write task ended");
                ("ws_write_ended".to_string(), false)
            }
            r = dispatcher_handle => {
                match r {
                    Ok(Ok(())) => { eprintln!("[client] dispatcher ended normally"); ("dispatcher_ok".to_string(), false) }
                    Ok(Err(e)) => { eprintln!("[client] dispatcher error: {e}"); (format!("dispatcher_err: {e}"), false) }
                    Err(e) => { eprintln!("[client] dispatcher panicked: {e}"); (format!("dispatcher_panic: {e}"), false) }
                }
            }
            r = bbo_reader_handle => {
                match r {
                    Ok(Ok(())) => { eprintln!("[client] BBO reader ended normally"); ("bbo_reader_ok".to_string(), false) }
                    Ok(Err(e)) => { eprintln!("[client] BBO reader error: {e}"); (format!("bbo_reader_err: {e}"), false) }
                    Err(e) => { eprintln!("[client] BBO reader panicked: {e}"); (format!("bbo_reader_panic: {e}"), false) }
                }
            }
            r = pipeline_handle => {
                match r {
                    Ok(Ok(())) => { eprintln!("[client] pipeline ended normally"); ("pipeline_ok".to_string(), false) }
                    Ok(Err(crate::error::RithmicError::BookDegraded(ref msg))) => {
                        eprintln!("[client] pipeline DEGRADED: {msg}");
                        (format!("degraded: {msg}"), true)
                    }
                    Ok(Err(e)) => { eprintln!("[client] pipeline error: {e}"); (format!("pipeline_err: {e}"), false) }
                    Err(e) => { eprintln!("[client] pipeline panicked: {e}"); (format!("pipeline_panic: {e}"), false) }
                }
            }
            _ = recovery_handle => {
                eprintln!("[client] recovery task ended");
                ("recovery_ended".to_string(), false)
            }
            _ = output_handle => {
                eprintln!("[client] output task ended");
                ("output_ended".to_string(), false)
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[client] Ctrl+C received, shutting down...");
                ("ctrl_c".to_string(), false)
            }
        };

        stats_handle.abort();
        let final_stats = counters.summary();
        eprintln!("[client] final stats: {final_stats}");
        let s = counters.snapshot();
        health.log("shutdown", serde_json::json!({
            "exit_reason": exit_reason,
            "degraded": degraded,
            "recv": s.received,
            "proc": s.processed,
            "dbo": s.dbo,
            "bbo": s.bbo,
            "trade": s.trade,
            "gaps": s.sequence_gaps,
            "validations": s.bbo_validations,
            "divergences": s.bbo_divergences,
            "drops": s.capture_drops,
            "recoveries": s.snapshot_recoveries,
        }));

        Ok(())
    }
}
