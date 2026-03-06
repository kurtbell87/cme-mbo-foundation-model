//! Dispatcher: decodes raw DBO WebSocket messages and routes by template_id.
//!
//! In the split-routing architecture, the dispatcher only handles messages
//! from the DBO channel: DBO incrementals (160), snapshots (116),
//! snapshot end (161), DBO sub ack (118), heartbeats, rejects, and logouts.
//! BBO (151), LastTrade (150), and market data ack (101) are handled by the
//! BBO reader.
//!
//! ## Multi-instrument support
//!
//! Each instrument gets its own OrderIdMap and DispatcherState (snapshot
//! state machine). DBO sequence numbers are global across all symbols on
//! the connection, so gap detection uses a single global sequence tracker.
//! On gap: ClearBook for ALL instruments, request snapshots for ALL.
//!
//! ## Snapshot state machine
//!
//! Both initial cold-start and gap recovery use a unified `LoadingSnapshot`
//! state per instrument. In both cases:
//!   1. ClearBook sent to pipeline (book wiped)
//!   2. 116 (ResponseDepthByOrderSnapshot) entries applied as Add events
//!   3. Incoming 160 (DepthByOrder) messages buffered (not applied yet)
//!   4. 161 (DepthByOrderEndEvent) or 116-sentinel fires:
//!      - Records `snapshot_end_sequence`
//!      - Discards buffered 160s with seq <= snapshot_end_sequence
//!      - Replays buffered 160s with seq > snapshot_end_sequence
//!      - Sends SnapshotComplete to pipeline
//!      - Transitions to Streaming

use std::collections::{HashMap, VecDeque};

use prost::Message;
use tokio::sync::mpsc;

use crate::adapter::{
    self, OrderEvent, OrderIdMap,
};
use crate::config::SymbolConfig;
use crate::connection::{decode_ws_payload, encode_ws_message};
use crate::counters::MessageCounters;
use crate::error::RithmicError;
use crate::heartbeat::LivenessTracker;
use crate::rti;
use crate::subscription;
use crate::extract_template_id;

/// Maximum number of DBO messages to buffer during snapshot loading.
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
    ClearBook { instrument_id: u32 },
    /// Signal that snapshot loading is complete (after 161).
    /// The pipeline should re-enable BBO validation after receiving this.
    SnapshotComplete { instrument_id: u32 },
}

/// Per-instrument dispatcher state.
struct InstrumentDispatchState {
    symbol: String,
    exchange: String,
    order_id_map: OrderIdMap,
    state: DispatcherState,
}

/// Internal state of the dispatcher for one instrument.
enum DispatcherState {
    /// Normal operation — forward events to pipeline.
    Streaming,
    /// Loading a snapshot — either initial cold-start or gap recovery.
    LoadingSnapshot {
        buffer: VecDeque<(rti::DepthByOrder, u64)>,
        buffer_overflowed: bool,
        snapshot_levels: u64,
    },
}

/// Run the dispatcher task.
///
/// Reads raw WebSocket binary messages from the DBO connection, decodes them,
/// and routes to per-instrument pipeline tasks. Handles DBO (160), snapshots
/// (116/161), DBO sub ack (118), heartbeats, rejects, and logouts.
#[allow(clippy::too_many_arguments)]
pub async fn run_dispatcher(
    mut raw_msg_rx: mpsc::Receiver<(Vec<u8>, u64)>,
    instrument_txs: HashMap<u32, mpsc::Sender<PipelineCommand>>,
    ws_cmd_tx: mpsc::Sender<Vec<u8>>,
    raw_capture_tx: Option<mpsc::Sender<CaptureRecord>>,
    counters: MessageCounters,
    liveness: LivenessTracker,
    instruments: Vec<SymbolConfig>,
) -> Result<(), RithmicError> {
    // Build per-instrument state and symbol→instrument_id lookup
    let mut inst_state: HashMap<u32, InstrumentDispatchState> = HashMap::new();
    let mut symbol_to_id: HashMap<String, u32> = HashMap::new();

    for sc in &instruments {
        symbol_to_id.insert(sc.symbol.clone(), sc.instrument_id);
        inst_state.insert(sc.instrument_id, InstrumentDispatchState {
            symbol: sc.symbol.clone(),
            exchange: sc.exchange.clone(),
            order_id_map: OrderIdMap::new(),
            state: DispatcherState::Streaming,
        });
    }

    // Global sequence tracking (DBO sequences span all symbols)
    let mut last_sequence: Option<u64> = None;
    let mut dbo_streak: u64 = 0;

    /// Minimum consecutive DBO messages before gap detection activates.
    const GAP_DETECTION_MIN_STREAK: u64 = 50;

    while let Some((raw_data, receive_wall_ns)) = raw_msg_rx.recv().await {
        counters.inc_received();
        liveness.record_inbound();

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

                // Send capture record regardless of state
                if let Some(ref cap_tx) = raw_capture_tx {
                    let record = CaptureRecord {
                        template_id: tid,
                        sequence_number: dbo.sequence_number,
                        exchange_ts_ns: extract_exchange_ts(&dbo),
                        gateway_ts_ns: extract_gateway_ts_dbo(&dbo),
                        receive_ns: receive_wall_ns,
                        symbol: dbo.symbol.clone(),
                        raw_bytes: payload.to_vec(),
                    };
                    if cap_tx.try_send(record).is_err() {
                        counters.inc_capture_drops();
                    }
                }

                // Look up instrument by symbol
                let instrument_id = match dbo.symbol.as_deref().and_then(|s| symbol_to_id.get(s)) {
                    Some(&id) => id,
                    None => {
                        // Unknown symbol — skip (could be cross-symbol noise)
                        continue;
                    }
                };

                let inst = inst_state.get_mut(&instrument_id).unwrap();
                let tx = instrument_txs.get(&instrument_id).unwrap();

                // Global sequence gap detection
                let gap_detected = if let Some(seq) = dbo.sequence_number {
                    let gap = if let Some(last) = last_sequence {
                        if seq != last + 1 {
                            if dbo_streak >= GAP_DETECTION_MIN_STREAK {
                                true
                            } else {
                                dbo_streak = 0;
                                false
                            }
                        } else {
                            dbo_streak += 1;
                            false
                        }
                    } else {
                        false
                    };
                    last_sequence = Some(seq);
                    gap
                } else {
                    false
                };

                if gap_detected {
                    counters.inc_sequence_gaps();
                    eprintln!(
                        "[dispatcher] sequence gap detected — initiating recovery for ALL instruments"
                    );

                    // Transition ALL instruments to LoadingSnapshot
                    for (id, ist) in inst_state.iter_mut() {
                        let itx = instrument_txs.get(id).unwrap();

                        // Buffer the gap-triggering DBO if it belongs to this instrument
                        let mut buffer = VecDeque::new();
                        if *id == instrument_id {
                            if let Some(seq) = dbo.sequence_number {
                                buffer.push_back((dbo.clone(), seq));
                            }
                        }

                        ist.state = DispatcherState::LoadingSnapshot {
                            buffer,
                            buffer_overflowed: false,
                            snapshot_levels: 0,
                        };

                        if itx.send(PipelineCommand::ClearBook { instrument_id: *id }).await.is_err() {
                            return Err(RithmicError::Channel(
                                "instrument pipeline tx closed during recovery".into(),
                            ));
                        }

                        // Request new snapshot for this instrument
                        let snap_req = subscription::request_dbo_snapshot(&ist.symbol, &ist.exchange);
                        let encoded = encode_ws_message(&snap_req);
                        if let tokio_tungstenite::tungstenite::protocol::Message::Binary(data) = encoded {
                            if ws_cmd_tx.send(data.to_vec()).await.is_err() {
                                return Err(RithmicError::Channel(
                                    "ws_cmd_tx closed during recovery".into(),
                                ));
                            }
                        }
                    }

                    eprintln!("[dispatcher] ClearBook sent for all instruments, snapshot requests sent, buffering DBO messages");
                    dbo_streak = 0;
                    counters.inc_processed();
                    continue;
                }

                // Normal routing or buffering
                match &mut inst.state {
                    DispatcherState::Streaming => {
                        let events = adapter::depth_by_order_to_events(
                            &dbo, instrument_id, &mut inst.order_id_map, receive_wall_ns,
                        );
                        for event in events {
                            if tx.send(PipelineCommand::Event(event)).await.is_err() {
                                return Err(RithmicError::Channel("instrument pipeline tx closed".into()));
                            }
                        }
                    }

                    DispatcherState::LoadingSnapshot { buffer, buffer_overflowed, .. } => {
                        if let Some(seq) = dbo.sequence_number {
                            if buffer.len() < SNAPSHOT_BUFFER_LIMIT {
                                buffer.push_back((dbo, seq));
                            } else if !*buffer_overflowed {
                                *buffer_overflowed = true;
                                eprintln!(
                                    "[dispatcher] snapshot buffer overflow for instrument {} ({} messages)",
                                    instrument_id, SNAPSHOT_BUFFER_LIMIT
                                );
                            }
                        }
                    }
                }

                counters.inc_processed();
            }

            // ResponseDepthByOrderSnapshot (116) — snapshot data
            116 => {
                let snap = match rti::ResponseDepthByOrderSnapshot::decode(payload) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                // Detect sentinel: rp_code non-empty means end-of-snapshot
                let is_sentinel = snap.rp_code
                    .as_deref()
                    .map(|codes| !codes.is_empty())
                    .unwrap_or(false);

                // Route by symbol
                let instrument_id = match snap.symbol.as_deref().and_then(|s| symbol_to_id.get(s)) {
                    Some(&id) => id,
                    None => {
                        // Sentinel without symbol — try to find which instrument is loading
                        // Fall back: complete the first instrument in LoadingSnapshot
                        if is_sentinel {
                            let loading_id = inst_state.iter()
                                .find(|(_, ist)| matches!(ist.state, DispatcherState::LoadingSnapshot { .. }))
                                .map(|(&id, _)| id);
                            match loading_id {
                                Some(id) => id,
                                None => { counters.inc_processed(); continue; }
                            }
                        } else {
                            continue;
                        }
                    }
                };

                if is_sentinel {
                    // --- Snapshot terminator ---
                    eprintln!(
                        "[dispatcher] snapshot sentinel for instrument {}: rp_code={:?} seq={:?}",
                        instrument_id, snap.rp_code, snap.sequence_number
                    );

                    let snapshot_end_seq = snap.sequence_number
                        .or(last_sequence)
                        .unwrap_or(0);

                    if let Some(seq) = snap.sequence_number {
                        last_sequence = Some(seq);
                    }

                    complete_snapshot(
                        &mut inst_state, instrument_id, snapshot_end_seq,
                        &instrument_txs, &mut last_sequence, &counters, &mut dbo_streak,
                    ).await?;

                    counters.inc_processed();
                    continue;
                }

                // --- Normal data message ---
                let inst = inst_state.get_mut(&instrument_id).unwrap();
                let tx = instrument_txs.get(&instrument_id).unwrap();

                // Enter LoadingSnapshot on first 116 while in Streaming state (cold start)
                if let DispatcherState::Streaming = &inst.state {
                    inst.state = DispatcherState::LoadingSnapshot {
                        buffer: VecDeque::new(),
                        buffer_overflowed: false,
                        snapshot_levels: 0,
                    };
                    if tx.send(PipelineCommand::ClearBook { instrument_id }).await.is_err() {
                        return Err(RithmicError::Channel(
                            "instrument pipeline tx closed during initial snapshot".into(),
                        ));
                    }
                    eprintln!("[dispatcher] initial DBO snapshot started for instrument {} — ClearBook sent", instrument_id);
                }

                // Track sequence number
                if let Some(seq) = snap.sequence_number {
                    last_sequence = Some(seq);
                }

                if let DispatcherState::LoadingSnapshot { snapshot_levels, .. } = &mut inst.state {
                    *snapshot_levels += 1;
                }

                let events = adapter::snapshot_response_to_events(
                    &snap, instrument_id, &mut inst.order_id_map, 0,
                );
                for event in events {
                    if tx.send(PipelineCommand::Event(event)).await.is_err() {
                        return Err(RithmicError::Channel("instrument pipeline tx closed".into()));
                    }
                }

                counters.inc_processed();
            }

            // DepthByOrderEndEvent (161) — fallback snapshot completion
            161 => {
                let end_event = match rti::DepthByOrderEndEvent::decode(payload) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                // 161 doesn't carry symbol — find the instrument(s) in LoadingSnapshot
                // Complete all instruments that are loading (gap recovery affects all)
                let loading_ids: Vec<u32> = inst_state.iter()
                    .filter(|(_, ist)| matches!(ist.state, DispatcherState::LoadingSnapshot { .. }))
                    .map(|(&id, _)| id)
                    .collect();

                if loading_ids.is_empty() {
                    eprintln!(
                        "[dispatcher] WARNING: 161 received outside snapshot context (seq={:?}) — ignoring",
                        end_event.sequence_number
                    );
                    counters.inc_processed();
                    continue;
                }

                let snapshot_end_seq = end_event.sequence_number
                    .or(last_sequence)
                    .unwrap_or(0);

                if let Some(seq) = end_event.sequence_number {
                    last_sequence = Some(seq);
                }

                for id in loading_ids {
                    complete_snapshot(
                        &mut inst_state, id, snapshot_end_seq,
                        &instrument_txs, &mut last_sequence, &counters, &mut dbo_streak,
                    ).await?;
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
                last_sequence = None;
                dbo_streak = 0;
                counters.inc_processed();
            }

            // ResponseHeartbeat (19)
            19 => {
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

            // ResponseLogout (13)
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

/// Complete snapshot loading for one instrument: replay buffered DBO messages,
/// send SnapshotComplete, transition to Streaming.
async fn complete_snapshot(
    inst_state: &mut HashMap<u32, InstrumentDispatchState>,
    instrument_id: u32,
    snapshot_end_seq: u64,
    instrument_txs: &HashMap<u32, mpsc::Sender<PipelineCommand>>,
    last_sequence: &mut Option<u64>,
    counters: &MessageCounters,
    dbo_streak: &mut u64,
) -> Result<(), RithmicError> {
    let inst = inst_state.get_mut(&instrument_id).unwrap();
    let tx = instrument_txs.get(&instrument_id).unwrap();

    match std::mem::replace(&mut inst.state, DispatcherState::Streaming) {
        DispatcherState::LoadingSnapshot { buffer, buffer_overflowed, snapshot_levels } => {
            eprintln!(
                "[dispatcher] snapshot complete for instrument {}: end_seq={}, levels={}, buffered={}, overflowed={}",
                instrument_id, snapshot_end_seq, snapshot_levels, buffer.len(), buffer_overflowed
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
                        &dbo, instrument_id, &mut inst.order_id_map, 0,
                    );
                    for event in events {
                        if tx.send(PipelineCommand::Event(event)).await.is_err() {
                            return Err(RithmicError::Channel(
                                "instrument pipeline tx closed during replay".into(),
                            ));
                        }
                    }
                    *last_sequence = Some(seq);
                    replayed += 1;
                }
                eprintln!(
                    "[dispatcher] replay complete for instrument {}: replayed={}, discarded={}",
                    instrument_id, replayed, discarded
                );
            } else {
                eprintln!(
                    "[dispatcher] snapshot complete for instrument {} (buffer overflowed — snapshot only)",
                    instrument_id
                );
            }

            counters.inc_snapshot_recoveries();
            *dbo_streak = 0;

            if tx.send(PipelineCommand::SnapshotComplete { instrument_id }).await.is_err() {
                return Err(RithmicError::Channel(
                    "instrument pipeline tx closed after snapshot complete".into(),
                ));
            }
        }

        DispatcherState::Streaming => {
            eprintln!(
                "[dispatcher] WARNING: snapshot completion for instrument {} outside snapshot context — ignoring",
                instrument_id
            );
        }
    }

    Ok(())
}

// Helper functions to extract timestamps

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
