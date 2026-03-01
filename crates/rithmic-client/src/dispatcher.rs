//! Dispatcher: decodes raw WebSocket messages and routes by template_id.
//!
//! The dispatcher extracts metadata once per message (no double-parsing)
//! and sends typed events to the appropriate downstream channels.
//!
//! Implements sequence gap recovery:
//! 1. On gap detection → transition to Recovering state
//! 2. Buffer incoming DBO (160) messages (bounded ~10k)
//! 3. Send RequestDepthByOrderSnapshot (115) via ws_cmd_tx
//! 4. Process ResponseDepthByOrderSnapshot (116) → OrderEvents for book rebuild
//! 5. On DepthByOrderEndEvent (161) → record snapshot_end_sequence
//! 6. Discard buffered events where seq <= snapshot_end_sequence
//! 7. Forward remaining buffered events → resume Streaming

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

/// Maximum number of DBO messages to buffer during recovery.
/// If exceeded, discard buffer and rely entirely on snapshot + post-snapshot stream.
const RECOVERY_BUFFER_LIMIT: usize = 10_000;

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
}

/// Internal state of the dispatcher.
enum DispatcherState {
    /// Normal operation — forward events to pipeline.
    Streaming,
    /// Recovering from a sequence gap.
    /// Buffer DBO messages while waiting for snapshot response.
    Recovering {
        /// Buffered raw DBO messages (decoded DepthByOrder + sequence number).
        buffer: VecDeque<(rti::DepthByOrder, u64)>,
        /// Whether the buffer overflowed (discard all buffered on completion).
        buffer_overflowed: bool,
    },
}

/// Run the dispatcher task.
///
/// Reads raw WebSocket binary messages from `raw_msg_rx`, decodes them,
/// and routes to the appropriate downstream channels. Handles sequence
/// gap recovery by requesting a DBO snapshot and rebuilding the book.
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
    let mut state = DispatcherState::Streaming;
    let epoch = Instant::now();

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
                        // Sequence gap detection
                        if let Some(seq) = dbo.sequence_number {
                            if let Some(last) = last_sequence {
                                if seq != last + 1 {
                                    counters.inc_sequence_gaps();
                                    eprintln!(
                                        "[dispatcher] sequence gap: expected {}, got {} — initiating recovery",
                                        last + 1, seq
                                    );

                                    // Transition to Recovering state
                                    let mut buffer = VecDeque::new();
                                    buffer.push_back((dbo, seq));
                                    state = DispatcherState::Recovering {
                                        buffer,
                                        buffer_overflowed: false,
                                    };

                                    // Signal pipeline to clear book before snapshot rebuild
                                    if order_event_tx.send(PipelineCommand::ClearBook).await.is_err() {
                                        return Err(RithmicError::Channel(
                                            "order_event_tx closed during recovery".into(),
                                        ));
                                    }

                                    // Send RequestDepthByOrderSnapshot via ws_cmd_tx
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
                                    continue;
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

                    DispatcherState::Recovering { buffer, buffer_overflowed } => {
                        // Buffer the DBO message for replay after snapshot
                        if let Some(seq) = dbo.sequence_number {
                            if buffer.len() < RECOVERY_BUFFER_LIMIT {
                                buffer.push_back((dbo, seq));
                            } else if !*buffer_overflowed {
                                *buffer_overflowed = true;
                                eprintln!(
                                    "[dispatcher] recovery buffer overflow ({} messages), \
                                     will rely on snapshot only",
                                    RECOVERY_BUFFER_LIMIT
                                );
                            }
                            // If overflowed, just drop the message (it's still captured above)
                        }
                    }
                }

                counters.inc_processed();
            }

            // ResponseDepthByOrderSnapshot (116) — snapshot data during recovery
            116 => {
                let snap = match rti::ResponseDepthByOrderSnapshot::decode(payload) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                if let DispatcherState::Recovering { .. } = &state {
                    // Track the sequence number from snapshot responses
                    if let Some(seq) = snap.sequence_number {
                        last_sequence = Some(seq);
                    }

                    // Convert snapshot entries to OrderEvents and send to pipeline
                    // (pipeline should have already received ClearBook before the first snapshot response)
                    let events = adapter::snapshot_response_to_events(&snap, instrument_id, &mut order_id_map);
                    for event in events {
                        if order_event_tx.send(PipelineCommand::Event(event)).await.is_err() {
                            return Err(RithmicError::Channel("order_event_tx closed".into()));
                        }
                    }
                } else {
                    // Unexpected snapshot response outside recovery — log and skip
                    eprintln!("[dispatcher] unexpected snapshot response (116) outside recovery");
                }

                counters.inc_processed();
            }

            // DepthByOrderEndEvent (161) — snapshot completion marker
            161 => {
                let end_event = match rti::DepthByOrderEndEvent::decode(payload) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                match std::mem::replace(&mut state, DispatcherState::Streaming) {
                    DispatcherState::Recovering { buffer, buffer_overflowed } => {
                        let snapshot_end_seq = end_event.sequence_number
                            .or(last_sequence)
                            .unwrap_or(0);

                        if let Some(seq) = end_event.sequence_number {
                            last_sequence = Some(seq);
                        }

                        eprintln!(
                            "[dispatcher] snapshot complete: end_seq={}, buffered={}, overflowed={}",
                            snapshot_end_seq, buffer.len(), buffer_overflowed
                        );

                        if !buffer_overflowed {
                            // Replay buffered events with seq > snapshot_end_seq
                            let mut replayed = 0u64;
                            let mut discarded = 0u64;
                            for (dbo, seq) in buffer {
                                if seq <= snapshot_end_seq {
                                    discarded += 1;
                                    continue;
                                }
                                // Apply this event — it's newer than the snapshot
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
                                "[dispatcher] recovery complete: replayed={}, discarded={}",
                                replayed, discarded
                            );
                        } else {
                            // Buffer overflowed — rely entirely on snapshot + new stream
                            eprintln!(
                                "[dispatcher] recovery complete (buffer overflowed, no replay)"
                            );
                        }

                        counters.inc_snapshot_recoveries();
                        // state is already set to Streaming by std::mem::replace above
                    }

                    DispatcherState::Streaming => {
                        // DepthByOrderEndEvent outside recovery — this happens during
                        // initial subscription when the server sends an initial snapshot.
                        if let Some(seq) = end_event.sequence_number {
                            last_sequence = Some(seq);
                        }
                        eprintln!(
                            "[dispatcher] DBO end event (not in recovery): seq={:?}",
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

                // Trades are forwarded even during recovery — they don't affect
                // book state (action='T') and the book builder handles them safely.
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

                // BBO always forwarded (even during recovery — for latest state)
                if let Some(update) = adapter::best_bid_offer_to_update(&bbo) {
                    if bbo_tx.send(update).await.is_err() {
                        return Err(RithmicError::Channel("bbo_tx closed".into()));
                    }
                }

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
