pub mod flow;

use std::collections::{BTreeMap, HashMap};

/// Number of price levels per side (bid/ask).
pub const BOOK_DEPTH: usize = 10;

/// Per-order tracking info.
#[derive(Debug, Clone)]
struct OrderInfo {
    side: char,
    price: i64,
    size: u32,
}

/// Compact committed book state after an F_LAST event.
///
/// Stores only the top BOOK_DEPTH levels per side plus precomputed mid/spread.
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

/// Reconstructs an L2 order book from MBO events.
///
/// Processes Databento MBO events (Add/Cancel/Modify/Trade/Fill/Clear)
/// and maintains per-order tracking + aggregated price levels. After each
/// F_LAST flag, snapshots the flow accumulators.
pub struct BookBuilder {
    instrument_id: u32,

    // Per-order tracking: order_id -> {side, price, size}
    orders: HashMap<u64, OrderInfo>,

    // Aggregated price levels (price -> total size)
    bid_levels: BTreeMap<i64, u32>, // ascending by price (last = best bid)
    ask_levels: BTreeMap<i64, u32>, // ascending by price (first = best ask)

    // Flow accumulators for microstructure dynamics
    flow_accums: flow::FlowAccumulators,

    // Cached flow state from last commit
    last_flow_state: Option<flow::FlowState>,

    // Previous BBO prices for bbo_changed detection (raw i64 fixed-point)
    prev_best_bid: Option<i64>,
    prev_best_ask: Option<i64>,
}

const F_LAST: u8 = 0x80;

fn fixed_to_float(fixed: i64) -> f32 {
    (fixed as f64 / 1e9) as f32
}

/// Snapshot the top BOOK_DEPTH bid levels into a fixed-size array (descending by price).
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

/// Snapshot the top BOOK_DEPTH ask levels into a fixed-size array (ascending by price).
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

fn compute_mid_spread_from_levels(
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
            flow_accums: flow::FlowAccumulators::with_defaults(),
            last_flow_state: None,
            prev_best_bid: None,
            prev_best_ask: None,
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

    // --- Public accessors ---

    /// Returns the best bid price as raw i64 fixed-point (1e-9 scale).
    pub fn best_bid_price(&self) -> Option<i64> {
        self.bid_levels.keys().next_back().copied()
    }

    /// Returns the best ask price as raw i64 fixed-point (1e-9 scale).
    pub fn best_ask_price(&self) -> Option<i64> {
        self.ask_levels.keys().next().copied()
    }

    /// Returns the best bid size (0 if no bids).
    pub fn best_bid_size(&self) -> u32 {
        self.bid_levels
            .iter()
            .next_back()
            .map(|(_, &s)| s)
            .unwrap_or(0)
    }

    /// Returns the best ask size (0 if no asks).
    pub fn best_ask_size(&self) -> u32 {
        self.ask_levels
            .iter()
            .next()
            .map(|(_, &s)| s)
            .unwrap_or(0)
    }

    /// Snapshot the current book state as a CommittedState.
    pub fn current_committed_state(&self, ts: u64) -> CommittedState {
        let has_bid = !self.bid_levels.is_empty();
        let has_ask = !self.ask_levels.is_empty();
        let (bids, n_bids) = snapshot_bids(&self.bid_levels);
        let (asks, n_asks) = snapshot_asks(&self.ask_levels);
        let (mid, spread) = if has_bid && has_ask {
            compute_mid_spread_from_levels(&self.bid_levels, &self.ask_levels)
        } else {
            (0.0, 0.0)
        };
        let cur_best_bid = self.bid_levels.keys().next_back().copied();
        let cur_best_ask = self.ask_levels.keys().next().copied();
        let bbo_changed = cur_best_bid != self.prev_best_bid || cur_best_ask != self.prev_best_ask;
        CommittedState {
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
        }
    }

    /// Get the flow state from the most recent commit.
    /// Returns a default FlowState if no commit has occurred yet.
    pub fn current_flow_state(&self) -> flow::FlowState {
        self.last_flow_state.clone().unwrap_or_else(|| flow::FlowState {
            ts: 0,
            trade_flow: [0.0; flow::NUM_SCALES],
            cancel_bid: [0.0; flow::NUM_SCALES],
            cancel_ask: [0.0; flow::NUM_SCALES],
            add_bid: [0.0; flow::NUM_SCALES],
            add_ask: [0.0; flow::NUM_SCALES],
            event_intensity: [0.0; flow::NUM_SCALES],
            trade_intensity: [0.0; flow::NUM_SCALES],
            ofi: [0.0; flow::NUM_SCALES],
            inter_event_time_ns: 0.0,
            event_rate: 0.0,
            bbo_change_cause: flow::BboChangeCause::None,
        })
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
            self.orders.insert(order_id, OrderInfo { side, price: new_price, size: new_size });
            self.add_to_level(side, new_price, new_size);
        }
    }

    fn apply_trade(&mut self, _side: char, _price: i64, _size: u32) {
        // Trades are tracked by flow accumulators only — no trade buffer needed
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
        // Update BBO tracking
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
        self.last_flow_state = Some(flow_state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_builder() -> BookBuilder {
        BookBuilder::new(1)
    }

    #[test]
    fn test_add_order() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);

        assert_eq!(bb.best_bid_price(), Some(4500_000_000_000));
        assert_eq!(bb.best_ask_price(), Some(4501_000_000_000));
        assert_eq!(bb.best_bid_size(), 10);
        assert_eq!(bb.best_ask_size(), 5);
    }

    #[test]
    fn test_cancel_removes_order() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'C', 'B', 4500_000_000_000, 0, F_LAST);

        assert_eq!(bb.best_bid_price(), None);
    }

    #[test]
    fn test_modify_updates_price_and_size() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'M', 'B', 4501_000_000_000, 15, F_LAST);

        assert_eq!(bb.best_bid_price(), Some(4501_000_000_000));
        assert_eq!(bb.best_bid_size(), 15);
    }

    #[test]
    fn test_modify_unknown_order_is_noop() {
        let mut bb = make_builder();
        bb.process_event(1000, 999, 1, 'M', 'B', 4500_000_000_000, 10, F_LAST);
        assert_eq!(bb.best_bid_price(), None);
    }

    #[test]
    fn test_fill_reduces_size() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'F', 'B', 4500_000_000_000, 7, F_LAST);

        assert_eq!(bb.best_bid_size(), 7);
    }

    #[test]
    fn test_fill_removes_when_zero() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2000, 100, 1, 'F', 'B', 4500_000_000_000, 0, F_LAST);

        assert_eq!(bb.best_bid_price(), None);
    }

    #[test]
    fn test_clear() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);
        bb.process_event(2000, 0, 1, 'R', ' ', 0, 0, F_LAST);

        assert_eq!(bb.best_bid_price(), None);
        assert_eq!(bb.best_ask_price(), None);
    }

    #[test]
    fn test_instrument_filter() {
        let mut bb = make_builder();
        bb.process_event(1000, 100, 99, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        assert_eq!(bb.best_bid_price(), None);
    }

    #[test]
    fn test_fixed_to_float() {
        let price = 4500_250_000_000i64;
        let f = fixed_to_float(price);
        assert!((f - 4500.25).abs() < 0.01);
    }

    #[test]
    fn test_current_committed_state() {
        let mut bb = make_builder();
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1_000_000_000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);

        let cs = bb.current_committed_state(1_000_000_000);
        assert!(cs.has_bid);
        assert!(cs.has_ask);
        assert!((cs.bids[0][1] - 10.0).abs() < 0.01);
        assert!((cs.asks[0][1] - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_flow_trade_through_process_event() {
        let mut bb = make_builder();
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, 0);
        bb.process_event(1_000_000_000, 101, 1, 'A', 'A', 4501_000_000_000, 5, F_LAST);
        bb.process_event(1_000_000_000, 0, 1, 'T', 'B', 4501_000_000_000, 3, F_LAST);

        let flow = bb.current_flow_state();
        assert!(flow.trade_flow[0] > 0.0, "Expected positive trade flow, got {}", flow.trade_flow[0]);
    }

    #[test]
    fn test_flow_bbo_cause_cancel_at_best() {
        let mut bb = make_builder();
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2_000_000_000, 100, 1, 'C', 'B', 4500_000_000_000, 0, F_LAST);

        let flow = bb.current_flow_state();
        assert_eq!(flow.bbo_change_cause, flow::BboChangeCause::Cancel);
    }

    #[test]
    fn test_flow_bbo_cause_new_level() {
        let mut bb = make_builder();
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2_000_000_000, 101, 1, 'A', 'B', 4500_500_000_000, 5, F_LAST);

        let flow = bb.current_flow_state();
        assert_eq!(flow.bbo_change_cause, flow::BboChangeCause::NewLevel);
    }

    #[test]
    fn test_flow_bbo_cause_none_depth_behind() {
        let mut bb = make_builder();
        bb.process_event(1_000_000_000, 100, 1, 'A', 'B', 4500_000_000_000, 10, F_LAST);
        bb.process_event(2_000_000_000, 101, 1, 'A', 'B', 4499_000_000_000, 8, F_LAST);

        let flow = bb.current_flow_state();
        assert_eq!(flow.bbo_change_cause, flow::BboChangeCause::None);
    }
}
