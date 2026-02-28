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

/// Committed book state after an F_LAST event.
#[derive(Debug, Clone)]
struct CommittedState {
    ts: u64,
    bid_levels: BTreeMap<i64, u32>,
    ask_levels: BTreeMap<i64, u32>,
    has_bid: bool,
    has_ask: bool,
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
}

const F_LAST: u8 = 0x80;

fn fixed_to_float(fixed: i64) -> f32 {
    (fixed as f64 / 1e9) as f32
}

fn compute_mid_spread(
    bids: &BTreeMap<i64, u32>,
    asks: &BTreeMap<i64, u32>,
) -> (f32, f32) {
    let best_bid = fixed_to_float(*bids.keys().next_back().unwrap());
    let best_ask = fixed_to_float(*asks.keys().next().unwrap());
    ((best_bid + best_ask) / 2.0, best_ask - best_bid)
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

        match action {
            'A' => self.apply_add(order_id, side, price, size),
            'C' => self.apply_cancel(order_id),
            'M' => self.apply_modify(order_id, side, price, size),
            'T' => self.apply_trade(side, price, size),
            'F' => self.apply_fill(order_id, size),
            'R' => self.apply_clear(),
            _ => {}
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
                let (mid, sprd) = compute_mid_spread(&cs.bid_levels, &cs.ask_levels);
                carry_mid = mid;
                carry_spread = sprd;
                both_sides_seen = true;
            }
        }
        self.last_mid_price = carry_mid;
        self.last_spread = carry_spread;
        self.ever_had_both_sides = both_sides_seen;

        let mut ts = aligned_start;
        while ts < eff_end {
            if let Some(state) = self.get_committed_state_at(ts) {
                let state = state.clone(); // clone to avoid borrow issues

                if state.has_bid && state.has_ask {
                    self.ever_had_both_sides = true;
                }

                if (!state.has_bid || !state.has_ask) && !self.ever_had_both_sides {
                    ts += SNAPSHOT_INTERVAL_NS;
                    continue;
                }

                let mut snap = BookSnapshot::default();
                snap.timestamp = ts;

                // Fill bids (descending by price)
                let mut bid_idx = 0;
                for (&price, &size) in state.bid_levels.iter().rev() {
                    if bid_idx >= BOOK_DEPTH {
                        break;
                    }
                    snap.bids[bid_idx][0] = fixed_to_float(price);
                    snap.bids[bid_idx][1] = size as f32;
                    bid_idx += 1;
                }

                // Fill asks (ascending by price)
                let mut ask_idx = 0;
                for (&price, &size) in state.ask_levels.iter() {
                    if ask_idx >= BOOK_DEPTH {
                        break;
                    }
                    snap.asks[ask_idx][0] = fixed_to_float(price);
                    snap.asks[ask_idx][1] = size as f32;
                    ask_idx += 1;
                }

                // Mid price and spread
                if state.has_bid && state.has_ask {
                    let (mid, sprd) = compute_mid_spread(&state.bid_levels, &state.ask_levels);
                    snap.mid_price = mid;
                    snap.spread = sprd;
                    self.last_mid_price = mid;
                    self.last_spread = sprd;
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
        if let Some(info) = self.orders.remove(&order_id) {
            self.remove_from_level(&info);
        }
        let info = OrderInfo {
            side,
            price: new_price,
            size: new_size,
        };
        self.orders.insert(order_id, info);
        self.add_to_level(side, new_price, new_size);
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
        self.committed_states.push(CommittedState {
            ts,
            bid_levels: self.bid_levels.clone(),
            ask_levels: self.ask_levels.clone(),
            has_bid: !self.bid_levels.is_empty(),
            has_ask: !self.ask_levels.is_empty(),
        });
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
        assert_eq!(*cs.bid_levels.get(&4500_000_000_000).unwrap(), 10);
        assert_eq!(*cs.ask_levels.get(&4501_000_000_000).unwrap(), 5);
    }

    #[test]
    fn test_cancel_removes_order() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'C', 'B', 4500_000_000_000, 0, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(cs.bid_levels.is_empty());
    }

    #[test]
    fn test_modify_updates_price_and_size() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'M', 'B', 4501_000_000_000, 15, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(cs.bid_levels.get(&4500_000_000_000).is_none());
        assert_eq!(*cs.bid_levels.get(&4501_000_000_000).unwrap(), 15);
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
        assert_eq!(*cs.bid_levels.get(&4500_000_000_000).unwrap(), 7);
    }

    #[test]
    fn test_fill_removes_when_zero() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'F', 'B', 4500_000_000_000, 0, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(cs.bid_levels.is_empty());
    }

    #[test]
    fn test_clear() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);
        bb.process_event(2000, 0, 1, 'R', ' ', 0, 0, F_LAST);

        let cs = bb.committed_states.last().unwrap();
        assert!(cs.bid_levels.is_empty());
        assert!(cs.ask_levels.is_empty());
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
    fn test_mid_spread_computation() {
        let mut bids = BTreeMap::new();
        let mut asks = BTreeMap::new();
        bids.insert(4500_000_000_000i64, 10u32); // 4500.00
        asks.insert(4500_250_000_000i64, 5u32); // 4500.25

        let (mid, spread) = compute_mid_spread(&bids, &asks);
        assert!((mid - 4500.125).abs() < 0.01);
        assert!((spread - 0.25).abs() < 0.01);
    }
}
