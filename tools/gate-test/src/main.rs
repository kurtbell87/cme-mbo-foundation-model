//! Gate test: univariate signal strength analysis for flow features.
//!
//! Streams through event Parquet files computing per-geometry statistics
//! for OFI, cancel asymmetry, and trade flow signals vs barrier outcomes.
//! Stratified by time-of-day, spread regime, and BBO change cause.
//! Includes threshold rule PnL analysis for OFI.
//!
//! Memory: O(geometries × accumulators) — ~50KB total. Processes 220M+ rows
//! in a single streaming pass with no data collection.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use arrow::array::{Float32Array, Int8Array, Int32Array, UInt64Array};
use clap::Parser;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Serialize;

use common::time_utils;

// ── Constants ──────────────────────────────────────────────────

/// Signal columns to test (read from Parquet by name).
const SIGNAL_COLS: [&str; 6] = [
    "ofi_fast",
    "ofi_med",
    "ofi_slow",
    "ofi_norm_fast",
    "cancel_asym_fast",
    "trade_flow_fast",
];
const N_SIG: usize = SIGNAL_COLS.len();

/// Time-of-day bucket boundaries (fraction of 6.5-hour RTH session).
/// morning: 9:30–11:00, midday: 11:00–13:00, afternoon: 13:00–15:00, close: 15:00–16:00
const TIME_BOUNDS: [f64; 3] = [
    1.5 / 6.5,  // 11:00 → 0.231
    3.5 / 6.5,  // 13:00 → 0.538
    5.5 / 6.5,  // 15:00 → 0.846
];
const TIME_NAMES: [&str; 4] = ["morning", "midday", "afternoon", "close"];
const N_TIME: usize = 4;

/// OFI thresholds for threshold rule (EMA-decayed contracts).
const THRESHOLDS: [f32; 9] = [-10.0, -5.0, -2.0, -1.0, 0.0, 1.0, 2.0, 5.0, 10.0];
const N_THRESH: usize = THRESHOLDS.len();

/// RTH duration in nanoseconds (6.5 hours).
const RTH_NS: f64 = 23_400_000_000_000.0;

/// BBO cause buckets: aggressive_trade(1), cancel(2), other(0,3,4,5).
const CAUSE_NAMES: [&str; 3] = ["aggressive_trade", "cancel", "other"];
const N_CAUSE: usize = 3;

/// Spread regime: tight(0) vs wide(1).
const SPREAD_NAMES: [&str; 2] = ["tight", "wide"];
const N_SPREAD: usize = 2;

// ── CLI ────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "gate-test")]
#[command(about = "Univariate gate test: flow signals vs barrier outcomes")]
struct Args {
    /// Directory containing event Parquet files
    #[arg(long)]
    data_dir: String,

    /// Output JSON report path
    #[arg(long, default_value = "gate-test-report.json")]
    output: String,

    /// Tick size for spread regime split
    #[arg(long, default_value = "0.25")]
    tick_size: f32,
}

// ── Accumulators ───────────────────────────────────────────────

#[derive(Default, Clone, Copy)]
struct SigAccum {
    n_pos_target: u64,
    n_pos_stop: u64,
    n_neg_target: u64,
    n_neg_stop: u64,
    sum_target: f64,
    sum_stop: f64,
}

impl SigAccum {
    #[inline]
    fn observe(&mut self, positive: bool, is_target: bool, val: f64) {
        match (positive, is_target) {
            (true, true) => {
                self.n_pos_target += 1;
                self.sum_target += val;
            }
            (true, false) => {
                self.n_pos_stop += 1;
                self.sum_stop += val;
            }
            (false, true) => {
                self.n_neg_target += 1;
                self.sum_target += val;
            }
            (false, false) => {
                self.n_neg_stop += 1;
                self.sum_stop += val;
            }
        }
    }

    fn n_pos(&self) -> u64 {
        self.n_pos_target + self.n_pos_stop
    }
    fn n_neg(&self) -> u64 {
        self.n_neg_target + self.n_neg_stop
    }
    fn n_target(&self) -> u64 {
        self.n_pos_target + self.n_neg_target
    }
    fn n(&self) -> u64 {
        self.n_pos_target + self.n_pos_stop + self.n_neg_target + self.n_neg_stop
    }
}

#[derive(Default, Clone, Copy)]
struct ThreshAccum {
    n_above: u64,
    n_above_target: u64,
    sum_pnl_above: f64,
}

/// Per-geometry accumulator. Fixed-size, no heap allocations.
struct GeomAccum {
    target_ticks: i32,
    stop_ticks: i32,
    n: u64,
    n_target: u64,
    /// Overall per-signal.
    signals: [SigAccum; N_SIG],
    /// Stratified by time-of-day.
    time_sig: [[SigAccum; N_SIG]; N_TIME],
    /// Stratified by spread regime.
    spread_sig: [[SigAccum; N_SIG]; N_SPREAD],
    /// Stratified by BBO change cause.
    cause_sig: [[SigAccum; N_SIG]; N_CAUSE],
    /// Threshold rule for ofi_fast.
    thresholds: [ThreshAccum; N_THRESH],
}

impl GeomAccum {
    fn new(target_ticks: i32, stop_ticks: i32) -> Self {
        Self {
            target_ticks,
            stop_ticks,
            n: 0,
            n_target: 0,
            signals: [SigAccum::default(); N_SIG],
            time_sig: [[SigAccum::default(); N_SIG]; N_TIME],
            spread_sig: [[SigAccum::default(); N_SIG]; N_SPREAD],
            cause_sig: [[SigAccum::default(); N_SIG]; N_CAUSE],
            thresholds: [ThreshAccum::default(); N_THRESH],
        }
    }

    #[inline]
    fn observe(
        &mut self,
        is_target: bool,
        pnl_ticks: f32,
        sig_vals: &[f32; N_SIG],
        time_bucket: usize,
        spread_bucket: usize,
        cause_bucket: usize,
    ) {
        self.n += 1;
        if is_target {
            self.n_target += 1;
        }

        for i in 0..N_SIG {
            let v = sig_vals[i];
            let pos = v > 0.0;
            let vf = v as f64;
            self.signals[i].observe(pos, is_target, vf);
            self.time_sig[time_bucket][i].observe(pos, is_target, vf);
            self.spread_sig[spread_bucket][i].observe(pos, is_target, vf);
            self.cause_sig[cause_bucket][i].observe(pos, is_target, vf);
        }

        // Threshold rule on ofi_fast (index 0)
        let ofi = sig_vals[0];
        for (ti, &thresh) in THRESHOLDS.iter().enumerate() {
            if ofi > thresh {
                self.thresholds[ti].n_above += 1;
                if is_target {
                    self.thresholds[ti].n_above_target += 1;
                }
                self.thresholds[ti].sum_pnl_above += pnl_ticks as f64;
            }
        }
    }

    fn null_p(&self) -> f64 {
        let s = self.stop_ticks as f64;
        let t = self.target_ticks as f64;
        s / (t + s)
    }
}

// ── File processing ────────────────────────────────────────────

fn scan_parquet_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).context("Failed to read data directory")? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("parquet") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn parse_date_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let date_part = stem.strip_suffix("-events")?;
    if date_part.len() == 8 {
        Some(date_part.to_string())
    } else {
        None
    }
}

fn time_bucket(ts: u64, rth_open: u64) -> usize {
    let frac = (ts.saturating_sub(rth_open)) as f64 / RTH_NS;
    if frac < TIME_BOUNDS[0] {
        0
    } else if frac < TIME_BOUNDS[1] {
        1
    } else if frac < TIME_BOUNDS[2] {
        2
    } else {
        3
    }
}

fn cause_bucket(cause_f32: f32) -> usize {
    let c = cause_f32 as u8;
    match c {
        1 => 0, // AggressiveTrade
        2 => 1, // Cancel
        _ => 2, // Other (None=0, NewLevel=3, Modify=4, Multiple=5)
    }
}

fn process_file(
    path: &Path,
    tick_size: f32,
    accums: &mut BTreeMap<(i32, i32), GeomAccum>,
) -> Result<u64> {
    let date_str = parse_date_from_path(path)
        .with_context(|| format!("Cannot parse date from {:?}", path))?;
    let rth_open = time_utils::rth_open_for_date(&date_str);

    let file = File::open(path).with_context(|| format!("Cannot open {:?}", path))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("Cannot read {:?}", path))?;
    let reader = builder.build().context("Cannot build reader")?;

    let mut row_count: u64 = 0;

    for batch_result in reader {
        let batch = batch_result.context("Failed to read batch")?;
        let n = batch.num_rows();

        // Extract required columns
        let ts_col = batch
            .column_by_name("timestamp_ns")
            .context("Missing timestamp_ns")?
            .as_any()
            .downcast_ref::<UInt64Array>()
            .context("timestamp_ns not UInt64")?;
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
        let pnl_col = batch
            .column_by_name("pnl_ticks")
            .context("Missing pnl_ticks")?
            .as_any()
            .downcast_ref::<Float32Array>()
            .context("pnl_ticks not Float32")?;
        let spread_col = batch
            .column_by_name("spread")
            .context("Missing spread")?
            .as_any()
            .downcast_ref::<Float32Array>()
            .context("spread not Float32")?;
        let cause_col = batch
            .column_by_name("bbo_change_cause")
            .context("Missing bbo_change_cause")?
            .as_any()
            .downcast_ref::<Float32Array>()
            .context("bbo_change_cause not Float32")?;

        // Signal columns
        let mut sig_arrays: [Option<&Float32Array>; N_SIG] = [None; N_SIG];
        for (i, name) in SIGNAL_COLS.iter().enumerate() {
            sig_arrays[i] = Some(
                batch
                    .column_by_name(name)
                    .with_context(|| format!("Missing {}", name))?
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .with_context(|| format!("{} not Float32", name))?,
            );
        }

        for row in 0..n {
            let outcome = outcome_col.value(row);
            // Skip horizon outcomes
            if outcome == -1 {
                continue;
            }

            let is_target = outcome == 1;
            let target_ticks = target_col.value(row);
            let stop_ticks = stop_col.value(row);
            let pnl = pnl_col.value(row);
            let ts = ts_col.value(row);
            let spread = spread_col.value(row);
            let cause = cause_col.value(row);

            let mut sig_vals = [0.0f32; N_SIG];
            for i in 0..N_SIG {
                sig_vals[i] = sig_arrays[i].unwrap().value(row);
            }

            let tb = time_bucket(ts, rth_open);
            let sb = if spread > tick_size { 1 } else { 0 };
            let cb = cause_bucket(cause);

            let key = (target_ticks, stop_ticks);
            let accum = accums
                .entry(key)
                .or_insert_with(|| GeomAccum::new(target_ticks, stop_ticks));
            accum.observe(is_target, pnl, &sig_vals, tb, sb, cb);
            row_count += 1;
        }
    }

    Ok(row_count)
}

// ── Report types (serde) ──────────────────────────────────────

#[derive(Serialize)]
struct Report {
    config: ReportConfig,
    summary: ReportSummary,
    geometries: Vec<GeomReport>,
}

#[derive(Serialize)]
struct ReportConfig {
    data_dir: String,
    tick_size: f32,
    n_files: usize,
    signal_columns: Vec<String>,
}

#[derive(Serialize)]
struct ReportSummary {
    total_rows: u64,
    n_days: usize,
    n_geometries: usize,
}

#[derive(Serialize)]
struct GeomReport {
    target_ticks: i32,
    stop_ticks: i32,
    n: u64,
    n_target: u64,
    null_p: f64,
    actual_p: f64,
    signals: Vec<SignalReport>,
    by_time_of_day: BTreeMap<String, StratumReport>,
    by_spread: BTreeMap<String, StratumReport>,
    by_bbo_cause: BTreeMap<String, StratumReport>,
    threshold_rule: Vec<ThresholdReport>,
}

#[derive(Serialize)]
struct SignalReport {
    name: String,
    n_positive: u64,
    n_negative: u64,
    p_when_positive: f64,
    p_when_negative: f64,
    lift_positive: f64,
    lift_negative: f64,
    mean_target: f64,
    mean_stop: f64,
}

#[derive(Serialize)]
struct StratumReport {
    n: u64,
    n_target: u64,
    actual_p: f64,
    signals: Vec<SignalReport>,
}

#[derive(Serialize)]
struct ThresholdReport {
    threshold: f32,
    n_above: u64,
    n_above_target: u64,
    p_target_above: f64,
    mean_pnl_above: f64,
}

// ── Report generation ─────────────────────────────────────────

fn sig_report(a: &SigAccum, null_p: f64, name: &str) -> SignalReport {
    let p_pos = if a.n_pos() > 0 {
        a.n_pos_target as f64 / a.n_pos() as f64
    } else {
        0.0
    };
    let p_neg = if a.n_neg() > 0 {
        a.n_neg_target as f64 / a.n_neg() as f64
    } else {
        0.0
    };
    let mean_t = if a.n_target() > 0 {
        a.sum_target / a.n_target() as f64
    } else {
        0.0
    };
    let mean_s = if (a.n() - a.n_target()) > 0 {
        a.sum_stop / (a.n() - a.n_target()) as f64
    } else {
        0.0
    };
    SignalReport {
        name: name.to_string(),
        n_positive: a.n_pos(),
        n_negative: a.n_neg(),
        p_when_positive: p_pos,
        p_when_negative: p_neg,
        lift_positive: p_pos - null_p,
        lift_negative: p_neg - null_p,
        mean_target: mean_t,
        mean_stop: mean_s,
    }
}

fn sig_reports(sigs: &[SigAccum; N_SIG], null_p: f64) -> Vec<SignalReport> {
    SIGNAL_COLS
        .iter()
        .enumerate()
        .map(|(i, &name)| sig_report(&sigs[i], null_p, name))
        .collect()
}

fn stratum_report(sigs: &[SigAccum; N_SIG], null_p: f64) -> StratumReport {
    let n: u64 = sigs[0].n(); // all signals see the same rows
    let n_target: u64 = sigs[0].n_target();
    let actual_p = if n > 0 { n_target as f64 / n as f64 } else { 0.0 };
    StratumReport {
        n,
        n_target,
        actual_p,
        signals: sig_reports(sigs, null_p),
    }
}

fn build_report(
    accums: &BTreeMap<(i32, i32), GeomAccum>,
    total_rows: u64,
    n_days: usize,
    args: &Args,
    n_files: usize,
) -> Report {
    let mut geometries = Vec::new();

    for accum in accums.values() {
        let null_p = accum.null_p();
        let actual_p = if accum.n > 0 {
            accum.n_target as f64 / accum.n as f64
        } else {
            0.0
        };

        let mut by_time = BTreeMap::new();
        for t in 0..N_TIME {
            by_time.insert(
                TIME_NAMES[t].to_string(),
                stratum_report(&accum.time_sig[t], null_p),
            );
        }

        let mut by_spread = BTreeMap::new();
        for s in 0..N_SPREAD {
            by_spread.insert(
                SPREAD_NAMES[s].to_string(),
                stratum_report(&accum.spread_sig[s], null_p),
            );
        }

        let mut by_cause = BTreeMap::new();
        for c in 0..N_CAUSE {
            by_cause.insert(
                CAUSE_NAMES[c].to_string(),
                stratum_report(&accum.cause_sig[c], null_p),
            );
        }

        let threshold_rule: Vec<ThresholdReport> = THRESHOLDS
            .iter()
            .enumerate()
            .map(|(ti, &thresh)| {
                let ta = &accum.thresholds[ti];
                ThresholdReport {
                    threshold: thresh,
                    n_above: ta.n_above,
                    n_above_target: ta.n_above_target,
                    p_target_above: if ta.n_above > 0 {
                        ta.n_above_target as f64 / ta.n_above as f64
                    } else {
                        0.0
                    },
                    mean_pnl_above: if ta.n_above > 0 {
                        ta.sum_pnl_above / ta.n_above as f64
                    } else {
                        0.0
                    },
                }
            })
            .collect();

        geometries.push(GeomReport {
            target_ticks: accum.target_ticks,
            stop_ticks: accum.stop_ticks,
            n: accum.n,
            n_target: accum.n_target,
            null_p,
            actual_p,
            signals: sig_reports(&accum.signals, null_p),
            by_time_of_day: by_time,
            by_spread: by_spread,
            by_bbo_cause: by_cause,
            threshold_rule,
        });
    }

    Report {
        config: ReportConfig {
            data_dir: args.data_dir.clone(),
            tick_size: args.tick_size,
            n_files,
            signal_columns: SIGNAL_COLS.iter().map(|s| s.to_string()).collect(),
        },
        summary: ReportSummary {
            total_rows,
            n_days,
            n_geometries: geometries.len(),
        },
        geometries,
    }
}

// ── Main ──────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args = Args::parse();
    let data_dir = Path::new(&args.data_dir);

    eprintln!("gate-test");
    eprintln!("  data_dir:  {}", args.data_dir);
    eprintln!("  output:    {}", args.output);
    eprintln!("  tick_size: {}", args.tick_size);

    // Scan for Parquet files
    let files = scan_parquet_files(data_dir)?;
    eprintln!("  files:     {}", files.len());

    if files.is_empty() {
        anyhow::bail!("No Parquet files found in {}", args.data_dir);
    }

    // Process all files
    let mut accums: BTreeMap<(i32, i32), GeomAccum> = BTreeMap::new();
    let mut total_rows: u64 = 0;
    let mut n_days: usize = 0;

    for (fi, path) in files.iter().enumerate() {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        match process_file(path, args.tick_size, &mut accums) {
            Ok(rows) => {
                total_rows += rows;
                n_days += 1;
                if (fi + 1) % 25 == 0 || fi + 1 == files.len() {
                    eprintln!(
                        "  [{}/{}] {} — {} rows (cumulative: {})",
                        fi + 1,
                        files.len(),
                        stem,
                        rows,
                        total_rows
                    );
                }
            }
            Err(e) => {
                eprintln!("  SKIP {} — {}", stem, e);
            }
        }
    }

    eprintln!(
        "\n  Total: {} rows across {} days, {} geometries",
        total_rows,
        n_days,
        accums.len()
    );

    // Print key results to stderr
    eprintln!("\n══════════════════════════════════════════");
    eprintln!("  Gate Test Results — OFI Sign Split");
    eprintln!("══════════════════════════════════════════");
    for accum in accums.values() {
        let a = &accum.signals[0]; // ofi_fast
        let null_p = accum.null_p();
        let p_pos = if a.n_pos() > 0 {
            a.n_pos_target as f64 / a.n_pos() as f64
        } else {
            0.0
        };
        let p_neg = if a.n_neg() > 0 {
            a.n_neg_target as f64 / a.n_neg() as f64
        } else {
            0.0
        };
        eprintln!(
            "  T={:>2} S={:>2} | null={:.4} | OFI>0: {:.4} (lift {:+.4}) | OFI≤0: {:.4} (lift {:+.4}) | n={}",
            accum.target_ticks,
            accum.stop_ticks,
            null_p,
            p_pos,
            p_pos - null_p,
            p_neg,
            p_neg - null_p,
            accum.n,
        );
    }
    eprintln!("══════════════════════════════════════════");

    // Build and write report
    let report = build_report(&accums, total_rows, n_days, &args, files.len());
    let json = serde_json::to_string_pretty(&report).context("JSON serialization failed")?;
    std::fs::write(&args.output, &json).context("Failed to write report")?;
    eprintln!("\n  Report written to {}", args.output);

    Ok(())
}
