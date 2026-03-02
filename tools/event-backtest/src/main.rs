//! Event-level CPCV backtest with serial PnL computation.
//!
//! Reads event-level Parquet files (from event-export), trains XGBoost regression
//! models predicting P(target | LOB state, T, S), and evaluates via CPCV and
//! serial backtest.

#![allow(dead_code)] // Structs/fns for CPCV fold runner (used on EC2)

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use arrow::array::{Float32Array, Int8Array, Int32Array, UInt64Array};
use clap::Parser;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};

use event_features::NUM_LOB_FEATURES;

/// Event-level CPCV backtest.
#[derive(Parser, Debug)]
#[command(name = "event-backtest")]
#[command(about = "CPCV backtest on event-level LOB probability model")]
struct Args {
    /// Directory containing event Parquet files (YYYY-MM-DD-events.parquet)
    #[arg(long)]
    data_dir: String,

    /// Output directory for results
    #[arg(long)]
    output_dir: String,

    /// Number of CPCV groups
    #[arg(long, default_value = "10")]
    n_groups: usize,

    /// Number of test groups per split
    #[arg(long, default_value = "2")]
    k_test: usize,

    /// Purge seconds before test boundary
    #[arg(long, default_value = "300")]
    purge_seconds: u64,

    /// Embargo seconds after test boundary
    #[arg(long, default_value = "300")]
    embargo_seconds: u64,

    /// XGBoost max_depth
    #[arg(long, default_value = "6")]
    max_depth: u32,

    /// XGBoost learning rate
    #[arg(long, default_value = "0.01")]
    eta: f64,

    /// XGBoost min_child_weight
    #[arg(long, default_value = "50")]
    min_child_weight: u32,

    /// XGBoost subsample
    #[arg(long, default_value = "0.6")]
    subsample: f64,

    /// XGBoost colsample_bytree
    #[arg(long, default_value = "0.7")]
    colsample_bytree: f64,

    /// Maximum number of boosting rounds
    #[arg(long, default_value = "3000")]
    n_estimators: u32,

    /// Early stopping rounds
    #[arg(long, default_value = "100")]
    early_stopping: u32,

    /// Decision margin above null hypothesis
    #[arg(long, default_value = "0.02")]
    margin: f64,

    /// Tick size for the instrument
    #[arg(long, default_value = "0.25")]
    tick_size: f64,

    /// Tick value (contract_multiplier * tick_size)
    #[arg(long, default_value = "1.25")]
    tick_value: f64,

    /// Round-trip commission in dollars (excluding spread)
    #[arg(long, default_value = "1.24")]
    commission: f64,

    /// Run null hypothesis test (shuffled features)
    #[arg(long)]
    null_test: bool,

    /// Run temporal holdout (first 170 days train, last 81 test)
    #[arg(long)]
    temporal_holdout: bool,
}

/// A single row from the event Parquet.
#[derive(Debug, Clone)]
struct EventRow {
    timestamp_ns: u64,
    best_bid: f32,
    best_ask: f32,
    mid_price: f32,
    spread: f32,
    target_ticks: i32,
    stop_ticks: i32,
    features: [f32; NUM_LOB_FEATURES],
    outcome: i8,
    exit_ts: u64,
    pnl_ticks: f32,
}

/// Results from a single CPCV fold.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FoldResult {
    fold_idx: usize,
    test_groups: Vec<usize>,
    n_train: usize,
    n_test: usize,
    n_trades: usize,
    win_rate: f64,
    expectancy: f64,
    net_pnl: f64,
    sharpe: f64,
    profit_factor: f64,
    feature_importance: Vec<(String, f64)>,
}

/// Aggregated results across all folds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BacktestResults {
    config: BacktestConfig,
    folds: Vec<FoldResult>,
    aggregate: AggregateMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BacktestConfig {
    n_groups: usize,
    k_test: usize,
    n_days: usize,
    total_rows: usize,
    margin: f64,
    max_depth: u32,
    eta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AggregateMetrics {
    mean_sharpe: f64,
    median_sharpe: f64,
    mean_expectancy: f64,
    mean_win_rate: f64,
    pbo: f64,
    total_trades: usize,
    positive_folds: usize,
    total_folds: usize,
}

/// Serial PnL simulation: given predictions and event data,
/// simulate trades with serial constraint (no overlapping positions).
fn compute_serial_pnl(
    predictions: &[f64],
    events: &[EventRow],
    margin: f64,
    tick_value: f64,
    commission: f64,
) -> (Vec<f64>, usize, usize) {
    let mut pnls: Vec<f64> = Vec::new();
    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut next_available_ts: u64 = 0;

    for (i, event) in events.iter().enumerate() {
        // Skip if we're still in a position
        if event.timestamp_ns < next_available_ts {
            continue;
        }

        // Skip horizon outcomes (no clear label)
        if event.outcome == -1 {
            continue;
        }

        let t = event.target_ticks as f64;
        let s = event.stop_ticks as f64;
        let p_null = s / (t + s);
        let p_model = predictions[i];

        // Only trade when model predicts edge above null + margin
        if p_model <= p_null + margin {
            continue;
        }

        // Trade!
        let pnl = event.pnl_ticks as f64 * tick_value - commission;
        pnls.push(pnl);

        if pnl > 0.0 {
            wins += 1;
        } else {
            losses += 1;
        }

        // Lock out until exit
        next_available_ts = event.exit_ts;
    }

    (pnls, wins, losses)
}

fn compute_sharpe(pnls: &[f64]) -> f64 {
    if pnls.len() < 2 {
        return 0.0;
    }
    let n = pnls.len() as f64;
    let mean = pnls.iter().sum::<f64>() / n;
    let variance = pnls.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std = variance.sqrt();
    if std < 1e-12 {
        return 0.0;
    }
    // Annualize: assume ~252 trading days, ~50 trades/day
    let trades_per_year = 252.0 * 50.0;
    mean / std * (trades_per_year / n).sqrt().min(trades_per_year.sqrt())
}

fn compute_profit_factor(pnls: &[f64]) -> f64 {
    let gross_profit: f64 = pnls.iter().filter(|&&p| p > 0.0).sum();
    let gross_loss: f64 = pnls.iter().filter(|&&p| p < 0.0).map(|p| p.abs()).sum();
    if gross_loss < 1e-12 {
        if gross_profit > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    } else {
        gross_profit / gross_loss
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    eprintln!("event-backtest");
    eprintln!("  data_dir:      {}", args.data_dir);
    eprintln!("  output_dir:    {}", args.output_dir);
    eprintln!("  n_groups:      {}", args.n_groups);
    eprintln!("  k_test:        {}", args.k_test);
    eprintln!("  margin:        {}", args.margin);

    // -----------------------------------------------------------------------
    // Step 1: Discover and load Parquet files
    // -----------------------------------------------------------------------
    eprintln!("[1/4] Loading event Parquet files...");

    let mut parquet_files: Vec<PathBuf> = std::fs::read_dir(&args.data_dir)
        .context("Failed to read data directory")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "parquet")
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();

    parquet_files.sort();
    eprintln!("  Found {} Parquet files", parquet_files.len());

    if parquet_files.is_empty() {
        bail!("No Parquet files found in {}", args.data_dir);
    }

    // Load all rows, grouped by day (file)
    let mut all_rows: Vec<EventRow> = Vec::new();
    let mut day_boundaries: Vec<(usize, usize)> = Vec::new(); // (start_idx, end_idx) per day

    let mut skipped_files = 0usize;
    for path in &parquet_files {
        let start = all_rows.len();
        let file = File::open(path).context("Failed to open Parquet file")?;
        let reader = match ParquetRecordBatchReaderBuilder::try_new(file) {
            Ok(builder) => match builder.build() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("  SKIP {} (corrupt: {})", path.file_name().unwrap_or_default().to_string_lossy(), e);
                    skipped_files += 1;
                    continue;
                }
            },
            Err(e) => {
                eprintln!("  SKIP {} (corrupt: {})", path.file_name().unwrap_or_default().to_string_lossy(), e);
                skipped_files += 1;
                continue;
            }
        };

        for batch_result in reader {
            let batch = batch_result.context("Failed to read record batch")?;
            let n = batch.num_rows();

            let ts_col = batch.column_by_name("timestamp_ns")
                .context("Missing timestamp_ns")?
                .as_any().downcast_ref::<UInt64Array>()
                .context("timestamp_ns not UInt64")?;
            let bid_col = batch.column_by_name("best_bid")
                .context("Missing best_bid")?
                .as_any().downcast_ref::<Float32Array>()
                .context("best_bid not Float32")?;
            let ask_col = batch.column_by_name("best_ask")
                .context("Missing best_ask")?
                .as_any().downcast_ref::<Float32Array>()
                .context("best_ask not Float32")?;
            let mid_col = batch.column_by_name("mid_price")
                .context("Missing mid_price")?
                .as_any().downcast_ref::<Float32Array>()
                .context("mid_price not Float32")?;
            let spread_col = batch.column_by_name("spread")
                .context("Missing spread")?
                .as_any().downcast_ref::<Float32Array>()
                .context("spread not Float32")?;
            let target_col = batch.column_by_name("target_ticks")
                .context("Missing target_ticks")?
                .as_any().downcast_ref::<Int32Array>()
                .context("target_ticks not Int32")?;
            let stop_col = batch.column_by_name("stop_ticks")
                .context("Missing stop_ticks")?
                .as_any().downcast_ref::<Int32Array>()
                .context("stop_ticks not Int32")?;
            let outcome_col = batch.column_by_name("outcome")
                .context("Missing outcome")?
                .as_any().downcast_ref::<Int8Array>()
                .context("outcome not Int8")?;
            let exit_col = batch.column_by_name("exit_ts")
                .context("Missing exit_ts")?
                .as_any().downcast_ref::<UInt64Array>()
                .context("exit_ts not UInt64")?;
            let pnl_col = batch.column_by_name("pnl_ticks")
                .context("Missing pnl_ticks")?
                .as_any().downcast_ref::<Float32Array>()
                .context("pnl_ticks not Float32")?;

            // Read feature columns
            let mut feature_arrays: Vec<&Float32Array> = Vec::with_capacity(NUM_LOB_FEATURES);
            for name in &event_features::LOB_FEATURE_NAMES {
                let arr = batch.column_by_name(name)
                    .with_context(|| format!("Missing feature column: {}", name))?
                    .as_any().downcast_ref::<Float32Array>()
                    .with_context(|| format!("Feature {} not Float32", name))?;
                feature_arrays.push(arr);
            }

            for row_idx in 0..n {
                let mut features = [0.0f32; NUM_LOB_FEATURES];
                for (fi, arr) in feature_arrays.iter().enumerate() {
                    features[fi] = arr.value(row_idx);
                }

                all_rows.push(EventRow {
                    timestamp_ns: ts_col.value(row_idx),
                    best_bid: bid_col.value(row_idx),
                    best_ask: ask_col.value(row_idx),
                    mid_price: mid_col.value(row_idx),
                    spread: spread_col.value(row_idx),
                    target_ticks: target_col.value(row_idx),
                    stop_ticks: stop_col.value(row_idx),
                    features,
                    outcome: outcome_col.value(row_idx),
                    exit_ts: exit_col.value(row_idx),
                    pnl_ticks: pnl_col.value(row_idx),
                });
            }
        }

        let end = all_rows.len();
        day_boundaries.push((start, end));
        eprintln!(
            "  Loaded {} rows from {}",
            end - start,
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }

    if skipped_files > 0 {
        eprintln!("  Skipped {} corrupt files", skipped_files);
    }
    eprintln!(
        "  Total: {} rows across {} days",
        all_rows.len(),
        day_boundaries.len()
    );

    // -----------------------------------------------------------------------
    // Step 2: CPCV split generation
    // -----------------------------------------------------------------------
    eprintln!("[2/4] Generating CPCV splits...");

    let n_days = day_boundaries.len();
    let _groups = backtest::cpcv::assign_groups(n_days, args.n_groups);

    // Build day metas (we use row counts as the bar_counts equivalent)
    let row_counts: Vec<usize> = day_boundaries
        .iter()
        .map(|(s, e)| e - s)
        .collect();
    let dates: Vec<i32> = (0..n_days as i32).collect(); // placeholder dates

    let day_metas = backtest::cpcv::build_day_metas(&dates, &row_counts, args.n_groups);
    let cpcv_config = backtest::cpcv::CpcvConfig {
        n_groups: args.n_groups,
        k_test: args.k_test,
        purge_bars: 0, // We handle purge/embargo at the event level via timestamps
        embargo_bars: 0,
    };
    let splits = backtest::cpcv::generate_splits(&day_metas, &cpcv_config);

    eprintln!("  {} CPCV splits generated", splits.len());

    // -----------------------------------------------------------------------
    // Step 3: Run folds (placeholder — requires XGBoost training)
    // -----------------------------------------------------------------------
    eprintln!("[3/4] Running CPCV folds...");
    eprintln!("  NOTE: Full XGBoost training requires EC2. Running baseline analysis.");

    // For now, compute baseline metrics: outcome distribution, null hypothesis check
    let mut target_count = 0u64;
    let mut stop_count = 0u64;
    let mut horizon_count = 0u64;
    let mut geometry_stats: BTreeMap<(i32, i32), (u64, u64, u64)> = BTreeMap::new();

    for row in &all_rows {
        match row.outcome {
            1 => target_count += 1,
            0 => stop_count += 1,
            _ => horizon_count += 1,
        }
        let entry = geometry_stats
            .entry((row.target_ticks, row.stop_ticks))
            .or_insert((0, 0, 0));
        match row.outcome {
            1 => entry.0 += 1,
            0 => entry.1 += 1,
            _ => entry.2 += 1,
        }
    }

    // -----------------------------------------------------------------------
    // Step 4: Write results
    // -----------------------------------------------------------------------
    eprintln!("[4/4] Writing results...");
    std::fs::create_dir_all(&args.output_dir).context("Failed to create output dir")?;

    // Write baseline analysis
    let total_labeled = target_count + stop_count;
    let empirical_target_rate = if total_labeled > 0 {
        target_count as f64 / total_labeled as f64
    } else {
        0.0
    };

    let mut report = String::new();
    report.push_str("# Event-Level Baseline Analysis\n\n");
    report.push_str(&format!("Total rows: {}\n", all_rows.len()));
    report.push_str(&format!("Days: {}\n", n_days));
    report.push_str(&format!(
        "Outcomes: {} target, {} stop, {} horizon\n",
        target_count, stop_count, horizon_count
    ));
    report.push_str(&format!(
        "Overall target rate (excl. horizon): {:.4}\n\n",
        empirical_target_rate
    ));

    report.push_str("## Per-Geometry Null Hypothesis Check\n\n");
    report.push_str("| T | S | P_null | P_empirical | Target | Stop | Horizon | Delta |\n");
    report.push_str("|---|---|--------|-------------|--------|------|---------|-------|\n");

    for (&(t, s), &(targets, stops, horizons)) in &geometry_stats {
        let p_null = s as f64 / (t as f64 + s as f64);
        let labeled = targets + stops;
        let p_emp = if labeled > 0 {
            targets as f64 / labeled as f64
        } else {
            0.0
        };
        let delta = p_emp - p_null;
        report.push_str(&format!(
            "| {} | {} | {:.4} | {:.4} | {} | {} | {} | {:+.4} |\n",
            t, s, p_null, p_emp, targets, stops, horizons, delta
        ));
    }

    let report_path = format!("{}/baseline-analysis.md", args.output_dir);
    std::fs::write(&report_path, &report).context("Failed to write report")?;
    eprintln!("  Baseline analysis: {}", report_path);

    // Write config JSON
    let config = BacktestConfig {
        n_groups: args.n_groups,
        k_test: args.k_test,
        n_days,
        total_rows: all_rows.len(),
        margin: args.margin,
        max_depth: args.max_depth,
        eta: args.eta,
    };

    let config_path = format!("{}/config.json", args.output_dir);
    let config_json = serde_json::to_string_pretty(&config)?;
    std::fs::write(&config_path, &config_json)?;

    eprintln!("\nDone. Results in {}", args.output_dir);
    eprintln!("  Next: Run full CPCV training on EC2 with XGBoost.");

    Ok(())
}
