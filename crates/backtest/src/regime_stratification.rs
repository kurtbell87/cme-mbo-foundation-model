use crate::trade_record::TradeRecord;
use common::bar::Bar;
use std::collections::BTreeMap;

/// Per-regime aggregation.
#[derive(Debug, Clone, Default)]
pub struct RegimeResult {
    pub trade_count: i32,
    pub expectancy: f32,
    pub net_pnl: f32,
    pub win_rate: f32,
    pub profit_factor: f32,
    pub sharpe: f32,
}

/// Time-of-day session classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Session {
    Open,
    Mid,
    Close,
}

pub fn classify_session(hour_of_day: f32) -> Session {
    if hour_of_day < 10.5 {
        Session::Open
    } else if hour_of_day < 14.0 {
        Session::Mid
    } else {
        Session::Close
    }
}

/// Daily trend classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Trend {
    RangeBound,
    Moderate,
    StrongTrend,
}

pub fn classify_trend(otc_return_pct: f32) -> Trend {
    let abs_return = otc_return_pct.abs();
    if abs_return > 1.0 {
        Trend::StrongTrend
    } else if abs_return < 0.3 {
        Trend::RangeBound
    } else {
        Trend::Moderate
    }
}

/// Compute OTC return from a day's bars: (close - open) / open * 100.
pub fn compute_otc_return(bars: &[Bar]) -> f32 {
    if bars.is_empty() {
        return 0.0;
    }
    let open = bars.first().unwrap().open_mid;
    let close = bars.last().unwrap().close_mid;
    if open == 0.0 {
        return 0.0;
    }
    (close - open) / open * 100.0
}

/// Compute realized volatility from a bar window.
pub fn compute_realized_vol(bars: &[Bar], window: usize) -> f32 {
    if bars.len() < window || window < 2 {
        return 0.0;
    }
    let start = bars.len() - window;
    let mut sum = 0.0f32;
    let mut sum_sq = 0.0f32;
    let mut n = 0;
    for i in (start + 1)..bars.len() {
        let ret = bars[i].close_mid - bars[i - 1].close_mid;
        sum += ret;
        sum_sq += ret * ret;
        n += 1;
    }
    if n < 2 {
        return 0.0;
    }
    let mean = sum / n as f32;
    let var = sum_sq / n as f32 - mean * mean;
    var.abs().sqrt()
}

/// Assign quartiles to a sorted collection.
pub fn assign_quartiles<T: PartialOrd>(values: &[T]) -> Vec<i32> {
    let n = values.len();
    if n == 0 {
        return vec![];
    }
    let mut indices: Vec<usize> = (0..n).collect();
    indices.sort_by(|&a, &b| values[a].partial_cmp(&values[b]).unwrap());

    let mut quartiles = vec![0i32; n];
    for (rank, &idx) in indices.iter().enumerate() {
        let q = ((rank * 4) / n) as i32 + 1;
        quartiles[idx] = q.min(4);
    }
    quartiles
}

/// Compute stability score across regime strata.
pub fn compute_stability_score<K: Ord>(strat: &BTreeMap<K, RegimeResult>) -> f32 {
    if strat.is_empty() {
        return 0.0;
    }
    if strat.len() == 1 {
        return 1.0;
    }
    let mut min_exp = f32::MAX;
    let mut max_exp = f32::MIN;
    for r in strat.values() {
        min_exp = min_exp.min(r.expectancy);
        max_exp = max_exp.max(r.expectancy);
    }
    if max_exp == 0.0 {
        return 0.0;
    }
    min_exp / max_exp
}

pub fn classify_stability(score: f32) -> &'static str {
    if score > 0.5 {
        "robust"
    } else if score >= 0.2 {
        "regime-dependent"
    } else {
        "fragile"
    }
}

/// Stratification engine for backtesting across market regimes.
pub struct RegimeStratifier;

impl RegimeStratifier {
    pub fn new() -> Self {
        Self
    }

    pub fn by_volatility(
        &self,
        daily_vols: &[f32],
        daily_trades: &[Vec<TradeRecord>],
    ) -> BTreeMap<i32, RegimeResult> {
        if daily_vols.is_empty() {
            return BTreeMap::new();
        }
        let quartiles = assign_quartiles(daily_vols);
        self.aggregate_by_key(&quartiles, daily_trades)
    }

    pub fn by_time_of_day(
        &self,
        trades: &[TradeRecord],
        bars: &[Bar],
    ) -> BTreeMap<Session, RegimeResult> {
        let mut bucketed: BTreeMap<Session, Vec<&TradeRecord>> = BTreeMap::new();
        for trade in trades {
            let tod = if trade.entry_bar_idx < bars.len() {
                bars[trade.entry_bar_idx].time_of_day
            } else {
                9.5
            };
            let session = classify_session(tod);
            bucketed.entry(session).or_default().push(trade);
        }

        bucketed
            .into_iter()
            .map(|(session, trades)| (session, Self::compute_regime_result_ref(&trades)))
            .collect()
    }

    pub fn by_trend(
        &self,
        daily_otc_returns: &[f32],
        daily_trades: &[Vec<TradeRecord>],
    ) -> BTreeMap<Trend, RegimeResult> {
        let mut bucketed: BTreeMap<Trend, Vec<&TradeRecord>> = BTreeMap::new();
        let n = daily_otc_returns.len().min(daily_trades.len());
        for i in 0..n {
            let trend = classify_trend(daily_otc_returns[i]);
            for t in &daily_trades[i] {
                bucketed.entry(trend).or_default().push(t);
            }
        }

        bucketed
            .into_iter()
            .map(|(trend, trades)| (trend, Self::compute_regime_result_ref(&trades)))
            .collect()
    }

    fn aggregate_by_key(
        &self,
        keys: &[i32],
        daily_trades: &[Vec<TradeRecord>],
    ) -> BTreeMap<i32, RegimeResult> {
        let mut bucketed: BTreeMap<i32, Vec<&TradeRecord>> = BTreeMap::new();
        let n = keys.len().min(daily_trades.len());
        for i in 0..n {
            for t in &daily_trades[i] {
                bucketed.entry(keys[i]).or_default().push(t);
            }
        }

        bucketed
            .into_iter()
            .map(|(key, trades)| (key, Self::compute_regime_result_ref(&trades)))
            .collect()
    }

    fn compute_regime_result_ref(trades: &[&TradeRecord]) -> RegimeResult {
        let mut r = RegimeResult::default();
        r.trade_count = trades.len() as i32;
        if r.trade_count == 0 {
            return r;
        }

        let mut sum_net = 0.0f32;
        let mut gross_wins = 0.0f32;
        let mut gross_losses = 0.0f32;
        let mut wins = 0;

        for t in trades {
            sum_net += t.net_pnl;
            if t.net_pnl > 0.0 {
                wins += 1;
                gross_wins += t.gross_pnl;
            } else {
                gross_losses += t.gross_pnl.abs();
            }
        }

        r.net_pnl = sum_net;
        r.expectancy = sum_net / r.trade_count as f32;
        r.win_rate = wins as f32 / r.trade_count as f32;

        if gross_losses > 0.0 {
            r.profit_factor = gross_wins / gross_losses;
        }

        if r.trade_count >= 2 {
            let mean = r.expectancy;
            let sum_sq: f32 = trades.iter().map(|t| (t.net_pnl - mean).powi(2)).sum();
            let var = sum_sq / (r.trade_count - 1) as f32;
            let std_dev = var.sqrt();
            if std_dev > 0.0 {
                r.sharpe = mean / std_dev;
            }
        }

        r
    }
}

impl Default for RegimeStratifier {
    fn default() -> Self {
        Self::new()
    }
}
