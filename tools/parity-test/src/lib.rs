//! Parity test library — types and functions for comparing C++ reference
//! features against the Rust pipeline output.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use arrow::array::{Array, BooleanArray, Float64Array};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

use bars::{BarBuilder, TimeBarBuilder};
use common::bar::Bar;
use common::book::{BookSnapshot, BOOK_DEPTH, SNAPSHOT_INTERVAL_NS, TRADE_BUF_LEN};
use common::time_utils;
use dbn::decode::{DbnDecoder, DecodeRecord};
use dbn::MboMsg;
use features::BarFeatureComputer;

/// The 20 model feature column names in canonical (XGBoost) order.
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

/// A matched pair of reference Parquet and DBN data files for the same day.
pub struct DayPair {
    /// Date in YYYYMMDD format.
    pub date: String,
    /// Path to the reference Parquet file (YYYY-MM-DD.parquet).
    pub reference_path: PathBuf,
    /// Path to the DBN file (glbx-mdp3-YYYYMMDD.mbo.dbn.zst).
    pub dbn_path: PathBuf,
}

/// Per-feature deviation statistics from a comparison run.
pub struct FeatureDeviation {
    /// Feature name (one of FEATURE_NAMES).
    pub name: String,
    /// Maximum absolute deviation across all bars.
    pub max_dev: f64,
    /// Mean absolute deviation across all bars.
    pub mean_dev: f64,
    /// Whether this feature passed (max_dev <= tolerance).
    pub passed: bool,
    /// Bar index where the maximum deviation occurred.
    pub worst_bar: Option<usize>,
    /// Rust value at the worst bar.
    pub worst_rust_val: Option<f64>,
    /// Reference value at the worst bar.
    pub worst_ref_val: Option<f64>,
}

/// Result of comparing Rust and reference feature vectors.
pub struct ComparisonResult {
    /// Overall pass/fail.
    pub passed: bool,
    /// Number of bars in the Rust output.
    pub bar_count_rust: usize,
    /// Number of bars in the reference output.
    pub bar_count_ref: usize,
    /// Per-feature deviation statistics (length 20).
    pub per_feature: Vec<FeatureDeviation>,
}

// ---------------------------------------------------------------------------
// StreamingBook — order-book reconstruction from MBO messages
// ---------------------------------------------------------------------------

const F_LAST: u8 = 0x80;

fn fixed_to_float(fixed: i64) -> f32 {
    (fixed as f64 / 1e9) as f32
}

struct OrderEntry {
    side: char,
    price: i64,
    size: u32,
}

struct StreamingBook {
    orders: HashMap<u64, OrderEntry>,
    bid_levels: BTreeMap<i64, u32>,
    ask_levels: BTreeMap<i64, u32>,
    trades: VecDeque<[f32; 3]>,
    last_mid: f32,
    last_spread: f32,
    both_sides_seen: bool,
}

impl StreamingBook {
    fn new() -> Self {
        Self {
            orders: HashMap::new(),
            bid_levels: BTreeMap::new(),
            ask_levels: BTreeMap::new(),
            trades: VecDeque::new(),
            last_mid: 0.0,
            last_spread: 0.0,
            both_sides_seen: false,
        }
    }

    fn process(&mut self, msg: &MboMsg, target_id: u32) {
        if msg.hd.instrument_id != target_id {
            return;
        }
        let action = msg.action as u8 as char;
        let side = msg.side as u8 as char;
        let price = msg.price;
        let size = msg.size;
        let order_id = msg.order_id;

        match action {
            'A' => {
                self.orders.insert(order_id, OrderEntry { side, price, size });
                self.add_level(side, price, size);
            }
            'C' => {
                if let Some(entry) = self.orders.remove(&order_id) {
                    self.remove_level(entry.side, entry.price, entry.size);
                }
            }
            'M' => {
                if let Some(entry) = self.orders.remove(&order_id) {
                    self.remove_level(entry.side, entry.price, entry.size);
                }
                self.orders.insert(order_id, OrderEntry { side, price, size });
                self.add_level(side, price, size);
            }
            'T' => {
                let agg = if side == 'B' { 1.0f32 } else { -1.0f32 };
                self.trades
                    .push_back([fixed_to_float(price), size as f32, agg]);
                if self.trades.len() > TRADE_BUF_LEN {
                    self.trades.pop_front();
                }
            }
            'F' => {
                if let Some(entry) = self.orders.remove(&order_id) {
                    self.remove_level(entry.side, entry.price, entry.size);
                    if size > 0 {
                        self.orders.insert(
                            order_id,
                            OrderEntry { side: entry.side, price: entry.price, size },
                        );
                        self.add_level(entry.side, entry.price, size);
                    }
                }
            }
            'R' => {
                self.orders.clear();
                self.bid_levels.clear();
                self.ask_levels.clear();
            }
            _ => {}
        }
    }

    fn levels_for_side(&mut self, side: char) -> &mut BTreeMap<i64, u32> {
        if side == 'B' {
            &mut self.bid_levels
        } else {
            &mut self.ask_levels
        }
    }

    fn add_level(&mut self, side: char, price: i64, size: u32) {
        *self.levels_for_side(side).entry(price).or_insert(0) += size;
    }

    fn remove_level(&mut self, side: char, price: i64, size: u32) {
        let levels = self.levels_for_side(side);
        if let Some(lvl) = levels.get_mut(&price) {
            if *lvl <= size {
                levels.remove(&price);
            } else {
                *lvl -= size;
            }
        }
    }

    fn has_both_sides(&self) -> bool {
        !self.bid_levels.is_empty() && !self.ask_levels.is_empty()
    }

    fn snapshot(&mut self, ts: u64) -> Option<BookSnapshot> {
        if self.has_both_sides() {
            self.both_sides_seen = true;
        }
        if !self.both_sides_seen {
            return None;
        }

        let mut snap = BookSnapshot::default();
        snap.timestamp = ts;

        // Bids (descending)
        for (i, (&price, &size)) in self.bid_levels.iter().rev().enumerate() {
            if i >= BOOK_DEPTH {
                break;
            }
            snap.bids[i] = [fixed_to_float(price), size as f32];
        }

        // Asks (ascending)
        for (i, (&price, &size)) in self.ask_levels.iter().enumerate() {
            if i >= BOOK_DEPTH {
                break;
            }
            snap.asks[i] = [fixed_to_float(price), size as f32];
        }

        // Mid/spread
        if self.has_both_sides() {
            let best_bid = fixed_to_float(*self.bid_levels.keys().next_back().unwrap());
            let best_ask = fixed_to_float(*self.ask_levels.keys().next().unwrap());
            snap.mid_price = (best_bid + best_ask) / 2.0;
            snap.spread = best_ask - best_bid;
            self.last_mid = snap.mid_price;
            self.last_spread = snap.spread;
        } else {
            snap.mid_price = self.last_mid;
            snap.spread = self.last_spread;
        }

        // Trades
        let count = self.trades.len();
        let start = TRADE_BUF_LEN - count;
        for (i, t) in self.trades.iter().enumerate() {
            snap.trades[start + i] = *t;
        }

        snap.time_of_day = time_utils::compute_time_of_day(ts);
        Some(snap)
    }
}

// ---------------------------------------------------------------------------
// run_rust_pipeline_all_bars — streaming pipeline that returns ALL bars
// ---------------------------------------------------------------------------

/// Run the Rust pipeline and return ALL bars including warmup,
/// with full Bar metadata (timestamps, snapshot_count, etc.).
///
/// Pipeline: dbn.zst → streaming book → 100ms snapshots → 5s time bars.
pub fn run_rust_pipeline_all_bars(dbn_path: &Path, instrument_id: u32) -> Result<Vec<Bar>> {
    let mut decoder = DbnDecoder::from_zstd_file(dbn_path)
        .map_err(|e| anyhow::anyhow!("Failed to open DBN file: {}", e))?;

    let mut book = StreamingBook::new();
    let mut first_ts: Option<u64> = None;
    let mut rth_close: u64 = 0;
    let mut next_snap_ts: u64 = 0;
    let mut bar_builder = TimeBarBuilder::new(5);
    let mut bar_list = Vec::new();

    while let Some(msg) = decoder
        .decode_record::<MboMsg>()
        .map_err(|e| anyhow::anyhow!("DBN decode error: {}", e))?
    {
        let ts = msg.hd.ts_event;
        let id = msg.hd.instrument_id;

        if first_ts.is_none() && id == instrument_id {
            first_ts = Some(ts);
            rth_close = time_utils::rth_close_ns(ts);
            next_snap_ts = time_utils::rth_open_ns(ts);
        }

        book.process(msg, instrument_id);

        let flags = msg.flags.raw();
        if flags & F_LAST != 0 && id == instrument_id {
            while next_snap_ts < rth_close && next_snap_ts <= ts {
                if let Some(snap) = book.snapshot(next_snap_ts) {
                    if let Some(bar) = bar_builder.on_snapshot(&snap) {
                        bar_list.push(bar);
                    }
                }
                next_snap_ts += SNAPSHOT_INTERVAL_NS;
            }
        }
    }

    if first_ts.is_none() {
        bail!("No records found for instrument {}", instrument_id);
    }

    // Emit remaining snapshots up to RTH close
    while next_snap_ts < rth_close {
        if let Some(snap) = book.snapshot(next_snap_ts) {
            if let Some(bar) = bar_builder.on_snapshot(&snap) {
                bar_list.push(bar);
            }
        }
        next_snap_ts += SNAPSHOT_INTERVAL_NS;
    }

    // Flush any partial bar
    if let Some(bar) = bar_builder.flush() {
        bar_list.push(bar);
    }

    Ok(bar_list)
}

// ---------------------------------------------------------------------------
// load_reference_parquet
// ---------------------------------------------------------------------------

/// Load a reference Parquet file and extract the 20 model features.
///
/// Skips warmup bars (is_warmup == true or first 50 bars).
/// Returns one `[f64; 20]` per non-warmup bar.
pub fn load_reference_parquet(path: &Path) -> Result<Vec<[f64; 20]>> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open reference Parquet file: {}", path.display()))?;

    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("Failed to read Parquet metadata: {}", path.display()))?;

    let reader = builder.build()?;

    let mut rows: Vec<[f64; 20]> = Vec::new();
    let mut row_index: usize = 0;

    for batch_result in reader {
        let batch = batch_result?;
        let n_rows = batch.num_rows();

        // Try to get is_warmup column
        let warmup_col = batch
            .column_by_name("is_warmup")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>().cloned());

        // Extract the 20 feature columns
        let mut feature_cols: Vec<Float64Array> = Vec::with_capacity(20);
        for &feat_name in &FEATURE_NAMES {
            let col = batch
                .column_by_name(feat_name)
                .with_context(|| format!("Missing column '{}' in Parquet", feat_name))?;
            let f64_arr = col
                .as_any()
                .downcast_ref::<Float64Array>()
                .with_context(|| format!("Column '{}' is not Float64", feat_name))?;
            feature_cols.push(f64_arr.clone());
        }

        for i in 0..n_rows {
            let is_warmup = if let Some(ref w) = warmup_col {
                w.value(i)
            } else {
                row_index < 50
            };

            if !is_warmup {
                let mut feat_row = [0.0f64; 20];
                for (j, col) in feature_cols.iter().enumerate() {
                    feat_row[j] = col.value(i);
                }
                rows.push(feat_row);
            }

            row_index += 1;
        }
    }

    Ok(rows)
}

// ---------------------------------------------------------------------------
// run_rust_pipeline
// ---------------------------------------------------------------------------

/// Run the full Rust pipeline on a DBN file and return 20 model features
/// per non-warmup bar.
///
/// Pipeline: dbn.zst → streaming book → 100ms snapshots → 5s time bars
///           → BarFeatureComputer → extract 20 model features.
///
/// Uses a streaming approach to avoid storing all committed states in memory.
pub fn run_rust_pipeline(dbn_path: &Path, instrument_id: u32) -> Result<Vec<[f64; 20]>> {
    let bar_list = run_rust_pipeline_all_bars(dbn_path, instrument_id)?;

    // Compute features (batch mode with fixup)
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bar_list);

    // Extract 20 model features, skipping warmup bars
    let mut result = Vec::new();
    for row in &rows {
        if row.is_warmup {
            continue;
        }
        result.push(extract_20_features(row));
    }

    Ok(result)
}

/// Extract the 20 model features from a BarFeatureRow as f64.
fn extract_20_features(row: &features::BarFeatureRow) -> [f64; 20] {
    [
        row.weighted_imbalance as f64,
        row.spread as f64,
        row.net_volume as f64,
        row.volume_imbalance as f64,
        row.trade_count as f64,
        row.avg_trade_size as f64,
        row.vwap_distance as f64,
        row.return_1 as f64,
        row.return_5 as f64,
        row.return_20 as f64,
        row.volatility_20 as f64,
        row.volatility_50 as f64,
        row.high_low_range_50 as f64,
        row.close_position as f64,
        row.cancel_add_ratio as f64,
        row.message_rate as f64,
        row.modify_fraction as f64,
        row.time_sin as f64,
        row.time_cos as f64,
        row.minutes_since_open as f64,
    ]
}

// ---------------------------------------------------------------------------
// match_day_files
// ---------------------------------------------------------------------------

/// Scan reference and data directories and match files by date.
///
/// Reference files: `YYYY-MM-DD.parquet`
/// Data files: `glbx-mdp3-YYYYMMDD.mbo.dbn.zst`
///
/// Returns matched pairs sorted by date.
pub fn match_day_files(ref_dir: &Path, data_dir: &Path) -> Result<Vec<DayPair>> {
    if !ref_dir.exists() {
        bail!(
            "Reference directory does not exist: {}",
            ref_dir.display()
        );
    }
    if !data_dir.exists() {
        bail!("Data directory does not exist: {}", data_dir.display());
    }

    // Scan reference dir for YYYY-MM-DD.parquet files
    let mut ref_dates: HashMap<String, PathBuf> = HashMap::new();
    for entry in std::fs::read_dir(ref_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(date_str) = name.strip_suffix(".parquet") {
            // date_str should be "YYYY-MM-DD"
            if date_str.len() == 10 && date_str.chars().nth(4) == Some('-') {
                let compact = date_str.replace('-', "");
                ref_dates.insert(compact, entry.path());
            }
        }
    }

    // Scan data dir for glbx-mdp3-YYYYMMDD.mbo.dbn.zst files
    let mut data_dates: HashMap<String, PathBuf> = HashMap::new();
    for entry in std::fs::read_dir(data_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(rest) = name.strip_prefix("glbx-mdp3-") {
            if let Some(date_str) = rest.strip_suffix(".mbo.dbn.zst") {
                if date_str.len() == 8 {
                    data_dates.insert(date_str.to_string(), entry.path());
                }
            }
        }
    }

    // Match by date
    let mut pairs: Vec<DayPair> = Vec::new();
    for (date, ref_path) in &ref_dates {
        if let Some(dbn_path) = data_dates.get(date) {
            pairs.push(DayPair {
                date: date.clone(),
                reference_path: ref_path.clone(),
                dbn_path: dbn_path.clone(),
            });
        }
    }

    pairs.sort_by(|a, b| a.date.cmp(&b.date));
    Ok(pairs)
}

// ---------------------------------------------------------------------------
// compare_features
// ---------------------------------------------------------------------------

/// Compare Rust and reference feature vectors bar-by-bar.
///
/// Uses the minimum bar count from both inputs for comparison.
/// A feature fails if its max absolute deviation > tolerance.
pub fn compare_features(
    rust: &[[f64; 20]],
    reference: &[[f64; 20]],
    tolerance: f64,
) -> ComparisonResult {
    let n = rust.len().min(reference.len());

    let mut per_feature: Vec<FeatureDeviation> = Vec::with_capacity(20);

    for feat_idx in 0..20 {
        let mut max_dev: f64 = 0.0;
        let mut sum_dev: f64 = 0.0;
        let mut worst_bar: Option<usize> = None;
        let mut worst_rust_val: Option<f64> = None;
        let mut worst_ref_val: Option<f64> = None;

        for bar_idx in 0..n {
            let dev = (rust[bar_idx][feat_idx] - reference[bar_idx][feat_idx]).abs();
            sum_dev += dev;
            if dev > max_dev {
                max_dev = dev;
                worst_bar = Some(bar_idx);
                worst_rust_val = Some(rust[bar_idx][feat_idx]);
                worst_ref_val = Some(reference[bar_idx][feat_idx]);
            }
        }

        let mean_dev = if n > 0 { sum_dev / n as f64 } else { 0.0 };
        let passed = max_dev <= tolerance;

        // Only report worst bar details if the feature failed
        let (wb, wrv, wrefv) = if !passed {
            (worst_bar, worst_rust_val, worst_ref_val)
        } else {
            (None, None, None)
        };

        per_feature.push(FeatureDeviation {
            name: FEATURE_NAMES[feat_idx].to_string(),
            max_dev,
            mean_dev,
            passed,
            worst_bar: wb,
            worst_rust_val: wrv,
            worst_ref_val: wrefv,
        });
    }

    let all_passed = per_feature.iter().all(|f| f.passed);

    ComparisonResult {
        passed: all_passed,
        bar_count_rust: rust.len(),
        bar_count_ref: reference.len(),
        per_feature,
    }
}
