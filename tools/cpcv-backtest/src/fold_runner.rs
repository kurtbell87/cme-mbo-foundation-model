//! Per-fold XGBoost training, prediction, and PnL computation.

use anyhow::{Context, Result};

use backtest::CpcvSplit;
use crate::DayData;
use xgboost_ffi::training::{Booster, DMatrix};

/// Z-score normalization statistics (mean/std per feature).
pub struct NormStats {
    pub mean: [f64; 20],
    pub std: [f64; 20],
}

impl NormStats {
    /// Compute mean/std from a set of feature vectors.
    pub fn from_data(features: &[[f64; 20]]) -> Self {
        let n = features.len() as f64;
        let mut mean = [0.0f64; 20];
        let mut std = [0.0f64; 20];

        if n < 2.0 {
            return Self { mean, std: [1.0; 20] };
        }

        // Compute mean
        for row in features {
            for j in 0..20 {
                mean[j] += row[j];
            }
        }
        for j in 0..20 {
            mean[j] /= n;
        }

        // Compute std (unbiased, ddof=1)
        for row in features {
            for j in 0..20 {
                let diff = row[j] - mean[j];
                std[j] += diff * diff;
            }
        }
        for j in 0..20 {
            std[j] = (std[j] / (n - 1.0)).sqrt();
            // Floor to prevent division by zero
            if std[j] < 1e-10 {
                std[j] = 1.0;
            }
        }

        Self { mean, std }
    }

    /// Z-score normalize a single feature vector.
    pub fn normalize(&self, raw: &[f64; 20]) -> [f64; 20] {
        let mut out = [0.0f64; 20];
        for j in 0..20 {
            let z = (raw[j] - self.mean[j]) / self.std[j];
            out[j] = if z.is_nan() { 0.0 } else { z };
        }
        out
    }
}

/// Per-fold metrics for a single cost scenario.
#[derive(Debug, Clone)]
pub struct FoldMetrics {
    pub expectancy: f64,
    pub total_trades: i32,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub annualized_sharpe: f64,
    /// Daily PnL (one entry per test day).
    pub daily_pnl: Vec<f64>,
}

/// Result of running a single CPCV fold.
#[derive(Debug, Clone)]
pub struct FoldResult {
    pub split_idx: usize,
    pub test_groups: Vec<usize>,
    pub predictions: Vec<i32>,
    // Overlapping (per-bar) PnL metrics:
    pub metrics_optimistic: FoldMetrics,
    pub metrics_base: FoldMetrics,
    pub metrics_pessimistic: FoldMetrics,
    // Serial-execution PnL metrics:
    pub serial_optimistic: FoldMetrics,
    pub serial_base: FoldMetrics,
    pub serial_pessimistic: FoldMetrics,
    pub n_train_bars: usize,
    pub n_test_bars: usize,
    pub stage1_best_iter: u32,
    pub stage2_best_iter: u32,
}

/// XGBoost hyperparameters matching the Python pipeline.
const N_ESTIMATORS: u32 = 2000;
const EARLY_STOPPING_ROUNDS: u32 = 50;
/// Evaluate validation loss every N rounds (reduces O(n*i) predict overhead).
const EVAL_EVERY: u32 = 10;

/// Round-trip costs per scenario.
const RT_COST_OPTIMISTIC: f64 = 1.24;
const RT_COST_BASE: f64 = 2.49;
const RT_COST_PESSIMISTIC: f64 = 4.99;

/// Tick value: $5.00 multiplier x $0.25 tick_size = $1.25 per tick.
const TICK_VALUE: f64 = 1.25;

/// Run a single CPCV fold: normalize, train 2-stage XGBoost, predict, compute PnL.
///
/// `serial_target_override` / `serial_stop_override`: when set, serial PnL uses these
/// barrier ticks instead of `target_ticks`/`stop_ticks`. This allows decoupling the
/// label definition (used for training) from the serial execution barriers.
pub fn run_fold(
    split: &CpcvSplit,
    all_days: &[DayData],
    target_ticks: i32,
    stop_ticks: i32,
    tick_size: f64,
    nthread: i32,
    serial_target_override: Option<i32>,
    serial_stop_override: Option<i32>,
) -> Result<FoldResult> {
    // ── 1. Assemble data ──────────────────────────────────────────────────
    // Split last 20% of training days as validation (by days)
    let n_train_days = split.train_day_indices.len();
    let val_start_day = n_train_days * 4 / 5;
    let train_day_indices_actual = &split.train_day_indices[..val_start_day];
    let val_day_indices = &split.train_day_indices[val_start_day..];

    let mut actual_train_features: Vec<[f64; 20]> = Vec::new();
    let mut actual_train_labels: Vec<i32> = Vec::new();
    for &day_idx in train_day_indices_actual {
        let day = &all_days[day_idx];
        actual_train_features.extend_from_slice(&day.features);
        actual_train_labels.extend_from_slice(&day.labels);
    }

    let mut val_features: Vec<[f64; 20]> = Vec::new();
    let mut val_labels: Vec<i32> = Vec::new();
    for &day_idx in val_day_indices {
        let day = &all_days[day_idx];
        val_features.extend_from_slice(&day.features);
        val_labels.extend_from_slice(&day.labels);
    }

    // Collect test data (pre-filtered: no warmup, no NaN fwd)
    let mut test_features: Vec<[f64; 20]> = Vec::new();
    let mut test_labels: Vec<i32> = Vec::new();
    let mut test_fwd_returns: Vec<f64> = Vec::new();
    let mut test_dates: Vec<i32> = Vec::new();
    for &day_idx in &split.test_day_indices {
        let day = &all_days[day_idx];
        test_features.extend_from_slice(&day.features);
        test_labels.extend_from_slice(&day.labels);
        test_fwd_returns.extend_from_slice(&day.fwd_returns);
        test_dates.extend(vec![day.date; day.features.len()]);
    }

    let n_train = actual_train_features.len();
    let n_test = test_features.len();

    // ── 2. Normalize ──────────────────────────────────────────────────────
    let norm = NormStats::from_data(&actual_train_features);

    let norm_train: Vec<[f64; 20]> = actual_train_features
        .iter()
        .map(|f| norm.normalize(f))
        .collect();
    let norm_val: Vec<[f64; 20]> = val_features
        .iter()
        .map(|f| norm.normalize(f))
        .collect();
    let norm_test: Vec<[f64; 20]> = test_features
        .iter()
        .map(|f| norm.normalize(f))
        .collect();

    // ── 3. Train Stage 1: Directional filter (label != 0) ─────────────
    let s1_train_y: Vec<f32> = actual_train_labels
        .iter()
        .map(|&l| if l != 0 { 1.0 } else { 0.0 })
        .collect();
    let s1_val_y: Vec<f32> = val_labels
        .iter()
        .map(|&l| if l != 0 { 1.0 } else { 0.0 })
        .collect();

    let (stage1_booster, stage1_best_iter) = train_xgboost_binary(
        &norm_train,
        &s1_train_y,
        &norm_val,
        &s1_val_y,
        nthread,
    )
    .context("Stage 1 training failed")?;

    // ── 4. Train Stage 2: Direction (label == 1 vs label == -1) ────────
    let dir_train_feat: Vec<[f64; 20]> = norm_train
        .iter()
        .zip(actual_train_labels.iter())
        .filter(|(_, &l)| l != 0)
        .map(|(f, _)| *f)
        .collect();
    let dir_train_y: Vec<f32> = actual_train_labels
        .iter()
        .filter(|&&l| l != 0)
        .map(|&l| if l == 1 { 1.0 } else { 0.0 })
        .collect();
    let dir_val_feat: Vec<[f64; 20]> = norm_val
        .iter()
        .zip(val_labels.iter())
        .filter(|(_, &l)| l != 0)
        .map(|(f, _)| *f)
        .collect();
    let dir_val_y: Vec<f32> = val_labels
        .iter()
        .filter(|&&l| l != 0)
        .map(|&l| if l == 1 { 1.0 } else { 0.0 })
        .collect();

    let (stage2_booster, stage2_best_iter) = if !dir_train_feat.is_empty() && !dir_val_feat.is_empty() {
        train_xgboost_binary(
            &dir_train_feat,
            &dir_train_y,
            &dir_val_feat,
            &dir_val_y,
            nthread,
        )
        .context("Stage 2 training failed")?
    } else {
        // Fallback: no directional bars — create dummy booster
        let dummy_feat: Vec<[f64; 20]> = vec![[0.0; 20]];
        let dummy_y = vec![0.5f32];
        train_xgboost_binary(&dummy_feat, &dummy_y, &dummy_feat, &dummy_y, nthread)?
    };

    // ── 5. Predict on test set with ntree_limit ───────────────────────────
    let test_flat_f32 = flatten_f32(&norm_test);
    let test_dmat = DMatrix::from_dense(&test_flat_f32, norm_test.len(), 20)
        .map_err(|e| anyhow::anyhow!("Failed to create test DMatrix: {}", e))?;

    let p_directional = stage1_booster
        .predict(&test_dmat, stage1_best_iter + 1)
        .map_err(|e| anyhow::anyhow!("Stage 1 prediction failed: {}", e))?;
    let p_long = stage2_booster
        .predict(&test_dmat, stage2_best_iter + 1)
        .map_err(|e| anyhow::anyhow!("Stage 2 prediction failed: {}", e))?;

    let predictions: Vec<i32> = p_directional
        .iter()
        .zip(p_long.iter())
        .map(|(&pd, &pl)| {
            if pd < 0.50 {
                0
            } else if pl > 0.50 {
                1
            } else {
                -1
            }
        })
        .collect();

    // ── 6. Compute overlapping PnL ─────────────────────────────────────
    let metrics_optimistic = compute_fold_pnl(
        &predictions,
        &test_labels,
        &test_fwd_returns,
        &test_dates,
        target_ticks,
        stop_ticks,
        RT_COST_OPTIMISTIC,
    );
    let metrics_base = compute_fold_pnl(
        &predictions,
        &test_labels,
        &test_fwd_returns,
        &test_dates,
        target_ticks,
        stop_ticks,
        RT_COST_BASE,
    );
    let metrics_pessimistic = compute_fold_pnl(
        &predictions,
        &test_labels,
        &test_fwd_returns,
        &test_dates,
        target_ticks,
        stop_ticks,
        RT_COST_PESSIMISTIC,
    );

    // ── 7. Compute serial-execution PnL ─────────────────────────────────
    // Build test day data references and offsets for serial PnL
    let test_day_data: Vec<&DayData> = split
        .test_day_indices
        .iter()
        .map(|&idx| &all_days[idx])
        .collect();
    let mut test_day_offsets: Vec<usize> = Vec::with_capacity(test_day_data.len());
    let mut offset = 0;
    for day in &test_day_data {
        test_day_offsets.push(offset);
        offset += day.features.len();
    }

    // Check if test days have tick-level data
    let has_tick_data = test_day_data.iter().any(|d| !d.tick_mids.is_empty());

    let serial_fn = if has_tick_data {
        compute_serial_pnl_ticks
    } else {
        compute_serial_pnl
    };

    let ser_target = serial_target_override.unwrap_or(target_ticks) as f64;
    let ser_stop = serial_stop_override.unwrap_or(stop_ticks) as f64;

    let serial_optimistic = serial_fn(
        &predictions,
        &test_fwd_returns,
        &test_day_data,
        &test_day_offsets,
        ser_target,
        ser_stop,
        tick_size,
        RT_COST_OPTIMISTIC,
    );
    let serial_base = serial_fn(
        &predictions,
        &test_fwd_returns,
        &test_day_data,
        &test_day_offsets,
        ser_target,
        ser_stop,
        tick_size,
        RT_COST_BASE,
    );
    let serial_pessimistic = serial_fn(
        &predictions,
        &test_fwd_returns,
        &test_day_data,
        &test_day_offsets,
        ser_target,
        ser_stop,
        tick_size,
        RT_COST_PESSIMISTIC,
    );

    Ok(FoldResult {
        split_idx: split.split_idx,
        test_groups: split.test_groups.clone(),
        predictions,
        metrics_optimistic,
        metrics_base,
        metrics_pessimistic,
        serial_optimistic,
        serial_base,
        serial_pessimistic,
        n_train_bars: n_train,
        n_test_bars: n_test,
        stage1_best_iter,
        stage2_best_iter,
    })
}

/// Set all XGBoost hyperparameters on a Booster (exact float values via string API).
fn set_hyperparams(booster: &Booster, nthread: i32) -> Result<()> {
    let nthread_str = nthread.to_string();
    let params = [
        ("objective", "binary:logistic"),
        ("max_depth", "6"),
        ("eta", "0.0134"),
        ("min_child_weight", "20"),
        ("subsample", "0.561"),
        ("colsample_bytree", "0.748"),
        ("reg_alpha", "0.0014"),
        ("reg_lambda", "6.586"),
        ("verbosity", "0"),
        ("nthread", nthread_str.as_str()),
    ];
    for (name, value) in &params {
        booster.set_param(name, value)
            .map_err(|e| anyhow::anyhow!("set_param({}, {}): {}", name, value, e))?;
    }
    Ok(())
}

/// Train a binary XGBoost model with manual early stopping.
///
/// Returns the booster and best iteration. Use `ntree_limit = best_iter + 1`
/// on predict to use only the best trees (no retrain needed).
fn train_xgboost_binary(
    train_features: &[[f64; 20]],
    train_labels: &[f32],
    val_features: &[[f64; 20]],
    val_labels: &[f32],
    nthread: i32,
) -> Result<(Booster, u32)> {
    let train_flat = flatten_f32(train_features);
    let val_flat = flatten_f32(val_features);

    let dtrain = DMatrix::from_dense(&train_flat, train_features.len(), 20)
        .map_err(|e| anyhow::anyhow!("Failed to create training DMatrix: {}", e))?;
    dtrain.set_labels(train_labels)
        .map_err(|e| anyhow::anyhow!("Failed to set training labels: {}", e))?;

    let dval = DMatrix::from_dense(&val_flat, val_features.len(), 20)
        .map_err(|e| anyhow::anyhow!("Failed to create validation DMatrix: {}", e))?;
    dval.set_labels(val_labels)
        .map_err(|e| anyhow::anyhow!("Failed to set validation labels: {}", e))?;

    let booster = Booster::new(&[&dtrain, &dval])
        .map_err(|e| anyhow::anyhow!("Failed to create booster: {}", e))?;
    set_hyperparams(&booster, nthread)?;

    let mut best_loss = f64::MAX;
    let mut best_iter = 0u32;
    let mut rounds_no_improve = 0u32;

    for i in 0..N_ESTIMATORS {
        booster.update(&dtrain, i)
            .map_err(|e| anyhow::anyhow!("Update failed at iteration {}: {}", i, e))?;

        // Evaluate every EVAL_EVERY rounds to reduce O(n_val * ntree_limit) prediction cost
        if (i + 1) % EVAL_EVERY != 0 {
            continue;
        }

        let val_preds = booster.predict(&dval, i + 1)
            .map_err(|e| anyhow::anyhow!("Validation prediction failed: {}", e))?;
        let loss = binary_logloss(&val_preds, val_labels);

        if loss < best_loss {
            best_loss = loss;
            best_iter = i;
            rounds_no_improve = 0;
        } else {
            rounds_no_improve += EVAL_EVERY;
        }

        if rounds_no_improve >= EARLY_STOPPING_ROUNDS {
            break;
        }
    }

    Ok((booster, best_iter))
}

/// Compute binary log-loss.
fn binary_logloss(preds: &[f32], labels: &[f32]) -> f64 {
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

/// Flatten a slice of [f64; 20] arrays into a Vec<f32> (row-major) for DMatrix.
fn flatten_f32(features: &[[f64; 20]]) -> Vec<f32> {
    features.iter().flat_map(|row| row.iter().map(|&v| v as f32)).collect()
}

/// Compute serial-execution PnL via barrier re-simulation.
///
/// Walks through filtered bars chronologically per day. When the model signals a trade
/// and no position is open, enters at `close_mid[bar_index]` and walks forward through
/// ALL bars (not just filtered) to find the first barrier crossing.
fn compute_serial_pnl(
    predictions: &[i32],
    fwd_returns: &[f64],
    test_day_data: &[&DayData],
    test_day_offsets: &[usize],
    target_ticks: f64,
    stop_ticks: f64,
    tick_size: f64,
    rt_cost: f64,
) -> FoldMetrics {
    let mut total_trades = 0i32;
    let mut wins = 0i32;
    let mut gross_wins = 0.0f64;
    let mut gross_losses = 0.0f64;
    let mut daily_pnl_values: Vec<f64> = Vec::new();

    for (day_i, day) in test_day_data.iter().enumerate() {
        let day_start = test_day_offsets[day_i];
        let day_end = if day_i + 1 < test_day_offsets.len() {
            test_day_offsets[day_i + 1]
        } else {
            predictions.len()
        };
        let n_filtered = day_end - day_start;

        let mut day_pnl = 0.0f64;
        let mut next_available_bar: usize = 0; // Parquet row index; nothing held yet

        for local_j in 0..n_filtered {
            let j = day_start + local_j;
            let orig = day.bar_indices[local_j];

            // Still holding a position from a previous trade
            if orig < next_available_bar {
                continue;
            }

            // No signal
            if predictions[j] == 0 {
                continue;
            }

            let entry_price = day.close_mids[orig];
            let direction = predictions[j] as f64;

            // Walk forward through ALL bars to find barrier crossing
            let horizon_end = (orig + 720).min(day.n_bars);
            let mut exited = false;

            for k in (orig + 1)..=horizon_end {
                if k >= day.n_bars {
                    break;
                }
                let move_ticks =
                    (day.close_mids[k] - entry_price) * direction / tick_size;

                if move_ticks >= target_ticks {
                    // Target hit
                    let pnl = target_ticks * TICK_VALUE - rt_cost;
                    day_pnl += pnl;
                    total_trades += 1;
                    if pnl > 0.0 {
                        wins += 1;
                        gross_wins += pnl;
                    } else {
                        gross_losses += pnl.abs();
                    }
                    next_available_bar = k + 1;
                    exited = true;
                    break;
                }

                if move_ticks <= -stop_ticks {
                    // Stop hit
                    let pnl = -stop_ticks * TICK_VALUE - rt_cost;
                    day_pnl += pnl;
                    total_trades += 1;
                    if pnl > 0.0 {
                        wins += 1;
                        gross_wins += pnl;
                    } else {
                        gross_losses += pnl.abs();
                    }
                    next_available_bar = k + 1;
                    exited = true;
                    break;
                }
            }

            if !exited {
                // Horizon exhausted — use actual fwd_return
                let pnl = fwd_returns[j] * TICK_VALUE * direction - rt_cost;
                day_pnl += pnl;
                total_trades += 1;
                if pnl > 0.0 {
                    wins += 1;
                    gross_wins += pnl;
                } else {
                    gross_losses += pnl.abs();
                }
                next_available_bar = orig + 720 + 1;
            }
        }

        daily_pnl_values.push(day_pnl);
    }

    let expectancy = if total_trades > 0 {
        (gross_wins - gross_losses) / total_trades as f64
    } else {
        0.0
    };

    let win_rate = if total_trades > 0 {
        wins as f64 / total_trades as f64
    } else {
        0.0
    };

    let profit_factor = if gross_losses > 0.0 {
        gross_wins / gross_losses
    } else if gross_wins > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let annualized_sharpe = compute_sharpe_from_daily(&daily_pnl_values);

    FoldMetrics {
        expectancy,
        total_trades,
        win_rate,
        profit_factor,
        annualized_sharpe,
        daily_pnl: daily_pnl_values,
    }
}

/// Compute serial-execution PnL via tick-level barrier re-simulation.
///
/// Uses (timestamp_ns, mid_price) tick series instead of bar-level close_mid.
/// Timestamps gate serial execution (no new trade until current exits).
/// Binary search for entry/exit tick positions. Time-based horizon (3600s).
fn compute_serial_pnl_ticks(
    predictions: &[i32],
    fwd_returns: &[f64],
    test_day_data: &[&DayData],
    test_day_offsets: &[usize],
    target_ticks: f64,
    stop_ticks: f64,
    tick_size: f64,
    rt_cost: f64,
) -> FoldMetrics {
    let mut total_trades = 0i32;
    let mut wins = 0i32;
    let mut gross_wins = 0.0f64;
    let mut gross_losses = 0.0f64;
    let mut daily_pnl_values: Vec<f64> = Vec::new();

    /// Horizon in nanoseconds (3600 seconds).
    const HORIZON_NS: u64 = 3_600_000_000_000;
    /// Staleness threshold for EOD fallback (30 seconds in ns).
    const STALENESS_NS: u64 = 30_000_000_000;

    for (day_i, day) in test_day_data.iter().enumerate() {
        let day_start = test_day_offsets[day_i];
        let day_end = if day_i + 1 < test_day_offsets.len() {
            test_day_offsets[day_i + 1]
        } else {
            predictions.len()
        };
        let n_filtered = day_end - day_start;
        let tick_mids = &day.tick_mids;

        if tick_mids.is_empty() {
            // No tick data for this day — skip (shouldn't happen when tick dir is provided)
            daily_pnl_values.push(0.0);
            continue;
        }

        let mut day_pnl = 0.0f64;
        let mut next_available_ts: u64 = 0; // nanosecond timestamp; nothing held yet

        for local_j in 0..n_filtered {
            let j = day_start + local_j;
            let orig = day.bar_indices[local_j]; // Parquet row of this filtered bar
            let bar_close_ts = day.bar_close_timestamps[orig];

            // Still holding a position from a previous trade
            if bar_close_ts < next_available_ts {
                continue;
            }

            // No signal
            if predictions[j] == 0 {
                continue;
            }

            // Find entry tick via binary search on tick_mids timestamps
            let entry_tick_idx = tick_mids.partition_point(|(ts, _)| *ts <= bar_close_ts);
            if entry_tick_idx == 0 {
                continue;
            }
            let entry_price = tick_mids[entry_tick_idx - 1].1 as f64;
            let direction = predictions[j] as f64;
            let horizon_ts = bar_close_ts + HORIZON_NS;

            // Walk forward through ticks checking barriers
            let mut exited = false;
            for &(ts, price) in &tick_mids[entry_tick_idx..] {
                if ts > horizon_ts {
                    break;
                }
                let move_ticks = (price as f64 - entry_price) * direction / tick_size;

                if move_ticks >= target_ticks {
                    // Target hit
                    let pnl = target_ticks * TICK_VALUE - rt_cost;
                    day_pnl += pnl;
                    total_trades += 1;
                    if pnl > 0.0 {
                        wins += 1;
                        gross_wins += pnl;
                    } else {
                        gross_losses += pnl.abs();
                    }
                    next_available_ts = ts;
                    exited = true;
                    break;
                }

                if move_ticks <= -stop_ticks {
                    // Stop hit
                    let pnl = -stop_ticks * TICK_VALUE - rt_cost;
                    day_pnl += pnl;
                    total_trades += 1;
                    if pnl > 0.0 {
                        wins += 1;
                        gross_wins += pnl;
                    } else {
                        gross_losses += pnl.abs();
                    }
                    next_available_ts = ts;
                    exited = true;
                    break;
                }
            }

            if !exited {
                // Horizon exhausted — compute return from tick data at horizon_ts
                let horizon_tick_idx = tick_mids.partition_point(|(ts, _)| *ts <= horizon_ts);
                if horizon_tick_idx > 0 {
                    let horizon_tick = &tick_mids[horizon_tick_idx - 1];
                    let staleness = horizon_ts.saturating_sub(horizon_tick.0);

                    let pnl = if staleness > STALENESS_NS {
                        // Stale tick near EOD — fall back to bar-level fwd_return
                        fwd_returns[j] * TICK_VALUE * direction - rt_cost
                    } else {
                        let exit_price = horizon_tick.1 as f64;
                        (exit_price - entry_price) * direction / tick_size * TICK_VALUE - rt_cost
                    };

                    day_pnl += pnl;
                    total_trades += 1;
                    if pnl > 0.0 {
                        wins += 1;
                        gross_wins += pnl;
                    } else {
                        gross_losses += pnl.abs();
                    }
                } else {
                    // No tick data after entry — use bar-level fallback
                    let pnl = fwd_returns[j] * TICK_VALUE * direction - rt_cost;
                    day_pnl += pnl;
                    total_trades += 1;
                    if pnl > 0.0 {
                        wins += 1;
                        gross_wins += pnl;
                    } else {
                        gross_losses += pnl.abs();
                    }
                }
                next_available_ts = horizon_ts;
            }
        }

        daily_pnl_values.push(day_pnl);
    }

    let expectancy = if total_trades > 0 {
        (gross_wins - gross_losses) / total_trades as f64
    } else {
        0.0
    };

    let win_rate = if total_trades > 0 {
        wins as f64 / total_trades as f64
    } else {
        0.0
    };

    let profit_factor = if gross_losses > 0.0 {
        gross_wins / gross_losses
    } else if gross_wins > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let annualized_sharpe = compute_sharpe_from_daily(&daily_pnl_values);

    FoldMetrics {
        expectancy,
        total_trades,
        win_rate,
        profit_factor,
        annualized_sharpe,
        daily_pnl: daily_pnl_values,
    }
}

/// Compute per-bar PnL and aggregate to daily PnL + fold metrics.
fn compute_fold_pnl(
    predictions: &[i32],
    labels: &[i32],
    fwd_returns: &[f64],
    dates: &[i32],
    target_ticks: i32,
    stop_ticks: i32,
    rt_cost: f64,
) -> FoldMetrics {
    let target_ticks = target_ticks as f64;
    let stop_ticks = stop_ticks as f64;

    let mut total_trades = 0i32;
    let mut wins = 0i32;
    let mut gross_wins = 0.0f64;
    let mut gross_losses = 0.0f64;

    // Collect (date, pnl) per bar
    let mut bar_pnls: Vec<(i32, f64)> = Vec::new();

    for i in 0..predictions.len() {
        let pred = predictions[i];
        if pred == 0 {
            continue; // No trade
        }

        total_trades += 1;
        let label = labels[i];
        let pnl: f64;

        if label != 0 {
            // Directional bar: discrete target/stop outcome
            let correct = (label > 0 && pred > 0) || (label < 0 && pred < 0);
            if correct {
                pnl = target_ticks * TICK_VALUE - rt_cost;
            } else {
                pnl = -stop_ticks * TICK_VALUE - rt_cost;
            }
        } else {
            // Hold bar: use forward return
            let fwd = fwd_returns[i];
            if fwd.is_nan() {
                continue; // No forward return available, skip
            }
            pnl = fwd * TICK_VALUE * (pred as f64).signum() - rt_cost;
        }

        if pnl > 0.0 {
            wins += 1;
            gross_wins += pnl;
        } else {
            gross_losses += pnl.abs();
        }

        bar_pnls.push((dates[i], pnl));
    }

    // Aggregate to daily PnL
    let mut daily_pnl: Vec<(i32, f64)> = Vec::new();
    bar_pnls.sort_by_key(|(d, _)| *d);
    let mut i = 0;
    while i < bar_pnls.len() {
        let date = bar_pnls[i].0;
        let mut day_sum = 0.0;
        while i < bar_pnls.len() && bar_pnls[i].0 == date {
            day_sum += bar_pnls[i].1;
            i += 1;
        }
        daily_pnl.push((date, day_sum));
    }

    let daily_pnl_values: Vec<f64> = daily_pnl.iter().map(|(_, p)| *p).collect();

    // Compute metrics
    let expectancy = if total_trades > 0 {
        (gross_wins - gross_losses) / total_trades as f64
    } else {
        0.0
    };

    let win_rate = if total_trades > 0 {
        wins as f64 / total_trades as f64
    } else {
        0.0
    };

    let profit_factor = if gross_losses > 0.0 {
        gross_wins / gross_losses
    } else if gross_wins > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let annualized_sharpe = compute_sharpe_from_daily(&daily_pnl_values);

    FoldMetrics {
        expectancy,
        total_trades,
        win_rate,
        profit_factor,
        annualized_sharpe,
        daily_pnl: daily_pnl_values,
    }
}

/// Annualized Sharpe from daily PnL: (mean / std) * sqrt(252).
fn compute_sharpe_from_daily(daily_pnl: &[f64]) -> f64 {
    if daily_pnl.len() < 2 {
        return 0.0;
    }
    let n = daily_pnl.len() as f64;
    let mean = daily_pnl.iter().sum::<f64>() / n;
    let variance = daily_pnl.iter().map(|&d| (d - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std = variance.sqrt();
    if std > 0.0 {
        (mean / std) * (252.0f64).sqrt()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_norm_stats_identity() {
        let data = vec![[1.0; 20], [3.0; 20]];
        let norm = NormStats::from_data(&data);
        assert!((norm.mean[0] - 2.0).abs() < 1e-10);
        let normalized = norm.normalize(&[3.0; 20]);
        // z = (3 - 2) / std; std = sqrt(2) ≈ 1.414
        assert!((normalized[0] - 0.7071).abs() < 0.01);
    }

    #[test]
    fn test_binary_logloss() {
        let preds = vec![0.9, 0.1, 0.8];
        let labels = vec![1.0, 0.0, 1.0];
        let loss = binary_logloss(&preds, &labels);
        assert!(loss < 0.2); // Should be low for good predictions
    }

    #[test]
    fn test_sharpe_flat_pnl() {
        let pnl = vec![1.0, 1.0, 1.0, 1.0, 1.0];
        let sharpe = compute_sharpe_from_daily(&pnl);
        // All same → std = 0 → sharpe = 0 (degenerate case)
        assert_eq!(sharpe, 0.0);
    }

    #[test]
    fn test_sharpe_positive() {
        let pnl = vec![10.0, 12.0, 8.0, 11.0, 9.0];
        let sharpe = compute_sharpe_from_daily(&pnl);
        assert!(sharpe > 0.0);
    }

    #[test]
    fn test_flatten_f32() {
        let features = vec![[1.0f64; 20], [2.0f64; 20]];
        let flat = flatten_f32(&features);
        assert_eq!(flat.len(), 40);
        assert_eq!(flat[0], 1.0f32);
        assert_eq!(flat[20], 2.0f32);
    }
}
