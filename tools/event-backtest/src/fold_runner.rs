//! Per-fold XGBoost training, prediction, and serial PnL computation.
//!
//! Single-stage binary:logistic model predicting P(target | LOB, T, S).
//! Decision rule: trade when P_model > S/(T+S) + margin.
//! No normalization (trees don't need it).

use anyhow::{Context, Result};

use backtest::CpcvSplit;
use crate::data::NUM_FEATURES;
use xgboost_ffi::training::{Booster, DMatrix};

use crate::data::{self, DayChunk, DayMeta, EventData};

/// Force glibc to return freed memory to the OS (Linux only).
/// Critical for sequential fold execution where training data must be
/// fully released before loading test data.
#[cfg(target_os = "linux")]
fn force_return_memory() {
    extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe { malloc_trim(0); }
}

#[cfg(not(target_os = "linux"))]
fn force_return_memory() {}

/// XGBoost hyperparameters for event-level model.
pub struct XgbParams {
    pub max_depth: u32,
    pub eta: f64,
    pub min_child_weight: u32,
    pub subsample: f64,
    pub colsample_bytree: f64,
    pub n_rounds: u32,
    pub early_stopping: u32,
    pub nthread: i32,
    pub max_bin: u32,
}

impl Default for XgbParams {
    fn default() -> Self {
        Self {
            max_depth: 6,
            eta: 0.01,
            min_child_weight: 100,
            subsample: 0.6,
            colsample_bytree: 0.7,
            n_rounds: 3000,
            early_stopping: 100,
            nthread: 32,
            max_bin: 256,
        }
    }
}

/// Evaluate validation loss every N rounds.
const EVAL_EVERY: u32 = 50;

/// Tick value: $5.00 multiplier x $0.25 tick_size = $1.25 per tick.
const TICK_VALUE: f64 = 1.25;

/// Per-fold metrics.
#[derive(Debug, Clone)]
pub struct FoldMetrics {
    pub expectancy: f64,
    pub total_trades: i32,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub annualized_sharpe: f64,
    pub net_pnl: f64,
    /// Per-trade PnL values (for daily aggregation upstream).
    pub trade_pnls: Vec<f64>,
}

/// Per-geometry metrics for null hypothesis comparison.
#[derive(Debug, Clone)]
pub struct GeometryMetrics {
    pub target_ticks: i32,
    pub stop_ticks: i32,
    pub p_null: f64,
    pub p_model_mean: f64,
    pub n_predictions: usize,
    pub n_trades: usize,
    pub trade_win_rate: f64,
}

/// Feature importance entry.
#[derive(Debug, Clone)]
pub struct FeatureImportance {
    pub name: String,
    pub gain: f64,
}

/// Result of running a single CPCV fold.
#[derive(Debug, Clone)]
pub struct FoldResult {
    pub split_idx: usize,
    pub test_groups: Vec<usize>,
    pub n_train: usize,
    pub n_test: usize,
    pub metrics: FoldMetrics,
    pub geometry_metrics: Vec<GeometryMetrics>,
    pub feature_importance: Vec<FeatureImportance>,
    pub best_iter: u32,
    pub calibration_bins: Vec<CalibrationBin>,
}

/// A single calibration bin: predicted probability range vs actual hit rate.
#[derive(Debug, Clone)]
pub struct CalibrationBin {
    pub bin_start: f64,
    pub bin_end: f64,
    pub mean_predicted: f64,
    pub actual_rate: f64,
    pub count: usize,
}

/// Run a single CPCV fold: load data, train XGBoost, predict, compute PnL.
pub fn run_fold(
    split: &CpcvSplit,
    day_metas: &[DayMeta],
    params: &XgbParams,
    margin: f64,
    commission: f64,
    subsample_pct: u32,
    seed: u64,
) -> Result<FoldResult> {
    // ── 1. Load training data (subsampled) ──────────────────────────────
    eprintln!("  Loading {} train days ({}% subsample)...", split.train_day_indices.len(), subsample_pct);
    let mut train_chunks: Vec<DayChunk> = Vec::new();
    for &day_idx in &split.train_day_indices {
        let meta = &day_metas[day_idx];
        let chunk = data::load_day_subsampled(&meta.path, subsample_pct, seed)
            .with_context(|| format!("Failed to load train day {}", meta.date))?;
        train_chunks.push(chunk);
    }

    // Split last 20% of training days as validation
    let n_train_days = train_chunks.len();
    let val_start = n_train_days * 4 / 5;
    let mut val_chunks: Vec<DayChunk> = train_chunks.split_off(val_start);

    // ── 2. Build DMatrices (memory-conscious) ───────────────────────────
    // assemble_buffers drains chunks' features to avoid 3x memory amplification.
    // Sequence: assemble → drop chunks → build DMatrix → drop flat buffer.
    let ncol = NUM_FEATURES;

    let (train_features, train_labels) = data::assemble_buffers(&mut train_chunks);
    drop(train_chunks); // features already drained, free struct overhead
    let n_train = train_labels.len();

    if n_train == 0 {
        anyhow::bail!("No training rows after loading and filtering");
    }

    let dtrain = DMatrix::from_dense(&train_features, n_train, ncol)
        .map_err(|e| anyhow::anyhow!("Failed to create train DMatrix: {}", e))?;
    dtrain.set_labels(&train_labels)
        .map_err(|e| anyhow::anyhow!("Failed to set train labels: {}", e))?;
    drop(train_features); // DMatrix has its own internal copy
    drop(train_labels);
    force_return_memory();

    let (val_features, val_labels) = data::assemble_buffers(&mut val_chunks);
    drop(val_chunks);
    let n_val = val_labels.len();

    let dval = if n_val > 0 {
        let dv = DMatrix::from_dense(&val_features, n_val, ncol)
            .map_err(|e| anyhow::anyhow!("Failed to create val DMatrix: {}", e))?;
        dv.set_labels(&val_labels)
            .map_err(|e| anyhow::anyhow!("Failed to set val labels: {}", e))?;
        drop(val_features);
        Some(dv)
    } else {
        None
    };
    // val_labels kept alive for early stopping evaluation

    eprintln!("  Train: {} rows, Val: {} rows", n_train, n_val);

    // ── 3. Train XGBoost ────────────────────────────────────────────────
    eprintln!("  Training XGBoost (max {} rounds, eta={})...", params.n_rounds, params.eta);
    let val_labels_ref = if n_val > 0 { Some(val_labels.as_slice()) } else { None };
    let (booster, best_iter) = train_xgboost(
        &dtrain,
        dval.as_ref(),
        val_labels_ref,
        params,
    )?;

    eprintln!("  Training done: best_iter={}, freeing train memory...", best_iter + 1);

    // Drop training DMatrices to free memory before loading test data
    drop(dtrain);
    drop(dval);
    drop(val_labels);

    // Force glibc to return freed pages to OS — prevents OOM when loading test data
    force_return_memory();

    // ── 4. Predict test data day-by-day (streaming, ~2 GB peak per day) ──
    eprintln!("  Predicting {} test days (streaming, {} trees)...", split.test_day_indices.len(), best_iter + 1);
    let mut predictions: Vec<f32> = Vec::new();
    let mut test_events: Vec<EventData> = Vec::new();

    for &day_idx in &split.test_day_indices {
        let meta = &day_metas[day_idx];
        let mut chunk = data::load_day_full(&meta.path)
            .with_context(|| format!("Failed to load test day {}", meta.date))?;

        if chunk.n_rows == 0 {
            continue;
        }

        // Build per-day DMatrix, predict, free immediately
        let n = chunk.n_rows;
        let dday = DMatrix::from_dense(&chunk.features, n, ncol)
            .map_err(|e| anyhow::anyhow!("Failed to create test DMatrix for day {}: {}", meta.date, e))?;
        chunk.features.clear();
        chunk.features.shrink_to_fit();

        let day_preds = booster
            .predict(&dday, best_iter + 1)
            .map_err(|e| anyhow::anyhow!("Prediction failed for day {}: {}", meta.date, e))?;

        predictions.extend_from_slice(&day_preds);
        test_events.append(&mut chunk.events);
        // dday + chunk dropped here
    }

    let n_test = test_events.len();

    drop(booster);
    force_return_memory();

    if n_test == 0 {
        anyhow::bail!("No test rows after loading and filtering");
    }

    eprintln!("  Test: {} rows predicted, computing serial PnL...", n_test);

    // ── 6. Compute serial PnL ───────────────────────────────────────────
    let metrics = compute_serial_pnl(&predictions, &test_events, margin, commission);

    // ── 7. Per-geometry breakdown ───────────────────────────────────────
    let geometry_metrics = compute_geometry_metrics(&predictions, &test_events, margin, commission);

    // ── 8. Calibration bins ─────────────────────────────────────────────
    let calibration_bins = compute_calibration(&predictions, &test_events);

    // ── 9. Feature importance (placeholder — gain not available via C API) ──
    let feature_importance = Vec::new();

    Ok(FoldResult {
        split_idx: split.split_idx,
        test_groups: split.test_groups.clone(),
        n_train,
        n_test,
        metrics,
        geometry_metrics,
        feature_importance,
        best_iter,
        calibration_bins,
    })
}

/// Run a single CPCV fold on imbalance-filtered data.
///
/// Uses `load_day_imbalance()` instead of subsampled loading — the filtered
/// dataset is small enough (~2-5M rows total) to fit entirely in memory.
pub fn run_imbalance_fold(
    split: &CpcvSplit,
    day_metas: &[DayMeta],
    params: &XgbParams,
    margin: f64,
    commission: f64,
    ofi_threshold: f32,
    target_geometry: Option<(i32, i32)>,
) -> Result<FoldResult> {
    // ── 1. Load training data (imbalance-filtered, no subsampling) ────
    eprintln!(
        "  Loading {} train days (imbalance, |ofi|>{})...",
        split.train_day_indices.len(),
        ofi_threshold
    );
    let mut train_chunks: Vec<DayChunk> = Vec::new();
    for &day_idx in &split.train_day_indices {
        let meta = &day_metas[day_idx];
        let chunk = data::load_day_imbalance(&meta.path, ofi_threshold, target_geometry)
            .with_context(|| format!("Failed to load train day {}", meta.date))?;
        if chunk.n_rows > 0 {
            train_chunks.push(chunk);
        }
    }

    // Split last 20% of training days as validation
    let n_train_days = train_chunks.len();
    let val_start = n_train_days * 4 / 5;
    let mut val_chunks: Vec<DayChunk> = train_chunks.split_off(val_start);

    // ── 2. Build DMatrices ────────────────────────────────────────────
    let ncol = NUM_FEATURES;

    let (train_features, train_labels) = data::assemble_buffers(&mut train_chunks);
    drop(train_chunks);
    let n_train = train_labels.len();

    if n_train == 0 {
        anyhow::bail!("No training rows after imbalance filtering");
    }

    let dtrain = DMatrix::from_dense(&train_features, n_train, ncol)
        .map_err(|e| anyhow::anyhow!("Failed to create train DMatrix: {}", e))?;
    dtrain
        .set_labels(&train_labels)
        .map_err(|e| anyhow::anyhow!("Failed to set train labels: {}", e))?;
    drop(train_features);
    drop(train_labels);
    force_return_memory();

    let (val_features, val_labels) = data::assemble_buffers(&mut val_chunks);
    drop(val_chunks);
    let n_val = val_labels.len();

    let dval = if n_val > 0 {
        let dv = DMatrix::from_dense(&val_features, n_val, ncol)
            .map_err(|e| anyhow::anyhow!("Failed to create val DMatrix: {}", e))?;
        dv.set_labels(&val_labels)
            .map_err(|e| anyhow::anyhow!("Failed to set val labels: {}", e))?;
        drop(val_features);
        Some(dv)
    } else {
        None
    };

    eprintln!("  Train: {} rows, Val: {} rows", n_train, n_val);

    // ── 3. Train XGBoost ──────────────────────────────────────────────
    eprintln!(
        "  Training XGBoost (max {} rounds, eta={})...",
        params.n_rounds, params.eta
    );
    let val_labels_ref = if n_val > 0 {
        Some(val_labels.as_slice())
    } else {
        None
    };
    let (booster, best_iter) = train_xgboost(&dtrain, dval.as_ref(), val_labels_ref, params)?;

    eprintln!(
        "  Training done: best_iter={}, freeing train memory...",
        best_iter + 1
    );

    drop(dtrain);
    drop(dval);
    drop(val_labels);
    force_return_memory();

    // ── 4. Predict test data day-by-day ───────────────────────────────
    eprintln!(
        "  Predicting {} test days (imbalance-filtered)...",
        split.test_day_indices.len()
    );
    let mut predictions: Vec<f32> = Vec::new();
    let mut test_events: Vec<EventData> = Vec::new();

    for &day_idx in &split.test_day_indices {
        let meta = &day_metas[day_idx];
        let mut chunk = data::load_day_imbalance(&meta.path, ofi_threshold, target_geometry)
            .with_context(|| format!("Failed to load test day {}", meta.date))?;

        if chunk.n_rows == 0 {
            continue;
        }

        let n = chunk.n_rows;
        let dday = DMatrix::from_dense(&chunk.features, n, ncol)
            .map_err(|e| {
                anyhow::anyhow!("Failed to create test DMatrix for day {}: {}", meta.date, e)
            })?;
        chunk.features.clear();
        chunk.features.shrink_to_fit();

        let day_preds = booster
            .predict(&dday, best_iter + 1)
            .map_err(|e| anyhow::anyhow!("Prediction failed for day {}: {}", meta.date, e))?;

        predictions.extend_from_slice(&day_preds);
        test_events.append(&mut chunk.events);
    }

    let n_test = test_events.len();

    drop(booster);
    force_return_memory();

    if n_test == 0 {
        anyhow::bail!("No test rows after imbalance filtering");
    }

    eprintln!(
        "  Test: {} rows predicted, computing serial PnL...",
        n_test
    );

    // ── 5. Compute serial PnL + metrics ───────────────────────────────
    let metrics = compute_serial_pnl(&predictions, &test_events, margin, commission);
    let geometry_metrics = compute_geometry_metrics(&predictions, &test_events, margin, commission);
    let calibration_bins = compute_calibration(&predictions, &test_events);
    let feature_importance = Vec::new();

    Ok(FoldResult {
        split_idx: split.split_idx,
        test_groups: split.test_groups.clone(),
        n_train,
        n_test,
        metrics,
        geometry_metrics,
        feature_importance,
        best_iter,
        calibration_bins,
    })
}

/// Train a single-stage XGBoost binary:logistic model with early stopping.
///
/// `val_labels` must be provided when `dval` is Some, for logloss-based early stopping.
fn train_xgboost(
    dtrain: &DMatrix,
    dval: Option<&DMatrix>,
    val_labels: Option<&[f32]>,
    params: &XgbParams,
) -> Result<(Booster, u32)> {
    let booster = if let Some(dv) = dval {
        Booster::new(&[dtrain, dv])
    } else {
        Booster::new(&[dtrain])
    }
    .map_err(|e| anyhow::anyhow!("Failed to create booster: {}", e))?;

    // Set hyperparameters
    let param_pairs = [
        ("objective", "binary:logistic".to_string()),
        ("eval_metric", "logloss".to_string()),
        ("tree_method", "hist".to_string()),
        ("max_depth", params.max_depth.to_string()),
        ("eta", params.eta.to_string()),
        ("min_child_weight", params.min_child_weight.to_string()),
        ("subsample", params.subsample.to_string()),
        ("colsample_bytree", params.colsample_bytree.to_string()),
        ("max_bin", params.max_bin.to_string()),
        ("nthread", params.nthread.to_string()),
        ("verbosity", "0".to_string()),
    ];

    for (name, value) in &param_pairs {
        booster.set_param(name, value)
            .map_err(|e| anyhow::anyhow!("set_param({}, {}): {}", name, value, e))?;
    }

    let mut best_loss = f64::MAX;
    let mut best_iter = 0u32;
    let mut rounds_no_improve = 0u32;

    for i in 0..params.n_rounds {
        booster.update(dtrain, i)
            .map_err(|e| anyhow::anyhow!("Update failed at iteration {}: {}", i, e))?;

        // Evaluate every EVAL_EVERY rounds
        if (i + 1) % EVAL_EVERY != 0 {
            continue;
        }

        if let (Some(dv), Some(vl)) = (dval, val_labels) {
            let val_preds = booster.predict(dv, i + 1)
                .map_err(|e| anyhow::anyhow!("Validation prediction failed: {}", e))?;

            let loss = binary_logloss(&val_preds, vl);

            if loss < best_loss {
                best_loss = loss;
                best_iter = i;
                rounds_no_improve = 0;
                eprintln!("  [round {}] val_logloss={:.6} (new best)", i + 1, loss);
            } else {
                rounds_no_improve += EVAL_EVERY;
                eprintln!(
                    "  [round {}] val_logloss={:.6} (no improve {}/{})",
                    i + 1, loss, rounds_no_improve, params.early_stopping
                );
            }

            if rounds_no_improve >= params.early_stopping {
                eprintln!("  Early stopping at round {} (best={})", i + 1, best_iter + 1);
                break;
            }
        } else {
            best_iter = i;
        }
    }

    // If no validation set, best_iter is last round
    if dval.is_none() {
        best_iter = params.n_rounds - 1;
    }

    Ok((booster, best_iter))
}

/// Compute serial PnL: walk forward through events, trade when model predicts edge.
pub fn compute_serial_pnl(
    predictions: &[f32],
    events: &[EventData],
    margin: f64,
    commission: f64,
) -> FoldMetrics {
    let mut trade_pnls: Vec<f64> = Vec::new();
    let mut wins = 0i32;
    let mut losses = 0i32;
    let mut next_available_ts: u64 = 0;

    for (i, event) in events.iter().enumerate() {
        // Skip if we're still in a position
        if event.timestamp_ns < next_available_ts {
            continue;
        }

        let t = event.target_ticks as f64;
        let s = event.stop_ticks as f64;
        let p_null = s / (t + s);
        let p_model = predictions[i] as f64;

        // Only trade when model predicts edge above null + margin
        if p_model <= p_null + margin {
            continue;
        }

        // Trade!
        let pnl = event.pnl_ticks as f64 * TICK_VALUE - commission;
        trade_pnls.push(pnl);

        if pnl > 0.0 {
            wins += 1;
        } else {
            losses += 1;
        }

        // Lock out until exit
        next_available_ts = event.exit_ts;
    }

    let total_trades = wins + losses;
    let net_pnl: f64 = trade_pnls.iter().sum();
    let expectancy = if total_trades > 0 {
        net_pnl / total_trades as f64
    } else {
        0.0
    };
    let win_rate = if total_trades > 0 {
        wins as f64 / total_trades as f64
    } else {
        0.0
    };

    let gross_wins: f64 = trade_pnls.iter().filter(|&&p| p > 0.0).sum();
    let gross_losses: f64 = trade_pnls.iter().filter(|&&p| p < 0.0).map(|p| p.abs()).sum();
    let profit_factor = if gross_losses > 0.0 {
        gross_wins / gross_losses
    } else if gross_wins > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let annualized_sharpe = compute_trade_sharpe(&trade_pnls);

    FoldMetrics {
        expectancy,
        total_trades,
        win_rate,
        profit_factor,
        annualized_sharpe,
        net_pnl,
        trade_pnls,
    }
}

/// Per-geometry breakdown: P(target) vs P_null for each (T, S) combination.
fn compute_geometry_metrics(
    predictions: &[f32],
    events: &[EventData],
    margin: f64,
    commission: f64,
) -> Vec<GeometryMetrics> {
    use std::collections::BTreeMap;

    // Group by (target_ticks, stop_ticks)
    let mut groups: BTreeMap<(i32, i32), Vec<(f32, &EventData)>> = BTreeMap::new();
    for (i, event) in events.iter().enumerate() {
        groups
            .entry((event.target_ticks, event.stop_ticks))
            .or_default()
            .push((predictions[i], event));
    }

    let mut metrics = Vec::new();
    for ((t, s), entries) in &groups {
        let t_f = *t as f64;
        let s_f = *s as f64;
        let p_null = s_f / (t_f + s_f);

        let p_model_mean = entries.iter().map(|(p, _)| *p as f64).sum::<f64>()
            / entries.len() as f64;

        // Count trades and wins using serial constraint within this geometry
        let mut n_trades = 0usize;
        let mut trade_wins = 0usize;
        let mut next_ts: u64 = 0;

        for &(pred, event) in entries {
            if event.timestamp_ns < next_ts {
                continue;
            }
            if (pred as f64) <= p_null + margin {
                continue;
            }
            n_trades += 1;
            let pnl = event.pnl_ticks as f64 * TICK_VALUE - commission;
            if pnl > 0.0 {
                trade_wins += 1;
            }
            next_ts = event.exit_ts;
        }

        metrics.push(GeometryMetrics {
            target_ticks: *t,
            stop_ticks: *s,
            p_null,
            p_model_mean,
            n_predictions: entries.len(),
            n_trades,
            trade_win_rate: if n_trades > 0 {
                trade_wins as f64 / n_trades as f64
            } else {
                0.0
            },
        });
    }

    metrics
}

/// Compute calibration bins: 10 equal-width bins from 0.0 to 1.0.
fn compute_calibration(predictions: &[f32], events: &[EventData]) -> Vec<CalibrationBin> {
    let n_bins = 10;
    let mut bin_sum_pred = vec![0.0f64; n_bins];
    let mut bin_sum_actual = vec![0.0f64; n_bins];
    let mut bin_count = vec![0usize; n_bins];

    for (i, event) in events.iter().enumerate() {
        let p = predictions[i] as f64;
        let actual = if event.outcome == 1 { 1.0 } else { 0.0 };

        let bin = ((p * n_bins as f64) as usize).min(n_bins - 1);
        bin_sum_pred[bin] += p;
        bin_sum_actual[bin] += actual;
        bin_count[bin] += 1;
    }

    (0..n_bins)
        .map(|b| {
            let count = bin_count[b];
            CalibrationBin {
                bin_start: b as f64 / n_bins as f64,
                bin_end: (b + 1) as f64 / n_bins as f64,
                mean_predicted: if count > 0 {
                    bin_sum_pred[b] / count as f64
                } else {
                    (b as f64 + 0.5) / n_bins as f64
                },
                actual_rate: if count > 0 {
                    bin_sum_actual[b] / count as f64
                } else {
                    0.0
                },
                count,
            }
        })
        .collect()
}

/// Annualized Sharpe from per-trade PnL.
/// Assumes ~252 trading days, ~50 trades/day as a rough scaling factor.
fn compute_trade_sharpe(pnls: &[f64]) -> f64 {
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
    // Annualize: trades_per_year / n gives the scaling
    let trades_per_year = 252.0 * 50.0;
    mean / std * (trades_per_year / n).sqrt().min(trades_per_year.sqrt())
}

/// Binary log-loss.
pub fn binary_logloss(preds: &[f32], labels: &[f32]) -> f64 {
    let eps = 1e-15;
    let n = preds.len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    let loss: f64 = preds
        .iter()
        .zip(labels.iter())
        .map(|(&p, &y)| {
            let p = (p as f64).clamp(eps, 1.0 - eps);
            let y = y as f64;
            -(y * p.ln() + (1.0 - y) * (1.0 - p).ln())
        })
        .sum();
    loss / n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_binary_logloss_perfect() {
        let preds = vec![0.99, 0.01, 0.99];
        let labels = vec![1.0, 0.0, 1.0];
        let loss = binary_logloss(&preds, &labels);
        assert!(loss < 0.05);
    }

    #[test]
    fn test_binary_logloss_random() {
        let preds = vec![0.5, 0.5, 0.5, 0.5];
        let labels = vec![1.0, 0.0, 1.0, 0.0];
        let loss = binary_logloss(&preds, &labels);
        assert!((loss - 0.6931).abs() < 0.01); // ln(2) ≈ 0.6931
    }

    #[test]
    fn test_compute_trade_sharpe_positive() {
        let pnls = vec![10.0, 12.0, 8.0, 11.0, 9.0];
        let sharpe = compute_trade_sharpe(&pnls);
        assert!(sharpe > 0.0);
    }

    #[test]
    fn test_compute_trade_sharpe_flat() {
        let pnls = vec![5.0, 5.0, 5.0];
        let sharpe = compute_trade_sharpe(&pnls);
        assert_eq!(sharpe, 0.0);
    }

    #[test]
    fn test_serial_pnl_basic() {
        let events = vec![
            EventData {
                timestamp_ns: 1000,
                target_ticks: 10,
                stop_ticks: 5,
                outcome: 1, // target hit
                exit_ts: 2000,
                pnl_ticks: 10.0,
                direction: 1,
                ofi_fast: 3.0,
            },
            EventData {
                timestamp_ns: 1500, // overlaps with first trade
                target_ticks: 10,
                stop_ticks: 5,
                outcome: 1,
                exit_ts: 2500,
                pnl_ticks: 10.0,
                direction: 1,
                ofi_fast: 2.5,
            },
            EventData {
                timestamp_ns: 3000, // after first trade exits
                target_ticks: 10,
                stop_ticks: 5,
                outcome: 0, // stop hit
                exit_ts: 4000,
                pnl_ticks: -5.0,
                direction: -1,
                ofi_fast: -3.0,
            },
        ];

        // All predictions high enough to trade
        let predictions = vec![0.8, 0.8, 0.8];
        let margin = 0.02;
        let commission = 1.24;

        let metrics = compute_serial_pnl(&predictions, &events, margin, commission);

        // Should take 2 trades (1st and 3rd; 2nd is locked out)
        assert_eq!(metrics.total_trades, 2);
    }

    #[test]
    fn test_calibration_bins() {
        let events: Vec<EventData> = (0..100).map(|i| EventData {
            timestamp_ns: i as u64,
            target_ticks: 10,
            stop_ticks: 5,
            outcome: if i % 3 == 0 { 1 } else { 0 },
            exit_ts: (i + 1) as u64,
            pnl_ticks: if i % 3 == 0 { 10.0 } else { -5.0 },
            direction: 1,
            ofi_fast: 0.0,
        }).collect();

        let predictions: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();

        let bins = compute_calibration(&predictions, &events);
        assert_eq!(bins.len(), 10);

        for bin in &bins {
            assert!(bin.bin_start >= 0.0 && bin.bin_end <= 1.0);
            assert!(bin.actual_rate >= 0.0 && bin.actual_rate <= 1.0);
        }
    }
}
