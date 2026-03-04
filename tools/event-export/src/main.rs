//! Event-level Parquet export: committed states + LOB features + barrier labels.
//!
//! Processes a single day's .dbn.zst file and outputs one Parquet file
//! with ~750K rows (BBO-change events × multiple geometries).

use std::fs::File;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use arrow::array::{ArrayRef, Float32Array, Int8Array, Int32Array, UInt64Array};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use clap::Parser;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use event_features::{
    compute_lob_features, EventWindowConfig, LOB_FEATURE_NAMES, NUM_LOB_FEATURES,
};
use event_labels::{generate_multi_geometry_labels, DEFAULT_GEOMETRIES};
use flow_features::{compute_flow_features, FLOW_FEATURE_NAMES, NUM_FLOW_FEATURES};

/// Export event-level LOB features + barrier labels to Parquet.
#[derive(Parser, Debug)]
#[command(name = "event-export")]
#[command(about = "Export event-level LOB features and barrier labels from .dbn.zst")]
struct Args {
    /// Input .dbn.zst file path
    #[arg(long)]
    input: String,

    /// Output .parquet file path
    #[arg(long)]
    output: String,

    /// Instrument ID (e.g., 11355 for MES)
    #[arg(long)]
    instrument_id: u32,

    /// Trading date (YYYYMMDD format)
    #[arg(long)]
    date: String,

    /// Barrier geometries as "T:S,T:S,..." (default: standard set)
    #[arg(long)]
    geometries: Option<String>,

    /// Event lookback window size
    #[arg(long, default_value = "200")]
    lookback_events: usize,

    /// Maximum barrier horizon in seconds
    #[arg(long, default_value = "3600")]
    max_horizon_s: u64,

    /// Tick size for the instrument
    #[arg(long, default_value = "0.25")]
    tick_size: f64,

    /// Export ALL committed states (not just BBO-change events)
    #[arg(long)]
    all_commits: bool,

    /// Use OFI-directed bilateral labels: ofi_fast >= 0 → long@ask, ofi_fast < 0 → short@bid
    #[arg(long)]
    ofi_direction: bool,
}

fn parse_geometries(s: &str) -> Result<Vec<(i32, i32)>> {
    s.split(',')
        .map(|pair| {
            let parts: Vec<&str> = pair.trim().split(':').collect();
            if parts.len() != 2 {
                bail!("Invalid geometry pair: {}", pair);
            }
            let t: i32 = parts[0].parse().context("Invalid target ticks")?;
            let s: i32 = parts[1].parse().context("Invalid stop ticks")?;
            Ok((t, s))
        })
        .collect()
}

fn build_schema() -> Schema {
    let mut fields = vec![
        Field::new("timestamp_ns", DataType::UInt64, false),
        Field::new("best_bid", DataType::Float32, false),
        Field::new("best_ask", DataType::Float32, false),
        Field::new("mid_price", DataType::Float32, false),
        Field::new("spread", DataType::Float32, false),
        Field::new("target_ticks", DataType::Int32, false),
        Field::new("stop_ticks", DataType::Int32, false),
    ];

    // 42 LOB feature columns
    for name in &LOB_FEATURE_NAMES {
        fields.push(Field::new(*name, DataType::Float32, false));
    }

    // 46 flow feature columns
    for name in &FLOW_FEATURE_NAMES {
        fields.push(Field::new(*name, DataType::Float32, false));
    }

    // Label columns
    fields.push(Field::new("outcome", DataType::Int8, false));
    fields.push(Field::new("exit_ts", DataType::UInt64, false));
    fields.push(Field::new("pnl_ticks", DataType::Float32, false));
    fields.push(Field::new("direction", DataType::Int8, false));

    Schema::new(fields)
}

fn main() -> Result<()> {
    let args = Args::parse();

    let geometries = if let Some(ref g) = args.geometries {
        parse_geometries(g)?
    } else {
        DEFAULT_GEOMETRIES.to_vec()
    };

    eprintln!("event-export");
    eprintln!("  input:         {}", args.input);
    eprintln!("  output:        {}", args.output);
    eprintln!("  instrument_id: {}", args.instrument_id);
    eprintln!("  date:          {}", args.date);
    eprintln!("  geometries:    {} configs", geometries.len());
    eprintln!("  lookback:      {} events", args.lookback_events);
    eprintln!("  max_horizon:   {}s", args.max_horizon_s);
    eprintln!(
        "  mode:          {}",
        if args.all_commits {
            "all commits"
        } else {
            "BBO-change only"
        }
    );
    eprintln!(
        "  ofi_direction: {}",
        if args.ofi_direction {
            "bilateral (ofi-directed)"
        } else {
            "long-only"
        }
    );

    // -----------------------------------------------------------------------
    // Step 1: Ingest .dbn.zst
    // -----------------------------------------------------------------------
    eprintln!("[1/4] Ingesting {}...", args.input);
    let ingest = databento_ingest::ingest_day_file(&args.input, args.instrument_id, &args.date)
        .context("Failed to ingest .dbn.zst file")?;

    let committed = &ingest.committed_states;
    let flow_states = &ingest.flow_states;
    let tick_mids = &ingest.tick_mids;
    let event_buf = &ingest.event_buffer;

    eprintln!(
        "  {} committed states, {} flow states, {} tick mids, {} MBO events",
        committed.len(),
        flow_states.len(),
        tick_mids.len(),
        event_buf.len()
    );

    if committed.is_empty() {
        bail!("No committed states produced from ingest");
    }

    // -----------------------------------------------------------------------
    // Step 2: Filter evaluation points
    // -----------------------------------------------------------------------
    let eval_indices: Vec<usize> = if args.all_commits {
        // All committed states with both sides quoted
        (0..committed.len())
            .filter(|&i| committed[i].has_bid && committed[i].has_ask)
            .collect()
    } else {
        // Only BBO-change events with both sides quoted
        (0..committed.len())
            .filter(|&i| committed[i].bbo_changed && committed[i].has_bid && committed[i].has_ask)
            .collect()
    };

    eprintln!(
        "[2/4] {} evaluation points (from {} total commits)",
        eval_indices.len(),
        committed.len()
    );

    // -----------------------------------------------------------------------
    // Step 3: Compute features + labels
    // -----------------------------------------------------------------------
    eprintln!("[3/4] Computing features and labels...");

    let cfg = EventWindowConfig {
        lookback_events: args.lookback_events,
        tick_size: args.tick_size as f32,
    };

    let max_horizon_ns = args.max_horizon_s * 1_000_000_000;
    let total_rows = eval_indices.len() * geometries.len();

    // Pre-allocate column arrays
    let mut timestamps = Vec::with_capacity(total_rows);
    let mut best_bids = Vec::with_capacity(total_rows);
    let mut best_asks = Vec::with_capacity(total_rows);
    let mut mid_prices = Vec::with_capacity(total_rows);
    let mut spreads = Vec::with_capacity(total_rows);
    let mut target_ticks_col = Vec::with_capacity(total_rows);
    let mut stop_ticks_col = Vec::with_capacity(total_rows);
    let mut feature_cols: Vec<Vec<f32>> = (0..NUM_LOB_FEATURES)
        .map(|_| Vec::with_capacity(total_rows))
        .collect();
    let mut flow_cols: Vec<Vec<f32>> = (0..NUM_FLOW_FEATURES)
        .map(|_| Vec::with_capacity(total_rows))
        .collect();
    let mut outcomes = Vec::with_capacity(total_rows);
    let mut exit_timestamps = Vec::with_capacity(total_rows);
    let mut pnl_ticks_col = Vec::with_capacity(total_rows);
    let mut direction_col: Vec<i8> = Vec::with_capacity(total_rows);

    // Index of ofi_fast in the flow feature array.
    const OFI_FAST_IDX: usize = 21;

    // Build an index mapping committed state timestamps to event buffer positions.
    // Each committed state corresponds to the event at the same point in time.
    // We approximate by finding the event buffer index closest to each committed state.
    let event_count = event_buf.len();

    for (progress_idx, &eval_idx) in eval_indices.iter().enumerate() {
        let state = &committed[eval_idx];

        // Get recent events for the lookback window
        // Find events before this committed state's timestamp
        let lookback_end = find_event_idx_at_ts(event_buf, state.ts, event_count);
        let lookback_start = lookback_end.saturating_sub(args.lookback_events);
        let recent_events = event_buf.get_events(lookback_start as u32, lookback_end as u32);

        // Compute LOB features once per evaluation point
        let features = compute_lob_features(state, recent_events, &cfg);

        // Compute flow features once per evaluation point
        let flow_feats = compute_flow_features(&flow_states[eval_idx]);

        // Determine direction and entry price
        let (direction, entry_price) = if args.ofi_direction {
            let ofi_fast = flow_feats[OFI_FAST_IDX];
            if ofi_fast >= 0.0 {
                (1.0_f64, state.asks[0][0] as f64) // long @ ask
            } else {
                (-1.0_f64, state.bids[0][0] as f64) // short @ bid
            }
        } else {
            (1.0_f64, state.asks[0][0] as f64) // long-only (legacy)
        };

        // Generate labels for all geometries
        let labels = generate_multi_geometry_labels(
            tick_mids,
            state.ts,
            entry_price,
            direction,
            args.tick_size,
            max_horizon_ns,
            &geometries,
        );

        for (t, s, outcome) in labels {
            timestamps.push(state.ts);
            best_bids.push(state.bids[0][0]);
            best_asks.push(state.asks[0][0]);
            mid_prices.push(state.mid);
            spreads.push(state.spread);
            target_ticks_col.push(t);
            stop_ticks_col.push(s);

            for (fi, &fv) in features.iter().enumerate() {
                feature_cols[fi].push(fv);
            }
            for (fi, &fv) in flow_feats.iter().enumerate() {
                flow_cols[fi].push(fv);
            }

            outcomes.push(outcome.outcome_code());
            exit_timestamps.push(outcome.exit_ts());
            pnl_ticks_col.push(outcome.ticks_pnl() as f32);
            direction_col.push(if direction >= 0.0 { 1i8 } else { -1i8 });
        }

        if (progress_idx + 1) % 10000 == 0 {
            eprintln!(
                "  {}/{} eval points processed ({} rows)",
                progress_idx + 1,
                eval_indices.len(),
                timestamps.len()
            );
        }
    }

    eprintln!(
        "  {} total rows ({} eval points × {} geometries)",
        timestamps.len(),
        eval_indices.len(),
        geometries.len()
    );

    // -----------------------------------------------------------------------
    // Step 4: Write Parquet
    // -----------------------------------------------------------------------
    eprintln!("[4/4] Writing Parquet to {}...", args.output);

    let schema = Arc::new(build_schema());

    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from(timestamps)),
        Arc::new(Float32Array::from(best_bids)),
        Arc::new(Float32Array::from(best_asks)),
        Arc::new(Float32Array::from(mid_prices)),
        Arc::new(Float32Array::from(spreads)),
        Arc::new(Int32Array::from(target_ticks_col)),
        Arc::new(Int32Array::from(stop_ticks_col)),
    ];

    for col in feature_cols {
        columns.push(Arc::new(Float32Array::from(col)));
    }
    for col in flow_cols {
        columns.push(Arc::new(Float32Array::from(col)));
    }

    columns.push(Arc::new(Int8Array::from(outcomes)));
    columns.push(Arc::new(UInt64Array::from(exit_timestamps)));
    columns.push(Arc::new(Float32Array::from(pnl_ticks_col)));
    columns.push(Arc::new(Int8Array::from(direction_col)));

    let batch = RecordBatch::try_new(schema.clone(), columns)
        .context("Failed to create RecordBatch")?;

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();

    let file = File::create(&args.output).context("Failed to create output file")?;
    let mut writer = ArrowWriter::try_new(file, schema, Some(props))
        .context("Failed to create Parquet writer")?;

    writer
        .write(&batch)
        .context("Failed to write batch to Parquet")?;
    writer.close().context("Failed to close Parquet writer")?;

    eprintln!("Done. {} rows written to {}", batch.num_rows(), args.output);

    Ok(())
}

/// Find the event buffer index for the event at or just before the given timestamp.
/// Uses binary search on the event timestamps.
fn find_event_idx_at_ts(
    event_buf: &common::event::DayEventBuffer,
    ts: u64,
    total_events: usize,
) -> usize {
    if total_events == 0 {
        return 0;
    }

    // Linear scan from end since events are time-sorted and we're processing
    // committed states in order. For the general case, we'd do binary search,
    // but DayEventBuffer doesn't expose a binary search method.
    // Instead, we do a simple partitioning approach.
    let events = event_buf.get_events(0, total_events as u32);
    events.partition_point(|e| e.ts_event <= ts)
}
