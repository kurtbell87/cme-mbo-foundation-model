pub mod flow;

use common::book::{BookSnapshot, BOOK_DEPTH, SNAPSHOT_INTERVAL_NS, TRADE_BUF_LEN};
use common::time_utils;

use std::collections::{BTreeMap, HashMap, VecDeque};

/// Per-order tracking info.
#[derive(Debug, Clone)]
struct OrderInfo {
    side: char,
    price: i64,
    size: u32,
}

/// Trade record for the rolling trade buffer.
#[derive(Debug, Clone)]
struct TradeRecord {
    price: f32,
    size: f32,
    aggressor_side: f32, // +1.0 for buyer, -1.0 for seller
}

/// Compact committed book state after an F_LAST event.
///
/// Stores only the top BOOK_DEPTH levels per side plus precomputed mid/spread,
/// instead of cloning full BTreeMaps. This reduces per-entry memory from ~5KB
/// to ~180 bytes, critical for files with 500K+ F_LAST events.
#[derive(Debug, Clone, Copy)]
pub struct CommittedState {
    pub ts: u64,
    pub has_bid: bool,
    pub has_ask: bool,
    /// Top BOOK_DEPTH bid levels: [price, size], descending by price (best bid first).
    pub bids: [[f32; 2]; BOOK_DEPTH],
    /// Top BOOK_DEPTH ask levels: [price, size], ascending by price (best ask first).
    pub asks: [[f32; 2]; BOOK_DEPTH],
    /// Precomputed mid price (0.0 if one-sided).
    pub mid: f32,
    /// Precomputed spread (0.0 if one-sided).
    pub spread: f32,
    /// Number of valid bid levels stored (0..BOOK_DEPTH).
    pub n_bids: u8,
    /// Number of valid ask levels stored (0..BOOK_DEPTH).
    pub n_asks: u8,
    /// True if the best bid or best ask price changed from the previous commit.
    pub bbo_changed: bool,
}

/// Reconstructs an L2 order book from MBO events and emits 100ms snapshots.
///
/// Processes Databento MBO events (Add/Cancel/Modify/Trade/Fill/Clear)
/// and maintains per-order tracking + aggregated price levels. After each
/// F_LAST flag, commits the current state. `emit_snapshots` produces
/// BookSnapshot structs at 100ms boundaries during RTH hours.
pub struct BookBuilder {
    instrument_id: u32,

    // Per-order tracking: order_id -> {side, price, size}
    orders: HashMap<u64, OrderInfo>,

    // Aggregated price levels (price -> total size)
    bid_levels: BTreeMap<i64, u32>, // ascending by price (last = best bid)
    ask_levels: BTreeMap<i64, u32>, // ascending by price (first = best ask)

    // Committed state snapshots (after F_LAST)
    committed_states: Vec<CommittedState>,

    // Trade buffer (rolling, max TRADE_BUF_LEN)
    trades: VecDeque<TradeRecord>,

    // Carry-forward mid/spread
    last_mid_price: f32,
    last_spread: f32,
    ever_had_both_sides: bool,

    /// (timestamp_ns, mid_price) at every F_LAST with both sides quoted.
    tick_mid_prices: Vec<(u64, f32)>,

    // Previous BBO prices for bbo_changed detection (raw i64 fixed-point)
    prev_best_bid: Option<i64>,
    prev_best_ask: Option<i64>,

    // Flow accumulators for microstructure dynamics
    flow_accums: flow::FlowAccumulators,
    flow_states: Vec<flow::FlowState>,
}

const F_LAST: u8 = 0x80;

fn fixed_to_float(fixed: i64) -> f32 {
    (fixed as f64 / 1e9) as f32
}

fn compute_mid_spread_from_levels(
    bids: &BTreeMap<i64, u32>,
    asks: &BTreeMap<i64, u32>,
) -> (f32, f32) {
    let best_bid = fixed_to_float(*bids.keys().next_back().unwrap());
    let best_ask = fixed_to_float(*asks.keys().next().unwrap());
    ((best_bid + best_ask) / 2.0, best_ask - best_bid)
}

/// Snapshot the top BOOK_DEPTH levels from a BTreeMap into a fixed-size array.
fn snapshot_bids(levels: &BTreeMap<i64, u32>) -> ([[f32; 2]; BOOK_DEPTH], u8) {
    let mut out = [[0.0f32; 2]; BOOK_DEPTH];
    let mut count = 0u8;
    for (&price, &size) in levels.iter().rev() {
        if (count as usize) >= BOOK_DEPTH {
            break;
        }
        out[count as usize] = [fixed_to_float(price), size as f32];
        count += 1;
    }
    (out, count)
}

fn snapshot_asks(levels: &BTreeMap<i64, u32>) -> ([[f32; 2]; BOOK_DEPTH], u8) {
    let mut out = [[0.0f32; 2]; BOOK_DEPTH];
    let mut count = 0u8;
    for (&price, &size) in levels.iter() {
        if (count as usize) >= BOOK_DEPTH {
            break;
        }
        out[count as usize] = [fixed_to_float(price), size as f32];
        count += 1;
    }
    (out, count)
}

impl BookBuilder {
    pub fn new(instrument_id: u32) -> Self {
        Self {
            instrument_id,
            orders: HashMap::new(),
            bid_levels: BTreeMap::new(),
            ask_levels: BTreeMap::new(),
            committed_states: Vec::new(),
            trades: VecDeque::new(),
            last_mid_price: 0.0,
            last_spread: 0.0,
            ever_had_both_sides: false,
            tick_mid_prices: Vec::new(),
            prev_best_bid: None,
            prev_best_ask: None,
            flow_accums: flow::FlowAccumulators::with_defaults(),
            flow_states: Vec::new(),
        }
    }

    /// Process a single MBO event.
    pub fn process_event(
        &mut self,
        ts_event: u64,
        order_id: u64,
        instrument_id: u32,
        action: char,
        side: char,
        price: i64,
        size: u32,
        flags: u8,
    ) {
        if instrument_id != self.instrument_id {
            return;
        }

        // Feed event to flow accumulators BEFORE book update
        self.flow_accums.on_event(ts_event, action, side, size);

        // Snapshot BBO before the action
        let pre_bid = self.bid_levels.keys().next_back().copied();
        let pre_ask = self.ask_levels.keys().next().copied();

        match action {
            'A' => self.apply_add(order_id, side, price, size),
            'C' => self.apply_cancel(order_id),
            'M' => self.apply_modify(order_id, side, price, size),
            'T' => self.apply_trade(side, price, size),
            'F' => self.apply_fill(order_id, size),
            'R' => self.apply_clear(),
            _ => {}
        }

        // Check if this action changed the BBO
        let post_bid = self.bid_levels.keys().next_back().copied();
        let post_ask = self.ask_levels.keys().next().copied();
        if post_bid != pre_bid || post_ask != pre_ask {
            self.flow_accums.record_bbo_action(action, side);
        }

        if flags & F_LAST != 0 {
            self.commit(ts_event);
        }
    }

    /// Emit snapshots at 100ms boundaries within `[start_ns, end_ns)`.
    /// Only emits during RTH (09:30:00 - 16:00:00 ET).
    pub fn emit_snapshots(&mut self, start_ns: u64, end_ns: u64) -> Vec<BookSnapshot> {
        let mut result = Vec::new();

        let rth_open = time_utils::rth_open_ns(start_ns);
        let rth_close = time_utils::rth_close_ns(start_ns);

        let eff_start = start_ns.max(rth_open);
        let eff_end = end_ns.min(rth_close);

        if eff_start >= eff_end {
            return result;
        }

        // Align start to the first 100ms boundary (relative to rth_open) >= eff_start
        let offset = eff_start - rth_open;
        let aligned_start =
            rth_open + ((offset + SNAPSHOT_INTERVAL_NS - 1) / SNAPSHOT_INTERVAL_NS) * SNAPSHOT_INTERVAL_NS;

        // Pre-scan committed states up to the first boundary to set carry-forward state
        let mut both_sides_seen = false;
        let mut carry_mid = 0.0f32;
        let mut carry_spread = 0.0f32;
        for cs in &self.committed_states {
            if cs.ts > aligned_start {
                break;
            }
            if cs.has_bid && cs.has_ask {
                carry_mid = cs.mid;
                carry_spread = cs.spread;
                both_sides_seen = true;
            }
        }
        self.last_mid_price = carry_mid;
        self.last_spread = carry_spread;
        self.ever_had_both_sides = both_sides_seen;

        let mut ts = aligned_start;
        while ts < eff_end {
            if let Some(state) = self.get_committed_state_at(ts) {
                let state = *state; // Copy (CommittedState is Copy now)

                if state.has_bid && state.has_ask {
                    self.ever_had_both_sides = true;
                }

                if (!state.has_bid || !state.has_ask) && !self.ever_had_both_sides {
                    ts += SNAPSHOT_INTERVAL_NS;
                    continue;
                }

                let mut snap = BookSnapshot::default();
                snap.timestamp = ts;

                // Fill bids from compact snapshot (already descending by price)
                for i in 0..(state.n_bids as usize) {
                    snap.bids[i][0] = state.bids[i][0];
                    snap.bids[i][1] = state.bids[i][1];
                }

                // Fill asks from compact snapshot (already ascending by price)
                for i in 0..(state.n_asks as usize) {
                    snap.asks[i][0] = state.asks[i][0];
                    snap.asks[i][1] = state.asks[i][1];
                }

                // Mid price and spread (precomputed at commit time)
                if state.has_bid && state.has_ask {
                    snap.mid_price = state.mid;
                    snap.spread = state.spread;
                    self.last_mid_price = state.mid;
                    self.last_spread = state.spread;
                } else if self.ever_had_both_sides {
                    snap.mid_price = self.last_mid_price;
                    snap.spread = self.last_spread;
                }

                // Fill trades
                self.fill_trades(&mut snap);

                // Time of day
                snap.time_of_day = time_utils::compute_time_of_day(ts);

                result.push(snap);
            }
            ts += SNAPSHOT_INTERVAL_NS;
        }

        result
    }

    // --- Public accessors for BBO validation ---

    /// Returns the best bid price as raw i64 fixed-point (1e-9 scale).
    /// Returns None if there are no bid levels.
    pub fn best_bid_price(&self) -> Option<i64> {
        self.bid_levels.keys().next_back().copied()
    }

    /// Returns the best ask price as raw i64 fixed-point (1e-9 scale).
    /// Returns None if there are no ask levels.
    pub fn best_ask_price(&self) -> Option<i64> {
        self.ask_levels.keys().next().copied()
    }

    /// Take the tick-level mid-price series (moves data out, leaving Vec empty).
    pub fn take_tick_mid_prices(&mut self) -> Vec<(u64, f32)> {
        std::mem::take(&mut self.tick_mid_prices)
    }

    /// Take the full committed state series (moves data out, leaving Vec empty).
    pub fn take_committed_states(&mut self) -> Vec<CommittedState> {
        std::mem::take(&mut self.committed_states)
    }

    /// Take the flow state series (moves data out, leaving Vec empty).
    /// Parallel to `take_committed_states()` — one FlowState per commit.
    pub fn take_flow_states(&mut self) -> Vec<flow::FlowState> {
        std::mem::take(&mut self.flow_states)
    }

    // --- Private methods ---

    fn levels_for_mut(&mut self, side: char) -> &mut BTreeMap<i64, u32> {
        if side == 'B' {
            &mut self.bid_levels
        } else {
            &mut self.ask_levels
        }
    }

    fn add_to_level(&mut self, side: char, price: i64, size: u32) {
        *self.levels_for_mut(side).entry(price).or_insert(0) += size;
    }

    fn remove_from_level(&mut self, info: &OrderInfo) {
        let levels = if info.side == 'B' {
            &mut self.bid_levels
        } else {
            &mut self.ask_levels
        };
        if let Some(lvl) = levels.get_mut(&info.price) {
            if *lvl <= info.size {
                levels.remove(&info.price);
            } else {
                *lvl -= info.size;
            }
        }
    }

    fn apply_add(&mut self, order_id: u64, side: char, price: i64, size: u32) {
        self.orders.insert(
            order_id,
            OrderInfo { side, price, size },
        );
        self.add_to_level(side, price, size);
    }

    fn apply_cancel(&mut self, order_id: u64) {
        if let Some(info) = self.orders.remove(&order_id) {
            self.remove_from_level(&info);
        }
    }

    fn apply_modify(&mut self, order_id: u64, side: char, new_price: i64, new_size: u32) {
        // Only apply if the order is already tracked.  Applying a CHANGE for an
        // unknown order_id would add a phantom entry to the level (wrong size),
        // which diverges from the exchange.  Unknown-order CHANGEs can arrive
        // during the brief window between DBO subscription and snapshot completion;
        // after a valid snapshot, every active order should be in our map.
        if let Some(info) = self.orders.remove(&order_id) {
            self.remove_from_level(&info);
            self.orders.insert(order_id, OrderInfo { side, price: new_price, size: new_size });
            self.add_to_level(side, new_price, new_size);
        }
    }

    fn apply_trade(&mut self, side: char, price: i64, size: u32) {
        let agg = if side == 'B' { 1.0f32 } else { -1.0f32 };
        self.trades.push_back(TradeRecord {
            price: fixed_to_float(price),
            size: size as f32,
            aggressor_side: agg,
        });
        if self.trades.len() > TRADE_BUF_LEN {
            self.trades.pop_front();
        }
    }

    fn apply_fill(&mut self, order_id: u64, remaining_size: u32) {
        if let Some(mut info) = self.orders.remove(&order_id) {
            self.remove_from_level(&info);
            if remaining_size > 0 {
                info.size = remaining_size;
                self.add_to_level(info.side, info.price, remaining_size);
                self.orders.insert(order_id, info);
            }
        }
    }

    fn apply_clear(&mut self) {
        self.orders.clear();
        self.bid_levels.clear();
        self.ask_levels.clear();
    }

    fn commit(&mut self, ts: u64) {
        let has_bid = !self.bid_levels.is_empty();
        let has_ask = !self.ask_levels.is_empty();

        // Snapshot top BOOK_DEPTH levels into compact fixed-size arrays
        let (bids, n_bids) = snapshot_bids(&self.bid_levels);
        let (asks, n_asks) = snapshot_asks(&self.ask_levels);

        // Precompute mid/spread
        let (mid, spread) = if has_bid && has_ask {
            compute_mid_spread_from_levels(&self.bid_levels, &self.ask_levels)
        } else {
            (0.0, 0.0)
        };

        // Detect BBO change: compare current best bid/ask to previous
        let cur_best_bid = self.bid_levels.keys().next_back().copied();
        let cur_best_ask = self.ask_levels.keys().next().copied();
        let bbo_changed = cur_best_bid != self.prev_best_bid || cur_best_ask != self.prev_best_ask;
        self.prev_best_bid = cur_best_bid;
        self.prev_best_ask = cur_best_ask;

        // Snapshot flow accumulators at this commit point
        let best_bid_size = cur_best_bid
            .and_then(|p| self.bid_levels.get(&p).copied())
            .unwrap_or(0);
        let best_ask_size = cur_best_ask
            .and_then(|p| self.ask_levels.get(&p).copied())
            .unwrap_or(0);
        let flow_state = self.flow_accums.snapshot(
            ts,
            bbo_changed,
            cur_best_bid,
            cur_best_ask,
            best_bid_size,
            best_ask_size,
        );
        self.flow_states.push(flow_state);

        self.committed_states.push(CommittedState {
            ts,
            has_bid,
            has_ask,
            bids,
            asks,
            mid,
            spread,
            n_bids,
            n_asks,
            bbo_changed,
        });

        // Record tick-level mid price when both sides are quoted
        if has_bid && has_ask {
            self.tick_mid_prices.push((ts, mid));
        }
    }

    fn get_committed_state_at(&self, ts: u64) -> Option<&CommittedState> {
        // Binary search: find the latest committed state with ts <= boundary ts
        let idx = self
            .committed_states
            .partition_point(|cs| cs.ts <= ts);
        if idx == 0 {
            None
        } else {
            Some(&self.committed_states[idx - 1])
        }
    }

    fn fill_trades(&self, snap: &mut BookSnapshot) {
        let count = self.trades.len();
        let start_idx = TRADE_BUF_LEN - count;
        for (i, trade) in self.trades.iter().enumerate() {
            snap.trades[start_idx + i][0] = trade.price;
            snap.trades[start_idx + i][1] = trade.size;
            snap.trades[start_idx + i][2] = trade.aggressor_side;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_builder() -> BookBuilder {
        BookBuilder::new(1)
    }

    #[test]
    fn test_add_and_commit() {
        let mut bb = make_builder();
        // Add a bid and an ask
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);

        assert_eq!(bb.committed_states.len(), 1);
        let cs = &bb.committed_states[0];
        assert!(cs.has_bid);
        assert!(cs.has_ask);
        // Check compact snapshot: best bid at index 0
        assert!((cs.bids[0][0] - 4500.0).abs() < 0.01);
        assert!((cs.bids[0][1] - 10.0).abs() < 0.01);
        // Check compact snapshot: best ask at index 0
        assert!((cs.asks[0][0] - 4501.0).abs() < 0.01);
        assert!((cs.asks[0][1] - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_cancel_removes_order() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'C', 'B', 4500_000_000_000, 0, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(!cs.has_bid);
    }

    #[test]
    fn test_modify_updates_price_and_size() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'M', 'B', 4501_000_000_000, 15, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        // After modify: bid moved to 4501.0 with size 15
        assert!((cs.bids[0][0] - 4501.0).abs() < 0.01);
        assert!((cs.bids[0][1] - 15.0).abs() < 0.01);
    }

    #[test]
    fn test_modify_unknown_order_is_noop() {
        // A CHANGE for an order we never saw ADD must not create a phantom level.
        let mut bb = make_builder();
        bb.process_event(1000, 999, 1, 'M', 'B', 4500_000_000_000, 10, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(!cs.has_bid, "unknown CHANGE must not create a phantom bid level");
    }

    #[test]
    fn test_trade_buffer() {
        let mut bb = make_builder();
        bb.process_event(1000, 0, 1, 'T', 'B', 4500_000_000_000, 5, F_LAST);
        assert_eq!(bb.trades.len(), 1);
        assert!((bb.trades[0].aggressor_side - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_fill_reduces_size() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'F', 'B', 4500_000_000_000, 7, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!((cs.bids[0][1] - 7.0).abs() < 0.01);
    }

    #[test]
    fn test_fill_removes_when_zero() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'F', 'B', 4500_000_000_000, 0, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(!cs.has_bid);
    }

    #[test]
    fn test_clear() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);
        bb.process_event(2000, 0, 1, 'R', ' ', 0, 0, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(!cs.has_bid);
        assert!(!cs.has_ask);
    }

    #[test]
    fn test_instrument_filter() {
        let mut bb = make_builder();
        // Different instrument_id should be ignored
        bb.process_event(1000, 100, 99, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        assert!(bb.committed_states.is_empty());
    }

    #[test]
    fn test_fixed_to_float() {
        let price = 4500_250_000_000i64; // 4500.25
        let f = fixed_to_float(price);
        assert!((f - 4500.25).abs() < 0.01);
    }

    #[test]
    fn test_tick_mid_prices_tracked() {
        let mut bb = make_builder();
        // Add bid + ask with F_LAST → should record tick mid
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4500_250_000_000, 5, F_LAST);

        assert_eq!(bb.tick_mid_prices.len(), 1);
        assert_eq!(bb.tick_mid_prices[0].0, 1000);
        assert!((bb.tick_mid_prices[0].1 - 4500.125).abs() < 0.01);

        // Another commit with both sides → should add another entry
        bb.process_event(2000, 102, 1, 'A', 'B', 4501_000_000_000, 8, F_LAST);
        assert_eq!(bb.tick_mid_prices.len(), 2);
    }

    #[test]
    fn test_tick_mid_prices_skip_one_sided() {
        let mut bb = make_builder();
        // Only bid, no ask → should NOT record tick mid
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        assert!(bb.tick_mid_prices.is_empty());
    }

    #[test]
    fn test_take_tick_mid_prices() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4500_250_000_000, 5, F_LAST);

        let ticks = bb.take_tick_mid_prices();
        assert_eq!(ticks.len(), 1);
        // After take, internal vec should be empty
        assert!(bb.tick_mid_prices.is_empty());
    }

    #[test]
    fn test_mid_spread_computation() {
        let mut bids = BTreeMap::new();
        let mut asks = BTreeMap::new();
        bids.insert(4500_000_000_000i64, 10u32); // 4500.00
        asks.insert(4500_250_000_000i64, 5u32); // 4500.25

        let (mid, spread) = compute_mid_spread_from_levels(&bids, &asks);
        assert!((mid - 4500.125).abs() < 0.01);
        assert!((spread - 0.25).abs() < 0.01);
    }

    #[test]
    fn test_compact_committed_state_size() {
        // Verify that CommittedState is now compact (no heap allocations)
        let size = std::mem::size_of::<CommittedState>();
        // Should be ~192 bytes: 8 (ts) + 3 (bools) + 160 (bids+asks) + 8 (mid+spread) + 2 (counts) + padding
        assert!(size < 256, "CommittedState should be compact, got {} bytes", size);
    }

    #[test]
    fn test_bbo_changed_detection() {
        let mut bb = make_builder();
        // First commit: bid + ask → bbo_changed = true (from None)
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4500_250_000_000, 5, F_LAST);
        assert!(bb.committed_states[0].bbo_changed);

        // Second commit: add depth behind BBO → bbo_changed = false
        bb.process_event(2000, 102, 1, 'A', 'B', 4499_750_000_000, 8, F_LAST);
        assert!(!bb.committed_states[1].bbo_changed);

        // Third commit: move best bid up → bbo_changed = true
        bb.process_event(3000, 103, 1, 'A', 'B', 4500_125_000_000, 15, F_LAST);
        assert!(bb.committed_states[2].bbo_changed);
    }

    #[test]
    fn test_take_committed_states() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4500_250_000_000, 5, F_LAST);

        let states = bb.take_committed_states();
        assert_eq!(states.len(), 1);
        assert!(states[0].has_bid);
        assert!(states[0].has_ask);
        assert!(states[0].bbo_changed);
        // After take, internal vec should be empty
        assert!(bb.committed_states.is_empty());
    }

    #[test]
    fn test_flow_states_parallel_to_committed() {
        let mut bb = make_builder();
        // Two commits → should have 2 flow states
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1_000_000_000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);
        bb.process_event(2_000_000_000, 102, 1, 'A', 'B', 4500_500_000_000, 8, F_LAST);

        assert_eq!(bb.committed_states.len(), 2);
        assert_eq!(bb.flow_states.len(), 2);
        assert_eq!(bb.flow_states[0].ts, bb.committed_states[0].ts);
        assert_eq!(bb.flow_states[1].ts, bb.committed_states[1].ts);
    }

    #[test]
    fn test_flow_trade_through_process_event() {
        let mut bb = make_builder();
        // Set up a book
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1_000_000_000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);
        // Buyer-initiated trade
        bb.process_event(1_000_000_000, 0, 1, 'T', 'B', 4501_000_000_000, 3, F_LAST);

        let flow = &bb.flow_states[1]; // second commit (after trade)
        // Trade flow should be positive (buyer aggressor)
        assert!(flow.trade_flow[0] > 0.0, "Expected positive trade flow, got {}", flow.trade_flow[0]);
    }

    #[test]
    fn test_flow_bbo_cause_cancel_at_best() {
        let mut bb = make_builder();
        // Add bid at best
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        // Cancel it → BBO changes, cause = Cancel
        bb.process_event(2_000_000_000, 100, 1, 'C', 'B', 4500_000_000_000, 0, F_LAST);

        let flow = &bb.flow_states[1];
        assert_eq!(flow.bbo_change_cause, flow::BboChangeCause::Cancel);
    }

    #[test]
    fn test_flow_bbo_cause_new_level() {
        let mut bb = make_builder();
        // Add bid
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        // Add better bid → BBO changes, cause = NewLevel
        bb.process_event(2_000_000_000, 101, 1, 'A', 'B', 4500_500_000_000, 5, F_LAST);

        let flow = &bb.flow_states[1];
        assert_eq!(flow.bbo_change_cause, flow::BboChangeCause::NewLevel);
    }

    #[test]
    fn test_flow_bbo_cause_none_depth_behind() {
        let mut bb = make_builder();
        // Add bid at best
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        // Add depth behind best → BBO unchanged
        bb.process_event(2_000_000_000, 101, 1, 'A', 'B', 4499_000_000_000, 8, F_LAST);

        let flow = &bb.flow_states[1];
        assert_eq!(flow.bbo_change_cause, flow::BboChangeCause::None);
    }

    #[test]
    fn test_take_flow_states() {
        let mut bb = make_builder();
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);

        let states = bb.take_flow_states();
        assert_eq!(states.len(), 1);
        assert!(bb.flow_states.is_empty());
    }
}
