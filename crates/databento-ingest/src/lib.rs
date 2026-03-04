//! Databento `.dbn.zst` file ingestion.
//!
//! Reads MBO (Market By Order) records from Databento's `.dbn.zst` files,
//! reconstructs the L2 order book via `BookBuilder`, and emits 100ms
//! `BookSnapshot` structs during RTH (09:30–16:00 ET).

use book_builder::flow::FlowState;
use book_builder::{BookBuilder, CommittedState};
use common::book::BookSnapshot;
use common::event::{DayEventBuffer, MBOEvent};
use common::time_utils;
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
    /// BookSnapshots at 100ms intervals during RTH.
    pub snapshots: Vec<BookSnapshot>,
    /// MBO events for message-level analysis (bar event ranges).
    pub event_buffer: DayEventBuffer,
    /// Timestamp of the first MBO event processed for the instrument.
    pub first_ts: u64,
    /// Timestamp of the last MBO event processed for the instrument.
    pub last_ts: u64,
    /// Total number of MBO records in the file (all instruments).
    pub total_records: u64,
    /// Number of MBO records matching the target instrument.
    pub instrument_records: u64,
    /// Tick-level mid-prices at every F_LAST boundary during RTH.
    pub tick_mids: Vec<(u64, f32)>,
    /// Committed book states at every F_LAST boundary during RTH.
    pub committed_states: Vec<CommittedState>,
    /// Flow accumulator snapshots at every F_LAST boundary during RTH.
    /// Parallel to `committed_states` — same length, same ordering.
    pub flow_states: Vec<FlowState>,
}

/// Convert a Databento action char to the integer code used in `MBOEvent`.
///
/// Mapping: A→0, C→1, M→2, T→3, F→3 (fill treated as trade), R→-1 (clear).
fn action_to_i32(action: char) -> i32 {
    match action {
        'A' => 0, // Add
        'C' => 1, // Cancel
        'M' => 2, // Modify
        'T' => 3, // Trade
        'F' => 3, // Fill (treated as trade for event accounting)
        _ => -1,  // Clear, None, etc.
    }
}

/// Convert a Databento side char to the integer code used in `MBOEvent`.
///
/// Mapping: B→0 (Bid), A→1 (Ask).
fn side_to_i32(side: char) -> i32 {
    match side {
        'B' => 0, // Bid
        'A' => 1, // Ask
        _ => -1,  // None/unknown
    }
}

/// Process a `.dbn.zst` file and return `BookSnapshot`s during RTH hours.
///
/// Reads all MBO records from the file, feeds them to a `BookBuilder`
/// for the specified `instrument_id`, then emits snapshots at 100ms
/// intervals during RTH (09:30–16:00 ET).
///
/// The `date` parameter (YYYYMMDD format) specifies the trading day. This
/// is required because `.dbn.zst` files contain events starting from the
/// previous evening's globex session, so deriving the date from event
/// timestamps would yield the wrong day.
///
/// Also populates a `DayEventBuffer` with all events for the instrument,
/// which can be used for bar-level message summary computation.
pub fn ingest_day_file(
    path: impl AsRef<Path>,
    instrument_id: u32,
    date: &str,
) -> Result<DayIngestResult, IngestError> {
    let path = path.as_ref();

    // Compute RTH boundaries from the explicit date (not from event timestamps).
    let rth_open = time_utils::rth_open_for_date(date);
    let rth_close = time_utils::rth_close_for_date(date);

    let mut decoder =
        DbnDecoder::from_zstd_file(path).map_err(|e| IngestError::Dbn(e.to_string()))?;

    let mut builder = BookBuilder::new(instrument_id);
    let mut event_buffer = DayEventBuffer::new();
    let mut first_ts: Option<u64> = None;
    let mut last_ts: u64 = 0;
    let mut total_records: u64 = 0;
    let mut instrument_records: u64 = 0;

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
        }
    }

    let first_ts = match first_ts {
        Some(ts) => ts,
        None => return Err(IngestError::NoRecords(instrument_id)),
    };

    // Extract tick-level mid-prices and filter to RTH range
    let all_tick_mids = builder.take_tick_mid_prices();
    let tick_mids: Vec<(u64, f32)> = all_tick_mids
        .into_iter()
        .filter(|(ts, _)| *ts >= rth_open && *ts < rth_close)
        .collect();

    // Emit snapshots during RTH (must happen before take_committed_states)
    let snapshots = builder.emit_snapshots(rth_open, rth_close);

    // Extract committed states and flow states, filter to RTH range.
    // Both are parallel (same length, same ordering) from BookBuilder.
    let all_committed = builder.take_committed_states();
    let all_flow = builder.take_flow_states();
    let (committed_states, flow_states): (Vec<CommittedState>, Vec<FlowState>) = all_committed
        .into_iter()
        .zip(all_flow)
        .filter(|(cs, _)| cs.ts >= rth_open && cs.ts < rth_close)
        .unzip();

    Ok(DayIngestResult {
        snapshots,
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
///
/// Pattern: `{data_dir}/glbx-mdp3-{YYYYMMDD}.mbo.dbn.zst`
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

    /// Date string for the reference date used in all synthetic test data.
    const TEST_DATE: &str = "20220103";

    #[test]
    fn test_missing_file_returns_error() {
        let result = ingest_day_file("/nonexistent/path.dbn.zst", 12345, TEST_DATE);
        assert!(result.is_err());
    }

    // --- Integration tests using synthetic .dbn.zst files ---

    use common::book::SNAPSHOT_INTERVAL_NS;
    use common::time_utils::{NS_PER_HOUR, NS_PER_SEC, REF_MIDNIGHT_ET_NS};
    use dbn::encode::dbn::Encoder;
    use dbn::encode::EncodeRecord;
    use dbn::enums::rtype;
    use dbn::{FlagSet, MetadataBuilder, RecordHeader, Schema, SType};
    use std::os::raw::c_char;
    use tempfile::NamedTempFile;

    const TEST_INSTRUMENT: u32 = 11355; // MES contract ID

    /// RTH open for the reference date (2022-01-03 09:30:00 ET).
    fn rth_open() -> u64 {
        REF_MIDNIGHT_ET_NS + 9 * NS_PER_HOUR + 30 * 60 * NS_PER_SEC
    }

    fn make_mbo(
        ts: u64,
        order_id: u64,
        instrument_id: u32,
        action: u8,
        side: u8,
        price_fixed: i64,
        size: u32,
        last: bool,
    ) -> MboMsg {
        let flags = if last {
            FlagSet::new(0x80)
        } else {
            FlagSet::empty()
        };
        MboMsg {
            hd: RecordHeader::new::<MboMsg>(rtype::MBO, 1, instrument_id, ts),
            order_id,
            price: price_fixed,
            size,
            flags,
            channel_id: 0,
            action: action as c_char,
            side: side as c_char,
            ts_recv: ts + 1000,
            ts_in_delta: 500,
            sequence: 0,
        }
    }

    /// Write MBO records to a temp .dbn.zst file and return the path.
    fn write_test_dbn(records: &[MboMsg]) -> NamedTempFile {
        let tmp = NamedTempFile::new().expect("create temp file");
        let metadata = MetadataBuilder::new()
            .dataset("GLBX.MDP3")
            .schema(Some(Schema::Mbo))
            .start(records[0].hd.ts_event)
            .stype_in(Some(SType::RawSymbol))
            .stype_out(SType::InstrumentId)
            .build();
        let mut encoder =
            Encoder::with_zstd(tmp.reopen().expect("reopen for writing"), &metadata)
                .expect("create encoder");
        for rec in records {
            encoder.encode_record(rec).expect("encode record");
        }
        encoder.flush().expect("flush encoder");
        drop(encoder); // finalize zstd frame
        tmp
    }

    #[test]
    fn test_ingest_basic_book() {
        // Create a simple order book: 1 bid + 1 ask, during RTH
        let ts = rth_open() + NS_PER_SEC; // 09:30:01 ET
        let bid_price = 4500_000_000_000i64; // 4500.00
        let ask_price = 4500_250_000_000i64; // 4500.25

        let records = vec![
            make_mbo(ts, 100, TEST_INSTRUMENT, b'A', b'B', bid_price, 10, false),
            make_mbo(ts, 101, TEST_INSTRUMENT, b'A', b'A', ask_price, 5, true),
        ];

        let tmp = write_test_dbn(&records);
        let result = ingest_day_file(tmp.path(), TEST_INSTRUMENT, TEST_DATE).expect("ingest should succeed");

        assert_eq!(result.total_records, 2);
        assert_eq!(result.instrument_records, 2);
        assert_eq!(result.first_ts, ts);
        assert_eq!(result.last_ts, ts);
        assert_eq!(result.event_buffer.len(), 2);

        // Should have at least 1 snapshot (the one at or after the event)
        assert!(
            !result.snapshots.is_empty(),
            "should produce snapshots during RTH"
        );

        // Check the first snapshot has valid book data
        let snap = &result.snapshots[0];
        assert!((snap.mid_price - 4500.125).abs() < 0.01);
        assert!((snap.spread - 0.25).abs() < 0.01);
        assert!((snap.bids[0][0] - 4500.0).abs() < 0.01);
        assert!((snap.asks[0][0] - 4500.25).abs() < 0.01);
    }

    #[test]
    fn test_ingest_multiple_events_over_time() {
        // Create events spread across RTH so we get multiple committed states
        let base_ts = rth_open() + NS_PER_SEC;
        let bid_price = 4500_000_000_000i64;
        let ask_price = 4500_250_000_000i64;

        let mut records = Vec::new();

        // Initial book: bid @ 4500, ask @ 4500.25
        records.push(make_mbo(base_ts, 100, TEST_INSTRUMENT, b'A', b'B', bid_price, 10, false));
        records.push(make_mbo(base_ts, 101, TEST_INSTRUMENT, b'A', b'A', ask_price, 5, true));

        // 1 second later: trade + new bid level
        let ts2 = base_ts + NS_PER_SEC;
        records.push(make_mbo(ts2, 0, TEST_INSTRUMENT, b'T', b'B', bid_price, 3, false));
        records.push(make_mbo(ts2, 102, TEST_INSTRUMENT, b'A', b'B', 4499_750_000_000, 8, true));

        // 2 seconds later: modify the ask
        let ts3 = base_ts + 2 * NS_PER_SEC;
        records.push(make_mbo(
            ts3,
            101,
            TEST_INSTRUMENT,
            b'M',
            b'A',
            4500_500_000_000,
            12,
            true,
        ));

        let tmp = write_test_dbn(&records);
        let result = ingest_day_file(tmp.path(), TEST_INSTRUMENT, TEST_DATE).expect("ingest should succeed");

        assert_eq!(result.total_records, 5);
        assert_eq!(result.instrument_records, 5);
        assert!(!result.snapshots.is_empty());

        // Event buffer should have all 5 events
        assert_eq!(result.event_buffer.len(), 5);
    }

    #[test]
    fn test_ingest_filters_instrument() {
        // Mix events from two different instruments
        let ts = rth_open() + NS_PER_SEC;
        let bid_price = 4500_000_000_000i64;
        let ask_price = 4500_250_000_000i64;

        let records = vec![
            // Target instrument
            make_mbo(ts, 100, TEST_INSTRUMENT, b'A', b'B', bid_price, 10, false),
            make_mbo(ts, 101, TEST_INSTRUMENT, b'A', b'A', ask_price, 5, true),
            // Different instrument (should be ignored in event_buffer)
            make_mbo(ts, 200, 99999, b'A', b'B', bid_price, 20, false),
            make_mbo(ts, 201, 99999, b'A', b'A', ask_price, 15, true),
        ];

        let tmp = write_test_dbn(&records);
        let result = ingest_day_file(tmp.path(), TEST_INSTRUMENT, TEST_DATE).expect("ingest should succeed");

        assert_eq!(result.total_records, 4);
        assert_eq!(result.instrument_records, 2);
        assert_eq!(result.event_buffer.len(), 2);
    }

    #[test]
    fn test_ingest_no_matching_instrument() {
        let ts = rth_open() + NS_PER_SEC;
        let records = vec![make_mbo(
            ts,
            100,
            99999,
            b'A',
            b'B',
            4500_000_000_000,
            10,
            true,
        )];

        let tmp = write_test_dbn(&records);
        let result = ingest_day_file(tmp.path(), TEST_INSTRUMENT, TEST_DATE);

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), IngestError::NoRecords(id) if id == TEST_INSTRUMENT)
        );
    }

    #[test]
    fn test_ingest_pre_rth_events_produce_rth_snapshots() {
        // Pre-RTH events build the book; RTH snapshots reflect that state.
        // This is correct — the book carries forward into RTH.
        let pre_rth = REF_MIDNIGHT_ET_NS + 5 * NS_PER_HOUR; // 05:00 ET
        let bid_price = 4500_000_000_000i64;
        let ask_price = 4500_250_000_000i64;

        let records = vec![
            make_mbo(pre_rth, 100, TEST_INSTRUMENT, b'A', b'B', bid_price, 10, false),
            make_mbo(pre_rth, 101, TEST_INSTRUMENT, b'A', b'A', ask_price, 5, true),
        ];

        let tmp = write_test_dbn(&records);
        let result = ingest_day_file(tmp.path(), TEST_INSTRUMENT, TEST_DATE).expect("ingest should succeed");

        assert_eq!(result.instrument_records, 2);
        // Pre-RTH book carries into RTH, so we get snapshots from 09:30 onwards
        assert!(
            !result.snapshots.is_empty(),
            "pre-RTH book should produce RTH snapshots"
        );
        // Verify snapshots are within RTH time range
        let rth_open_ts = rth_open();
        for snap in &result.snapshots {
            assert!(
                snap.timestamp >= rth_open_ts,
                "snapshot timestamp {} should be >= RTH open {}",
                snap.timestamp,
                rth_open_ts,
            );
        }
    }

    #[test]
    fn test_ingest_trade_appears_in_snapshots() {
        let ts = rth_open() + NS_PER_SEC;
        let bid_price = 4500_000_000_000i64;
        let ask_price = 4500_250_000_000i64;

        let records = vec![
            make_mbo(ts, 100, TEST_INSTRUMENT, b'A', b'B', bid_price, 10, false),
            make_mbo(ts, 101, TEST_INSTRUMENT, b'A', b'A', ask_price, 5, false),
            make_mbo(ts, 0, TEST_INSTRUMENT, b'T', b'B', bid_price, 3, true),
        ];

        let tmp = write_test_dbn(&records);
        let result = ingest_day_file(tmp.path(), TEST_INSTRUMENT, TEST_DATE).expect("ingest should succeed");

        // Check that the trade appears in at least one snapshot's trade buffer
        let has_trade = result.snapshots.iter().any(|snap| {
            snap.trades
                .iter()
                .any(|t| t[1] > 0.0) // size > 0 means a trade exists
        });
        assert!(has_trade, "trade should appear in snapshot trade buffer");
    }

    #[test]
    fn test_ingest_snapshot_count_matches_time_span() {
        // Create events spanning 1 second during RTH
        // At 100ms intervals, 1 second should yield ~10 snapshots
        let ts_start = rth_open() + NS_PER_SEC;
        let ts_end = ts_start + NS_PER_SEC;
        let bid_price = 4500_000_000_000i64;
        let ask_price = 4500_250_000_000i64;

        let mut records = Vec::new();
        // Initial book at ts_start
        records.push(make_mbo(ts_start, 100, TEST_INSTRUMENT, b'A', b'B', bid_price, 10, false));
        records.push(make_mbo(ts_start, 101, TEST_INSTRUMENT, b'A', b'A', ask_price, 5, true));
        // Another commit at ts_end
        records.push(make_mbo(ts_end, 102, TEST_INSTRUMENT, b'A', b'B', bid_price, 15, true));

        let tmp = write_test_dbn(&records);
        let result = ingest_day_file(tmp.path(), TEST_INSTRUMENT, TEST_DATE).expect("ingest should succeed");

        // Snapshots span from just after ts_start to rth_close.
        // The book is only committed twice, but emit_snapshots generates one per 100ms
        // based on the most recent committed state at each boundary.
        // We should get many snapshots (from ts_start boundary through all of RTH).
        let expected_approx = ((time_utils::rth_close_ns(ts_start) - ts_start)
            / SNAPSHOT_INTERVAL_NS) as usize;
        // Allow some tolerance for alignment
        assert!(
            result.snapshots.len() >= expected_approx - 2
                && result.snapshots.len() <= expected_approx + 2,
            "expected ~{} snapshots, got {}",
            expected_approx,
            result.snapshots.len()
        );
    }
}
