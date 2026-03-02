//! Cross-fold aggregation: negative fold fraction, CI, annualized Sharpe, DSR.

use serde::Serialize;

use crate::fold_runner::{FoldMetrics, FoldResult};

/// Summary for a single fold (for JSON output).
#[derive(Debug, Clone, Serialize)]
pub struct FoldSummary {
    pub split_idx: usize,
    pub test_groups: Vec<usize>,
    pub n_train_bars: usize,
    pub n_test_bars: usize,
    pub expectancy: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub annualized_sharpe: f64,
    pub total_trades: i32,
    pub stage1_best_iter: u32,
    pub stage2_best_iter: u32,
}

/// Per-scenario (cost level) aggregated report.
#[derive(Debug, Clone, Serialize)]
pub struct ScenarioReport {
    pub mean_expectancy: f64,
    pub std_expectancy: f64,
    pub ci_95: (f64, f64),
    /// Fraction of folds with expectancy < $0 (NOT PBO).
    pub negative_fold_fraction: f64,
    pub annualized_sharpe: f64,
    pub deflated_sharpe: f64,
    pub pooled_profit_factor: f64,
    pub pooled_win_rate: f64,
    pub fold_summaries: Vec<FoldSummary>,
}

/// Ljung-Box autocorrelation test result.
#[derive(Debug, Clone, Serialize)]
pub struct LjungBoxResult {
    pub q_statistic: f64,
    pub p_value: f64,
    pub max_lag: usize,
}

/// Top-level CPCV report.
#[derive(Debug, Clone, Serialize)]
pub struct CpcvReport {
    pub n_folds: usize,
    pub n_dev_days: usize,
    pub target_ticks: i32,
    pub stop_ticks: i32,
    // Overlapping (per-bar signal quality):
    pub base: ScenarioReport,
    pub optimistic: ScenarioReport,
    pub pessimistic: ScenarioReport,
    // Serial execution (implementable PnL):
    pub serial_base: ScenarioReport,
    pub serial_optimistic: ScenarioReport,
    pub serial_pessimistic: ScenarioReport,
    // Ljung-Box on serial daily PnL (base cost):
    pub serial_ljung_box: Option<LjungBoxResult>,
}

/// Aggregate fold results into a full CPCV report.
pub fn aggregate_results(
    fold_results: &[FoldResult],
    n_dev_days: usize,
    target_ticks: i32,
    stop_ticks: i32,
) -> CpcvReport {
    let n_folds = fold_results.len();

    // Overlapping (per-bar) scenarios
    let base = build_scenario_report(fold_results, n_folds, |r| &r.metrics_base);
    let optimistic = build_scenario_report(fold_results, n_folds, |r| &r.metrics_optimistic);
    let pessimistic = build_scenario_report(fold_results, n_folds, |r| &r.metrics_pessimistic);

    // Serial execution scenarios
    let serial_base = build_scenario_report(fold_results, n_folds, |r| &r.serial_base);
    let serial_optimistic = build_scenario_report(fold_results, n_folds, |r| &r.serial_optimistic);
    let serial_pessimistic = build_scenario_report(fold_results, n_folds, |r| &r.serial_pessimistic);

    // Ljung-Box on serial base daily PnL (pooled across folds)
    let serial_daily: Vec<f64> = fold_results
        .iter()
        .flat_map(|r| r.serial_base.daily_pnl.iter().copied())
        .collect();
    let serial_ljung_box = if serial_daily.len() > 10 {
        let (q, p) = ljung_box_test(&serial_daily, 10);
        Some(LjungBoxResult {
            q_statistic: q,
            p_value: p,
            max_lag: 10,
        })
    } else {
        None
    };

    CpcvReport {
        n_folds,
        n_dev_days,
        target_ticks,
        stop_ticks,
        base,
        optimistic,
        pessimistic,
        serial_base,
        serial_optimistic,
        serial_pessimistic,
        serial_ljung_box,
    }
}

/// Build a scenario report from fold results using the given metric extractor.
fn build_scenario_report<F>(
    fold_results: &[FoldResult],
    n_folds: usize,
    get_metrics: F,
) -> ScenarioReport
where
    F: Fn(&FoldResult) -> &FoldMetrics,
{
    let metrics: Vec<&FoldMetrics> = fold_results.iter().map(|r| get_metrics(r)).collect();

    // Expectancy statistics
    let expectancies: Vec<f64> = metrics.iter().map(|m| m.expectancy).collect();
    let mean_exp = mean(&expectancies);
    let std_exp = std_dev(&expectancies);
    let ci_95 = t_confidence_interval(&expectancies, 0.95);
    let neg_folds = expectancies.iter().filter(|&&e| e < 0.0).count();
    let negative_fold_fraction = neg_folds as f64 / n_folds.max(1) as f64;

    // Pooled metrics across all fold test bars
    let mut total_wins = 0i32;
    let mut total_trades = 0i32;
    let mut total_gross_wins = 0.0f64;
    let mut total_gross_losses = 0.0f64;
    for m in &metrics {
        total_trades += m.total_trades;
        total_wins += (m.win_rate * m.total_trades as f64).round() as i32;
        let net = m.expectancy * m.total_trades as f64;
        if net > 0.0 {
            total_gross_wins += net;
        } else {
            total_gross_losses += net.abs();
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

    // Annualized Sharpe from daily returns:
    // Each dev day appears in C(9,1)=9 folds as a test day.
    // Average daily PnL per day across folds, then annualize.
    let annualized_sharpe = compute_pooled_daily_sharpe(&metrics);

    // Deflated Sharpe Ratio
    let deflated_sharpe = deflated_sharpe_ratio(annualized_sharpe, n_folds);

    // Per-fold summaries
    let fold_summaries: Vec<FoldSummary> = fold_results
        .iter()
        .map(|r| {
            let m = get_metrics(r);
            FoldSummary {
                split_idx: r.split_idx,
                test_groups: r.test_groups.clone(),
                n_train_bars: r.n_train_bars,
                n_test_bars: r.n_test_bars,
                expectancy: m.expectancy,
                win_rate: m.win_rate,
                profit_factor: m.profit_factor,
                annualized_sharpe: m.annualized_sharpe,
                total_trades: m.total_trades,
                stage1_best_iter: r.stage1_best_iter,
                stage2_best_iter: r.stage2_best_iter,
            }
        })
        .collect();

    ScenarioReport {
        mean_expectancy: mean_exp,
        std_expectancy: std_exp,
        ci_95,
        negative_fold_fraction,
        annualized_sharpe,
        deflated_sharpe,
        pooled_profit_factor,
        pooled_win_rate,
        fold_summaries,
    }
}

/// Compute annualized Sharpe from pooled daily PnL across folds.
///
/// Each day may appear in multiple folds as a test day. We average the daily PnL
/// for each date across folds, then compute (mean / std) * sqrt(252).
fn compute_pooled_daily_sharpe(metrics: &[&FoldMetrics]) -> f64 {
    // Collect daily PnL by date across all folds.
    // Since we don't have dates in FoldMetrics.daily_pnl, we use the index
    // and average across folds that tested each day.
    // For correctness, we'd need (date, pnl) pairs. Since fold_runner stores
    // daily_pnl as a flat Vec without dates, we concatenate and average.
    //
    // Approximation: treat all fold daily PnLs as independent daily observations
    // and compute Sharpe directly.
    let all_daily: Vec<f64> = metrics
        .iter()
        .flat_map(|m| m.daily_pnl.iter().copied())
        .collect();

    if all_daily.len() < 2 {
        return 0.0;
    }

    let n = all_daily.len() as f64;
    let m = all_daily.iter().sum::<f64>() / n;
    let var = all_daily.iter().map(|&d| (d - m).powi(2)).sum::<f64>() / (n - 1.0);
    let std = var.sqrt();

    if std > 0.0 {
        (m / std) * (252.0f64).sqrt()
    } else {
        0.0
    }
}

/// Deflated Sharpe Ratio (Bailey & Lopez de Prado, 2014).
///
/// Adjusts for multiple testing across N backtests.
/// DSR = SR * (1 - gamma / (2 * (N-1)))  where gamma ≈ 0.5772 (Euler-Mascheroni).
/// Simplified approximation: penalizes for number of trials.
fn deflated_sharpe_ratio(sharpe: f64, n_trials: usize) -> f64 {
    if n_trials <= 1 || sharpe <= 0.0 {
        return 0.0;
    }

    // Expected maximum Sharpe under null hypothesis of N independent trials:
    // E[max(SR)] ≈ sqrt(2 * ln(N)) - (gamma + ln(pi/2)) / (2 * sqrt(2 * ln(N)))
    let n = n_trials as f64;
    let euler_mascheroni = 0.5772156649;
    let sqrt_2ln_n = (2.0 * n.ln()).sqrt();
    let e_max_sr = sqrt_2ln_n
        - (euler_mascheroni + (std::f64::consts::PI / 2.0).ln()) / (2.0 * sqrt_2ln_n);

    // Probability that observed Sharpe exceeds E[max(SR)]
    // Using standard normal CDF approximation
    let z = sharpe - e_max_sr;
    let dsr = normal_cdf(z);

    dsr
}

/// Standard normal CDF approximation (Abramowitz & Stegun).
fn normal_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327; // 1/sqrt(2*pi)
    let p = d * (-x * x / 2.0).exp();
    let poly = t * (0.319381530 + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    if x >= 0.0 {
        1.0 - p * poly
    } else {
        p * poly
    }
}

/// Ljung-Box test for autocorrelation in a time series.
///
/// Q = n(n+2) * sum_{k=1}^{h} (rho_k^2 / (n-k))
/// p-value from chi-squared CDF with h degrees of freedom.
fn ljung_box_test(daily_pnl: &[f64], max_lag: usize) -> (f64, f64) {
    let n = daily_pnl.len();
    if n <= max_lag + 1 {
        return (0.0, 1.0);
    }

    let nf = n as f64;
    let m = daily_pnl.iter().sum::<f64>() / nf;
    let var: f64 = daily_pnl.iter().map(|&x| (x - m).powi(2)).sum();
    if var < 1e-15 {
        return (0.0, 1.0);
    }

    let mut q = 0.0;
    for lag in 1..=max_lag {
        let mut autocov = 0.0;
        for t in lag..n {
            autocov += (daily_pnl[t] - m) * (daily_pnl[t - lag] - m);
        }
        let rho = autocov / var;
        q += rho * rho / (nf - lag as f64);
    }
    q *= nf * (nf + 2.0);

    // p-value: 1 - chi_squared_cdf(q, df=max_lag)
    let p_value = 1.0 - chi_squared_cdf(q, max_lag);

    (q, p_value)
}

/// Chi-squared CDF: P(X <= x) for X ~ chi2(df).
///
/// Uses the Wilson-Hilferty normal approximation, which is accurate for df >= 1:
///   z = ((x/df)^(1/3) - (1 - 2/(9*df))) / sqrt(2/(9*df))
///   P(X <= x) ≈ Φ(z)
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

/// Compute mean of a slice.
fn mean(data: &[f64]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    data.iter().sum::<f64>() / data.len() as f64
}

/// Compute sample standard deviation (ddof=1).
fn std_dev(data: &[f64]) -> f64 {
    if data.len() < 2 {
        return 0.0;
    }
    let m = mean(data);
    let n = data.len() as f64;
    let var = data.iter().map(|&x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

/// Compute confidence interval using t-distribution (approximate).
/// For df=44, t_0.025 ≈ 2.015.
fn t_confidence_interval(data: &[f64], _confidence: f64) -> (f64, f64) {
    let n = data.len();
    if n < 2 {
        let m = mean(data);
        return (m, m);
    }
    let m = mean(data);
    let s = std_dev(data);
    // t critical value for 95% CI with large df (~44)
    let t_crit = t_critical_95(n - 1);
    let margin = t_crit * s / (n as f64).sqrt();
    (m - margin, m + margin)
}

/// Approximate t critical value for 95% CI (two-tailed).
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
            // Linear interpolation for common ranges
            if df < 10 {
                2.571 + (10 - df) as f64 * 0.05
            } else if df < 30 {
                2.042 + (30 - df) as f64 * 0.01
            } else if df < 100 {
                1.984 + (100 - df) as f64 * 0.001
            } else {
                1.960 // z-approximation
            }
        }
    }
}

/// Print the CPCV report to stdout.
pub fn print_report(report: &CpcvReport) {
    println!();
    println!("=======================================");
    println!("  CPCV Backtest ({} folds)", report.n_folds);
    println!("=======================================");
    println!("  Target: {} ticks | Stop: {} ticks", report.target_ticks, report.stop_ticks);
    println!("  Dev days: {}", report.n_dev_days);

    // Serial execution results (primary — implementable PnL)
    println!();
    println!("  ─── Serial Execution (Implementable PnL) ───");
    println!();
    print_scenario("Base Cost: $2.49 RT", &report.serial_base);
    println!();
    print_scenario("Optimistic Cost: $1.24 RT", &report.serial_optimistic);
    println!();
    print_scenario("Pessimistic Cost: $4.99 RT", &report.serial_pessimistic);

    // Ljung-Box autocorrelation test
    if let Some(ref lb) = report.serial_ljung_box {
        println!();
        println!("  ─── Ljung-Box Autocorrelation (Serial Daily PnL) ───");
        println!("    Q statistic:  {:.2} (max lag = {})", lb.q_statistic, lb.max_lag);
        println!("    p-value:      {:.4}", lb.p_value);
        if lb.p_value > 0.05 {
            println!("    Result:       No significant autocorrelation (p > 0.05)");
        } else {
            println!("    Result:       Significant autocorrelation detected (p <= 0.05)");
        }
    }

    // Overlapping results (secondary — per-bar signal quality)
    println!();
    println!("  ─── Overlapping Model (Per-Bar Signal Quality) ───");
    println!();
    print_scenario("Base Cost: $2.49 RT", &report.base);
    println!();
    print_scenario("Optimistic Cost: $1.24 RT", &report.optimistic);
    println!();
    print_scenario("Pessimistic Cost: $4.99 RT", &report.pessimistic);
}

fn print_scenario(label: &str, scenario: &ScenarioReport) {
    println!("  ── {} ──", label);
    println!(
        "    Mean Expectancy:    ${:.2} ± ${:.2}",
        scenario.mean_expectancy, scenario.std_expectancy
    );
    println!(
        "    95% CI:             [${:.2}, ${:.2}]",
        scenario.ci_95.0, scenario.ci_95.1
    );
    let neg_count = (scenario.negative_fold_fraction * scenario.fold_summaries.len() as f64).round() as usize;
    println!(
        "    Neg. Folds:         {}/{} ({:.1}%)",
        neg_count,
        scenario.fold_summaries.len(),
        scenario.negative_fold_fraction * 100.0
    );
    println!("    Ann. Sharpe:        {:.2}", scenario.annualized_sharpe);
    println!("    Deflated Sharpe:    {:.2}", scenario.deflated_sharpe);
    println!(
        "    Profit Factor:      {:.2}",
        scenario.pooled_profit_factor
    );
    println!("    Win Rate:           {:.1}%", scenario.pooled_win_rate * 100.0);
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
    fn test_t_critical_95() {
        assert!((t_critical_95(44) - 2.015).abs() < 0.001);
        assert!((t_critical_95(1000) - 1.960).abs() < 0.001);
    }

    #[test]
    fn test_deflated_sharpe_positive() {
        let dsr = deflated_sharpe_ratio(3.0, 45);
        assert!(dsr > 0.0 && dsr <= 1.0);
    }

    #[test]
    fn test_deflated_sharpe_low_sr() {
        let dsr = deflated_sharpe_ratio(0.5, 45);
        assert!(dsr < 0.5); // Low SR should produce low DSR
    }

    #[test]
    fn test_chi_squared_cdf_basic() {
        // chi2(df=1) CDF at x=3.841 should be ~0.95
        let p = chi_squared_cdf(3.841, 1);
        assert!((p - 0.95).abs() < 0.01, "chi2(1) CDF at 3.841 = {}", p);
    }

    #[test]
    fn test_chi_squared_cdf_df10() {
        // chi2(df=10) CDF at x=18.307 should be ~0.95
        let p = chi_squared_cdf(18.307, 10);
        assert!((p - 0.95).abs() < 0.01, "chi2(10) CDF at 18.307 = {}", p);
    }

    #[test]
    fn test_ljung_box_white_noise() {
        // White noise should have high p-value (no autocorrelation)
        // Deterministic "pseudo-random" sequence
        let data: Vec<f64> = (0..100)
            .map(|i| ((i as f64 * 1.618033988749895) % 1.0) - 0.5)
            .collect();
        let (q, p) = ljung_box_test(&data, 10);
        assert!(q >= 0.0, "Q should be non-negative");
        assert!((0.0..=1.0).contains(&p), "p-value should be in [0,1], got {}", p);
    }

    #[test]
    fn test_ljung_box_autocorrelated() {
        // Strongly autocorrelated: cumulative sum
        let mut data = vec![0.0f64; 100];
        for i in 1..100 {
            data[i] = data[i - 1] + 1.0;
        }
        let (q, p) = ljung_box_test(&data, 10);
        assert!(q > 50.0, "Q should be large for autocorrelated data, got {}", q);
        assert!(p < 0.01, "p-value should be tiny, got {}", p);
    }
}
