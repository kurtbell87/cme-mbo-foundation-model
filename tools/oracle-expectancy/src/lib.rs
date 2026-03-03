//! Oracle expectancy library — MES MBO oracle backtests with FirstToHit
//! and TripleBarrier label methods, multi-day aggregation, and JSON reporting.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;

use anyhow::{bail, Result};
use serde::Serialize;

use backtest::{
    recompute_derived, BacktestResult, ContractSpec, ExitReason, RolloverCalendar,
};
use bars::TimeBarBuilder;
use bars::BarBuilder;
use common::book::{BookSnapshot, BOOK_DEPTH, SNAPSHOT_INTERVAL_NS, TRADE_BUF_LEN};
use common::execution_costs::ExecutionCosts;
use common::time_utils;
use dbn::decode::{DbnDecoder, DecodeRecord};
use dbn::MboMsg;

// ===========================================================================
// Section A: StreamingBook — order-book reconstruction from MBO messages
// (Copied from tools/parity-test/src/lib.rs:118-319 — accepted tech debt)
// ===========================================================================

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
    pending_add: u32,
    pending_cancel: u32,
    pending_modify: u32,
    pending_trade: u32,
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
            pending_add: 0,
            pending_cancel: 0,
            pending_modify: 0,
            pending_trade: 0,
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
                self.orders
                    .insert(order_id, OrderEntry { side, price, size });
                self.add_level(side, price, size);
                self.pending_add += 1;
            }
            'C' => {
                if let Some(entry) = self.orders.remove(&order_id) {
                    self.remove_level(entry.side, entry.price, entry.size);
                }
                self.pending_cancel += 1;
            }
            'M' => {
                if let Some(entry) = self.orders.remove(&order_id) {
                    self.remove_level(entry.side, entry.price, entry.size);
                }
                self.orders
                    .insert(order_id, OrderEntry { side, price, size });
                self.add_level(side, price, size);
                self.pending_modify += 1;
            }
            'T' => {
                let agg = if side == 'B' { 1.0f32 } else { -1.0f32 };
                self.trades
                    .push_back([fixed_to_float(price), size as f32, agg]);
                if self.trades.len() > TRADE_BUF_LEN {
                    self.trades.pop_front();
                }
                self.pending_trade += 1;
            }
            'F' => {
                if let Some(entry) = self.orders.remove(&order_id) {
                    self.remove_level(entry.side, entry.price, entry.size);
                    if size > 0 {
                        self.orders.insert(
                            order_id,
                            OrderEntry {
                                side: entry.side,
                                price: entry.price,
                                size,
                            },
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

        // MBO event counts since last snapshot
        snap.add_count = self.pending_add;
        snap.cancel_count = self.pending_cancel;
        snap.modify_count = self.pending_modify;
        snap.trade_count = self.pending_trade;
        self.pending_add = 0;
        self.pending_cancel = 0;
        self.pending_modify = 0;
        self.pending_trade = 0;

        Some(snap)
    }
}

// ===========================================================================
// Section B: build_bars_from_dbn — simplified bar pipeline
// ===========================================================================

/// Build 5-second time bars from a DBN MBO file for a given instrument and date.
///
/// Pipeline: DbnDecoder → StreamingBook → 100ms snapshots during RTH → TimeBarBuilder.
/// No event attribution or features — just bars for oracle replay.
pub fn build_bars_from_dbn(dbn_path: &Path, instrument_id: u32, date: &str) -> Result<Vec<common::bar::Bar>> {
    let mut decoder = DbnDecoder::from_zstd_file(dbn_path)
        .map_err(|e| anyhow::anyhow!("Failed to open DBN file: {}", e))?;

    let mut book = StreamingBook::new();
    let mut first_ts: Option<u64> = None;
    let rth_open_ns: u64 = time_utils::rth_open_for_date(date);
    let rth_close: u64 = time_utils::rth_close_for_date(date);
    let mut next_snap_ts: u64 = rth_open_ns;
    let mut counting_started = false;
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
        }

        // Clear pre-RTH event counts before processing the first RTH event
        if !counting_started && rth_open_ns > 0 && ts >= rth_open_ns && id == instrument_id {
            book.pending_add = 0;
            book.pending_cancel = 0;
            book.pending_modify = 0;
            book.pending_trade = 0;
            counting_started = true;
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

// ===========================================================================
// Section C: MES Contract Table + Quarter Assignment
// ===========================================================================

struct MesContract {
    symbol: &'static str,
    instrument_id: u32,
    start_date: i32,
    end_date: i32,
    rollover_date: i32,
}

const MES_CONTRACTS: &[MesContract] = &[
    MesContract { symbol: "MESH2",  instrument_id: 11355, start_date: 20220103, end_date: 20220318, rollover_date: 20220318 },
    MesContract { symbol: "MESM2",  instrument_id: 13615, start_date: 20220319, end_date: 20220617, rollover_date: 20220617 },
    MesContract { symbol: "MESU2",  instrument_id: 10039, start_date: 20220618, end_date: 20220916, rollover_date: 20220916 },
    MesContract { symbol: "MESZ2",  instrument_id: 10299, start_date: 20220917, end_date: 20221216, rollover_date: 20221216 },
    MesContract { symbol: "MESH3",  instrument_id: 2080,  start_date: 20221217, end_date: 20221230, rollover_date: 20230317 },
];

/// Look up the MES instrument_id for a given trading date (YYYYMMDD integer).
pub fn get_instrument_id(date: i32) -> u32 {
    for c in MES_CONTRACTS {
        if date >= c.start_date && date <= c.end_date {
            return c.instrument_id;
        }
    }
    13615 // Default to MESM2
}

/// Look up the MES contract symbol for a given trading date.
pub fn get_contract_symbol(date: i32) -> &'static str {
    for c in MES_CONTRACTS {
        if date >= c.start_date && date <= c.end_date {
            return c.symbol;
        }
    }
    "MESM2"
}

/// Build a RolloverCalendar from the MES contract table.
pub fn build_rollover_calendar() -> RolloverCalendar {
    let mut cal = RolloverCalendar::new();
    for c in MES_CONTRACTS {
        cal.add_contract(ContractSpec {
            symbol: c.symbol.to_string(),
            instrument_id: c.instrument_id,
            start_date: c.start_date,
            end_date: c.end_date,
            rollover_date: c.rollover_date,
        });
    }
    cal
}

/// Assign a date to a quarter label for per-quarter reporting.
pub fn date_to_quarter(date: i32) -> &'static str {
    if date <= 20220318 {
        "Q1-2022"
    } else if date <= 20220617 {
        "Q2-2022"
    } else if date <= 20220916 {
        "Q3-2022"
    } else {
        "Q4-2022"
    }
}

// ===========================================================================
// Section D: Day Selection
// ===========================================================================

/// Sakamoto's day-of-week algorithm: 0=Sun, 1=Mon, ..., 6=Sat.
fn day_of_week(y: i32, m: i32, d: i32) -> i32 {
    static T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d) % 7
}

/// Check if a YYYYMMDD integer is a weekday (Mon-Fri).
fn is_weekday(date: i32) -> bool {
    let y = date / 10000;
    let m = (date / 100) % 100;
    let d = date % 100;
    let dow = day_of_week(y, m, d);
    dow >= 1 && dow <= 5
}

/// Scan a data directory for available trading days (by DBN filename pattern).
///
/// Returns sorted YYYYMMDD integers for weekdays only.
pub fn get_available_days(data_dir: &Path) -> Result<Vec<i32>> {
    let mut dates = Vec::new();
    for entry in std::fs::read_dir(data_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(rest) = name.strip_prefix("glbx-mdp3-") {
            if let Some(date_str) = rest.strip_suffix(".mbo.dbn.zst") {
                if date_str.len() == 8 {
                    if let Ok(date) = date_str.parse::<i32>() {
                        if is_weekday(date) {
                            dates.push(date);
                        }
                    }
                }
            }
        }
    }
    dates.sort();
    Ok(dates)
}

/// Select stratified days: N evenly-spaced days per quarter, excluding rollover dates.
pub fn select_stratified_days(data_dir: &Path, n_per_quarter: usize) -> Result<Vec<i32>> {
    let all_days = get_available_days(data_dir)?;
    let calendar = build_rollover_calendar();

    // Group by quarter, excluding rollover dates
    let mut quarters: BTreeMap<&str, Vec<i32>> = BTreeMap::new();
    for &d in &all_days {
        if calendar.is_excluded(d) {
            continue;
        }
        let q = date_to_quarter(d);
        quarters.entry(q).or_default().push(d);
    }

    let mut selected = Vec::new();
    for (_q, days) in &quarters {
        let count = days.len();
        if count == 0 {
            continue;
        }
        let n = n_per_quarter.min(count);
        for i in 0..n {
            // C++ spacing formula: idx = (i * (count-1)) / max(n-1, 1)
            let idx = if n <= 1 {
                0
            } else {
                (i * (count - 1)) / (n - 1)
            };
            selected.push(days[idx]);
        }
    }

    selected.sort();
    selected.dedup();
    Ok(selected)
}

// ===========================================================================
// Section E: Report Structs + Aggregation
// ===========================================================================

/// Per-day result holding both FTH and TB backtest results.
pub struct DayResult {
    pub date: i32,
    pub fth_result: BacktestResult,
    pub tb_result: BacktestResult,
    pub bar_count: usize,
}

/// Exit reason counts for JSON serialization.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ExitReasonCounts {
    pub target: i32,
    pub stop: i32,
    pub take_profit: i32,
    pub expiry: i32,
    pub session_end: i32,
    pub safety_cap: i32,
}

impl ExitReasonCounts {
    fn from_btree(map: &BTreeMap<ExitReason, i32>) -> Self {
        Self {
            target: *map.get(&ExitReason::Target).unwrap_or(&0),
            stop: *map.get(&ExitReason::Stop).unwrap_or(&0),
            take_profit: *map.get(&ExitReason::TakeProfit).unwrap_or(&0),
            expiry: *map.get(&ExitReason::Expiry).unwrap_or(&0),
            session_end: *map.get(&ExitReason::SessionEnd).unwrap_or(&0),
            safety_cap: *map.get(&ExitReason::SafetyCap).unwrap_or(&0),
        }
    }
}

/// Summary of a BacktestResult (no trades vec — for JSON output).
#[derive(Debug, Clone, Serialize)]
pub struct BacktestResultSummary {
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
    pub exit_reasons: ExitReasonCounts,
}

impl BacktestResultSummary {
    pub fn from_result(r: &BacktestResult) -> Self {
        Self {
            total_trades: r.total_trades,
            winning_trades: r.winning_trades,
            losing_trades: r.losing_trades,
            win_rate: r.win_rate,
            gross_pnl: r.gross_pnl,
            net_pnl: r.net_pnl,
            profit_factor: r.profit_factor,
            expectancy: r.expectancy,
            max_drawdown: r.max_drawdown,
            sharpe: r.sharpe,
            annualized_sharpe: r.annualized_sharpe,
            hold_fraction: r.hold_fraction,
            avg_bars_held: r.avg_bars_held,
            avg_duration_s: r.avg_duration_s,
            trades_per_day: r.trades_per_day,
            exit_reasons: ExitReasonCounts::from_btree(&r.exit_reason_counts),
        }
    }
}

/// Per-quarter report with both label methods.
#[derive(Debug, Clone, Serialize)]
pub struct QuarterReport {
    pub first_to_hit: BacktestResultSummary,
    pub triple_barrier: BacktestResultSummary,
}

/// Top-level oracle expectancy report.
#[derive(Debug, Clone, Serialize)]
pub struct OracleExpectancyReport {
    pub days_processed: i32,
    pub days_skipped: i32,
    pub first_to_hit: BacktestResultSummary,
    pub triple_barrier: BacktestResultSummary,
    pub per_quarter: BTreeMap<String, QuarterReport>,
}

/// Aggregate day results into overall + per-quarter reports.
pub fn aggregate_day_results(day_results: &[DayResult]) -> OracleExpectancyReport {
    let days_processed = day_results.len() as i32;

    // Overall aggregates
    let mut fth_agg = BacktestResult::default();
    let mut tb_agg = BacktestResult::default();

    // Per-quarter aggregates
    let mut quarter_fth: BTreeMap<String, BacktestResult> = BTreeMap::new();
    let mut quarter_tb: BTreeMap<String, BacktestResult> = BTreeMap::new();
    let mut quarter_days: BTreeMap<String, i32> = BTreeMap::new();

    for dr in day_results {
        let q = date_to_quarter(dr.date).to_string();

        // Accumulate overall FTH
        accumulate_trades(&mut fth_agg, &dr.fth_result);
        // Accumulate overall TB
        accumulate_trades(&mut tb_agg, &dr.tb_result);

        // Accumulate per-quarter
        accumulate_trades(quarter_fth.entry(q.clone()).or_default(), &dr.fth_result);
        accumulate_trades(quarter_tb.entry(q.clone()).or_default(), &dr.tb_result);
        *quarter_days.entry(q).or_insert(0) += 1;
    }

    // Recompute derived metrics
    recompute_derived(&mut fth_agg, days_processed);
    recompute_derived(&mut tb_agg, days_processed);

    // Per-quarter derived metrics
    let mut per_quarter = BTreeMap::new();
    for (q, mut fth) in quarter_fth {
        let mut tb = quarter_tb.remove(&q).unwrap_or_default();
        let q_days = *quarter_days.get(&q).unwrap_or(&1);
        recompute_derived(&mut fth, q_days);
        recompute_derived(&mut tb, q_days);
        per_quarter.insert(
            q,
            QuarterReport {
                first_to_hit: BacktestResultSummary::from_result(&fth),
                triple_barrier: BacktestResultSummary::from_result(&tb),
            },
        );
    }

    OracleExpectancyReport {
        days_processed,
        days_skipped: 0,
        first_to_hit: BacktestResultSummary::from_result(&fth_agg),
        triple_barrier: BacktestResultSummary::from_result(&tb_agg),
        per_quarter,
    }
}

/// Accumulate trades from a single-day result into an aggregate.
fn accumulate_trades(agg: &mut BacktestResult, day: &BacktestResult) {
    for trade in &day.trades {
        agg.trades.push(trade.clone());
        agg.total_trades += 1;
        agg.gross_pnl += trade.gross_pnl;
        agg.net_pnl += trade.net_pnl;
        if trade.net_pnl > 0.0 {
            agg.winning_trades += 1;
        } else {
            agg.losing_trades += 1;
        }
        *agg.exit_reason_counts
            .entry(trade.exit_reason)
            .or_insert(0) += 1;
    }
    agg.daily_pnl.push(day.net_pnl);
}

// ===========================================================================
// Section F: JSON Output
// ===========================================================================

/// Oracle replay configuration for JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct ReportConfig {
    pub bar_type: String,
    pub bar_interval_s: u64,
    pub target_ticks: i32,
    pub stop_ticks: i32,
    pub take_profit_ticks: i32,
    pub max_time_horizon_s: u32,
    pub volume_horizon: u32,
}

/// Execution costs summary for JSON output.
#[derive(Debug, Clone, Serialize)]
pub struct CostsSummary {
    pub commission_per_side: f32,
    pub spread_ticks: i32,
    pub slippage_ticks: i32,
    pub contract_multiplier: f32,
    pub tick_size: f32,
}

impl CostsSummary {
    pub fn from_costs(c: &ExecutionCosts) -> Self {
        Self {
            commission_per_side: c.commission_per_side,
            spread_ticks: c.fixed_spread_ticks,
            slippage_ticks: c.slippage_ticks,
            contract_multiplier: c.contract_multiplier,
            tick_size: c.tick_size,
        }
    }
}

/// Full JSON report output.
#[derive(Debug, Clone, Serialize)]
pub struct FullReport {
    pub config: ReportConfig,
    pub costs: CostsSummary,
    pub days_processed: i32,
    pub days_skipped: i32,
    pub first_to_hit: BacktestResultSummary,
    pub triple_barrier: BacktestResultSummary,
    pub per_quarter: BTreeMap<String, QuarterReport>,
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date_to_quarter() {
        assert_eq!(date_to_quarter(20220103), "Q1-2022");
        assert_eq!(date_to_quarter(20220318), "Q1-2022");
        assert_eq!(date_to_quarter(20220319), "Q2-2022");
        assert_eq!(date_to_quarter(20220617), "Q2-2022");
        assert_eq!(date_to_quarter(20220618), "Q3-2022");
        assert_eq!(date_to_quarter(20220916), "Q3-2022");
        assert_eq!(date_to_quarter(20220917), "Q4-2022");
        assert_eq!(date_to_quarter(20221230), "Q4-2022");
    }

    #[test]
    fn test_day_of_week() {
        // 2022-01-03 is Monday (1)
        assert_eq!(day_of_week(2022, 1, 3), 1);
        // 2022-01-08 is Saturday (6)
        assert_eq!(day_of_week(2022, 1, 8), 6);
        // 2022-01-09 is Sunday (0)
        assert_eq!(day_of_week(2022, 1, 9), 0);
    }

    #[test]
    fn test_is_weekday() {
        assert!(is_weekday(20220103)); // Monday
        assert!(is_weekday(20220107)); // Friday
        assert!(!is_weekday(20220108)); // Saturday
        assert!(!is_weekday(20220109)); // Sunday
    }

    #[test]
    fn test_get_instrument_id() {
        assert_eq!(get_instrument_id(20220103), 11355); // MESH2
        assert_eq!(get_instrument_id(20220319), 13615); // MESM2
        assert_eq!(get_instrument_id(20220618), 10039); // MESU2
        assert_eq!(get_instrument_id(20220917), 10299); // MESZ2
        assert_eq!(get_instrument_id(20221220), 2080);  // MESH3
    }

    #[test]
    fn test_get_contract_symbol() {
        assert_eq!(get_contract_symbol(20220103), "MESH2");
        assert_eq!(get_contract_symbol(20220401), "MESM2");
        assert_eq!(get_contract_symbol(20220701), "MESU2");
        assert_eq!(get_contract_symbol(20221001), "MESZ2");
    }

    #[test]
    fn test_build_rollover_calendar() {
        let cal = build_rollover_calendar();
        assert_eq!(cal.contracts().len(), 5);
        assert!(cal.is_excluded(20220318)); // MESH2 rollover
        assert!(cal.is_excluded(20220617)); // MESM2 rollover
        assert!(!cal.is_excluded(20220401)); // normal day
    }

    #[test]
    fn test_aggregate_empty() {
        let report = aggregate_day_results(&[]);
        assert_eq!(report.days_processed, 0);
        assert_eq!(report.first_to_hit.total_trades, 0);
        assert_eq!(report.triple_barrier.total_trades, 0);
    }

    #[test]
    fn test_exit_reason_counts_default() {
        let counts = ExitReasonCounts::default();
        assert_eq!(counts.target, 0);
        assert_eq!(counts.stop, 0);
        assert_eq!(counts.take_profit, 0);
    }

    #[test]
    fn test_backtest_result_summary_roundtrip_json() {
        let summary = BacktestResultSummary {
            total_trades: 100,
            winning_trades: 60,
            losing_trades: 40,
            win_rate: 0.60,
            gross_pnl: 500.0,
            net_pnl: 250.0,
            profit_factor: 1.5,
            expectancy: 2.5,
            max_drawdown: 50.0,
            sharpe: 0.8,
            annualized_sharpe: 0.0,
            hold_fraction: 0.3,
            avg_bars_held: 5.0,
            avg_duration_s: 25.0,
            trades_per_day: 20.0,
            exit_reasons: ExitReasonCounts::default(),
        };
        let json = serde_json::to_string_pretty(&summary).unwrap();
        assert!(json.contains("\"total_trades\": 100"));
        assert!(json.contains("\"win_rate\":"));
        assert!(json.contains("\"expectancy\":"));
    }

    #[test]
    fn test_full_report_serialization() {
        let config = ReportConfig {
            bar_type: "time".to_string(),
            bar_interval_s: 5,
            target_ticks: 10,
            stop_ticks: 5,
            take_profit_ticks: 20,
            max_time_horizon_s: 3600,
            volume_horizon: 50000,
        };
        let costs = CostsSummary {
            commission_per_side: 0.62,
            spread_ticks: 1,
            slippage_ticks: 0,
            contract_multiplier: 5.0,
            tick_size: 0.25,
        };
        let summary = BacktestResultSummary {
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate: 0.0,
            gross_pnl: 0.0,
            net_pnl: 0.0,
            profit_factor: 0.0,
            expectancy: 0.0,
            max_drawdown: 0.0,
            sharpe: 0.0,
            annualized_sharpe: 0.0,
            hold_fraction: 0.0,
            avg_bars_held: 0.0,
            avg_duration_s: 0.0,
            trades_per_day: 0.0,
            exit_reasons: ExitReasonCounts::default(),
        };
        let report = FullReport {
            config,
            costs,
            days_processed: 0,
            days_skipped: 0,
            first_to_hit: summary.clone(),
            triple_barrier: summary,
            per_quarter: BTreeMap::new(),
        };
        let json = serde_json::to_string_pretty(&report).unwrap();
        assert!(json.contains("\"config\""));
        assert!(json.contains("\"costs\""));
        assert!(json.contains("\"first_to_hit\""));
        assert!(json.contains("\"triple_barrier\""));
    }
}
