//! CPCV Backtest Pipeline — Combinatorial Purged Cross-Validation with XGBoost.
//!
//! Orchestrates: Parquet loading → CPCV fold generation → per-fold XGBoost training →
//! OOS backtest → aggregated metrics.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use rayon::prelude::*;

use backtest::{CpcvConfig, CpcvSplit, build_day_metas, generate_splits};

use cpcv_backtest::DayData;
use cpcv_backtest::fold_runner::{self, FoldResult};
use cpcv_backtest::statistics;

#[derive(Parser)]
#[command(name = "cpcv-backtest", about = "CPCV Backtest with XGBoost")]
struct Args {
    /// Directory containing pre-computed Parquet feature files (YYYY-MM-DD.parquet).
    #[arg(long)]
    features_dir: PathBuf,

    /// Target ticks for triple-barrier label.
    #[arg(long, default_value_t = 19)]
    target: i32,

    /// Stop ticks for triple-barrier label.
    #[arg(long, default_value_t = 7)]
    stop: i32,

    /// Number of CPCV groups.
    #[arg(long, default_value_t = 10)]
    n_groups: usize,

    /// Number of test groups per split.
    #[arg(long, default_value_t = 2)]
    k_test: usize,

    /// Purge bars before test boundary.
    #[arg(long, default_value_t = 500)]
    purge_bars: usize,

    /// Embargo bars after test boundary.
    #[arg(long, default_value_t = 4600)]
    embargo_bars: usize,

    /// Number of dev days (first N days).
    #[arg(long, default_value_t = 201)]
    n_dev_days: usize,

    /// Forward return horizon in bars.
    #[arg(long, default_value_t = 720)]
    fwd_horizon: usize,

    /// JSON output path.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Use all available days (ignore --n-dev-days for split).
    #[arg(long)]
    all_days: bool,

    /// Number of folds to run in parallel (0 = sequential with all cores per fold).
    #[arg(long, default_value_t = 0)]
    parallel_folds: usize,

    /// Tick size for barrier simulation (default: 0.25 for ES).
    #[arg(long, default_value_t = 0.25)]
    tick_size: f64,

    /// Temporal holdout: reserve last N days as strict out-of-sample test.
    /// Disables CPCV; trains on first (total-N) days, tests on last N.
    #[arg(long)]
    temporal_holdout: Option<usize>,

    /// Directory containing tick-level mid-price series ({date}-ticks.parquet).
    /// When provided, serial execution uses tick-level barrier re-simulation.
    #[arg(long)]
    tick_series_dir: Option<PathBuf>,

    /// Override target ticks for serial execution only (labels still use --target).
    #[arg(long)]
    serial_target: Option<i32>,

    /// Override stop ticks for serial execution only (labels still use --stop).
    #[arg(long)]
    serial_stop: Option<i32>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let total_start = Instant::now();

    // ── 1. Scan Parquet directory ────────────────────────────────────────
    println!("Scanning features directory: {:?}", args.features_dir);
    let available_days = cpcv_backtest::scan_parquet_dir(&args.features_dir)
        .context("Failed to scan features directory")?;

    println!("Found {} Parquet day files", available_days.len());

    let n_dev = if args.all_days {
        available_days.len()
    } else {
        args.n_dev_days.min(available_days.len())
    };
    let dev_days = &available_days[..n_dev];
    let n_holdout = available_days.len().saturating_sub(n_dev);

    // ── 2. Load dev days sequentially ───────────────────────────────────
    // Each Parquet file is ~200KB ZSTD compressed — no OOM risk.
    println!("Loading {} dev days from Parquet...", n_dev);
    let load_start = Instant::now();

    let mut all_days: Vec<DayData> = Vec::with_capacity(n_dev);
    let mut load_failures = 0;

    for (i, (date, path)) in dev_days.iter().enumerate() {
        match cpcv_backtest::load_day_from_parquet(path, *date) {
            Ok(day) => {
                if (i + 1) % 25 == 0 || i + 1 == n_dev {
                    println!(
                        "  [{}/{}] {} — {} bars ({} after filtering)",
                        i + 1,
                        n_dev,
                        date,
                        day.n_bars,
                        day.features.len(),
                    );
                }
                all_days.push(day);
            }
            Err(e) => {
                eprintln!("  Warning: date {} — {}", date, e);
                load_failures += 1;
            }
        }
    }

    let load_elapsed = load_start.elapsed();
    println!(
        "Loaded {} days in {:.1}s ({} failures)",
        all_days.len(),
        load_elapsed.as_secs_f64(),
        load_failures
    );

    if all_days.is_empty() {
        anyhow::bail!("No days loaded successfully");
    }

    // ── 2b. Load tick series if provided ─────────────────────────────────
    if let Some(ref tick_dir) = args.tick_series_dir {
        println!("Loading tick series from {:?}...", tick_dir);
        let tick_map = cpcv_backtest::scan_tick_series_dir(tick_dir)
            .context("Failed to scan tick series directory")?;
        println!("  Found {} tick series files", tick_map.len());

        let mut loaded = 0;
        for day in all_days.iter_mut() {
            if let Some(tick_path) = tick_map.get(&day.date) {
                match cpcv_backtest::load_tick_series(tick_path) {
                    Ok(ticks) => {
                        loaded += 1;
                        day.tick_mids = ticks;
                    }
                    Err(e) => {
                        eprintln!("  Warning: date {} tick series — {}", day.date, e);
                    }
                }
            }
        }
        println!("  Attached tick series to {} of {} days", loaded, all_days.len());
    }

    // ── 3. Branch: Temporal Holdout vs CPCV ────────────────────────────────
    if let Some(holdout_days) = args.temporal_holdout {
        return run_temporal_holdout(&args, &all_days, holdout_days, total_start);
    }

    // ── 3b. Compute DayMeta and generate CPCV splits ────────────────────
    let dates: Vec<i32> = all_days.iter().map(|d| d.date).collect();
    let bar_counts: Vec<usize> = all_days.iter().map(|d| d.n_bars).collect();
    let day_metas = build_day_metas(&dates, &bar_counts, args.n_groups);

    let cpcv_config = CpcvConfig {
        n_groups: args.n_groups,
        k_test: args.k_test,
        purge_bars: args.purge_bars,
        embargo_bars: args.embargo_bars,
    };
    let splits = generate_splits(&day_metas, &cpcv_config);

    println!();
    println!("=======================================");
    println!("  CPCV Backtest ({} folds)", splits.len());
    println!("=======================================");
    println!("  Target: {} ticks | Stop: {} ticks", args.target, args.stop);
    println!("  Dev days: {} | Holdout: {}", all_days.len(), n_holdout);
    println!("  Groups: {} | Test groups/split: {}", args.n_groups, args.k_test);
    println!("  Purge: {} bars | Embargo: {} bars", args.purge_bars, args.embargo_bars);
    println!("  Fwd horizon: {} bars", args.fwd_horizon);
    println!();

    // ── 4. Run folds ───────────────────────────────────────────────────────
    let n_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let n_parallel = if args.parallel_folds > 0 {
        args.parallel_folds
    } else {
        1
    };
    let nthread = (n_cpus / n_parallel).max(1) as i32;

    if n_parallel > 1 {
        println!("  Running {} folds in parallel ({} threads/fold)", n_parallel, nthread);
    }
    println!();

    let fold_results: Vec<FoldResult> = if n_parallel > 1 {
        // Parallel fold execution with rayon
        rayon::ThreadPoolBuilder::new()
            .num_threads(n_parallel)
            .build_global()
            .ok(); // ignore if already initialized

        let completed = Mutex::new(0usize);
        let n_folds = splits.len();

        let results: Vec<Option<FoldResult>> = splits
            .par_iter()
            .map(|split| {
                let fold_start = Instant::now();
                match fold_runner::run_fold(split, &all_days, args.target, args.stop, args.tick_size, nthread, args.serial_target, args.serial_stop) {
                    Ok(result) => {
                        let elapsed = fold_start.elapsed();
                        let mut done = completed.lock().unwrap();
                        *done += 1;
                        println!(
                            "  [{}/{}] Groups {{{}}} ... {} test bars, exp=${:.2}, Sharpe={:.2} ({:.1}s)",
                            *done,
                            n_folds,
                            result.test_groups.iter().map(|g| g.to_string()).collect::<Vec<_>>().join(","),
                            result.n_test_bars,
                            result.metrics_base.expectancy,
                            result.metrics_base.annualized_sharpe,
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
        // Sequential: each fold gets all cores
        let mut results = Vec::with_capacity(splits.len());
        for split in &splits {
            let fold_start = Instant::now();
            match fold_runner::run_fold(split, &all_days, args.target, args.stop, args.tick_size, nthread, args.serial_target, args.serial_stop) {
                Ok(result) => {
                    let elapsed = fold_start.elapsed();
                    println!(
                        "  [{}/{}] Groups {{{}}} ... {} test bars, exp=${:.2}, Sharpe={:.2} ({:.1}s)",
                        result.split_idx + 1,
                        splits.len(),
                        result.test_groups.iter().map(|g| g.to_string()).collect::<Vec<_>>().join(","),
                        result.n_test_bars,
                        result.metrics_base.expectancy,
                        result.metrics_base.annualized_sharpe,
                        elapsed.as_secs_f64(),
                    );
                    results.push(result);
                }
                Err(e) => {
                    eprintln!("  [{}/{}] FAILED: {}", split.split_idx + 1, splits.len(), e);
                }
            }
        }
        results
    };

    if fold_results.is_empty() {
        anyhow::bail!("All folds failed");
    }

    // ── 5. Aggregate and report ───────────────────────────────────────────
    let report = statistics::aggregate_results(
        &fold_results,
        all_days.len(),
        args.target,
        args.stop,
    );

    statistics::print_report(&report);

    let total_elapsed = total_start.elapsed();
    println!();
    println!(
        "  Total elapsed: {:.1} minutes",
        total_elapsed.as_secs_f64() / 60.0
    );

    // ── 6. Write JSON output ──────────────────────────────────────────────
    if let Some(output_path) = &args.output {
        let json = serde_json::to_string_pretty(&report)
            .context("Failed to serialize report")?;
        std::fs::write(output_path, json)
            .with_context(|| format!("Failed to write output to {:?}", output_path))?;
        println!("  Report written to {:?}", output_path);
    }

    Ok(())
}

/// Temporal holdout: train on first (total-N) days, test on last N days.
/// Single train/test split — no CPCV fold generation.
fn run_temporal_holdout(
    args: &Args,
    all_days: &[DayData],
    holdout_days: usize,
    total_start: Instant,
) -> Result<()> {
    let total = all_days.len();
    if holdout_days >= total {
        anyhow::bail!(
            "temporal_holdout={} >= total days={}; need at least 1 train day",
            holdout_days,
            total
        );
    }

    let train_end = total - holdout_days;
    let train_day_indices: Vec<usize> = (0..train_end).collect();
    let test_day_indices: Vec<usize> = (train_end..total).collect();

    println!();
    println!("=======================================");
    println!("  Temporal Holdout");
    println!("=======================================");
    println!("  Target: {} ticks | Stop: {} ticks", args.target, args.stop);
    println!("  Train days: {} | Test days: {}", train_end, holdout_days);
    println!(
        "  Train period: {} — {}",
        all_days[0].date,
        all_days[train_end - 1].date
    );
    println!(
        "  Test  period: {} — {}",
        all_days[train_end].date,
        all_days[total - 1].date
    );
    println!();

    // Build a synthetic CpcvSplit for a single train/test split
    let split = CpcvSplit {
        split_idx: 0,
        test_groups: vec![0], // placeholder
        train_groups: vec![1], // placeholder
        train_day_indices,
        test_day_indices,
    };

    let n_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as i32;

    let fold_start = Instant::now();
    let result = fold_runner::run_fold(
        &split,
        all_days,
        args.target,
        args.stop,
        args.tick_size,
        n_cpus,
        args.serial_target,
        args.serial_stop,
    )
    .context("Temporal holdout fold failed")?;

    let fold_elapsed = fold_start.elapsed();
    println!(
        "  Fold completed: {} train bars, {} test bars ({:.1}s)",
        result.n_train_bars,
        result.n_test_bars,
        fold_elapsed.as_secs_f64(),
    );

    // Report as single-fold aggregation
    let fold_results = vec![result];
    let report = statistics::aggregate_results(
        &fold_results,
        total,
        args.target,
        args.stop,
    );

    statistics::print_report(&report);

    let total_elapsed = total_start.elapsed();
    println!();
    println!(
        "  Total elapsed: {:.1} minutes",
        total_elapsed.as_secs_f64() / 60.0
    );

    if let Some(output_path) = &args.output {
        let json = serde_json::to_string_pretty(&report)
            .context("Failed to serialize report")?;
        std::fs::write(output_path, json)
            .with_context(|| format!("Failed to write output to {:?}", output_path))?;
        println!("  Report written to {:?}", output_path);
    }

    Ok(())
}
