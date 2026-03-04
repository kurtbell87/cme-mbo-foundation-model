//! Dispatcher: decodes raw WebSocket messages and routes by template_id.
//!
//! The dispatcher extracts metadata once per message (no double-parsing)
//! and sends typed events to the appropriate downstream channels.
//!
//! ## Snapshot state machine
//!
//! Both initial cold-start and gap recovery use a unified `LoadingSnapshot`
//! state. In both cases:
//!   1. ClearBook sent to pipeline (book wiped)
//!   2. 116 (ResponseDepthByOrderSnapshot) entries applied as Add events
//!   3. Incoming 160 (DepthByOrder) messages buffered (not applied yet)
//!   4. 161 (DepthByOrderEndEvent) fires:
//!      - Records `snapshot_end_sequence`
//!      - Discards buffered 160s with seq <= snapshot_end_sequence
//!      - Replays buffered 160s with seq > snapshot_end_sequence
//!      - Sends SnapshotComplete to pipeline
//!      - Transitions to Streaming
//!
//! This guarantees the book is fully loaded before any incremental update
//! is applied, and no incremental is applied twice (snapshot + incremental).
//!
//! ## Gap recovery
//!
//! On sequence gap detection:
//!   1. Buffer the gap-triggering 160 message
//!   2. Send ClearBook
//!   3. Request new snapshot (115)
//!   4. Enter LoadingSnapshot — same flow as cold start

use std::collections::VecDeque;
use std::time::Instant;

use prost::Message;
use tokio::sync::mpsc;

use crate::adapter::{
    self, BboUpdate, OrderEvent, OrderIdMap,
};
use crate::connection::{decode_ws_payload, encode_ws_message};
use crate::counters::MessageCounters;
use crate::error::RithmicError;
use crate::heartbeat::LivenessTracker;
use crate::rti;
use crate::subscription;
use crate::extract_template_id;

/// Maximum number of DBO messages to buffer during snapshot loading.
/// The Rithmic MES snapshot sends one 116 message per price level and can take
/// 30–60 s while DBO incrementals arrive at ~400–500 msgs/sec.  100k gives
/// ~3–4 min of headroom before overflow.
const SNAPSHOT_BUFFER_LIMIT: usize = 100_000;

/// Pre-parsed capture record for the S3 capture task.
#[derive(Debug, Clone)]
pub struct CaptureRecord {
    pub template_id: i32,
    pub sequence_number: Option<u64>,
    pub exchange_ts_ns: Option<u64>,
    pub gateway_ts_ns: Option<u64>,
    pub receive_ns: u64,
    pub symbol: Option<String>,
    pub raw_bytes: Vec<u8>,
}

/// Commands sent from the dispatcher to the pipeline task.
#[derive(Debug)]
pub enum PipelineCommand {
    /// A normal order event (DBO, trade, or snapshot entry).
    Event(OrderEvent),
    /// Signal the pipeline to clear the book builder (before snapshot rebuild).
    ClearBook,
    /// Signal that snapshot loading is complete (after 161).
    /// The pipeline should re-enable BBO validation after receiving this.
    SnapshotComplete,
}

/// Internal state of the dispatcher.
enum DispatcherState {
    /// Normal operation — forward events to pipeline.
    Streaming,
    /// Loading a snapshot — either initial cold-start or gap recovery.
    ///
    /// In this state:
    ///   - 116 (ResponseDepthByOrderSnapshot) entries are applied immediately
    ///   - 160 (DepthByOrder) incrementals are buffered for replay after 161
    ///   - 161 triggers buffer replay, SnapshotComplete, and → Streaming
    LoadingSnapshot {
        /// Buffered raw DBO messages (decoded DepthByOrder + sequence number).
        buffer: VecDeque<(rti::DepthByOrder, u64)>,
        /// Whether the buffer overflowed (discard all buffered on completion).
        buffer_overflowed: bool,
        /// Number of 116 snapshot entries received (diagnostics).
        snapshot_levels: u64,
    },
}

/// Run the dispatcher task.
///
/// Reads raw WebSocket binary messages from `raw_msg_rx`, decodes them,
/// and routes to the appropriate downstream channels. Handles both cold-start
/// snapshot loading and sequence gap recovery via unified LoadingSnapshot state.
pub async fn run_dispatcher(
    mut raw_msg_rx: mpsc::Receiver<Vec<u8>>,
    order_event_tx: mpsc::Sender<PipelineCommand>,
    bbo_tx: mpsc::Sender<BboUpdate>,
    ws_cmd_tx: mpsc::Sender<Vec<u8>>,
    raw_capture_tx: Option<mpsc::Sender<CaptureRecord>>,
    counters: MessageCounters,
    liveness: LivenessTracker,
    instrument_id: u32,
    symbol: String,
    exchange: String,
) -> Result<(), RithmicError> {
    let mut order_id_map = OrderIdMap::new();
    let mut last_sequence: Option<u64> = None;
    // Start Streaming so incremental DBO messages are processed immediately.
    // The first 116 (ResponseDepthByOrderSnapshot) message transitions us to
    // LoadingSnapshot and sends ClearBook.  The pipeline starts with
    // in_recovery=true independently, so no BBO validation runs against the
    // partial pre-snapshot book state.
    let mut state = DispatcherState::Streaming;
    let mut dbo_streak: u64 = 0;
    let epoch = Instant::now();

    /// Minimum consecutive DBO messages before gap detection activates.
    /// DBO sequences are global (all symbols), so small gaps between
    /// per-symbol messages are normal during subscription warmup.
    const GAP_DETECTION_MIN_STREAK: u64 = 50;

    while let Some(raw_data) = raw_msg_rx.recv().await {
        counters.inc_received();
        liveness.record_inbound();

        let receive_ns = epoch.elapsed().as_nanos() as u64;

        // Build a fake WsMessage::Binary to reuse decode_ws_payload
        let ws_msg = tokio_tungstenite::tungstenite::protocol::Message::Binary(raw_data.clone().into());
        let payload = match decode_ws_payload(&ws_msg) {
            Some(p) => p,
            None => continue,
        };

        let tid = match extract_template_id(payload) {
            Ok(t) => t,
            Err(_) => continue,
        };

        match tid {
            // DepthByOrder (160) — MBO updates
            160 => {
                counters.inc_dbo();
                let dbo = match rti::DepthByOrder::decode(payload) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                // Send capture record (lossy — try_send) regardless of state
                if let Some(ref cap_tx) = raw_capture_tx {
                    let record = CaptureRecord {
                        template_id: tid,
                        sequence_number: dbo.sequence_number,
                        exchange_ts_ns: extract_exchange_ts(&dbo),
                        gateway_ts_ns: extract_gateway_ts_dbo(&dbo),
                        receive_ns,
                        symbol: dbo.symbol.clone(),
                        raw_bytes: payload.to_vec(),
                    };
                    if cap_tx.try_send(record).is_err() {
                        counters.inc_capture_drops();
                    }
                }

                match &mut state {
                    DispatcherState::Streaming => {
                        // Sequence gap detection (only after warmup streak)
                        if let Some(seq) = dbo.sequence_number {
                            if let Some(last) = last_sequence {
                                if seq != last + 1 {
                                    if dbo_streak >= GAP_DETECTION_MIN_STREAK {
                                        counters.inc_sequence_gaps();
                                        eprintln!(
                                            "[dispatcher] sequence gap: expected {}, got {} — initiating recovery",
                                            last + 1, seq
                                        );

                                        // Transition to LoadingSnapshot, buffer the gap message
                                        let mut buffer = VecDeque::new();
                                        buffer.push_back((dbo, seq));
                                        state = DispatcherState::LoadingSnapshot {
                                            buffer,
                                            buffer_overflowed: false,
                                            snapshot_levels: 0,
                                        };

                                        // Signal pipeline to clear book before snapshot rebuild
                                        if order_event_tx.send(PipelineCommand::ClearBook).await.is_err() {
                                            return Err(RithmicError::Channel(
                                                "order_event_tx closed during recovery".into(),
                                            ));
                                        }

                                        // Request new snapshot via WebSocket
                                        let snap_req = subscription::request_dbo_snapshot(&symbol, &exchange);
                                        let encoded = encode_ws_message(&snap_req);
                                        if let tokio_tungstenite::tungstenite::protocol::Message::Binary(data) = encoded {
                                            if ws_cmd_tx.send(data.to_vec()).await.is_err() {
                                                return Err(RithmicError::Channel(
                                                    "ws_cmd_tx closed during recovery".into(),
                                                ));
                                            }
                                        }

                                        eprintln!("[dispatcher] ClearBook sent, snapshot request sent, buffering DBO messages");
                                        dbo_streak = 0;
                                        continue;
                                    }
                                    // Below warmup threshold — log but don't recover
                                    dbo_streak = 0;
                                } else {
                                    dbo_streak += 1;
                                }
                            }
                            last_sequence = Some(seq);
                        }

                        // Normal path: convert and forward
                        let events = adapter::depth_by_order_to_events(&dbo, instrument_id, &mut order_id_map);
                        for event in events {
                            if order_event_tx.send(PipelineCommand::Event(event)).await.is_err() {
                                return Err(RithmicError::Channel("order_event_tx closed".into()));
                            }
                        }
                    }

                    DispatcherState::LoadingSnapshot { buffer, buffer_overflowed, .. } => {
                        // Buffer the DBO message for replay after 161 confirms snapshot end.
                        // Do NOT forward to pipeline — the book is being rebuilt from 116s.
                        if let Some(seq) = dbo.sequence_number {
                            if buffer.len() < SNAPSHOT_BUFFER_LIMIT {
                                buffer.push_back((dbo, seq));
                            } else if !*buffer_overflowed {
                                *buffer_overflowed = true;
                                eprintln!(
                                    "[dispatcher] snapshot buffer overflow ({} messages), \
                                     will rely on snapshot only after 161",
                                    SNAPSHOT_BUFFER_LIMIT
                                );
                            }
                        }
                    }
                }

                counters.inc_processed();
            }

            // ResponseDepthByOrderSnapshot (116) — snapshot data (initial or recovery)
            //
            // Rithmic uses the multi-response pattern for snapshots:
            //   - Data messages:   rp_code is EMPTY, rq_handler_rp_code[0]=="0"
            //   - Sentinel message: rp_code is NON-EMPTY (signals end-of-snapshot)
            //
            // There is no separate DepthByOrderEndEvent (161) for snapshot completion.
            // We detect the sentinel here and trigger the same replay+SnapshotComplete
            // logic that a 161 would have triggered.
            116 => {
                let snap = match rti::ResponseDepthByOrderSnapshot::decode(payload) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                // Detect sentinel: rp_code non-empty means end-of-snapshot.
                let is_sentinel = snap.rp_code
                    .as_deref()
                    .map(|codes| !codes.is_empty())
                    .unwrap_or(false);

                if is_sentinel {
                    // --- Snapshot terminator ---
                    eprintln!(
                        "[dispatcher] snapshot sentinel received: rp_code={:?} seq={:?}",
                        snap.rp_code, snap.sequence_number
                    );

                    let snapshot_end_seq = snap.sequence_number
                        .or(last_sequence)
                        .unwrap_or(0);

                    if let Some(seq) = snap.sequence_number {
                        last_sequence = Some(seq);
                    }

                    match std::mem::replace(&mut state, DispatcherState::Streaming) {
                        DispatcherState::LoadingSnapshot { buffer, buffer_overflowed, snapshot_levels } => {
                            eprintln!(
                                "[dispatcher] snapshot complete: end_seq={}, snapshot_levels={}, \
                                 buffered={}, overflowed={}",
                                snapshot_end_seq, snapshot_levels, buffer.len(), buffer_overflowed
                            );

                            if !buffer_overflowed {
                                let mut replayed = 0u64;
                                let mut discarded = 0u64;
                                for (dbo, seq) in buffer {
                                    if seq <= snapshot_end_seq {
                                        discarded += 1;
                                        continue;
                                    }
                                    let events = adapter::depth_by_order_to_events(
                                        &dbo, instrument_id, &mut order_id_map,
                                    );
                                    for event in events {
                                        if order_event_tx
                                            .send(PipelineCommand::Event(event))
                                            .await
                                            .is_err()
                                        {
                                            return Err(RithmicError::Channel(
                                                "order_event_tx closed during replay".into(),
                                            ));
                                        }
                                    }
                                    last_sequence = Some(seq);
                                    replayed += 1;
                                }
                                eprintln!(
                                    "[dispatcher] replay complete: replayed={}, discarded={}",
                                    replayed, discarded
                                );
                            } else {
                                eprintln!(
                                    "[dispatcher] snapshot complete (buffer overflowed — \
                                     relying on snapshot state only)"
                                );
                            }

                            counters.inc_snapshot_recoveries();
                            dbo_streak = 0;

                            if order_event_tx.send(PipelineCommand::SnapshotComplete).await.is_err() {
                                return Err(RithmicError::Channel(
                                    "order_event_tx closed after snapshot complete".into(),
                                ));
                            }
                        }

                        DispatcherState::Streaming => {
                            // Sentinel outside LoadingSnapshot — unexpected, ignore.
                            eprintln!(
                                "[dispatcher] WARNING: snapshot sentinel outside snapshot \
                                 context (seq={:?}) — ignoring",
                                snap.sequence_number
                            );
                        }
                    }

                    counters.inc_processed();
                    continue;
                }

                // --- Normal data message ---

                // Enter LoadingSnapshot on first 116 while in Streaming state (cold start).
                // For gap recovery, we're already in LoadingSnapshot when 116s arrive.
                if let DispatcherState::Streaming = &state {
                    state = DispatcherState::LoadingSnapshot {
                        buffer: VecDeque::new(),
                        buffer_overflowed: false,
                        snapshot_levels: 0,
                    };
                    if order_event_tx.send(PipelineCommand::ClearBook).await.is_err() {
                        return Err(RithmicError::Channel(
                            "order_event_tx closed during initial snapshot".into(),
                        ));
                    }
                    eprintln!("[dispatcher] initial DBO snapshot started — ClearBook sent");
                }

                // Track sequence number from snapshot for post-snapshot gap detection
                if let Some(seq) = snap.sequence_number {
                    last_sequence = Some(seq);
                }

                // Count snapshot levels for diagnostics
                if let DispatcherState::LoadingSnapshot { snapshot_levels, .. } = &mut state {
                    *snapshot_levels += 1;
                }

                // Convert snapshot entries to OrderEvents and send to pipeline.
                // The pipeline is in recovery mode (in_recovery=true), so BBO
                // validation is suppressed while these are applied.
                let events = adapter::snapshot_response_to_events(&snap, instrument_id, &mut order_id_map);
                for event in events {
                    if order_event_tx.send(PipelineCommand::Event(event)).await.is_err() {
                        return Err(RithmicError::Channel("order_event_tx closed".into()));
                    }
                }

                counters.inc_processed();
            }

            // DepthByOrderEndEvent (161) — kept as fallback; Rithmic typically signals
            // snapshot completion via the rp_code sentinel on the final 116 message
            // rather than a separate 161.  If a 161 does arrive, handle it the same way.
            161 => {
                let end_event = match rti::DepthByOrderEndEvent::decode(payload) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                match std::mem::replace(&mut state, DispatcherState::Streaming) {
                    DispatcherState::LoadingSnapshot { buffer, buffer_overflowed, snapshot_levels } => {
                        let snapshot_end_seq = end_event.sequence_number
                            .or(last_sequence)
                            .unwrap_or(0);

                        if let Some(seq) = end_event.sequence_number {
                            last_sequence = Some(seq);
                        }

                        eprintln!(
                            "[dispatcher] snapshot complete (via 161): end_seq={}, \
                             snapshot_levels={}, buffered={}, overflowed={}",
                            snapshot_end_seq, snapshot_levels, buffer.len(), buffer_overflowed
                        );

                        if !buffer_overflowed {
                            let mut replayed = 0u64;
                            let mut discarded = 0u64;
                            for (dbo, seq) in buffer {
                                if seq <= snapshot_end_seq {
                                    discarded += 1;
                                    continue;
                                }
                                let events = adapter::depth_by_order_to_events(
                                    &dbo, instrument_id, &mut order_id_map,
                                );
                                for event in events {
                                    if order_event_tx
                                        .send(PipelineCommand::Event(event))
                                        .await
                                        .is_err()
                                    {
                                        return Err(RithmicError::Channel(
                                            "order_event_tx closed during replay".into(),
                                        ));
                                    }
                                }
                                last_sequence = Some(seq);
                                replayed += 1;
                            }
                            eprintln!(
                                "[dispatcher] replay complete: replayed={}, discarded={}",
                                replayed, discarded
                            );
                        } else {
                            eprintln!(
                                "[dispatcher] snapshot complete (buffer overflowed — \
                                 relying on snapshot state only)"
                            );
                        }

                        counters.inc_snapshot_recoveries();
                        dbo_streak = 0;

                        if order_event_tx.send(PipelineCommand::SnapshotComplete).await.is_err() {
                            return Err(RithmicError::Channel(
                                "order_event_tx closed after snapshot complete".into(),
                            ));
                        }
                    }

                    DispatcherState::Streaming => {
                        eprintln!(
                            "[dispatcher] WARNING: 161 received outside snapshot context \
                             (seq={:?}) — ignoring",
                            end_event.sequence_number
                        );
                    }
                }

                counters.inc_processed();
            }

            // LastTrade (150)
            150 => {
                counters.inc_trade();
                let trade = match rti::LastTrade::decode(payload) {
                    Ok(t) => t,
                    Err(_) => continue,
                };

                // Send capture record (lossy)
                if let Some(ref cap_tx) = raw_capture_tx {
                    let record = CaptureRecord {
                        template_id: tid,
                        sequence_number: None,
                        exchange_ts_ns: extract_exchange_ts_trade(&trade),
                        gateway_ts_ns: extract_gateway_ts_trade(&trade),
                        receive_ns,
                        symbol: trade.symbol.clone(),
                        raw_bytes: payload.to_vec(),
                    };
                    if cap_tx.try_send(record).is_err() {
                        counters.inc_capture_drops();
                    }
                }

                // Trades are forwarded even during snapshot loading — they don't affect
                // book state (action='T') and are safe to apply at any time.
                if let Some(event) = adapter::last_trade_to_event(&trade, instrument_id) {
                    if order_event_tx.send(PipelineCommand::Event(event)).await.is_err() {
                        return Err(RithmicError::Channel("order_event_tx closed".into()));
                    }
                }

                counters.inc_processed();
            }

            // BestBidOffer (151)
            151 => {
                counters.inc_bbo();
                let bbo = match rti::BestBidOffer::decode(payload) {
                    Ok(b) => b,
                    Err(_) => continue,
                };

                // Send capture record (lossy)
                if let Some(ref cap_tx) = raw_capture_tx {
                    let record = CaptureRecord {
                        template_id: tid,
                        sequence_number: None,
                        exchange_ts_ns: None,
                        gateway_ts_ns: extract_gateway_ts_bbo(&bbo),
                        receive_ns,
                        symbol: bbo.symbol.clone(),
                        raw_bytes: payload.to_vec(),
                    };
                    if cap_tx.try_send(record).is_err() {
                        counters.inc_capture_drops();
                    }
                }

                // BBO always forwarded — pipeline suppresses validation during recovery
                if let Some(update) = adapter::best_bid_offer_to_update(&bbo) {
                    if bbo_tx.send(update).await.is_err() {
                        return Err(RithmicError::Channel("bbo_tx closed".into()));
                    }
                }

                counters.inc_processed();
            }

            // ResponseMarketDataUpdate (101) — subscription ack
            101 => {
                if let Ok(resp) = rti::ResponseMarketDataUpdate::decode(payload) {
                    eprintln!(
                        "[dispatcher] market data sub ack: rp_code={:?} user_msg={:?}",
                        resp.rp_code, resp.user_msg
                    );
                }
                counters.inc_processed();
            }

            // ResponseDepthByOrderUpdates (118) — DBO subscription ack
            118 => {
                if let Ok(resp) = rti::ResponseDepthByOrderUpdates::decode(payload) {
                    eprintln!(
                        "[dispatcher] DBO sub ack: rp_code={:?} user_msg={:?}",
                        resp.rp_code, resp.user_msg
                    );
                }
                // Reset sequence tracking — the incremental stream starts fresh
                last_sequence = None;
                dbo_streak = 0;
                counters.inc_processed();
            }

            // ResponseHeartbeat (19)
            19 => {
                // Liveness already recorded above
                counters.inc_processed();
            }

            // Reject (75)
            75 => {
                if let Ok(reject) = rti::Reject::decode(payload) {
                    eprintln!(
                        "[dispatcher] server reject: {:?} {:?}",
                        reject.rp_code, reject.user_msg
                    );
                }
                counters.inc_processed();
            }

            // ResponseLogout (13) — graceful server-initiated session close
            13 => {
                eprintln!("[dispatcher] server sent ResponseLogout — closing");
                return Err(RithmicError::ForcedLogout(
                    "server graceful logout".to_string(),
                ));
            }

            // ForcedLogout (77)
            77 => {
                if let Ok(logout) = rti::ForcedLogout::decode(payload) {
                    eprintln!(
                        "[dispatcher] forced logout: {:?} {:?}",
                        logout.rp_code, logout.user_msg
                    );
                }
                return Err(RithmicError::ForcedLogout(
                    "server forced logout".to_string(),
                ));
            }

            // All other messages — skip
            _ => {
                eprintln!("[dispatcher] unhandled template_id={}", tid);
                counters.inc_processed();
            }
        }
    }

    Ok(())
}

// Helper functions to extract timestamps without duplicating adapter logic

fn extract_exchange_ts(dbo: &rti::DepthByOrder) -> Option<u64> {
    match (dbo.source_ssboe, dbo.source_nsecs) {
        (Some(ss), Some(ns)) => Some(adapter::exchange_ts_ns(ss, ns)),
        _ => None,
    }
}

fn extract_gateway_ts_dbo(dbo: &rti::DepthByOrder) -> Option<u64> {
    match (dbo.ssboe, dbo.usecs) {
        (Some(ss), Some(us)) => Some(adapter::gateway_ts_ns(ss, us)),
        _ => None,
    }
}

fn extract_exchange_ts_trade(trade: &rti::LastTrade) -> Option<u64> {
    match (trade.source_ssboe, trade.source_nsecs) {
        (Some(ss), Some(ns)) => Some(adapter::exchange_ts_ns(ss, ns)),
        _ => None,
    }
}

fn extract_gateway_ts_trade(trade: &rti::LastTrade) -> Option<u64> {
    match (trade.ssboe, trade.usecs) {
        (Some(ss), Some(us)) => Some(adapter::gateway_ts_ns(ss, us)),
        _ => None,
    }
}

fn extract_gateway_ts_bbo(bbo: &rti::BestBidOffer) -> Option<u64> {
    match (bbo.ssboe, bbo.usecs) {
        (Some(ss), Some(us)) => Some(adapter::gateway_ts_ns(ss, us)),
        _ => None,
    }
}
