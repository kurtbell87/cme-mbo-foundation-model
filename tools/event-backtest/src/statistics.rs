//! Cross-fold aggregation: expectancy CI, Sharpe, DSR, geometry breakdown,
//! feature importance stability, calibration.

use serde::Serialize;

use crate::fold_runner::FoldResult;

/// Per-fold summary for JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct FoldSummary {
    pub split_idx: usize,
    pub test_groups: Vec<usize>,
    pub n_train: usize,
    pub n_test: usize,
    pub expectancy: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub annualized_sharpe: f64,
    pub total_trades: i32,
    pub net_pnl: f64,
    pub best_iter: u32,
}

/// Aggregated geometry-level results across folds.
#[derive(Debug, Clone, Serialize)]
pub struct GeometrySummary {
    pub target_ticks: i32,
    pub stop_ticks: i32,
    pub p_null: f64,
    pub mean_p_model: f64,
    pub total_predictions: usize,
    pub total_trades: usize,
    pub mean_win_rate: f64,
}

/// Aggregated calibration bin across folds.
#[derive(Debug, Clone, Serialize)]
pub struct AggCalibrationBin {
    pub bin_start: f64,
    pub bin_end: f64,
    pub mean_predicted: f64,
    pub actual_rate: f64,
    pub total_count: usize,
}

/// Top-level CPCV report for event-level model.
#[derive(Debug, Clone, Serialize)]
pub struct EventCpcvReport {
    pub n_folds: usize,
    pub n_days: usize,
    pub subsample_pct: u32,
    pub margin: f64,
    pub commission: f64,
    // Aggregate metrics
    pub mean_expectancy: f64,
    pub std_expectancy: f64,
    pub ci_95: (f64, f64),
    pub negative_fold_fraction: f64,
    pub annualized_sharpe: f64,
    pub deflated_sharpe: f64,
    pub pooled_profit_factor: f64,
    pub pooled_win_rate: f64,
    pub total_trades: i64,
    pub total_net_pnl: f64,
    // Per-fold
    pub fold_summaries: Vec<FoldSummary>,
    // Per-geometry
    pub geometry_summaries: Vec<GeometrySummary>,
    // Calibration
    pub calibration: Vec<AggCalibrationBin>,
    // Ljung-Box on pooled trade PnL
    pub ljung_box: Option<LjungBoxResult>,
}

/// Ljung-Box autocorrelation test result.
#[derive(Debug, Clone, Serialize)]
pub struct LjungBoxResult {
    pub q_statistic: f64,
    pub p_value: f64,
    pub max_lag: usize,
}

/// Aggregate fold results into a full report.
pub fn aggregate_results(
    fold_results: &[FoldResult],
    n_days: usize,
    subsample_pct: u32,
    margin: f64,
    commission: f64,
) -> EventCpcvReport {
    let n_folds = fold_results.len();

    // ── Expectancy statistics ───────────────────────────────────────────
    let expectancies: Vec<f64> = fold_results.iter().map(|r| r.metrics.expectancy).collect();
    let mean_exp = mean(&expectancies);
    let std_exp = std_dev(&expectancies);
    let ci_95 = t_confidence_interval(&expectancies, 0.95);
    let neg_folds = expectancies.iter().filter(|&&e| e < 0.0).count();
    let negative_fold_fraction = neg_folds as f64 / n_folds.max(1) as f64;

    // ── Pooled metrics ──────────────────────────────────────────────────
    let mut total_trades: i64 = 0;
    let mut total_wins: i64 = 0;
    let mut total_gross_wins = 0.0f64;
    let mut total_gross_losses = 0.0f64;
    let mut total_net_pnl = 0.0f64;
    let mut all_trade_pnls: Vec<f64> = Vec::new();

    for r in fold_results {
        let m = &r.metrics;
        total_trades += m.total_trades as i64;
        total_wins += (m.win_rate * m.total_trades as f64).round() as i64;
        total_net_pnl += m.net_pnl;
        for &pnl in &m.trade_pnls {
            all_trade_pnls.push(pnl);
            if pnl > 0.0 {
                total_gross_wins += pnl;
            } else {
                total_gross_losses += pnl.abs();
            }
        }
    }

    let pooled_win_rate = if total_trades > 0 {
        total_wins as f64 / total_trades as f64
    } else {
        0.0
    };
    let pooled_profit_factor = if total_gross_losses > 0.0 {
        total_gross_wins / total_gross_losses
    } else if total_gross_wins > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    // ── Annualized Sharpe from pooled trade PnL ─────────────────────────
    let annualized_sharpe = compute_pooled_sharpe(&all_trade_pnls);
    let deflated_sharpe = deflated_sharpe_ratio(annualized_sharpe, n_folds);

    // ── Per-fold summaries ──────────────────────────────────────────────
    let fold_summaries: Vec<FoldSummary> = fold_results
        .iter()
        .map(|r| FoldSummary {
            split_idx: r.split_idx,
            test_groups: r.test_groups.clone(),
            n_train: r.n_train,
            n_test: r.n_test,
            expectancy: r.metrics.expectancy,
            win_rate: r.metrics.win_rate,
            profit_factor: r.metrics.profit_factor,
            annualized_sharpe: r.metrics.annualized_sharpe,
            total_trades: r.metrics.total_trades,
            net_pnl: r.metrics.net_pnl,
            best_iter: r.best_iter,
        })
        .collect();

    // ── Per-geometry summaries ──────────────────────────────────────────
    let geometry_summaries = aggregate_geometry_metrics(fold_results);

    // ── Calibration ─────────────────────────────────────────────────────
    let calibration = aggregate_calibration(fold_results);

    // ── Ljung-Box on pooled trade PnL ───────────────────────────────────
    let ljung_box = if all_trade_pnls.len() > 20 {
        let (q, p) = ljung_box_test(&all_trade_pnls, 10);
        Some(LjungBoxResult {
            q_statistic: q,
            p_value: p,
            max_lag: 10,
        })
    } else {
        None
    };

    EventCpcvReport {
        n_folds,
        n_days,
        subsample_pct,
        margin,
        commission,
        mean_expectancy: mean_exp,
        std_expectancy: std_exp,
        ci_95,
        negative_fold_fraction,
        annualized_sharpe,
        deflated_sharpe,
        pooled_profit_factor,
        pooled_win_rate,
        total_trades,
        total_net_pnl,
        fold_summaries,
        geometry_summaries,
        calibration,
        ljung_box,
    }
}

/// Aggregate geometry metrics across folds.
fn aggregate_geometry_metrics(fold_results: &[FoldResult]) -> Vec<GeometrySummary> {
    use std::collections::BTreeMap;

    let mut agg: BTreeMap<(i32, i32), (f64, f64, usize, usize, f64, usize)> = BTreeMap::new();
    // key: (T, S) → (p_null, sum_p_model, total_preds, total_trades, sum_win_rate, n_folds_with_trades)

    for r in fold_results {
        for g in &r.geometry_metrics {
            let entry = agg.entry((g.target_ticks, g.stop_ticks)).or_insert((
                g.p_null, 0.0, 0, 0, 0.0, 0,
            ));
            entry.1 += g.p_model_mean * g.n_predictions as f64;
            entry.2 += g.n_predictions;
            entry.3 += g.n_trades;
            if g.n_trades > 0 {
                entry.4 += g.trade_win_rate;
                entry.5 += 1;
            }
        }
    }

    agg.into_iter()
        .map(|((t, s), (p_null, sum_p_model, total_preds, total_trades, sum_wr, n_wr))| {
            GeometrySummary {
                target_ticks: t,
                stop_ticks: s,
                p_null,
                mean_p_model: if total_preds > 0 {
                    sum_p_model / total_preds as f64
                } else {
                    0.0
                },
                total_predictions: total_preds,
                total_trades,
                mean_win_rate: if n_wr > 0 {
                    sum_wr / n_wr as f64
                } else {
                    0.0
                },
            }
        })
        .collect()
}

/// Aggregate calibration bins across folds.
fn aggregate_calibration(fold_results: &[FoldResult]) -> Vec<AggCalibrationBin> {
    if fold_results.is_empty() {
        return Vec::new();
    }

    let n_bins = fold_results[0].calibration_bins.len();
    if n_bins == 0 {
        return Vec::new();
    }

    let mut sum_pred = vec![0.0f64; n_bins];
    let mut sum_actual = vec![0.0f64; n_bins];
    let mut total_count = vec![0usize; n_bins];

    for r in fold_results {
        for (b, bin) in r.calibration_bins.iter().enumerate() {
            sum_pred[b] += bin.mean_predicted * bin.count as f64;
            sum_actual[b] += bin.actual_rate * bin.count as f64;
            total_count[b] += bin.count;
        }
    }

    (0..n_bins)
        .map(|b| {
            let ref_bin = &fold_results[0].calibration_bins[b];
            let count = total_count[b];
            AggCalibrationBin {
                bin_start: ref_bin.bin_start,
                bin_end: ref_bin.bin_end,
                mean_predicted: if count > 0 {
                    sum_pred[b] / count as f64
                } else {
                    ref_bin.mean_predicted
                },
                actual_rate: if count > 0 {
                    sum_actual[b] / count as f64
                } else {
                    0.0
                },
                total_count: count,
            }
        })
        .collect()
}

/// Compute annualized Sharpe from pooled per-trade PnL.
///
/// Assumes ~252 trading days × ~50 trades/day = ~12600 trades/year.
fn compute_pooled_sharpe(trade_pnls: &[f64]) -> f64 {
    if trade_pnls.len() < 2 {
        return 0.0;
    }
    let n = trade_pnls.len() as f64;
    let m = trade_pnls.iter().sum::<f64>() / n;
    let var = trade_pnls.iter().map(|&d| (d - m).powi(2)).sum::<f64>() / (n - 1.0);
    let std = var.sqrt();

    if std < 1e-12 {
        return 0.0;
    }

    let trades_per_year = 252.0 * 50.0;
    m / std * (trades_per_year / n).sqrt().min(trades_per_year.sqrt())
}

/// Deflated Sharpe Ratio (Bailey & Lopez de Prado, 2014).
fn deflated_sharpe_ratio(sharpe: f64, n_trials: usize) -> f64 {
    if n_trials <= 1 || sharpe <= 0.0 {
        return 0.0;
    }
    let n = n_trials as f64;
    let euler_mascheroni = 0.5772156649;
    let sqrt_2ln_n = (2.0 * n.ln()).sqrt();
    let e_max_sr = sqrt_2ln_n
        - (euler_mascheroni + (std::f64::consts::PI / 2.0).ln()) / (2.0 * sqrt_2ln_n);
    let z = sharpe - e_max_sr;
    normal_cdf(z)
}

/// Standard normal CDF approximation (Abramowitz & Stegun).
fn normal_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327; // 1/sqrt(2*pi)
    let p = d * (-x * x / 2.0).exp();
    let poly = t
        * (0.319381530
            + t * (-0.356563782
                + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    if x >= 0.0 {
        1.0 - p * poly
    } else {
        p * poly
    }
}

/// Ljung-Box test for autocorrelation.
fn ljung_box_test(data: &[f64], max_lag: usize) -> (f64, f64) {
    let n = data.len();
    if n <= max_lag + 1 {
        return (0.0, 1.0);
    }

    let nf = n as f64;
    let m = data.iter().sum::<f64>() / nf;
    let var: f64 = data.iter().map(|&x| (x - m).powi(2)).sum();
    if var < 1e-15 {
        return (0.0, 1.0);
    }

    let mut q = 0.0;
    for lag in 1..=max_lag {
        let mut autocov = 0.0;
        for t in lag..n {
            autocov += (data[t] - m) * (data[t - lag] - m);
        }
        let rho = autocov / var;
        q += rho * rho / (nf - lag as f64);
    }
    q *= nf * (nf + 2.0);

    let p_value = 1.0 - chi_squared_cdf(q, max_lag);
    (q, p_value)
}

/// Chi-squared CDF via Wilson-Hilferty normal approximation.
fn chi_squared_cdf(x: f64, df: usize) -> f64 {
    if x <= 0.0 || df == 0 {
        return 0.0;
    }
    let k = df as f64;
    let ratio = 2.0 / (9.0 * k);
    let cube_root = (x / k).powf(1.0 / 3.0);
    let z = (cube_root - (1.0 - ratio)) / ratio.sqrt();
    normal_cdf(z)
}

fn mean(data: &[f64]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().sum::<f64>() / data.len() as f64
}

fn std_dev(data: &[f64]) -> f64 {
    if data.len() < 2 {
        return 0.0;
    }
    let m = mean(data);
    let n = data.len() as f64;
    let var = data.iter().map(|&x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

fn t_confidence_interval(data: &[f64], _confidence: f64) -> (f64, f64) {
    let n = data.len();
    if n < 2 {
        let m = mean(data);
        return (m, m);
    }
    let m = mean(data);
    let s = std_dev(data);
    let t_crit = t_critical_95(n - 1);
    let margin = t_crit * s / (n as f64).sqrt();
    (m - margin, m + margin)
}

fn t_critical_95(df: usize) -> f64 {
    match df {
        1 => 12.706,
        2 => 4.303,
        3 => 3.182,
        4 => 2.776,
        5 => 2.571,
        10 => 2.228,
        20 => 2.086,
        30 => 2.042,
        40 => 2.021,
        44 => 2.015,
        50 => 2.009,
        100 => 1.984,
        _ => {
            if df < 10 {
                2.571 + (10 - df) as f64 * 0.05
            } else if df < 30 {
                2.042 + (30 - df) as f64 * 0.01
            } else if df < 100 {
                1.984 + (100 - df) as f64 * 0.001
            } else {
                1.960
            }
        }
    }
}

/// Print the report to stdout.
pub fn print_report(report: &EventCpcvReport) {
    println!();
    println!("================================================");
    println!("  Event-Level CPCV Report ({} folds)", report.n_folds);
    println!("================================================");
    println!("  Days: {} | Subsample: {}%", report.n_days, report.subsample_pct);
    println!("  Margin: {} | Commission: ${:.2}", report.margin, report.commission);
    println!();

    println!("  ─── Aggregate Metrics ───");
    println!(
        "    Mean Expectancy:    ${:.2} ± ${:.2}",
        report.mean_expectancy, report.std_expectancy
    );
    println!(
        "    95% CI:             [${:.2}, ${:.2}]",
        report.ci_95.0, report.ci_95.1
    );
    let neg_count = (report.negative_fold_fraction * report.n_folds as f64).round() as usize;
    println!(
        "    Neg. Folds:         {}/{} ({:.1}%)",
        neg_count,
        report.n_folds,
        report.negative_fold_fraction * 100.0
    );
    println!("    Ann. Sharpe:        {:.2}", report.annualized_sharpe);
    println!("    Deflated Sharpe:    {:.4}", report.deflated_sharpe);
    println!(
        "    Profit Factor:      {:.2}",
        report.pooled_profit_factor
    );
    println!("    Win Rate:           {:.1}%", report.pooled_win_rate * 100.0);
    println!("    Total Trades:       {}", report.total_trades);
    println!("    Total Net PnL:      ${:.2}", report.total_net_pnl);
    println!();

    // Geometry breakdown
    println!("  ─── Per-Geometry Breakdown ───");
    println!("    {:>4} {:>4} {:>7} {:>9} {:>10} {:>8} {:>8}",
        "T", "S", "P_null", "P_model", "Preds", "Trades", "WinRate");
    for g in &report.geometry_summaries {
        println!("    {:>4} {:>4} {:>7.4} {:>9.4} {:>10} {:>8} {:>7.1}%",
            g.target_ticks,
            g.stop_ticks,
            g.p_null,
            g.mean_p_model,
            g.total_predictions,
            g.total_trades,
            g.mean_win_rate * 100.0,
        );
    }
    println!();

    // Calibration
    println!("  ─── Calibration ───");
    println!("    {:>8} {:>8} {:>10} {:>10} {:>8}",
        "Bin", "Pred", "Actual", "Count", "Delta");
    for bin in &report.calibration {
        let delta = bin.actual_rate - bin.mean_predicted;
        println!("    [{:.1},{:.1}) {:>8.4} {:>10.4} {:>10} {:>+8.4}",
            bin.bin_start,
            bin.bin_end,
            bin.mean_predicted,
            bin.actual_rate,
            bin.total_count,
            delta,
        );
    }
    println!();

    // Ljung-Box
    if let Some(ref lb) = report.ljung_box {
        println!("  ─── Ljung-Box Autocorrelation (Trade PnL) ───");
        println!("    Q statistic:  {:.2} (max lag = {})", lb.q_statistic, lb.max_lag);
        println!("    p-value:      {:.4}", lb.p_value);
        if lb.p_value > 0.05 {
            println!("    Result:       No significant autocorrelation (p > 0.05)");
        } else {
            println!("    Result:       Significant autocorrelation detected (p <= 0.05)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mean() {
        assert!((mean(&[1.0, 2.0, 3.0]) - 2.0).abs() < 1e-10);
    }

    #[test]
    fn test_std_dev() {
        let s = std_dev(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert!((s - 2.138).abs() < 0.01);
    }

    #[test]
    fn test_normal_cdf() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 0.01);
        assert!((normal_cdf(1.96) - 0.975).abs() < 0.01);
        assert!((normal_cdf(-1.96) - 0.025).abs() < 0.01);
    }

    #[test]
    fn test_deflated_sharpe_positive() {
        let dsr = deflated_sharpe_ratio(3.0, 45);
        assert!(dsr > 0.0 && dsr <= 1.0);
    }

    #[test]
    fn test_chi_squared_cdf_basic() {
        let p = chi_squared_cdf(3.841, 1);
        assert!((p - 0.95).abs() < 0.01, "chi2(1) CDF at 3.841 = {}", p);
    }

    #[test]
    fn test_t_critical_95() {
        assert!((t_critical_95(44) - 2.015).abs() < 0.001);
    }
}
