use std::fs::File;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use clap::Parser;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

use common::bar::Bar;
use common::book::BOOK_DEPTH;
use common::event::DayEventBuffer;
use features::{BarFeatureComputer, BarFeatureRow};
use labels::{
    compute_bidirectional_tb_label, compute_tb_label, BidirectionalTBResult, TripleBarrierConfig,
    TripleBarrierResult,
};

/// Export .dbn.zst MBO data to 152-column Parquet via bar construction + feature computation.
#[derive(Parser, Debug)]
#[command(name = "bar-feature-export")]
#[command(about = "Convert Databento .dbn.zst MBO data to bar features in Parquet format")]
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

    /// Bar type: time, tick, volume, dollar
    #[arg(long, default_value = "time")]
    bar_type: String,

    /// Bar parameter (e.g., 5 for 5-second time bars)
    #[arg(long, default_value = "5")]
    bar_param: f64,

    /// Triple barrier target in ticks
    #[arg(long, default_value = "19")]
    target: i32,

    /// Triple barrier stop in ticks
    #[arg(long, default_value = "7")]
    stop: i32,

    /// Maximum time horizon in seconds
    #[arg(long, default_value = "3600")]
    max_time_horizon: u32,

    /// Volume horizon in contracts
    #[arg(long, default_value = "50000")]
    volume_horizon: u32,

    /// Use legacy 149-column output format (no bidirectional labels)
    #[arg(long)]
    legacy_labels: bool,

    /// Date tag for metadata (YYYYMMDD format)
    #[arg(long)]
    date: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("bar-feature-export");
    eprintln!("  input:         {}", args.input);
    eprintln!("  output:        {}", args.output);
    eprintln!("  instrument_id: {}", args.instrument_id);
    eprintln!("  bar:           {} (param={})", args.bar_type, args.bar_param);
    eprintln!(
        "  target: {} ticks, stop: {} ticks",
        args.target, args.stop
    );
    eprintln!(
        "  time_horizon: {}s, volume_horizon: {}",
        args.max_time_horizon, args.volume_horizon
    );
    if let Some(ref d) = args.date {
        eprintln!("  date:          {}", d);
    }

    // -----------------------------------------------------------------------
    // Step 1: Ingest .dbn.zst
    // -----------------------------------------------------------------------
    eprintln!("[1/7] Ingesting {}...", args.input);
    let ingest = databento_ingest::ingest_day_file(&args.input, args.instrument_id)
        .context("Failed to ingest .dbn.zst file")?;

    if ingest.snapshots.is_empty() {
        bail!("No snapshots produced from ingest");
    }
    eprintln!(
        "  {} snapshots, {} MBO events",
        ingest.snapshots.len(),
        ingest.event_buffer.len()
    );

    // -----------------------------------------------------------------------
    // Step 2: Build bars from snapshots
    // -----------------------------------------------------------------------
    eprintln!(
        "[2/7] Building {} bars (param={})...",
        args.bar_type, args.bar_param
    );
    let mut builder = bars::create_bar_builder(&args.bar_type, args.bar_param)
        .ok_or_else(|| anyhow::anyhow!("Unknown bar type: {}", args.bar_type))?;

    let mut bars: Vec<Bar> = Vec::new();
    for snap in &ingest.snapshots {
        if let Some(bar) = builder.on_snapshot(snap) {
            bars.push(bar);
        }
    }
    if let Some(bar) = builder.flush() {
        bars.push(bar);
    }
    eprintln!("  {} bars constructed", bars.len());

    if bars.is_empty() {
        bail!(
            "No bars constructed from {} snapshots",
            ingest.snapshots.len()
        );
    }

    // -----------------------------------------------------------------------
    // Step 3: Reassign MBO events to bars (post-hoc)
    // -----------------------------------------------------------------------
    eprintln!("[3/7] Reassigning MBO events to bars...");
    reassign_mbo_events(&mut bars, &ingest.event_buffer);

    // -----------------------------------------------------------------------
    // Step 4: Compute features
    // -----------------------------------------------------------------------
    eprintln!("[4/7] Computing features...");
    let mut computer = BarFeatureComputer::new();
    let mut rows = computer.compute_all(&bars);

    let day_val: i32 = args
        .date
        .as_deref()
        .and_then(|d| d.replace('-', "").parse().ok())
        .unwrap_or(0);

    for row in rows.iter_mut() {
        row.bar_type = args.bar_type.clone();
        row.bar_param = args.bar_param as f32;
        row.day = day_val;
    }

    // -----------------------------------------------------------------------
    // Step 5: Compute labels
    // -----------------------------------------------------------------------
    eprintln!("[5/7] Computing labels...");
    let tb_cfg = TripleBarrierConfig {
        target_ticks: args.target,
        stop_ticks: args.stop,
        volume_horizon: args.volume_horizon,
        max_time_horizon_s: args.max_time_horizon,
        tick_size: 0.25,
        bidirectional: true,
        min_return_ticks: 2,
    };

    let mut tb_results: Vec<Option<TripleBarrierResult>> = vec![None; bars.len()];
    let mut bidir_results: Vec<Option<BidirectionalTBResult>> = vec![None; bars.len()];

    for i in 0..bars.len() {
        if !rows[i].is_warmup && !rows[i].fwd_return_1.is_nan() {
            tb_results[i] = Some(compute_tb_label(&bars, i, &tb_cfg));
            if !args.legacy_labels {
                bidir_results[i] = Some(compute_bidirectional_tb_label(&bars, i, &tb_cfg));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Step 6: Compute book snapshots and message summaries
    // -----------------------------------------------------------------------
    eprintln!("[6/7] Computing book snapshots and message summaries...");
    let book_snaps: Vec<[f64; 40]> = bars.iter().map(|b| flatten_book_snapshot(b)).collect();
    let msg_summaries: Vec<[f64; 33]> = bars
        .iter()
        .map(|b| compute_message_summary(b, &ingest.event_buffer))
        .collect();

    // -----------------------------------------------------------------------
    // Step 7: Write Parquet
    // -----------------------------------------------------------------------
    eprintln!("[7/7] Writing Parquet...");

    // Filter: skip warmup and bars with NaN forward returns
    let valid_indices: Vec<usize> = (0..bars.len())
        .filter(|&i| !rows[i].is_warmup && !rows[i].fwd_return_1.is_nan())
        .collect();

    eprintln!(
        "  {} valid rows (of {} total bars)",
        valid_indices.len(),
        bars.len()
    );

    write_parquet(
        &args.output,
        &bars,
        &rows,
        &book_snaps,
        &msg_summaries,
        &tb_results,
        &bidir_results,
        &valid_indices,
        args.legacy_labels,
    )?;

    eprintln!(
        "Done. Wrote {} rows to {}",
        valid_indices.len(),
        args.output
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Step 3: Post-hoc MBO event reassignment
// ---------------------------------------------------------------------------

/// Walk the event buffer and assign `[begin, end)` ranges to bars by `close_ts`.
/// Recount add/cancel/modify/trade from actual events and recompute derived fields.
fn reassign_mbo_events(bars: &mut [Bar], event_buffer: &DayEventBuffer) {
    if bars.is_empty() || event_buffer.is_empty() {
        return;
    }

    let n_events = event_buffer.len();
    let all_events = event_buffer.get_events(0, n_events as u32);

    // Reset all bars' event ranges and counts
    for bar in bars.iter_mut() {
        bar.mbo_event_begin = 0;
        bar.mbo_event_end = 0;
        bar.add_count = 0;
        bar.cancel_count = 0;
        bar.modify_count = 0;
        bar.trade_event_count = 0;
    }

    let mut event_idx = 0usize;

    for bar_idx in 0..bars.len() {
        let bar_close = bars[bar_idx].close_ts;
        bars[bar_idx].mbo_event_begin = event_idx as u32;

        // Consume events up to and including this bar's close_ts
        while event_idx < n_events {
            let event = &all_events[event_idx];
            if event.ts_event > bar_close {
                break;
            }

            match event.action {
                0 => bars[bar_idx].add_count += 1,    // Add
                1 => bars[bar_idx].cancel_count += 1,  // Cancel
                2 => bars[bar_idx].modify_count += 1,  // Modify
                3 => bars[bar_idx].trade_event_count += 1, // Trade
                _ => {}
            }

            event_idx += 1;
        }

        bars[bar_idx].mbo_event_end = event_idx as u32;

        // Recompute derived fields
        let add = bars[bar_idx].add_count as f32;
        bars[bar_idx].cancel_add_ratio = if add > 0.0 {
            bars[bar_idx].cancel_count as f32 / add
        } else {
            0.0
        };

        let total_msgs =
            (bars[bar_idx].add_count + bars[bar_idx].cancel_count + bars[bar_idx].modify_count)
                as f32;
        bars[bar_idx].message_rate = if bars[bar_idx].bar_duration_s > 0.0 {
            total_msgs / bars[bar_idx].bar_duration_s
        } else {
            0.0
        };
    }
}

// ---------------------------------------------------------------------------
// Step 6: Book snapshot flattening & message summary
// ---------------------------------------------------------------------------

/// Flatten the bar's book state into 40 f64 values.
///
/// Layout: 20 rows x 2 values (price relative to close_mid, size).
/// Rows 0-9: bids reversed (deepest first, best bid at row 9).
/// Rows 10-19: asks in order (best ask at row 10).
fn flatten_book_snapshot(bar: &Bar) -> [f64; 40] {
    let mut out = [0.0f64; 40];
    let mid = bar.close_mid;

    // Bids: reverse order (deepest first)
    for i in 0..BOOK_DEPTH {
        let bid_idx = BOOK_DEPTH - 1 - i;
        out[i * 2] = (bar.bids[bid_idx][0] - mid) as f64;
        out[i * 2 + 1] = bar.bids[bid_idx][1] as f64;
    }

    // Asks: natural order (best ask first)
    for i in 0..BOOK_DEPTH {
        out[20 + i * 2] = (bar.asks[i][0] - mid) as f64;
        out[20 + i * 2 + 1] = bar.asks[i][1] as f64;
    }

    out
}

/// Compute a 33-element message summary for a bar's MBO events.
///
/// Layout:
///   [0..30]: per-decile action counts (add/cancel/modify x 10 deciles)
///   [30]: first-half cancel/add ratio
///   [31]: second-half cancel/add ratio
///   [32]: max decile message rate
fn compute_message_summary(bar: &Bar, event_buffer: &DayEventBuffer) -> [f64; 33] {
    let mut out = [0.0f64; 33];

    let events = event_buffer.get_events(bar.mbo_event_begin, bar.mbo_event_end);
    if events.is_empty() {
        return out;
    }

    let bar_start = bar.open_ts;
    let bar_end = bar.close_ts;
    if bar_end <= bar_start {
        return out;
    }
    let bar_dur = (bar_end - bar_start) as f64;

    let mut first_half_add = 0.0f64;
    let mut first_half_cancel = 0.0f64;
    let mut second_half_add = 0.0f64;
    let mut second_half_cancel = 0.0f64;
    let mut decile_total = [0.0f64; 10];

    for event in events {
        let frac = if event.ts_event >= bar_start {
            ((event.ts_event - bar_start) as f64 / bar_dur).min(0.999999)
        } else {
            0.0
        };
        let decile = (frac * 10.0) as usize;

        match event.action {
            0 => {
                // Add
                out[decile * 3] += 1.0;
                if frac < 0.5 {
                    first_half_add += 1.0;
                } else {
                    second_half_add += 1.0;
                }
            }
            1 => {
                // Cancel
                out[decile * 3 + 1] += 1.0;
                if frac < 0.5 {
                    first_half_cancel += 1.0;
                } else {
                    second_half_cancel += 1.0;
                }
            }
            2 => {
                // Modify
                out[decile * 3 + 2] += 1.0;
            }
            _ => {}
        }

        decile_total[decile] += 1.0;
    }

    // First-half cancel/add ratio
    out[30] = if first_half_add > 0.0 {
        first_half_cancel / first_half_add
    } else {
        0.0
    };

    // Second-half cancel/add ratio
    out[31] = if second_half_add > 0.0 {
        second_half_cancel / second_half_add
    } else {
        0.0
    };

    // Max decile message rate
    let decile_dur_s = bar.bar_duration_s as f64 / 10.0;
    if decile_dur_s > 0.0 {
        out[32] = decile_total
            .iter()
            .copied()
            .fold(0.0f64, f64::max)
            / decile_dur_s;
    }

    out
}

// ---------------------------------------------------------------------------
// Feature extraction helper
// ---------------------------------------------------------------------------

/// Extract the 62 Track A feature values from a BarFeatureRow in canonical order.
fn extract_features(row: &BarFeatureRow) -> [f64; 62] {
    [
        // Cat 1: Book Shape (32)
        row.book_imbalance_1 as f64,
        row.book_imbalance_3 as f64,
        row.book_imbalance_5 as f64,
        row.book_imbalance_10 as f64,
        row.weighted_imbalance as f64,
        row.spread as f64,
        row.bid_depth_profile[0] as f64,
        row.bid_depth_profile[1] as f64,
        row.bid_depth_profile[2] as f64,
        row.bid_depth_profile[3] as f64,
        row.bid_depth_profile[4] as f64,
        row.bid_depth_profile[5] as f64,
        row.bid_depth_profile[6] as f64,
        row.bid_depth_profile[7] as f64,
        row.bid_depth_profile[8] as f64,
        row.bid_depth_profile[9] as f64,
        row.ask_depth_profile[0] as f64,
        row.ask_depth_profile[1] as f64,
        row.ask_depth_profile[2] as f64,
        row.ask_depth_profile[3] as f64,
        row.ask_depth_profile[4] as f64,
        row.ask_depth_profile[5] as f64,
        row.ask_depth_profile[6] as f64,
        row.ask_depth_profile[7] as f64,
        row.ask_depth_profile[8] as f64,
        row.ask_depth_profile[9] as f64,
        row.depth_concentration_bid as f64,
        row.depth_concentration_ask as f64,
        row.book_slope_bid as f64,
        row.book_slope_ask as f64,
        row.level_count_bid as f64,
        row.level_count_ask as f64,
        // Cat 2: Order Flow (7)
        row.net_volume as f64,
        row.volume_imbalance as f64,
        row.trade_count as f64,
        row.avg_trade_size as f64,
        row.large_trade_count as f64,
        row.vwap_distance as f64,
        row.kyle_lambda as f64,
        // Cat 3: Price Dynamics (9)
        row.return_1 as f64,
        row.return_5 as f64,
        row.return_20 as f64,
        row.volatility_20 as f64,
        row.volatility_50 as f64,
        row.momentum as f64,
        row.high_low_range_20 as f64,
        row.high_low_range_50 as f64,
        row.close_position as f64,
        // Cat 4: Cross-Scale Dynamics (4)
        row.volume_surprise as f64,
        row.duration_surprise as f64,
        row.acceleration as f64,
        row.vol_price_corr as f64,
        // Cat 5: Time Context (5)
        row.time_sin as f64,
        row.time_cos as f64,
        row.minutes_since_open as f64,
        row.minutes_to_close as f64,
        row.session_volume_frac as f64,
        // Cat 6: Message Microstructure (5)
        row.cancel_add_ratio as f64,
        row.message_rate as f64,
        row.modify_fraction as f64,
        row.order_flow_toxicity as f64,
        row.cancel_concentration as f64,
    ]
}

// ---------------------------------------------------------------------------
// Step 7: Parquet writer
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn write_parquet(
    path: &str,
    bars: &[Bar],
    rows: &[BarFeatureRow],
    book_snaps: &[[f64; 40]],
    msg_summaries: &[[f64; 33]],
    tb_results: &[Option<TripleBarrierResult>],
    bidir_results: &[Option<BidirectionalTBResult>],
    valid_indices: &[usize],
    legacy_labels: bool,
) -> Result<()> {
    let feature_names = BarFeatureRow::feature_names();

    // Build Arrow schema
    let mut fields: Vec<Field> = Vec::new();

    // Metadata (6)
    fields.push(Field::new("timestamp", DataType::Int64, false));
    fields.push(Field::new("bar_type", DataType::Utf8, false));
    fields.push(Field::new("bar_param", DataType::Float64, false));
    fields.push(Field::new("day", DataType::Int64, false));
    fields.push(Field::new("is_warmup", DataType::Boolean, false));
    fields.push(Field::new("bar_index", DataType::Int64, false));

    // Track A features (62)
    for name in &feature_names {
        fields.push(Field::new(*name, DataType::Float64, true));
    }

    // Book snapshot (40)
    for i in 0..40 {
        fields.push(Field::new(
            format!("book_snap_{}", i),
            DataType::Float64,
            true,
        ));
    }

    // Message summary (33)
    for i in 0..33 {
        fields.push(Field::new(
            format!("msg_summary_{}", i),
            DataType::Float64,
            true,
        ));
    }

    // Forward returns (4)
    fields.push(Field::new("fwd_return_1", DataType::Float64, true));
    fields.push(Field::new("fwd_return_5", DataType::Float64, true));
    fields.push(Field::new("fwd_return_20", DataType::Float64, true));
    fields.push(Field::new("fwd_return_100", DataType::Float64, true));

    // MBO event count (1)
    fields.push(Field::new("mbo_event_count", DataType::Float64, true));

    // Label columns (3)
    fields.push(Field::new("tb_label", DataType::Float64, true));
    fields.push(Field::new("tb_exit_type", DataType::Utf8, true));
    fields.push(Field::new("tb_bars_held", DataType::Float64, true));

    // Bidirectional columns (3, non-legacy only)
    if !legacy_labels {
        fields.push(Field::new("tb_both_triggered", DataType::Float64, true));
        fields.push(Field::new("tb_long_triggered", DataType::Float64, true));
        fields.push(Field::new(
            "tb_short_triggered",
            DataType::Float64,
            true,
        ));
    }

    let schema = Arc::new(Schema::new(fields));

    // Accumulate column vectors
    let n = valid_indices.len();

    let mut col_timestamp: Vec<i64> = Vec::with_capacity(n);
    let mut col_bar_type: Vec<String> = Vec::with_capacity(n);
    let mut col_bar_param: Vec<f64> = Vec::with_capacity(n);
    let mut col_day: Vec<i64> = Vec::with_capacity(n);
    let mut col_is_warmup: Vec<bool> = Vec::with_capacity(n);
    let mut col_bar_index: Vec<i64> = Vec::with_capacity(n);

    let mut feature_cols: Vec<Vec<f64>> = (0..62).map(|_| Vec::with_capacity(n)).collect();
    let mut book_cols: Vec<Vec<f64>> = (0..40).map(|_| Vec::with_capacity(n)).collect();
    let mut msg_cols: Vec<Vec<f64>> = (0..33).map(|_| Vec::with_capacity(n)).collect();

    let mut col_fwd1: Vec<f64> = Vec::with_capacity(n);
    let mut col_fwd5: Vec<f64> = Vec::with_capacity(n);
    let mut col_fwd20: Vec<f64> = Vec::with_capacity(n);
    let mut col_fwd100: Vec<f64> = Vec::with_capacity(n);

    let mut col_mbo_count: Vec<f64> = Vec::with_capacity(n);

    let mut col_tb_label: Vec<f64> = Vec::with_capacity(n);
    let mut col_tb_exit: Vec<String> = Vec::with_capacity(n);
    let mut col_tb_held: Vec<f64> = Vec::with_capacity(n);

    let mut col_tb_both: Vec<f64> = Vec::new();
    let mut col_tb_long: Vec<f64> = Vec::new();
    let mut col_tb_short: Vec<f64> = Vec::new();
    if !legacy_labels {
        col_tb_both.reserve(n);
        col_tb_long.reserve(n);
        col_tb_short.reserve(n);
    }

    for &i in valid_indices {
        let row = &rows[i];
        let bar = &bars[i];

        col_timestamp.push(row.timestamp as i64);
        col_bar_type.push(row.bar_type.clone());
        col_bar_param.push(row.bar_param as f64);
        col_day.push(row.day as i64);
        col_is_warmup.push(row.is_warmup);
        col_bar_index.push(i as i64);

        let feats = extract_features(row);
        for (j, &val) in feats.iter().enumerate() {
            feature_cols[j].push(val);
        }

        for j in 0..40 {
            book_cols[j].push(book_snaps[i][j]);
        }

        for j in 0..33 {
            msg_cols[j].push(msg_summaries[i][j]);
        }

        col_fwd1.push(row.fwd_return_1 as f64);
        col_fwd5.push(row.fwd_return_5 as f64);
        col_fwd20.push(row.fwd_return_20 as f64);
        col_fwd100.push(row.fwd_return_100 as f64);

        col_mbo_count.push((bar.mbo_event_end - bar.mbo_event_begin) as f64);

        if let Some(ref tb) = tb_results[i] {
            col_tb_label.push(tb.label as f64);
            col_tb_exit.push(tb.exit_type.clone());
            col_tb_held.push(tb.bars_held as f64);
        } else {
            col_tb_label.push(f64::NAN);
            col_tb_exit.push(String::new());
            col_tb_held.push(f64::NAN);
        }

        if !legacy_labels {
            if let Some(ref bi) = bidir_results[i] {
                col_tb_both.push(if bi.both_triggered { 1.0 } else { 0.0 });
                col_tb_long.push(if bi.long_triggered { 1.0 } else { 0.0 });
                col_tb_short.push(if bi.short_triggered { 1.0 } else { 0.0 });
            } else {
                col_tb_both.push(f64::NAN);
                col_tb_long.push(f64::NAN);
                col_tb_short.push(f64::NAN);
            }
        }
    }

    // Build Arrow arrays
    let mut columns: Vec<ArrayRef> = Vec::new();

    columns.push(Arc::new(Int64Array::from(col_timestamp)));
    columns.push(Arc::new(StringArray::from(
        col_bar_type
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
    )));
    columns.push(Arc::new(Float64Array::from(col_bar_param)));
    columns.push(Arc::new(Int64Array::from(col_day)));
    columns.push(Arc::new(BooleanArray::from(col_is_warmup)));
    columns.push(Arc::new(Int64Array::from(col_bar_index)));

    for col in feature_cols {
        columns.push(Arc::new(Float64Array::from(col)));
    }

    for col in book_cols {
        columns.push(Arc::new(Float64Array::from(col)));
    }

    for col in msg_cols {
        columns.push(Arc::new(Float64Array::from(col)));
    }

    columns.push(Arc::new(Float64Array::from(col_fwd1)));
    columns.push(Arc::new(Float64Array::from(col_fwd5)));
    columns.push(Arc::new(Float64Array::from(col_fwd20)));
    columns.push(Arc::new(Float64Array::from(col_fwd100)));

    columns.push(Arc::new(Float64Array::from(col_mbo_count)));

    columns.push(Arc::new(Float64Array::from(col_tb_label)));
    columns.push(Arc::new(StringArray::from(
        col_tb_exit
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
    )));
    columns.push(Arc::new(Float64Array::from(col_tb_held)));

    if !legacy_labels {
        columns.push(Arc::new(Float64Array::from(col_tb_both)));
        columns.push(Arc::new(Float64Array::from(col_tb_long)));
        columns.push(Arc::new(Float64Array::from(col_tb_short)));
    }

    // Create RecordBatch and write to file
    let batch =
        RecordBatch::try_new(schema.clone(), columns).context("Failed to create RecordBatch")?;

    let file = File::create(path).context("Failed to create output file")?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();

    let mut writer =
        ArrowWriter::try_new(file, schema, Some(props)).context("Failed to create ArrowWriter")?;

    writer.write(&batch).context("Failed to write batch")?;
    writer.close().context("Failed to close writer")?;

    Ok(())
}
