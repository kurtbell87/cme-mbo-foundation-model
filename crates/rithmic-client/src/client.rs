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
//! ## Multi-instrument architecture
//!
//! Each instrument gets its own pipeline task (separate tokio::spawn, runs on
//! its own OS thread in the multi-threaded runtime). The dispatcher and BBO
//! reader demux by symbol and send to per-instrument channels.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};

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
use crate::adapter::BboUpdate;
use crate::InfraType;

/// Channel buffer sizes.
const RAW_MSG_BUF: usize = 4096;
const PIPELINE_CMD_BUF: usize = 8192;
const BBO_BUF: usize = 1024;
const RAW_CAPTURE_BUF: usize = 4096;
const OUTPUT_BUF: usize = 256;
const WS_CMD_BUF: usize = 64;

/// Structured result from a single `run()` session.
pub struct RunResult {
    pub exit_reason: String,
    pub degraded: bool,
    pub ran_duration: Duration,
}

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
    /// Spawns one pipeline task per instrument (separate threads via tokio).
    /// The dispatcher and BBO reader demux by symbol and route to per-instrument
    /// channels.
    pub async fn run(&self) -> Result<RunResult, RithmicError> {
        let run_start = Instant::now();
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

        let instrument_names: Vec<String> = self.config.instruments.iter()
            .map(|s| format!("{}@{}", s.symbol, s.exchange))
            .collect();

        health.log("session_start", serde_json::json!({
            "instruments": instrument_names,
            "dev_mode": self.config.dev_mode,
            "log_file": self.config.log_file,
            "architecture": "split-routing-multi-instrument",
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

        // Subscribe to market data + DBO + initial snapshot for each instrument
        for sc in &self.config.instruments {
            eprintln!(
                "[client] subscribing to {} on {} (instrument_id={})...",
                sc.symbol, sc.exchange, sc.instrument_id
            );

            let mdu = subscription::subscribe_market_data(&sc.symbol, &sc.exchange);
            ws_sink
                .send(encode_ws_message(&mdu))
                .await
                .map_err(|e| RithmicError::WebSocket(format!("send market data sub for {}: {e}", sc.symbol)))?;

            let dbo = subscription::subscribe_depth_by_order(&sc.symbol, &sc.exchange);
            ws_sink
                .send(encode_ws_message(&dbo))
                .await
                .map_err(|e| RithmicError::WebSocket(format!("send DBO sub for {}: {e}", sc.symbol)))?;

            let snap = subscription::request_dbo_snapshot(&sc.symbol, &sc.exchange);
            ws_sink
                .send(encode_ws_message(&snap))
                .await
                .map_err(|e| RithmicError::WebSocket(format!("send DBO snapshot req for {}: {e}", sc.symbol)))?;
        }

        eprintln!("[client] all instruments subscribed + snapshots requested, starting pipeline...");

        // ---------------------------------------------------------------
        // Create per-instrument channels
        // ---------------------------------------------------------------
        let mut instrument_cmd_txs: HashMap<u32, mpsc::Sender<PipelineCommand>> = HashMap::new();
        let mut instrument_bbo_txs: HashMap<u32, mpsc::Sender<BboUpdate>> = HashMap::new();
        let mut symbol_to_id: HashMap<String, u32> = HashMap::new();

        // Also collect trade_txs (clones of cmd_txs) for the BBO reader
        let mut instrument_trade_txs: HashMap<u32, mpsc::Sender<PipelineCommand>> = HashMap::new();

        let (output_tx, mut output_rx) = mpsc::channel::<FeatureOutput>(OUTPUT_BUF);
        let (ws_cmd_tx, mut ws_cmd_rx) = mpsc::channel::<Vec<u8>>(WS_CMD_BUF);

        let mut pipeline_handles = Vec::new();

        for sc in &self.config.instruments {
            let (cmd_tx, cmd_rx) = mpsc::channel::<PipelineCommand>(PIPELINE_CMD_BUF);
            let (bbo_tx, bbo_rx) = mpsc::channel::<BboUpdate>(BBO_BUF);

            // Trade events go through the same cmd channel (two producers: dispatcher + bbo_reader)
            let trade_tx = cmd_tx.clone();

            instrument_cmd_txs.insert(sc.instrument_id, cmd_tx);
            instrument_bbo_txs.insert(sc.instrument_id, bbo_tx);
            instrument_trade_txs.insert(sc.instrument_id, trade_tx);
            symbol_to_id.insert(sc.symbol.clone(), sc.instrument_id);

            // Spawn pipeline task for this instrument
            let pipe_counters = counters.clone();
            let pipe_health = health.clone();
            let pipe_output_tx = output_tx.clone();
            let pipe_instrument_id = sc.instrument_id;
            let pipe_symbol = sc.symbol.clone();
            let pipe_tick_size = sc.tick_size;

            let handle = tokio::spawn(async move {
                pipeline::run_pipeline(
                    cmd_rx,
                    bbo_rx,
                    pipe_output_tx,
                    pipe_health,
                    pipe_counters,
                    pipe_instrument_id,
                    pipe_symbol,
                    pipe_tick_size,
                )
                .await
            });

            pipeline_handles.push((sc.symbol.clone(), handle));
        }

        // Drop the original output_tx so the output task closes when all pipelines finish
        drop(output_tx);

        // ---------------------------------------------------------------
        // Router channels
        // ---------------------------------------------------------------
        let (dbo_raw_tx, dbo_raw_rx) = mpsc::channel::<(Vec<u8>, u64)>(RAW_MSG_BUF);
        let (bbo_raw_tx, bbo_raw_rx) = mpsc::channel::<(Vec<u8>, u64)>(RAW_MSG_BUF);

        // Optional S3 capture channel
        let raw_capture_tx: Option<mpsc::Sender<CaptureRecord>> = if let Some(ref bucket) = self.config.s3_bucket
        {
            let (tx, rx) = mpsc::channel::<CaptureRecord>(RAW_CAPTURE_BUF);
            let cap_bucket = bucket.clone();
            let cap_symbol = self.config.primary_symbol().to_string();
            tokio::spawn(async move {
                if let Err(e) = crate::capture::run_capture_uploader(rx, cap_bucket, cap_symbol).await {
                    eprintln!("[capture] uploader error: {e}");
                }
            });
            Some(tx)
        } else {
            None
        };

        // ---------------------------------------------------------------
        // Spawn WebSocket read + router task
        // ---------------------------------------------------------------
        let read_liveness = liveness.clone();
        let ws_read_handle = tokio::spawn(async move {
            while let Some(msg_result) = ws_stream.next().await {
                let receive_wall_ns = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64;
                match msg_result {
                    Ok(msg) => {
                        read_liveness.record_inbound();
                        if let tokio_tungstenite::tungstenite::protocol::Message::Binary(data) = msg {
                            let raw = data.to_vec();

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
                                    if dbo_raw_tx.send((raw, receive_wall_ns)).await.is_err() {
                                        eprintln!("[router] dbo_raw_tx closed");
                                        break;
                                    }
                                }
                                // BBO + trade messages → BBO reader
                                151 | 150 | 101 => {
                                    if bbo_raw_tx.send((raw, receive_wall_ns)).await.is_err() {
                                        eprintln!("[router] bbo_raw_tx closed");
                                        break;
                                    }
                                }
                                // Connection-level messages → both channels
                                19 | 75 | 77 | 13 => {
                                    let raw2 = raw.clone();
                                    if dbo_raw_tx.send((raw, receive_wall_ns)).await.is_err() {
                                        eprintln!("[router] dbo_raw_tx closed");
                                        break;
                                    }
                                    if bbo_raw_tx.send((raw2, receive_wall_ns)).await.is_err() {
                                        eprintln!("[router] bbo_raw_tx closed");
                                        break;
                                    }
                                }
                                // Unknown → dispatcher (for logging)
                                _ => {
                                    if dbo_raw_tx.send((raw, receive_wall_ns)).await.is_err() {
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
        // Spawn write task: heartbeat + outbound commands
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
        // Spawn dispatcher (DBO channel → per-instrument pipeline tasks)
        // ---------------------------------------------------------------
        let disp_counters = counters.clone();
        let disp_liveness = liveness.clone();
        let disp_instruments = self.config.instruments.clone();
        let disp_capture_tx = raw_capture_tx.clone();
        let dispatcher_handle = tokio::spawn(async move {
            dispatcher::run_dispatcher(
                dbo_raw_rx,
                instrument_cmd_txs,
                ws_cmd_tx,
                disp_capture_tx,
                disp_counters,
                disp_liveness,
                disp_instruments,
            )
            .await
        });

        // ---------------------------------------------------------------
        // Spawn BBO reader (BBO channel → per-instrument pipeline tasks)
        // ---------------------------------------------------------------
        let bbo_reader_counters = counters.clone();
        let bbo_reader_liveness = liveness.clone();
        let bbo_reader_capture_tx = raw_capture_tx.clone();
        let bbo_reader_handle = tokio::spawn(async move {
            bbo_reader::run_bbo_reader(
                bbo_raw_rx,
                instrument_bbo_txs,
                instrument_trade_txs,
                bbo_reader_capture_tx,
                bbo_reader_counters,
                bbo_reader_liveness,
                symbol_to_id,
            )
            .await
        });

        // Output task — prints features from ALL instruments
        let output_handle = tokio::spawn(async move {
            while let Some(output) = output_rx.recv().await {
                println!(
                    "instrument_id={} ts={} features={:?}",
                    output.instrument_id, output.timestamp, output.features
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
        // Monitor all tasks — first failure triggers shutdown
        // ---------------------------------------------------------------

        // Build a future that resolves when any pipeline task finishes
        let pipeline_monitor = async {
            // We can't select! over a dynamic vec, so use tokio::select! on a JoinSet
            // approach: poll all handles
            let mut set = tokio::task::JoinSet::new();
            for (sym, handle) in pipeline_handles {
                let sym_clone = sym.clone();
                set.spawn(async move {
                    let result = handle.await;
                    (sym_clone, result)
                });
            }
            set.join_next().await
        };

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
            r = pipeline_monitor => {
                match r {
                    Some(Ok((sym, Ok(Ok(()))))) => {
                        eprintln!("[client] pipeline {} ended normally", sym);
                        (format!("pipeline_{}_ok", sym), false)
                    }
                    Some(Ok((sym, Ok(Err(e))))) => {
                        let is_degraded = matches!(e, RithmicError::BookDegraded(_));
                        eprintln!("[client] pipeline {} error: {e}", sym);
                        (format!("pipeline_{}_err: {e}", sym), is_degraded)
                    }
                    Some(Ok((sym, Err(e)))) => {
                        eprintln!("[client] pipeline {} panicked: {e}", sym);
                        (format!("pipeline_{}_panic: {e}", sym), false)
                    }
                    Some(Err(e)) => {
                        eprintln!("[client] pipeline join error: {e}");
                        (format!("pipeline_join_err: {e}"), false)
                    }
                    None => {
                        eprintln!("[client] no pipeline tasks to monitor");
                        ("no_pipelines".to_string(), false)
                    }
                }
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
        let ran_duration = run_start.elapsed();
        let final_stats = counters.summary();
        eprintln!("[client] final stats: {final_stats}");
        let s = counters.snapshot();
        health.log("session_end", serde_json::json!({
            "exit_reason": exit_reason,
            "degraded": degraded,
            "ran_duration_s": ran_duration.as_secs(),
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

        if degraded {
            return Err(RithmicError::BookDegraded(exit_reason));
        }

        if exit_reason == "ctrl_c" {
            return Ok(RunResult {
                exit_reason,
                degraded: false,
                ran_duration,
            });
        }

        Err(RithmicError::Disconnected(exit_reason))
    }
}
