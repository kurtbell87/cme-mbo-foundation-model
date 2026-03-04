//! Event-level CPCV backtest with serial PnL computation.
//!
//! Two modes:
//! - `baseline`: streaming counters — outcome distribution, null hypothesis check
//! - `cpcv`: full CPCV XGBoost training with serial PnL evaluation
//!
//! CPCV mode: loads event-level Parquet files, trains XGBoost regression models
//! predicting P(target | LOB state, T, S), and evaluates via CPCV (45 folds).

pub mod data;
pub mod fold_runner;
pub mod statistics;

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use arrow::array::{Int8Array, Int32Array};
use clap::{Parser, ValueEnum};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use rayon::prelude::*;

use backtest::{CpcvConfig, build_day_metas, generate_splits};

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

    /// Run mode: baseline (streaming counters) or cpcv (full XGBoost training)
    #[arg(long, default_value = "baseline")]
    mode: RunMode,

    /// Number of CPCV groups
    #[arg(long, default_value = "10")]
    n_groups: usize,

    /// Number of test groups per split
    #[arg(long, default_value = "2")]
    k_test: usize,

    /// Purge seconds before test boundary (converted to row count internally)
    #[arg(long, default_value = "300")]
    purge_seconds: u64,

    /// Embargo seconds after test boundary (converted to row count internally)
    #[arg(long, default_value = "300")]
    embargo_seconds: u64,

    /// XGBoost max_depth
    #[arg(long, default_value = "6")]
    max_depth: u32,

    /// XGBoost learning rate
    #[arg(long, default_value = "0.01")]
    eta: f64,

    /// XGBoost min_child_weight
    #[arg(long, default_value = "100")]
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

    /// XGBoost max_bin for hist tree method
    #[arg(long, default_value = "256")]
    max_bin: u32,

    /// Decision margin above null hypothesis P(target) = S/(T+S)
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

    /// Eval-point subsampling percentage (0-100). Lower = less memory, faster.
    #[arg(long, default_value = "15")]
    subsample_pct: u32,

    /// Number of folds to run in parallel (0 = sequential, all cores per fold)
    #[arg(long, default_value = "4")]
    parallel_folds: usize,

    /// S3 path to upload results (e.g. s3://bucket/path/). Requires AWS CLI.
    #[arg(long)]
    s3_output: Option<String>,

    /// Random seed for subsampling reproducibility
    #[arg(long, default_value = "42")]
    seed: u64,

    /// OFI threshold for imbalance mode: minimum |ofi_fast| to include a bar
    #[arg(long, default_value = "2.0")]
    ofi_threshold: f32,

    /// Geometry filter for imbalance mode: "T:S" (e.g. "10:5")
    #[arg(long)]
    geometry: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum RunMode {
    Baseline,
    Cpcv,
    Imbalance,
}

fn main() -> Result<()> {
    let args = Args::parse();

    match args.mode {
        RunMode::Baseline => run_baseline(&args),
        RunMode::Cpcv => run_cpcv(&args),
        RunMode::Imbalance => run_imbalance(&args),
    }
}

// ---------------------------------------------------------------------------
// CPCV Mode
// ---------------------------------------------------------------------------

fn run_cpcv(args: &Args) -> Result<()> {
    let total_start = Instant::now();

    eprintln!("event-backtest (CPCV mode)");
    eprintln!("  data_dir:        {}", args.data_dir);
    eprintln!("  output_dir:      {}", args.output_dir);
    eprintln!("  n_groups:        {}", args.n_groups);
    eprintln!("  k_test:          {}", args.k_test);
    eprintln!("  margin:          {}", args.margin);
    eprintln!("  subsample_pct:   {}%", args.subsample_pct);
    eprintln!("  parallel_folds:  {}", args.parallel_folds);
    eprintln!("  seed:            {}", args.seed);

    // ── Phase 1: Scan day metadata ──────────────────────────────────────
    eprintln!("\n[Phase 1] Scanning day metadata...");
    let scan_start = Instant::now();

    let day_metas = data::scan_day_metadata(&PathBuf::from(&args.data_dir))
        .context("Failed to scan day metadata")?;

    if day_metas.is_empty() {
        bail!("No Parquet files found in {}", args.data_dir);
    }

    eprintln!(
        "  {} days, {} total rows, {} eval points ({:.1}s)",
        day_metas.len(),
        data::total_rows(&day_metas),
        data::total_eval_points(&day_metas),
        scan_start.elapsed().as_secs_f64()
    );

    // ── Phase 2: Generate CPCV splits ───────────────────────────────────
    eprintln!("\n[Phase 2] Generating CPCV splits...");

    let n_days = day_metas.len();
    let dates = data::dates(&day_metas);
    let row_counts = data::row_counts(&day_metas);

    let cpcv_day_metas = build_day_metas(&dates, &row_counts, args.n_groups);

    // For event-level data, purge/embargo is set to 0 because we handle
    // temporal separation at the day level (no intra-day leakage concern
    // since each day is a separate file).
    let cpcv_config = CpcvConfig {
        n_groups: args.n_groups,
        k_test: args.k_test,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let splits = generate_splits(&cpcv_day_metas, &cpcv_config);

    eprintln!("  {} CPCV splits generated", splits.len());

    // ── Phase 3: Run folds ──────────────────────────────────────────────
    eprintln!("\n[Phase 3] Running {} folds...", splits.len());

    let n_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let n_parallel = if args.parallel_folds > 0 {
        args.parallel_folds
    } else {
        1
    };
    let nthread = (n_cpus / n_parallel).max(1) as i32;

    let xgb_params = fold_runner::XgbParams {
        max_depth: args.max_depth,
        eta: args.eta,
        min_child_weight: args.min_child_weight,
        subsample: args.subsample,
        colsample_bytree: args.colsample_bytree,
        n_rounds: args.n_estimators,
        early_stopping: args.early_stopping,
        nthread,
        max_bin: args.max_bin,
    };

    if n_parallel > 1 {
        eprintln!("  {} parallel folds, {} threads/fold", n_parallel, nthread);
    } else {
        eprintln!("  Sequential, {} threads/fold", nthread);
    }

    let fold_results: Vec<fold_runner::FoldResult> = if n_parallel > 1 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n_parallel)
            .build_global()
            .ok();

        let completed = Mutex::new(0usize);
        let n_folds = splits.len();

        let results: Vec<Option<fold_runner::FoldResult>> = splits
            .par_iter()
            .map(|split| {
                let fold_start = Instant::now();
                match fold_runner::run_fold(
                    split,
                    &day_metas,
                    &xgb_params,
                    args.margin,
                    args.commission,
                    args.subsample_pct,
                    args.seed,
                ) {
                    Ok(result) => {
                        let elapsed = fold_start.elapsed();
                        let mut done = completed.lock().unwrap();
                        *done += 1;
                        eprintln!(
                            "  [{}/{}] Groups {{{}}} — {} test rows, exp=${:.2}, trades={} ({:.0}s)",
                            *done,
                            n_folds,
                            result.test_groups.iter()
                                .map(|g| g.to_string())
                                .collect::<Vec<_>>()
                                .join(","),
                            result.n_test,
                            result.metrics.expectancy,
                            result.metrics.total_trades,
                            elapsed.as_secs_f64(),
                        );
                        Some(result)
                    }
                    Err(e) => {
                        let mut done = completed.lock().unwrap();
                        *done += 1;
                        eprintln!("  [{}/{}] FAILED: {}", *done, n_folds, e);
                        None
                    }
                }
            })
            .collect();

        results.into_iter().flatten().collect()
    } else {
        let mut results = Vec::with_capacity(splits.len());
        for split in &splits {
            let fold_start = Instant::now();
            match fold_runner::run_fold(
                split,
                &day_metas,
                &xgb_params,
                args.margin,
                args.commission,
                args.subsample_pct,
                args.seed,
            ) {
                Ok(result) => {
                    let elapsed = fold_start.elapsed();
                    eprintln!(
                        "  [{}/{}] Groups {{{}}} — {} test rows, exp=${:.2}, trades={} ({:.0}s)",
                        result.split_idx + 1,
                        splits.len(),
                        result.test_groups.iter()
                            .map(|g| g.to_string())
                            .collect::<Vec<_>>()
                            .join(","),
                        result.n_test,
                        result.metrics.expectancy,
                        result.metrics.total_trades,
                        elapsed.as_secs_f64(),
                    );
                    results.push(result);
                }
                Err(e) => {
                    eprintln!(
                        "  [{}/{}] FAILED: {}",
                        split.split_idx + 1,
                        splits.len(),
                        e
                    );
                }
            }
        }
        results
    };

    if fold_results.is_empty() {
        bail!("All folds failed");
    }

    // ── Phase 4: Aggregate and report ───────────────────────────────────
    eprintln!("\n[Phase 4] Aggregating results...");

    let report = statistics::aggregate_results(
        &fold_results,
        n_days,
        args.subsample_pct,
        args.margin,
        args.commission,
    );

    statistics::print_report(&report);

    let total_elapsed = total_start.elapsed();
    eprintln!(
        "\n  Total elapsed: {:.1} minutes",
        total_elapsed.as_secs_f64() / 60.0
    );

    // Write JSON output
    std::fs::create_dir_all(&args.output_dir).context("Failed to create output dir")?;
    let json_path = format!("{}/cpcv-report.json", args.output_dir);
    let json = serde_json::to_string_pretty(&report)
        .context("Failed to serialize report")?;
    std::fs::write(&json_path, &json).context("Failed to write JSON report")?;
    eprintln!("  Report written to {}", json_path);

    // Upload to S3 if requested
    if let Some(ref s3_path) = args.s3_output {
        eprintln!("  Uploading to {}...", s3_path);
        let status = std::process::Command::new("aws")
            .args(["s3", "cp", &json_path, s3_path])
            .status()
            .context("Failed to run aws s3 cp")?;
        if status.success() {
            eprintln!("  Upload complete.");
        } else {
            eprintln!("  WARNING: S3 upload failed (exit code {:?})", status.code());
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Imbalance Mode (OFI-filtered CPCV — small dataset)
// ---------------------------------------------------------------------------

fn run_imbalance(args: &Args) -> Result<()> {
    let total_start = Instant::now();

    let target_geometry = if let Some(ref g) = args.geometry {
        let parts: Vec<&str> = g.split(':').collect();
        if parts.len() != 2 {
            bail!("Invalid --geometry format '{}', expected T:S (e.g. 10:5)", g);
        }
        let t: i32 = parts[0].parse().context("Invalid target ticks in --geometry")?;
        let s: i32 = parts[1].parse().context("Invalid stop ticks in --geometry")?;
        Some((t, s))
    } else {
        None
    };

    eprintln!("event-backtest (imbalance mode)");
    eprintln!("  data_dir:        {}", args.data_dir);
    eprintln!("  output_dir:      {}", args.output_dir);
    eprintln!("  ofi_threshold:   {}", args.ofi_threshold);
    eprintln!(
        "  geometry:        {}",
        target_geometry
            .map(|(t, s)| format!("{}:{}", t, s))
            .unwrap_or_else(|| "all".to_string())
    );
    eprintln!("  n_groups:        {}", args.n_groups);
    eprintln!("  k_test:          {}", args.k_test);
    eprintln!("  margin:          {}", args.margin);
    eprintln!("  parallel_folds:  {}", args.parallel_folds);

    // ── Phase 1: Scan day metadata ──────────────────────────────────────
    eprintln!("\n[Phase 1] Scanning day metadata...");
    let scan_start = Instant::now();

    let day_metas = data::scan_day_metadata(&PathBuf::from(&args.data_dir))
        .context("Failed to scan day metadata")?;

    if day_metas.is_empty() {
        bail!("No Parquet files found in {}", args.data_dir);
    }

    eprintln!(
        "  {} days, {} total rows ({:.1}s)",
        day_metas.len(),
        data::total_rows(&day_metas),
        scan_start.elapsed().as_secs_f64()
    );

    // ── Phase 2: Generate CPCV splits ───────────────────────────────────
    eprintln!("\n[Phase 2] Generating CPCV splits...");

    let n_days = day_metas.len();
    let dates = data::dates(&day_metas);
    let row_counts = data::row_counts(&day_metas);

    let cpcv_day_metas = build_day_metas(&dates, &row_counts, args.n_groups);
    let cpcv_config = CpcvConfig {
        n_groups: args.n_groups,
        k_test: args.k_test,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let splits = generate_splits(&cpcv_day_metas, &cpcv_config);

    eprintln!("  {} CPCV splits generated", splits.len());

    // ── Phase 3: Run folds ──────────────────────────────────────────────
    eprintln!("\n[Phase 3] Running {} imbalance folds...", splits.len());

    let n_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let n_parallel = if args.parallel_folds > 0 {
        args.parallel_folds
    } else {
        1
    };
    let nthread = (n_cpus / n_parallel).max(1) as i32;

    let xgb_params = fold_runner::XgbParams {
        max_depth: args.max_depth,
        eta: args.eta,
        min_child_weight: args.min_child_weight,
        subsample: args.subsample,
        colsample_bytree: args.colsample_bytree,
        n_rounds: args.n_estimators,
        early_stopping: args.early_stopping,
        nthread,
        max_bin: args.max_bin,
    };

    if n_parallel > 1 {
        eprintln!("  {} parallel folds, {} threads/fold", n_parallel, nthread);
    } else {
        eprintln!("  Sequential, {} threads/fold", nthread);
    }

    let fold_results: Vec<fold_runner::FoldResult> = if n_parallel > 1 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n_parallel)
            .build_global()
            .ok();

        let completed = Mutex::new(0usize);
        let n_folds = splits.len();

        let results: Vec<Option<fold_runner::FoldResult>> = splits
            .par_iter()
            .map(|split| {
                let fold_start = Instant::now();
                match fold_runner::run_imbalance_fold(
                    split,
                    &day_metas,
                    &xgb_params,
                    args.margin,
                    args.commission,
                    args.ofi_threshold,
                    target_geometry,
                ) {
                    Ok(result) => {
                        let elapsed = fold_start.elapsed();
                        let mut done = completed.lock().unwrap();
                        *done += 1;
                        eprintln!(
                            "  [{}/{}] Groups {{{}}} — {} test rows, exp=${:.2}, trades={} ({:.0}s)",
                            *done,
                            n_folds,
                            result
                                .test_groups
                                .iter()
                                .map(|g| g.to_string())
                                .collect::<Vec<_>>()
                                .join(","),
                            result.n_test,
                            result.metrics.expectancy,
                            result.metrics.total_trades,
                            elapsed.as_secs_f64(),
                        );
                        Some(result)
                    }
                    Err(e) => {
                        let mut done = completed.lock().unwrap();
                        *done += 1;
                        eprintln!("  [{}/{}] FAILED: {}", *done, n_folds, e);
                        None
                    }
                }
            })
            .collect();

        results.into_iter().flatten().collect()
    } else {
        let mut results = Vec::with_capacity(splits.len());
        for split in &splits {
            let fold_start = Instant::now();
            match fold_runner::run_imbalance_fold(
                split,
                &day_metas,
                &xgb_params,
                args.margin,
                args.commission,
                args.ofi_threshold,
                target_geometry,
            ) {
                Ok(result) => {
                    let elapsed = fold_start.elapsed();
                    eprintln!(
                        "  [{}/{}] Groups {{{}}} — {} test rows, exp=${:.2}, trades={} ({:.0}s)",
                        result.split_idx + 1,
                        splits.len(),
                        result
                            .test_groups
                            .iter()
                            .map(|g| g.to_string())
                            .collect::<Vec<_>>()
                            .join(","),
                        result.n_test,
                        result.metrics.expectancy,
                        result.metrics.total_trades,
                        elapsed.as_secs_f64(),
                    );
                    results.push(result);
                }
                Err(e) => {
                    eprintln!(
                        "  [{}/{}] FAILED: {}",
                        split.split_idx + 1,
                        splits.len(),
                        e
                    );
                }
            }
        }
        results
    };

    if fold_results.is_empty() {
        bail!("All folds failed");
    }

    // ── Phase 4: Aggregate and report ───────────────────────────────────
    eprintln!("\n[Phase 4] Aggregating results...");

    let report = statistics::aggregate_results(
        &fold_results,
        n_days,
        0, // no subsampling in imbalance mode
        args.margin,
        args.commission,
    );

    statistics::print_report(&report);

    let total_elapsed = total_start.elapsed();
    eprintln!(
        "\n  Total elapsed: {:.1} minutes",
        total_elapsed.as_secs_f64() / 60.0
    );

    // Write JSON output
    std::fs::create_dir_all(&args.output_dir).context("Failed to create output dir")?;
    let json_path = format!("{}/imbalance-cpcv-report.json", args.output_dir);
    let json =
        serde_json::to_string_pretty(&report).context("Failed to serialize report")?;
    std::fs::write(&json_path, &json).context("Failed to write JSON report")?;
    eprintln!("  Report written to {}", json_path);

    // Upload to S3 if requested
    if let Some(ref s3_path) = args.s3_output {
        eprintln!("  Uploading to {}...", s3_path);
        let status = std::process::Command::new("aws")
            .args(["s3", "cp", &json_path, s3_path])
            .status()
            .context("Failed to run aws s3 cp")?;
        if status.success() {
            eprintln!("  Upload complete.");
        } else {
            eprintln!(
                "  WARNING: S3 upload failed (exit code {:?})",
                status.code()
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Baseline Mode (streaming counters — no XGBoost)
// ---------------------------------------------------------------------------

/// A single row from the event Parquet (used in baseline mode only).
#[derive(Debug, Clone)]
struct EventRow {
    target_ticks: i32,
    stop_ticks: i32,
    outcome: i8,
}

fn run_baseline(args: &Args) -> Result<()> {
    eprintln!("event-backtest (baseline mode)");
    eprintln!("  data_dir:      {}", args.data_dir);
    eprintln!("  output_dir:    {}", args.output_dir);
    eprintln!("  n_groups:      {}", args.n_groups);
    eprintln!("  k_test:        {}", args.k_test);
    eprintln!("  margin:        {}", args.margin);

    // ── Step 1: Discover and load Parquet files ─────────────────────────
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

    let mut all_rows: Vec<EventRow> = Vec::new();
    let mut day_boundaries: Vec<(usize, usize)> = Vec::new();
    let mut skipped_files = 0usize;

    for path in &parquet_files {
        let start = all_rows.len();
        let file = File::open(path).context("Failed to open Parquet file")?;
        let reader = match ParquetRecordBatchReaderBuilder::try_new(file) {
            Ok(builder) => match builder.build() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "  SKIP {} (corrupt: {})",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        e
                    );
                    skipped_files += 1;
                    continue;
                }
            },
            Err(e) => {
                eprintln!(
                    "  SKIP {} (corrupt: {})",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    e
                );
                skipped_files += 1;
                continue;
            }
        };

        for batch_result in reader {
            let batch = batch_result.context("Failed to read record batch")?;
            let n = batch.num_rows();

            let target_col = batch
                .column_by_name("target_ticks")
                .context("Missing target_ticks")?
                .as_any()
                .downcast_ref::<Int32Array>()
                .context("target_ticks not Int32")?;
            let stop_col = batch
                .column_by_name("stop_ticks")
                .context("Missing stop_ticks")?
                .as_any()
                .downcast_ref::<Int32Array>()
                .context("stop_ticks not Int32")?;
            let outcome_col = batch
                .column_by_name("outcome")
                .context("Missing outcome")?
                .as_any()
                .downcast_ref::<Int8Array>()
                .context("outcome not Int8")?;

            for row_idx in 0..n {
                all_rows.push(EventRow {
                    target_ticks: target_col.value(row_idx),
                    stop_ticks: stop_col.value(row_idx),
                    outcome: outcome_col.value(row_idx),
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

    // ── Step 2: CPCV split generation ───────────────────────────────────
    eprintln!("[2/4] Generating CPCV splits...");

    let n_days = day_boundaries.len();
    let _groups = backtest::cpcv::assign_groups(n_days, args.n_groups);

    let row_counts: Vec<usize> = day_boundaries.iter().map(|(s, e)| e - s).collect();
    let dates: Vec<i32> = (0..n_days as i32).collect();

    let day_metas = backtest::cpcv::build_day_metas(&dates, &row_counts, args.n_groups);
    let cpcv_config = backtest::cpcv::CpcvConfig {
        n_groups: args.n_groups,
        k_test: args.k_test,
        purge_bars: 0,
        embargo_bars: 0,
    };
    let splits = backtest::cpcv::generate_splits(&day_metas, &cpcv_config);

    eprintln!("  {} CPCV splits generated", splits.len());

    // ── Step 3: Baseline analysis ───────────────────────────────────────
    eprintln!("[3/4] Computing baseline analysis...");

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

    // ── Step 4: Write results ───────────────────────────────────────────
    eprintln!("[4/4] Writing results...");
    std::fs::create_dir_all(&args.output_dir).context("Failed to create output dir")?;

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
    let config = serde_json::json!({
        "n_groups": args.n_groups,
        "k_test": args.k_test,
        "n_days": n_days,
        "total_rows": all_rows.len(),
        "margin": args.margin,
        "max_depth": args.max_depth,
        "eta": args.eta,
    });

    let config_path = format!("{}/config.json", args.output_dir);
    let config_json = serde_json::to_string_pretty(&config)?;
    std::fs::write(&config_path, &config_json)?;

    eprintln!("\nDone. Results in {}", args.output_dir);
    eprintln!("  Next: Run full CPCV training with --mode cpcv");

    Ok(())
}
