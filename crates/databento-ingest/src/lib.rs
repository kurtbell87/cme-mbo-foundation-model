//! Databento `.dbn.zst` file ingestion.
//!
//! Reads MBO (Market By Order) records from Databento's `.dbn.zst` files,
//! reconstructs the L2 order book via `BookBuilder`, and emits committed
//! states and flow features at each F_LAST boundary.

use book_builder::flow::FlowState;
use book_builder::{BookBuilder, CommittedState};
use common::event::{DayEventBuffer, MBOEvent};
use dbn::decode::{DbnDecoder, DecodeRecord};
use dbn::MboMsg;
use thiserror::Error;

use std::path::Path;

#[derive(Error, Debug)]
pub enum IngestError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("DBN decode error: {0}")]
    Dbn(String),
    #[error("No records found for instrument {0}")]
    NoRecords(u32),
}

/// Result of ingesting a single day's `.dbn.zst` file.
#[derive(Debug)]
pub struct DayIngestResult {
    /// MBO events for message-level analysis.
    pub event_buffer: DayEventBuffer,
    /// Timestamp of the first MBO event processed for the instrument.
    pub first_ts: u64,
    /// Timestamp of the last MBO event processed for the instrument.
    pub last_ts: u64,
    /// Total number of MBO records in the file (all instruments).
    pub total_records: u64,
    /// Number of MBO records matching the target instrument.
    pub instrument_records: u64,
    /// Tick-level mid-prices at every F_LAST boundary with both sides quoted.
    pub tick_mids: Vec<(u64, f32)>,
    /// Committed book states at every F_LAST boundary.
    pub committed_states: Vec<CommittedState>,
    /// Flow accumulator snapshots at every F_LAST boundary.
    /// Parallel to `committed_states` — same length, same ordering.
    pub flow_states: Vec<FlowState>,
}

/// Convert a Databento action char to the integer code used in `MBOEvent`.
fn action_to_i32(action: char) -> i32 {
    match action {
        'A' => 0,
        'C' => 1,
        'M' => 2,
        'T' => 3,
        'F' => 3,
        _ => -1,
    }
}

/// Convert a Databento side char to the integer code used in `MBOEvent`.
fn side_to_i32(side: char) -> i32 {
    match side {
        'B' => 0,
        'A' => 1,
        _ => -1,
    }
}

/// Process a `.dbn.zst` file and return committed states, flow states, and tick mids.
pub fn ingest_day_file(
    path: impl AsRef<Path>,
    instrument_id: u32,
    _date: &str,
) -> Result<DayIngestResult, IngestError> {
    let path = path.as_ref();

    let mut decoder =
        DbnDecoder::from_zstd_file(path).map_err(|e| IngestError::Dbn(e.to_string()))?;

    let mut builder = BookBuilder::new(instrument_id);
    let mut event_buffer = DayEventBuffer::new();
    let mut first_ts: Option<u64> = None;
    let mut last_ts: u64 = 0;
    let mut total_records: u64 = 0;
    let mut instrument_records: u64 = 0;
    let mut tick_mids: Vec<(u64, f32)> = Vec::new();
    let mut committed_states: Vec<CommittedState> = Vec::new();
    let mut flow_states: Vec<FlowState> = Vec::new();

    while let Some(msg) = decoder
        .decode_record::<MboMsg>()
        .map_err(|e| IngestError::Dbn(e.to_string()))?
    {
        total_records += 1;

        let ts = msg.hd.ts_event;
        let id = msg.hd.instrument_id;
        let action = msg.action as u8 as char;
        let side = msg.side as u8 as char;
        let flags = msg.flags.raw();

        // Feed every record to book builder (it filters by instrument_id internally)
        builder.process_event(ts, msg.order_id, id, action, side, msg.price, msg.size, flags);

        // Record to event buffer only for our instrument
        if id == instrument_id {
            instrument_records += 1;

            event_buffer.push(MBOEvent {
                action: action_to_i32(action),
                price: (msg.price as f64 / 1e9) as f32,
                size: msg.size,
                side: side_to_i32(side),
                ts_event: ts,
            });

            if first_ts.is_none() {
                first_ts = Some(ts);
            }
            last_ts = ts;

            // Capture committed state and flow state at each F_LAST boundary
            if flags & 0x80 != 0 {
                let cs = builder.current_committed_state(ts);
                if cs.has_bid && cs.has_ask {
                    tick_mids.push((ts, cs.mid));
                }
                committed_states.push(cs);

                let mut flow = builder.current_flow_state();
                flow.ts = ts;
                flow_states.push(flow);
            }
        }
    }

    let first_ts = match first_ts {
        Some(ts) => ts,
        None => return Err(IngestError::NoRecords(instrument_id)),
    };

    Ok(DayIngestResult {
        event_buffer,
        first_ts,
        last_ts,
        total_records,
        instrument_records,
        tick_mids,
        committed_states,
        flow_states,
    })
}

/// Construct the standard Databento file path for a given date.
pub fn dbn_file_path(data_dir: &str, date: &str) -> String {
    format!("{}/glbx-mdp3-{}.mbo.dbn.zst", data_dir, date)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_mapping() {
        assert_eq!(action_to_i32('A'), 0);
        assert_eq!(action_to_i32('C'), 1);
        assert_eq!(action_to_i32('M'), 2);
        assert_eq!(action_to_i32('T'), 3);
        assert_eq!(action_to_i32('F'), 3);
        assert_eq!(action_to_i32('R'), -1);
    }

    #[test]
    fn test_side_mapping() {
        assert_eq!(side_to_i32('B'), 0);
        assert_eq!(side_to_i32('A'), 1);
        assert_eq!(side_to_i32('N'), -1);
    }

    #[test]
    fn test_dbn_file_path() {
        let path = dbn_file_path("/DATA/GLBX-20260207-L953CAPU5B", "20220103");
        assert_eq!(
            path,
            "/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst"
        );
    }

    #[test]
    fn test_missing_file_returns_error() {
        let result = ingest_day_file("/nonexistent/path.dbn.zst", 12345, "20220103");
        assert!(result.is_err());
    }
}
