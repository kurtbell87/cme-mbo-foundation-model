//! Day data pipeline: load pre-computed features from Parquet files.
//!
//! Replaces the previous DBN → bars → features → labels pipeline.
//! Parquet files contain all 20 non-spatial features, `tb_label`, `fwd_return_720`,
//! `is_warmup`, and `close_mid` pre-computed by `bar-feature-export`.

pub mod fold_runner;
pub mod statistics;

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use arrow::array::{Array, AsArray, Float64Array};
use arrow::datatypes::{Float32Type, Float64Type, UInt64Type};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

/// Per-day data for CPCV: pre-filtered features, labels, forward returns.
///
/// All warmup bars and bars with NaN `fwd_return_720` are excluded during loading.
pub struct DayData {
    /// Date in YYYYMMDD format.
    pub date: i32,
    /// Total bar count for this day (before filtering, for DayMeta).
    pub n_bars: usize,
    /// 20 non-spatial features per bar (pre-filtered: no warmup, no NaN fwd).
    pub features: Vec<[f64; 20]>,
    /// Triple-barrier label: -1 (short), 0 (hold), +1 (long).
    pub labels: Vec<i32>,
    /// fwd_return_720 in ticks (pre-filtered).
    pub fwd_returns: Vec<f64>,
    /// close_mid for ALL Parquet rows (length = n_bars, not filtered).
    /// Used for serial-execution barrier re-simulation.
    pub close_mids: Vec<f64>,
    /// Mapping from filtered index → original Parquet row index.
    /// bar_indices[i] is the Parquet row of the i-th filtered bar.
    pub bar_indices: Vec<usize>,
    /// Tick-level mid-prices: (timestamp_ns, mid_price). Empty if no tick data.
    pub tick_mids: Vec<(u64, f32)>,
    /// Bar close timestamps for ALL Parquet rows (for bar→tick mapping).
    pub bar_close_timestamps: Vec<u64>,
}

/// The 20 non-spatial feature names in extraction order.
pub const FEATURE_NAMES: [&str; 20] = [
    "weighted_imbalance",
    "spread",
    "net_volume",
    "volume_imbalance",
    "trade_count",
    "avg_trade_size",
    "vwap_distance",
    "return_1",
    "return_5",
    "return_20",
    "volatility_20",
    "volatility_50",
    "high_low_range_50",
    "close_position",
    "cancel_add_ratio",
    "message_rate",
    "modify_fraction",
    "time_sin",
    "time_cos",
    "minutes_since_open",
];

/// Scan a directory for `YYYY-MM-DD.parquet` files.
///
/// Returns a sorted list of `(date_int, path)` pairs where `date_int` is YYYYMMDD.
pub fn scan_parquet_dir(dir: &Path) -> Result<Vec<(i32, PathBuf)>> {
    let mut entries: Vec<(i32, PathBuf)> = Vec::new();

    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory {:?}", dir))?
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
        // Parse YYYY-MM-DD → YYYYMMDD integer
        let date_int = parse_date_stem(stem);
        if let Some(d) = date_int {
            entries.push((d, path));
        }
    }

    entries.sort_by_key(|(d, _)| *d);
    Ok(entries)
}

/// Parse "YYYY-MM-DD" into YYYYMMDD integer.
fn parse_date_stem(s: &str) -> Option<i32> {
    if s.len() != 10 {
        return None;
    }
    let no_dash = s.replace('-', "");
    no_dash.parse::<i32>().ok()
}

/// Load a single day from a pre-computed Parquet file.
///
/// Reads all rows, filters out warmup bars and bars with NaN `fwd_return_720`,
/// and extracts 20 features + `tb_label` + `fwd_return_720`.
pub fn load_day_from_parquet(path: &Path, date: i32) -> Result<DayData> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open {:?}", path))?;

    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("Failed to create Parquet reader for {:?}", path))?;

    let reader = builder.build()
        .with_context(|| format!("Failed to build Parquet reader for {:?}", path))?;

    let mut features: Vec<[f64; 20]> = Vec::new();
    let mut labels: Vec<i32> = Vec::new();
    let mut fwd_returns: Vec<f64> = Vec::new();
    let mut close_mids: Vec<f64> = Vec::new();
    let mut bar_indices: Vec<usize> = Vec::new();
    let mut bar_close_timestamps: Vec<u64> = Vec::new();
    let mut total_rows: usize = 0;

    for batch_result in reader {
        let batch = batch_result
            .with_context(|| format!("Failed to read batch from {:?}", path))?;

        let n_rows = batch.num_rows();

        // Extract close_mid column (for ALL rows, before filtering)
        let close_mid_col = batch
            .column_by_name("close_mid")
            .context("Missing column: close_mid")?
            .as_primitive::<Float64Type>();

        // Extract timestamp column (Int64, for ALL rows — used for bar→tick mapping)
        let timestamp_col = batch
            .column_by_name("timestamp")
            .context("Missing column: timestamp")?;
        let timestamp_arr = timestamp_col.as_any().downcast_ref::<arrow::array::Int64Array>()
            .context("timestamp column is not Int64")?;

        // Extract is_warmup column (Boolean)
        let is_warmup_col = batch
            .column_by_name("is_warmup")
            .context("Missing column: is_warmup")?
            .as_boolean();

        // Extract fwd_return_720 column
        let fwd_720_col = batch
            .column_by_name("fwd_return_720")
            .context("Missing column: fwd_return_720")?
            .as_primitive::<Float64Type>();

        // Extract tb_label column
        let tb_label_col = batch
            .column_by_name("tb_label")
            .context("Missing column: tb_label")?
            .as_primitive::<Float64Type>();

        // Extract feature columns
        let mut feature_arrays: Vec<&Float64Array> = Vec::with_capacity(20);
        for name in &FEATURE_NAMES {
            let col = batch
                .column_by_name(name)
                .with_context(|| format!("Missing feature column: {}", name))?
                .as_primitive::<Float64Type>();
            feature_arrays.push(col);
        }

        // Store ALL close_mid and timestamp values (unfiltered)
        for row in 0..n_rows {
            close_mids.push(close_mid_col.value(row));
            bar_close_timestamps.push(timestamp_arr.value(row) as u64);
        }

        // Single pass: filter and extract
        for row in 0..n_rows {
            let parquet_row = total_rows + row;

            // Skip warmup bars
            if is_warmup_col.value(row) {
                continue;
            }

            // Skip bars with NaN fwd_return_720
            let fwd_val = fwd_720_col.value(row);
            if fwd_val.is_nan() || fwd_720_col.is_null(row) {
                continue;
            }

            // Extract features
            let mut feat = [0.0f64; 20];
            for (j, arr) in feature_arrays.iter().enumerate() {
                feat[j] = arr.value(row);
            }
            features.push(feat);

            // Extract label (round Float64 → i32)
            let label_val = tb_label_col.value(row);
            labels.push(label_val.round() as i32);

            // Store forward return
            fwd_returns.push(fwd_val);

            // Track original Parquet row index for this filtered bar
            bar_indices.push(parquet_row);
        }

        total_rows += n_rows;
    }

    Ok(DayData {
        date,
        n_bars: total_rows,
        features,
        labels,
        fwd_returns,
        close_mids,
        bar_indices,
        tick_mids: Vec::new(),
        bar_close_timestamps,
    })
}

/// Load a tick-level mid-price series from a `{date}-ticks.parquet` file.
///
/// Returns sorted Vec of (timestamp_ns, mid_price) pairs.
pub fn load_tick_series(path: &Path) -> Result<Vec<(u64, f32)>> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open tick series {:?}", path))?;

    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("Failed to create tick Parquet reader for {:?}", path))?;

    let reader = builder.build()
        .with_context(|| format!("Failed to build tick Parquet reader for {:?}", path))?;

    let mut result: Vec<(u64, f32)> = Vec::new();

    for batch_result in reader {
        let batch = batch_result
            .with_context(|| format!("Failed to read tick batch from {:?}", path))?;

        let ts_col = batch
            .column_by_name("timestamp_ns")
            .context("Missing column: timestamp_ns")?
            .as_primitive::<UInt64Type>();

        let mid_col = batch
            .column_by_name("mid_price")
            .context("Missing column: mid_price")?
            .as_primitive::<Float32Type>();

        for row in 0..batch.num_rows() {
            result.push((ts_col.value(row), mid_col.value(row)));
        }
    }

    Ok(result)
}

/// Scan a directory for `YYYY-MM-DD-ticks.parquet` files.
///
/// Returns a map from date_int (YYYYMMDD) to file path.
pub fn scan_tick_series_dir(dir: &Path) -> Result<HashMap<i32, PathBuf>> {
    let mut entries: HashMap<i32, PathBuf> = HashMap::new();

    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read tick series directory {:?}", dir))?
    {
        let entry = entry?;
        let path = entry.path();
        let fname = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // Match pattern: YYYY-MM-DD-ticks.parquet
        if !fname.ends_with("-ticks.parquet") {
            continue;
        }
        let date_part = &fname[..fname.len() - "-ticks.parquet".len()];
        if let Some(d) = parse_date_stem(date_part) {
            entries.insert(d, path);
        }
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_names_length() {
        assert_eq!(FEATURE_NAMES.len(), 20);
    }

    #[test]
    fn test_parse_date_stem() {
        assert_eq!(parse_date_stem("2024-01-15"), Some(20240115));
        assert_eq!(parse_date_stem("2024-12-31"), Some(20241231));
        assert_eq!(parse_date_stem("bad"), None);
        assert_eq!(parse_date_stem(""), None);
    }
}
