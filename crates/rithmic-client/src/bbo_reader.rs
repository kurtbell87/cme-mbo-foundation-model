//! BBO reader: decodes BBO (151) and LastTrade (150) messages from the
//! market-data WebSocket channel.
//!
//! Routes updates to per-instrument pipeline tasks by looking up the symbol
//! in each decoded message.

use std::collections::HashMap;

use prost::Message;
use tokio::sync::mpsc;

use crate::adapter::{self, BboUpdate};
use crate::connection::decode_ws_payload;
use crate::counters::MessageCounters;
use crate::dispatcher::{CaptureRecord, PipelineCommand};
use crate::error::RithmicError;
use crate::extract_template_id;
use crate::heartbeat::LivenessTracker;
use crate::rti;

/// Run the BBO reader task.
///
/// Receives raw WebSocket binary messages from the BBO channel,
/// decodes them, and routes BBO updates and trade events to per-instrument
/// pipeline tasks.
pub async fn run_bbo_reader(
    mut raw_rx: mpsc::Receiver<(Vec<u8>, u64)>,
    bbo_txs: HashMap<u32, mpsc::Sender<BboUpdate>>,
    trade_txs: HashMap<u32, mpsc::Sender<PipelineCommand>>,
    raw_capture_tx: Option<mpsc::Sender<CaptureRecord>>,
    counters: MessageCounters,
    liveness: LivenessTracker,
    symbol_to_id: HashMap<String, u32>,
) -> Result<(), RithmicError> {
    while let Some((raw_data, receive_wall_ns)) = raw_rx.recv().await {
        counters.inc_received();
        liveness.record_inbound();

        let ws_msg =
            tokio_tungstenite::tungstenite::protocol::Message::Binary(raw_data.clone().into());
        let payload = match decode_ws_payload(&ws_msg) {
            Some(p) => p,
            None => continue,
        };

        let tid = match extract_template_id(payload) {
            Ok(t) => t,
            Err(_) => continue,
        };

        match tid {
            // BestBidOffer (151)
            151 => {
                counters.inc_bbo();
                let bbo = match rti::BestBidOffer::decode(payload) {
                    Ok(b) => b,
                    Err(_) => continue,
                };

                if let Some(ref cap_tx) = raw_capture_tx {
                    let record = CaptureRecord {
                        template_id: tid,
                        sequence_number: None,
                        exchange_ts_ns: None,
                        gateway_ts_ns: extract_gateway_ts_bbo(&bbo),
                        receive_ns: receive_wall_ns,
                        symbol: bbo.symbol.clone(),
                        raw_bytes: payload.to_vec(),
                    };
                    if cap_tx.try_send(record).is_err() {
                        counters.inc_capture_drops();
                    }
                }

                // Route by symbol
                let instrument_id = match bbo.symbol.as_deref().and_then(|s| symbol_to_id.get(s)) {
                    Some(&id) => id,
                    None => { counters.inc_processed(); continue; }
                };

                if let Some(update) = adapter::best_bid_offer_to_update(&bbo, instrument_id, receive_wall_ns) {
                    if let Some(tx) = bbo_txs.get(&instrument_id) {
                        if tx.send(update).await.is_err() {
                            return Err(RithmicError::Channel("bbo_tx closed".into()));
                        }
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

                if let Some(ref cap_tx) = raw_capture_tx {
                    let record = CaptureRecord {
                        template_id: tid,
                        sequence_number: None,
                        exchange_ts_ns: extract_exchange_ts_trade(&trade),
                        gateway_ts_ns: extract_gateway_ts_trade(&trade),
                        receive_ns: receive_wall_ns,
                        symbol: trade.symbol.clone(),
                        raw_bytes: payload.to_vec(),
                    };
                    if cap_tx.try_send(record).is_err() {
                        counters.inc_capture_drops();
                    }
                }

                // Route by symbol
                let instrument_id = match trade.symbol.as_deref().and_then(|s| symbol_to_id.get(s)) {
                    Some(&id) => id,
                    None => { counters.inc_processed(); continue; }
                };

                if let Some(event) = adapter::last_trade_to_event(&trade, instrument_id, receive_wall_ns) {
                    if let Some(tx) = trade_txs.get(&instrument_id) {
                        if tx.send(PipelineCommand::Event(event)).await.is_err() {
                            return Err(RithmicError::Channel("trade_tx closed".into()));
                        }
                    }
                }

                counters.inc_processed();
            }

            // ResponseMarketDataUpdate (101) — subscription ack
            101 => {
                if let Ok(resp) = rti::ResponseMarketDataUpdate::decode(payload) {
                    eprintln!(
                        "[bbo_reader] market data sub ack: rp_code={:?} user_msg={:?}",
                        resp.rp_code, resp.user_msg
                    );
                }
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
                        "[bbo_reader] server reject: {:?} {:?}",
                        reject.rp_code, reject.user_msg
                    );
                }
                counters.inc_processed();
            }

            // ResponseLogout (13)
            13 => {
                eprintln!("[bbo_reader] server sent ResponseLogout — closing");
                return Err(RithmicError::ForcedLogout(
                    "bbo connection: server graceful logout".to_string(),
                ));
            }

            // ForcedLogout (77)
            77 => {
                if let Ok(logout) = rti::ForcedLogout::decode(payload) {
                    eprintln!(
                        "[bbo_reader] forced logout: {:?} {:?}",
                        logout.rp_code, logout.user_msg
                    );
                }
                return Err(RithmicError::ForcedLogout(
                    "bbo connection: server forced logout".to_string(),
                ));
            }

            // All other messages — skip
            _ => {
                eprintln!("[bbo_reader] unhandled template_id={}", tid);
                counters.inc_processed();
            }
        }
    }

    Ok(())
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
