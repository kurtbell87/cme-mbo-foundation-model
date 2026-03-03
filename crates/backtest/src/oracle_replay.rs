use common::bar::Bar;
use common::execution_costs::ExecutionCosts;
use crate::trade_record::{ExitReason, TradeRecord};
use std::collections::BTreeMap;

/// Oracle label method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelMethod {
    FirstToHit,
    TripleBarrier,
}

/// Oracle replay configuration.
#[derive(Debug, Clone)]
pub struct OracleConfig {
    pub volume_horizon: u32,
    pub max_time_horizon_s: u32,
    pub target_ticks: i32,
    pub stop_ticks: i32,
    pub take_profit_ticks: i32,
    pub tick_size: f32,
    pub label_method: LabelMethod,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            volume_horizon: 50000,
            max_time_horizon_s: 3600,
            target_ticks: 10,
            stop_ticks: 5,
            take_profit_ticks: 20,
            tick_size: 0.25,
            label_method: LabelMethod::FirstToHit,
        }
    }
}

/// Aggregate statistics from an oracle replay.
#[derive(Debug, Clone, Default)]
pub struct BacktestResult {
    pub trades: Vec<TradeRecord>,
    pub total_trades: i32,
    pub winning_trades: i32,
    pub losing_trades: i32,
    pub win_rate: f32,
    pub gross_pnl: f32,
    pub net_pnl: f32,
    pub profit_factor: f32,
    pub expectancy: f32,
    pub max_drawdown: f32,
    pub sharpe: f32,
    pub annualized_sharpe: f32,
    pub hold_fraction: f32,
    pub avg_bars_held: f32,
    pub avg_duration_s: f32,
    pub trades_per_day: f32,
    pub safety_cap_triggered_count: i32,
    pub safety_cap_fraction: f32,
    pub pnl_by_hour: BTreeMap<i32, f32>,
    pub label_counts: BTreeMap<i32, i32>,
    pub exit_reason_counts: BTreeMap<ExitReason, i32>,
    pub daily_pnl: Vec<f32>,
}

/// Compute max drawdown from trade sequence.
pub fn compute_max_drawdown(result: &mut BacktestResult) {
    if result.trades.is_empty() {
        return;
    }
    let mut peak = 0.0f32;
    let mut equity = 0.0f32;
    let mut max_dd = 0.0f32;
    for trade in &result.trades {
        equity += trade.net_pnl;
        if equity > peak {
            peak = equity;
        }
        let dd = peak - equity;
        if dd > max_dd {
            max_dd = dd;
        }
    }
    result.max_drawdown = max_dd;
}

/// Compute Sharpe ratio from trade PnLs.
pub fn compute_sharpe(result: &mut BacktestResult) {
    if result.trades.len() < 2 {
        return;
    }
    let n = result.trades.len() as f32;
    let mean = result.net_pnl / n;
    let sum_sq: f32 = result
        .trades
        .iter()
        .map(|t| {
            let diff = t.net_pnl - mean;
            diff * diff
        })
        .sum();
    let variance = sum_sq / (n - 1.0);
    let stddev = variance.sqrt();
    if stddev > 0.0 {
        result.sharpe = mean / stddev;
    }
}

/// Compute annualized Sharpe ratio from daily PnL.
/// Formula: (mean_daily / std_daily) * sqrt(252)
pub fn compute_annualized_sharpe(result: &mut BacktestResult) {
    if result.daily_pnl.len() < 2 {
        return;
    }
    let n = result.daily_pnl.len() as f32;
    let mean = result.daily_pnl.iter().sum::<f32>() / n;
    let sum_sq: f32 = result.daily_pnl.iter().map(|&d| (d - mean).powi(2)).sum();
    let variance = sum_sq / (n - 1.0);
    let stddev = variance.sqrt();
    if stddev > 0.0 {
        result.annualized_sharpe = (mean / stddev) * (252.0f32).sqrt();
    }
}

/// Recompute derived metrics on an aggregated BacktestResult.
pub fn recompute_derived(agg: &mut BacktestResult, active_days: i32) {
    if agg.total_trades > 0 {
        agg.win_rate = agg.winning_trades as f32 / agg.total_trades as f32;
        agg.expectancy = agg.net_pnl / agg.total_trades as f32;
    }

    let mut gross_wins = 0.0f32;
    let mut gross_losses = 0.0f32;
    for trade in &agg.trades {
        if trade.gross_pnl > 0.0 {
            gross_wins += trade.gross_pnl;
        } else {
            gross_losses += trade.gross_pnl.abs();
        }
    }
    if gross_losses > 0.0 {
        agg.profit_factor = gross_wins / gross_losses;
    }

    if active_days > 0 {
        agg.trades_per_day = agg.total_trades as f32 / active_days as f32;
    }

    compute_max_drawdown(agg);
    compute_sharpe(agg);
    compute_annualized_sharpe(agg);
}

/// Oracle replay engine.
pub struct OracleReplay {
    cfg: OracleConfig,
    costs: ExecutionCosts,
}

impl OracleReplay {
    pub fn new(cfg: OracleConfig, costs: ExecutionCosts) -> Self {
        Self { cfg, costs }
    }

    pub fn run(&self, bars: &[Bar]) -> BacktestResult {
        let mut result = BacktestResult::default();
        if bars.len() <= 1 {
            return result;
        }

        let n = bars.len();
        let mut hold_bars = 0i32;
        let mut i = 1;

        while i < n - 1 {
            let label = self.compute_label(bars, i);

            if label == 0 {
                hold_bars += 1;
                i += 1;
                continue;
            }

            *result.label_counts.entry(label).or_insert(0) += 1;

            let mut trade = TradeRecord::default();
            trade.entry_bar_idx = i;
            trade.entry_price = bars[i].close_mid;
            trade.entry_ts = bars[i].close_ts;
            trade.direction = label;

            self.find_exit(bars, i, label, &mut trade);

            let price_diff = trade.exit_price - trade.entry_price;
            trade.gross_pnl = price_diff * trade.direction as f32 * self.costs.contract_multiplier;

            let entry_spread_ticks = bars[trade.entry_bar_idx].spread / self.cfg.tick_size;
            let exit_spread_ticks = bars[trade.exit_bar_idx].spread / self.cfg.tick_size;
            let rt_cost = self.costs.round_trip_cost(entry_spread_ticks, exit_spread_ticks);
            trade.net_pnl = trade.gross_pnl - rt_cost;

            trade.bars_held = (trade.exit_bar_idx - trade.entry_bar_idx) as i32;
            trade.duration_s = (trade.exit_ts - trade.entry_ts) as f32 / 1.0e9;

            i = trade.exit_bar_idx + 1;
            result.trades.push(trade);
        }

        self.compute_aggregates(&mut result, bars, n, hold_bars);
        result
    }

    fn compute_label(&self, bars: &[Bar], idx: usize) -> i32 {
        let entry_mid = bars[idx].close_mid;
        let target_dist = self.cfg.target_ticks as f32 * self.cfg.tick_size;
        let stop_dist = self.cfg.stop_ticks as f32 * self.cfg.tick_size;

        match self.cfg.label_method {
            LabelMethod::FirstToHit => {
                self.first_to_hit_label(bars, idx, entry_mid, target_dist, stop_dist)
            }
            LabelMethod::TripleBarrier => {
                self.triple_barrier_label(bars, idx, entry_mid, target_dist, stop_dist)
            }
        }
    }

    fn first_to_hit_label(
        &self,
        bars: &[Bar],
        idx: usize,
        entry_mid: f32,
        target_dist: f32,
        stop_dist: f32,
    ) -> i32 {
        let n = bars.len();
        let mut cum_volume: u32 = 0;
        let mut long_target_idx = n;
        let mut long_stop_idx = n;
        let mut short_target_idx = n;
        let mut short_stop_idx = n;

        for j in (idx + 1)..n {
            cum_volume += bars[j].volume;
            let diff = bars[j].close_mid - entry_mid;

            if diff >= target_dist && long_target_idx == n {
                long_target_idx = j;
            }
            if -diff >= stop_dist && long_stop_idx == n {
                long_stop_idx = j;
            }
            if -diff >= target_dist && short_target_idx == n {
                short_target_idx = j;
            }
            if diff >= stop_dist && short_stop_idx == n {
                short_stop_idx = j;
            }

            if cum_volume >= self.cfg.volume_horizon {
                break;
            }
        }

        let long_viable = long_target_idx < long_stop_idx;
        let short_viable = short_target_idx < short_stop_idx;

        if long_viable && !short_viable {
            return 1;
        }
        if short_viable && !long_viable {
            return -1;
        }
        if long_viable && short_viable {
            return if long_target_idx <= short_target_idx {
                1
            } else {
                -1
            };
        }

        if idx + 1 < n {
            let next_diff = bars[idx + 1].close_mid - entry_mid;
            if next_diff > 0.0 {
                return 1;
            }
            if next_diff < 0.0 {
                return -1;
            }
        }

        0
    }

    fn triple_barrier_label(
        &self,
        bars: &[Bar],
        idx: usize,
        entry_mid: f32,
        target_dist: f32,
        stop_dist: f32,
    ) -> i32 {
        let n = bars.len();
        let mut cum_volume: u32 = 0;
        let min_return_dist = 2.0 * self.cfg.tick_size;

        for j in (idx + 1)..n {
            cum_volume += bars[j].volume;
            let diff = bars[j].close_mid - entry_mid;
            let elapsed_s = (bars[j].close_ts - bars[idx].close_ts) as f32 / 1.0e9;

            if elapsed_s >= self.cfg.max_time_horizon_s as f32 {
                if diff.abs() >= min_return_dist {
                    return if diff > 0.0 { 1 } else { -1 };
                }
                return 0;
            }

            if diff >= target_dist {
                return 1;
            }
            if -diff >= stop_dist {
                return -1;
            }

            if cum_volume >= self.cfg.volume_horizon {
                if diff.abs() >= min_return_dist {
                    return if diff > 0.0 { 1 } else { -1 };
                }
                return 0;
            }
        }

        if idx + 1 < n {
            let final_diff = bars[n - 1].close_mid - entry_mid;
            if final_diff.abs() >= min_return_dist {
                return if final_diff > 0.0 { 1 } else { -1 };
            }
        }
        0
    }

    fn find_exit(&self, bars: &[Bar], entry_idx: usize, direction: i32, trade: &mut TradeRecord) {
        let n = bars.len();
        let entry_mid = bars[entry_idx].close_mid;
        let target_dist = self.cfg.target_ticks as f32 * self.cfg.tick_size;
        let stop_dist = self.cfg.stop_ticks as f32 * self.cfg.tick_size;
        let tp_dist = self.cfg.take_profit_ticks as f32 * self.cfg.tick_size;

        for j in (entry_idx + 1)..n {
            let diff = bars[j].close_mid - entry_mid;
            let directional_diff = diff * direction as f32;

            if directional_diff >= tp_dist {
                trade.exit_bar_idx = j;
                trade.exit_price = bars[j].close_mid;
                trade.exit_ts = bars[j].close_ts;
                trade.exit_reason = ExitReason::TakeProfit;
                return;
            }

            if directional_diff >= target_dist {
                trade.exit_bar_idx = j;
                trade.exit_price = bars[j].close_mid;
                trade.exit_ts = bars[j].close_ts;
                trade.exit_reason = ExitReason::Target;
                return;
            }

            if directional_diff <= -stop_dist {
                trade.exit_bar_idx = j;
                trade.exit_price = bars[j].close_mid;
                trade.exit_ts = bars[j].close_ts;
                trade.exit_reason = ExitReason::Stop;
                return;
            }
        }

        trade.exit_bar_idx = n - 1;
        trade.exit_price = bars[n - 1].close_mid;
        trade.exit_ts = bars[n - 1].close_ts;
        trade.exit_reason = ExitReason::SessionEnd;
    }

    fn compute_aggregates(
        &self,
        result: &mut BacktestResult,
        bars: &[Bar],
        _total_bars: usize,
        hold_bars: i32,
    ) {
        result.total_trades = result.trades.len() as i32;

        let mut sum_bars_held = 0.0f32;
        let mut sum_duration = 0.0f32;

        for trade in &result.trades {
            result.gross_pnl += trade.gross_pnl;
            result.net_pnl += trade.net_pnl;
            sum_bars_held += trade.bars_held as f32;
            sum_duration += trade.duration_s;

            if trade.net_pnl > 0.0 {
                result.winning_trades += 1;
            } else {
                result.losing_trades += 1;
            }

            *result.exit_reason_counts.entry(trade.exit_reason).or_insert(0) += 1;

            if trade.exit_reason == ExitReason::SafetyCap {
                result.safety_cap_triggered_count += 1;
            }

            let entry_hour = bars[trade.entry_bar_idx].time_of_day as i32;
            *result.pnl_by_hour.entry(entry_hour).or_insert(0.0) += trade.net_pnl;
        }

        if result.total_trades > 0 {
            result.win_rate = result.winning_trades as f32 / result.total_trades as f32;
            result.expectancy = result.net_pnl / result.total_trades as f32;
            result.avg_bars_held = sum_bars_held / result.total_trades as f32;
            result.avg_duration_s = sum_duration / result.total_trades as f32;
            result.safety_cap_fraction =
                result.safety_cap_triggered_count as f32 / result.total_trades as f32;
        }

        let mut gross_wins = 0.0f32;
        let mut gross_losses = 0.0f32;
        for trade in &result.trades {
            if trade.gross_pnl > 0.0 {
                gross_wins += trade.gross_pnl;
            } else {
                gross_losses += trade.gross_pnl.abs();
            }
        }
        if gross_losses > 0.0 {
            result.profit_factor = gross_wins / gross_losses;
        }

        let total_opportunities = result.total_trades + hold_bars;
        if total_opportunities > 0 {
            result.hold_fraction = hold_bars as f32 / total_opportunities as f32;
        }

        compute_max_drawdown(result);
        compute_sharpe(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bar(close_ts: u64, close_mid: f32, volume: u32) -> Bar {
        Bar {
            close_ts,
            close_mid,
            open_mid: close_mid,
            high_mid: close_mid,
            low_mid: close_mid,
            volume,
            spread: 0.25,
            ..Default::default()
        }
    }

    #[test]
    fn test_empty_bars() {
        let replay = OracleReplay::new(OracleConfig::default(), ExecutionCosts::default());
        let result = replay.run(&[]);
        assert_eq!(result.total_trades, 0);
    }

    #[test]
    fn test_simple_long_trade() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4500.0, 100),
            make_bar(3_000_000_000, 4502.5, 100), // +2.5 = 10 ticks → target
            make_bar(4_000_000_000, 4503.0, 100),
        ];
        let cfg = OracleConfig {
            label_method: LabelMethod::FirstToHit,
            ..Default::default()
        };
        let replay = OracleReplay::new(cfg, ExecutionCosts::default());
        let result = replay.run(&bars);
        assert!(result.total_trades >= 1);
    }

    #[test]
    fn test_hold_fraction() {
        // All bars at same price → lots of holds
        let bars: Vec<Bar> = (0..100)
            .map(|i| make_bar(i as u64 * 5_000_000_000, 4500.0, 100))
            .collect();
        let replay = OracleReplay::new(OracleConfig::default(), ExecutionCosts::default());
        let result = replay.run(&bars);
        // With no movement, most labels should be HOLD
        assert!(result.hold_fraction > 0.0);
    }
}
