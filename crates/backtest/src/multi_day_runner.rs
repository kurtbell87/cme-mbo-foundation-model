use common::bar::Bar;
use common::execution_costs::ExecutionCosts;
use crate::oracle_replay::{self, BacktestResult, OracleConfig, OracleReplay};
use crate::rollover::RolloverCalendar;

/// Configuration for multi-day runner.
#[derive(Debug, Clone)]
pub struct BacktestConfig {
    pub bar_type: String,
    pub bar_param: f64,
    pub oracle: OracleConfig,
    pub costs: ExecutionCosts,
}

/// In-sample / out-of-sample date ranges.
#[derive(Debug, Clone, Default)]
pub struct DaySchedule {
    pub in_sample_start: i32,
    pub in_sample_end: i32,
    pub oos_start: i32,
    pub oos_end: i32,
}

impl DaySchedule {
    pub fn is_in_sample(&self, date: i32) -> bool {
        date >= self.in_sample_start && date <= self.in_sample_end
    }

    pub fn is_oos(&self, date: i32) -> bool {
        date >= self.oos_start && date <= self.oos_end
    }
}

/// Result of a single day's backtest.
#[derive(Debug, Clone)]
pub struct DayResult {
    pub date: i32,
    pub result: BacktestResult,
    pub skipped: bool,
    pub skip_reason: String,
    pub contract_symbol: String,
    pub instrument_id: u32,
    pub bar_count: i32,
}

impl Default for DayResult {
    fn default() -> Self {
        Self {
            date: 0,
            result: BacktestResult::default(),
            skipped: false,
            skip_reason: String::new(),
            contract_symbol: String::new(),
            instrument_id: 0,
            bar_count: 0,
        }
    }
}

/// IS / OOS split.
#[derive(Debug, Clone, Default)]
pub struct SplitResults {
    pub in_sample: Vec<DayResult>,
    pub oos: Vec<DayResult>,
}

/// Runs backtest across multiple days.
pub struct MultiDayRunner {
    config: BacktestConfig,
    calendar: RolloverCalendar,
    schedule: DaySchedule,
}

impl MultiDayRunner {
    pub fn new(
        config: BacktestConfig,
        calendar: RolloverCalendar,
        schedule: DaySchedule,
    ) -> Self {
        Self {
            config,
            calendar,
            schedule,
        }
    }

    pub fn run_day(&self, date: i32, bars: &[Bar]) -> DayResult {
        let mut day = DayResult::default();
        day.date = date;
        day.bar_count = bars.len() as i32;

        if self.calendar.is_excluded(date) {
            day.skipped = true;
            day.skip_reason = "Excluded: near rollover".to_string();
            return day;
        }

        if let Some(contract) = self.calendar.get_contract_for_date(date) {
            day.contract_symbol = contract.symbol.clone();
            day.instrument_id = contract.instrument_id;
        }

        let replay = OracleReplay::new(self.config.oracle.clone(), self.config.costs.clone());
        day.result = replay.run(bars);
        day
    }

    pub fn aggregate(&self, day_results: &[DayResult]) -> BacktestResult {
        let mut agg = BacktestResult::default();
        let mut active_days = 0;

        for day in day_results {
            if day.skipped {
                continue;
            }
            active_days += 1;

            agg.total_trades += day.result.total_trades;
            agg.winning_trades += day.result.winning_trades;
            agg.losing_trades += day.result.losing_trades;
            agg.net_pnl += day.result.net_pnl;
            agg.gross_pnl += day.result.gross_pnl;
            agg.trades.extend(day.result.trades.iter().cloned());
            agg.daily_pnl.push(day.result.net_pnl);
            agg.safety_cap_triggered_count += day.result.safety_cap_triggered_count;
        }

        oracle_replay::recompute_derived(&mut agg, active_days);

        if agg.total_trades > 0 {
            agg.safety_cap_fraction =
                agg.safety_cap_triggered_count as f32 / agg.total_trades as f32;
        }

        agg
    }

    pub fn split_results(&self, day_results: &[DayResult]) -> SplitResults {
        let mut split = SplitResults::default();
        for day in day_results {
            if self.schedule.is_in_sample(day.date) {
                split.in_sample.push(day.clone());
            } else if self.schedule.is_oos(day.date) {
                split.oos.push(day.clone());
            }
        }
        split
    }
}
