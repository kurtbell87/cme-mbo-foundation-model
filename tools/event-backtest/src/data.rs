//! Parquet loading, eval-point subsampling, and DMatrix assembly.
//!
//! Handles the 927M-row event-level dataset efficiently:
//! - Scan day metadata cheaply (row counts, eval point counts)
//! - Subsample eval points deterministically via hash(timestamp_ns, seed)
//! - Load full days for test sets (no subsampling)
//! - Assemble flat f32 buffers for XGBoost DMatrix construction

use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use arrow::array::{Float32Array, Int8Array, Int32Array, UInt64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use event_features::{LOB_FEATURE_NAMES, NUM_LOB_FEATURES};
use flow_features::{FLOW_FEATURE_NAMES, NUM_FLOW_FEATURES};

/// Total model inputs: 42 LOB + 46 flow + target_ticks + stop_ticks = 90.
pub const NUM_FEATURES: usize = NUM_LOB_FEATURES + NUM_FLOW_FEATURES + 2;

/// Number of geometry rows per eval point (11 T/S combinations).
pub const ROWS_PER_EVAL_POINT: usize = 11;

/// Metadata for one day (cheap to compute, kept in memory).
#[derive(Debug, Clone)]
pub struct DayMeta {
    pub path: PathBuf,
    /// Date in YYYYMMDD format.
    pub date: i32,
    /// Total rows in the Parquet file.
    pub n_rows: usize,
    /// Approximate number of eval points (n_rows / ROWS_PER_EVAL_POINT).
    pub n_eval_points: usize,
}

/// A chunk of loaded event data for one day.
pub struct DayChunk {
    pub date: i32,
    pub n_rows: usize,
    /// 90 model inputs per row: 42 LOB + 46 flow + target_ticks + stop_ticks (as f32).
    pub features: Vec<f32>,
    /// Binary label: 1.0 = target hit, 0.0 = stop hit.
    pub labels: Vec<f32>,
    /// Raw event metadata for serial PnL computation.
    pub events: Vec<EventData>,
}

/// Minimal event data needed for serial PnL computation.
#[derive(Debug, Clone, Copy)]
pub struct EventData {
    pub timestamp_ns: u64,
    pub target_ticks: i32,
    pub stop_ticks: i32,
    pub outcome: i8,
    pub exit_ts: u64,
    pub pnl_ticks: f32,
    /// Trade direction: +1 = long, -1 = short. Defaults to +1 for old Parquets.
    pub direction: i8,
    /// OFI fast signal value. Defaults to 0.0 for old Parquets.
    pub ofi_fast: f32,
}

/// Scan a directory for event Parquet files and return sorted metadata.
///
/// Expects filenames like `YYYY-MM-DD-events.parquet`.
pub fn scan_day_metadata(data_dir: &Path) -> Result<Vec<DayMeta>> {
    let mut entries: Vec<DayMeta> = Vec::new();

    for entry in std::fs::read_dir(data_dir)
        .with_context(|| format!("Failed to read data directory {:?}", data_dir))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("parquet") {
            continue;
        }

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        let date = match parse_event_date_stem(stem) {
            Some(d) => d,
            None => continue,
        };

        // Get row count from Parquet metadata (no data reading).
        let file = File::open(&path)
            .with_context(|| format!("Failed to open {:?}", path))?;
        let builder = match ParquetRecordBatchReaderBuilder::try_new(file) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  SKIP {} (corrupt: {})", stem, e);
                continue;
            }
        };
        let metadata = builder.metadata();
        let n_rows: usize = metadata
            .row_groups()
            .iter()
            .map(|rg| rg.num_rows() as usize)
            .sum();
        let n_eval_points = n_rows / ROWS_PER_EVAL_POINT;

        entries.push(DayMeta {
            path,
            date,
            n_rows,
            n_eval_points,
        });
    }

    entries.sort_by_key(|m| m.date);
    Ok(entries)
}

/// Load one day's Parquet with eval-point subsampling.
///
/// Subsampling: hash(timestamp_ns, seed) % 1000 < (subsample_pct * 10).
/// All ROWS_PER_EVAL_POINT geometry rows for a sampled eval point are included together.
/// Filters out horizon outcomes (outcome == -1).
pub fn load_day_subsampled(
    path: &Path,
    subsample_pct: u32,
    seed: u64,
) -> Result<DayChunk> {
    let threshold = subsample_pct * 10; // e.g. 15% → 150 out of 1000
    load_day_impl(path, Some((threshold, seed)), None)
}

/// Load one day's Parquet with NO subsampling (for test sets).
/// Filters out horizon outcomes (outcome == -1).
pub fn load_day_full(path: &Path) -> Result<DayChunk> {
    load_day_impl(path, None, None)
}

/// Load one day's Parquet filtered to imbalance bar events (streaming).
///
/// Filters inline during batch reading — only rows where |ofi_fast| > threshold
/// (and optionally matching a single geometry) are kept in memory.
/// Peak memory: one Parquet batch (~64K rows) + accumulated output rows.
pub fn load_day_imbalance(
    path: &Path,
    ofi_threshold: f32,
    target_geometry: Option<(i32, i32)>,
) -> Result<DayChunk> {
    let filter = ImbalanceFilter {
        ofi_threshold,
        target_geometry,
    };
    load_day_impl(path, None, Some(filter))
}

/// Imbalance filter config for streaming row-level filtering.
struct ImbalanceFilter {
    ofi_threshold: f32,
    target_geometry: Option<(i32, i32)>,
}

/// Internal loader. If `subsample` is Some((threshold, seed)), only include
/// eval points whose hash falls below threshold. If `imbalance` is Some,
/// only include rows where |ofi_fast| > threshold and geometry matches.
fn load_day_impl(
    path: &Path,
    subsample: Option<(u32, u64)>,
    imbalance: Option<ImbalanceFilter>,
) -> Result<DayChunk> {
    let date = parse_event_date_stem(
        path.file_stem().and_then(|s| s.to_str()).unwrap_or(""),
    )
    .unwrap_or(0);

    let file = File::open(path)
        .with_context(|| format!("Failed to open {:?}", path))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("Failed to create Parquet reader for {:?}", path))?;
    let reader = builder.build()
        .with_context(|| format!("Failed to build Parquet reader for {:?}", path))?;

    let mut features: Vec<f32> = Vec::new();
    let mut labels: Vec<f32> = Vec::new();
    let mut events: Vec<EventData> = Vec::new();

    for batch_result in reader {
        let batch = batch_result.context("Failed to read record batch")?;
        let n = batch.num_rows();

        // Extract columns
        let ts_col = batch.column_by_name("timestamp_ns")
            .context("Missing timestamp_ns")?
            .as_any().downcast_ref::<UInt64Array>()
            .context("timestamp_ns not UInt64")?;
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

        // Optional direction column (absent in pre-bilateral Parquets)
        let direction_col_opt = batch.column_by_name("direction")
            .and_then(|c| c.as_any().downcast_ref::<Int8Array>());

        // Optional ofi_fast column (read from flow features)
        let ofi_fast_col_opt = batch.column_by_name("ofi_fast")
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

        // Read LOB feature columns
        let mut feature_arrays: Vec<&Float32Array> = Vec::with_capacity(NUM_LOB_FEATURES + NUM_FLOW_FEATURES);
        for name in &LOB_FEATURE_NAMES {
            let arr = batch.column_by_name(name)
                .with_context(|| format!("Missing feature column: {}", name))?
                .as_any().downcast_ref::<Float32Array>()
                .with_context(|| format!("Feature {} not Float32", name))?;
            feature_arrays.push(arr);
        }

        // Read flow feature columns
        for name in &FLOW_FEATURE_NAMES {
            let arr = batch.column_by_name(name)
                .with_context(|| format!("Missing flow feature column: {}", name))?
                .as_any().downcast_ref::<Float32Array>()
                .with_context(|| format!("Flow feature {} not Float32", name))?;
            feature_arrays.push(arr);
        }

        for row_idx in 0..n {
            let outcome = outcome_col.value(row_idx);

            // Skip horizon outcomes
            if outcome == -1 {
                continue;
            }

            let ts = ts_col.value(row_idx);

            // Eval-point subsampling: hash the timestamp to decide inclusion.
            // All 11 geometry rows for the same eval point share the same timestamp_ns,
            // so they're included/excluded together.
            if let Some((threshold, seed)) = subsample {
                let h = hash_subsample(ts, seed);
                if h >= threshold {
                    continue;
                }
            }

            let target_ticks = target_col.value(row_idx);
            let stop_ticks = stop_col.value(row_idx);

            // Imbalance filter: skip rows that don't pass OFI threshold / geometry
            if let Some(ref filt) = imbalance {
                let ofi = ofi_fast_col_opt.map_or(0.0, |c| c.value(row_idx));
                if ofi.abs() <= filt.ofi_threshold {
                    continue;
                }
                if let Some((t, s)) = filt.target_geometry {
                    if target_ticks != t || stop_ticks != s {
                        continue;
                    }
                }
            }

            // Append 90 features: 42 LOB + 46 flow + target_ticks + stop_ticks
            for arr in &feature_arrays {
                features.push(arr.value(row_idx));
            }
            features.push(target_ticks as f32);
            features.push(stop_ticks as f32);

            // Binary label: 1 = target hit, 0 = stop hit
            labels.push(if outcome == 1 { 1.0 } else { 0.0 });

            events.push(EventData {
                timestamp_ns: ts,
                target_ticks,
                stop_ticks,
                outcome,
                exit_ts: exit_col.value(row_idx),
                pnl_ticks: pnl_col.value(row_idx),
                direction: direction_col_opt.map_or(1, |c| c.value(row_idx)),
                ofi_fast: ofi_fast_col_opt.map_or(0.0, |c| c.value(row_idx)),
            });
        }
    }

    let n_rows = labels.len();
    Ok(DayChunk {
        date,
        n_rows,
        features,
        labels,
        events,
    })
}

/// Deterministic hash-based subsampling: hash(timestamp_ns, seed) % 1000.
fn hash_subsample(timestamp_ns: u64, seed: u64) -> u32 {
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    timestamp_ns.hash(&mut hasher);
    (hasher.finish() % 1000) as u32
}

/// Parse event Parquet filename stems into YYYYMMDD integer.
///
/// Handles: "YYYYMMDD-events", "YYYY-MM-DD-events", "YYYYMMDD", "YYYY-MM-DD".
fn parse_event_date_stem(s: &str) -> Option<i32> {
    let date_part = if s.ends_with("-events") {
        &s[..s.len() - 7]
    } else {
        s
    };
    match date_part.len() {
        8 => date_part.parse::<i32>().ok().filter(|&d| d >= 19000101 && d <= 20991231),
        10 => date_part.replace('-', "").parse::<i32>().ok(),
        _ => None,
    }
}

/// Assemble a flat f32 feature buffer and label buffer from multiple day chunks.
///
/// Drains features/labels from chunks to avoid doubling memory.
/// After this call, chunks' feature/label vecs are empty.
/// Returns (flat_features, labels). flat_features is row-major with NUM_FEATURES columns.
pub fn assemble_buffers(chunks: &mut [DayChunk]) -> (Vec<f32>, Vec<f32>) {
    let total_rows: usize = chunks.iter().map(|c| c.n_rows).sum();
    let mut features = Vec::with_capacity(total_rows * NUM_FEATURES);
    let mut labels = Vec::with_capacity(total_rows);

    for chunk in chunks.iter_mut() {
        features.append(&mut chunk.features);
        labels.append(&mut chunk.labels);
    }

    (features, labels)
}

/// Collect all EventData from multiple day chunks (for serial PnL).
/// Drains events from chunks to avoid doubling memory.
pub fn collect_events(chunks: &mut [DayChunk]) -> Vec<EventData> {
    let total: usize = chunks.iter().map(|c| c.events.len()).sum();
    let mut events = Vec::with_capacity(total);
    for chunk in chunks.iter_mut() {
        events.append(&mut chunk.events);
    }
    events
}

/// Total row count across day metadata.
pub fn total_rows(metas: &[DayMeta]) -> usize {
    metas.iter().map(|m| m.n_rows).sum()
}

/// Total eval point count across day metadata.
pub fn total_eval_points(metas: &[DayMeta]) -> usize {
    metas.iter().map(|m| m.n_eval_points).sum()
}

/// Build CPCV-compatible row counts from day metadata.
/// Uses n_rows as the "bar count" equivalent for CPCV group assignment.
pub fn row_counts(metas: &[DayMeta]) -> Vec<usize> {
    metas.iter().map(|m| m.n_rows).collect()
}

/// Extract dates from day metadata.
pub fn dates(metas: &[DayMeta]) -> Vec<i32> {
    metas.iter().map(|m| m.date).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_event_date_stem() {
        // YYYY-MM-DD-events (dashed)
        assert_eq!(parse_event_date_stem("2024-01-15-events"), Some(20240115));
        assert_eq!(parse_event_date_stem("2024-12-31-events"), Some(20241231));
        // YYYYMMDD-events (no dashes — actual EC2 export format)
        assert_eq!(parse_event_date_stem("20220102-events"), Some(20220102));
        assert_eq!(parse_event_date_stem("20221230-events"), Some(20221230));
        // Bare date
        assert_eq!(parse_event_date_stem("2024-01-15"), Some(20240115));
        assert_eq!(parse_event_date_stem("20220102"), Some(20220102));
        // Invalid
        assert_eq!(parse_event_date_stem("bad"), None);
        assert_eq!(parse_event_date_stem(""), None);
    }

    #[test]
    fn test_hash_subsample_deterministic() {
        let h1 = hash_subsample(1234567890, 42);
        let h2 = hash_subsample(1234567890, 42);
        assert_eq!(h1, h2);
        assert!(h1 < 1000);
    }

    #[test]
    fn test_hash_subsample_different_seeds() {
        let h1 = hash_subsample(1234567890, 42);
        let h2 = hash_subsample(1234567890, 99);
        // Different seeds should (very likely) produce different hashes
        // This is probabilistic but extremely unlikely to collide
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_subsample_distribution() {
        // Check that ~15% of hashes fall below threshold 150
        let mut below = 0u32;
        let n = 10_000u32;
        for i in 0..n {
            let h = hash_subsample(i as u64 * 1_000_000_000, 42);
            if h < 150 {
                below += 1;
            }
        }
        let pct = below as f64 / n as f64;
        assert!(
            (pct - 0.15).abs() < 0.03,
            "Expected ~15% below 150, got {:.1}%",
            pct * 100.0
        );
    }

    #[test]
    fn test_assemble_buffers_empty() {
        let (features, labels) = assemble_buffers(&mut []);
        assert!(features.is_empty());
        assert!(labels.is_empty());
    }
}
