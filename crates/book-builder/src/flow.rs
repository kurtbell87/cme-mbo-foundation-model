//! Flow accumulators for microstructure dynamics.
//!
//! Maintains exponential moving averages of order flow metrics at multiple
//! timescales, updated O(1) per MBO event. Produces a compact FlowState
//! snapshot at each commit point alongside CommittedState.
//!
//! Designed for integration with the gate test: does OFI or cancel asymmetry
//! show predictive content for barrier outcomes?

/// Number of EMA timescales tracked in parallel.
pub const NUM_SCALES: usize = 3;

/// Default EMA halflives in nanoseconds.
/// Log-spaced: ~250ms (sub-second flow), ~2.5s (participant intent), ~25s (regime context).
/// These are initial values — calibrate empirically via trade flow autocorrelation.
pub const DEFAULT_HALFLIVES_NS: [u64; NUM_SCALES] = [
    250_000_000,      // 250ms — aggressive flow dynamics
    2_500_000_000,    // 2.5s  — participant-level intent
    25_000_000_000,   // 25s   — regime context
];

/// What caused the BBO to change at a commit point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BboChangeCause {
    /// No BBO change at this commit.
    None = 0,
    /// Trade or fill consumed the best level.
    AggressiveTrade = 1,
    /// Cancel removed last contracts at best level.
    Cancel = 2,
    /// New add created a better inside level.
    NewLevel = 3,
    /// Modify changed the best price.
    Modify = 4,
    /// Multiple action types in the same commit batch.
    Multiple = 5,
}

/// Multi-scale exponential moving average accumulator.
///
/// Each event contributes a value that decays exponentially over time.
/// Maintains parallel EMAs at NUM_SCALES different halflives.
#[derive(Debug, Clone)]
pub struct EmaAccumulator {
    values: [f64; NUM_SCALES],
    halflives_ns: [u64; NUM_SCALES],
    last_ts: u64,
    initialized: bool,
}

impl EmaAccumulator {
    pub fn new(halflives_ns: [u64; NUM_SCALES]) -> Self {
        Self {
            values: [0.0; NUM_SCALES],
            halflives_ns,
            last_ts: 0,
            initialized: false,
        }
    }

    /// Add a value at timestamp `ts`. Decays existing state, then adds.
    #[inline]
    pub fn update(&mut self, ts: u64, value: f64) {
        if self.initialized && ts > self.last_ts {
            let dt = (ts - self.last_ts) as f64;
            for i in 0..NUM_SCALES {
                let decay = (-0.693147 * dt / self.halflives_ns[i] as f64).exp();
                self.values[i] = self.values[i] * decay + value;
            }
        } else {
            for i in 0..NUM_SCALES {
                self.values[i] += value;
            }
        }
        self.last_ts = ts;
        self.initialized = true;
    }

    /// Decay to timestamp `ts` without adding a new value.
    /// Returns the current values at all scales.
    #[inline]
    pub fn query(&self, ts: u64) -> [f64; NUM_SCALES] {
        if self.initialized && ts > self.last_ts {
            let dt = (ts - self.last_ts) as f64;
            let mut out = [0.0; NUM_SCALES];
            for i in 0..NUM_SCALES {
                let decay = (-0.693147 * dt / self.halflives_ns[i] as f64).exp();
                out[i] = self.values[i] * decay;
            }
            out
        } else {
            self.values
        }
    }

    /// Reset all accumulators to zero.
    pub fn reset(&mut self) {
        self.values = [0.0; NUM_SCALES];
        self.last_ts = 0;
        self.initialized = false;
    }
}

/// Collection of flow accumulators maintained alongside the order book.
///
/// Updated on every MBO event in O(1). Produces FlowState snapshots at commit points.
#[derive(Debug, Clone)]
pub struct FlowAccumulators {
    /// Signed trade volume: +size for buyer aggressor, -size for seller.
    pub trade_flow: EmaAccumulator,
    /// Bid-side cancel volume.
    pub cancel_bid: EmaAccumulator,
    /// Ask-side cancel volume.
    pub cancel_ask: EmaAccumulator,
    /// Bid-side add volume.
    pub add_bid: EmaAccumulator,
    /// Ask-side add volume.
    pub add_ask: EmaAccumulator,
    /// Total event count (for event intensity).
    pub event_count: EmaAccumulator,
    /// Trade event count (for trade intensity).
    pub trade_count: EmaAccumulator,

    /// OFI accumulator — updated at commit time from queue size changes.
    pub ofi: EmaAccumulator,

    /// Previous best bid size (for OFI delta computation).
    prev_best_bid_size: u32,
    /// Previous best ask size (for OFI delta computation).
    prev_best_ask_size: u32,
    /// Previous best bid price (for OFI price-change handling).
    prev_best_bid_price: Option<i64>,
    /// Previous best ask price (for OFI price-change handling).
    prev_best_ask_price: Option<i64>,

    /// Actions that modified the best level since the last commit.
    /// Used to determine BboChangeCause.
    pending_bbo_actions: Vec<(char, char)>, // (action, side)
}

impl FlowAccumulators {
    pub fn new(halflives_ns: [u64; NUM_SCALES]) -> Self {
        Self {
            trade_flow: EmaAccumulator::new(halflives_ns),
            cancel_bid: EmaAccumulator::new(halflives_ns),
            cancel_ask: EmaAccumulator::new(halflives_ns),
            add_bid: EmaAccumulator::new(halflives_ns),
            add_ask: EmaAccumulator::new(halflives_ns),
            event_count: EmaAccumulator::new(halflives_ns),
            trade_count: EmaAccumulator::new(halflives_ns),
            ofi: EmaAccumulator::new(halflives_ns),
            prev_best_bid_size: 0,
            prev_best_ask_size: 0,
            prev_best_bid_price: None,
            prev_best_ask_price: None,
            pending_bbo_actions: Vec::new(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_HALFLIVES_NS)
    }

    /// Update accumulators with an MBO event.
    ///
    /// Called for every event BEFORE the book update, so `best_bid_price`/`best_ask_price`
    /// reflect the state prior to this event's effect. The `action_modifies_best` flag
    /// should be set after the book update if the action changed the best level.
    pub fn on_event(
        &mut self,
        ts: u64,
        action: char,
        side: char,
        size: u32,
    ) {
        let sz = size as f64;

        // Count all events
        self.event_count.update(ts, 1.0);

        match action {
            'T' => {
                // Trade: signed volume
                let signed = if side == 'B' { sz } else { -sz };
                self.trade_flow.update(ts, signed);
                self.trade_count.update(ts, 1.0);
            }
            'A' => {
                // Add: track by side
                if side == 'B' {
                    self.add_bid.update(ts, sz);
                } else {
                    self.add_ask.update(ts, sz);
                }
            }
            'C' => {
                // Cancel: track by side
                if side == 'B' {
                    self.cancel_bid.update(ts, sz);
                } else {
                    self.cancel_ask.update(ts, sz);
                }
            }
            'F' => {
                // Fill: like a trade for flow purposes (aggressive consumption)
                let signed = if side == 'B' { sz } else { -sz };
                self.trade_flow.update(ts, signed);
            }
            _ => {} // Modify, Clear — counted in event_count
        }
    }

    /// Record that an action potentially modified the BBO.
    /// Called after the book update when we detect a BBO-affecting action.
    pub fn record_bbo_action(&mut self, action: char, side: char) {
        self.pending_bbo_actions.push((action, side));
    }

    /// Snapshot flow state at a commit point.
    ///
    /// `bbo_changed`: whether the BBO changed at this commit.
    /// `best_bid_price`, `best_ask_price`: current best prices (i64 fixed-point).
    /// `best_bid_size`, `best_ask_size`: current best level sizes.
    pub fn snapshot(
        &mut self,
        ts: u64,
        bbo_changed: bool,
        best_bid_price: Option<i64>,
        best_ask_price: Option<i64>,
        best_bid_size: u32,
        best_ask_size: u32,
    ) -> FlowState {
        // Compute OFI (Cont, Kukanov, Stoikov 2014):
        // OFI = Δ(bid_queue_at_best) - Δ(ask_queue_at_best)
        // Accounts for price level changes.
        let bid_ofi = compute_side_ofi(
            self.prev_best_bid_price,
            self.prev_best_bid_size,
            best_bid_price,
            best_bid_size,
        );
        let ask_ofi = compute_side_ofi(
            self.prev_best_ask_price,
            self.prev_best_ask_size,
            best_ask_price,
            best_ask_size,
        );
        let ofi_value = bid_ofi - ask_ofi;

        if ofi_value.abs() > 0.0 {
            self.ofi.update(ts, ofi_value);
        }

        // Update previous state for next OFI computation
        self.prev_best_bid_price = best_bid_price;
        self.prev_best_ask_price = best_ask_price;
        self.prev_best_bid_size = best_bid_size;
        self.prev_best_ask_size = best_ask_size;

        // Determine BBO change cause
        let cause = if !bbo_changed {
            BboChangeCause::None
        } else {
            classify_bbo_cause(&self.pending_bbo_actions)
        };
        self.pending_bbo_actions.clear();

        // Query all accumulators at current time
        let trade_flow = to_f32_3(self.trade_flow.query(ts));
        let cancel_bid = to_f32_3(self.cancel_bid.query(ts));
        let cancel_ask = to_f32_3(self.cancel_ask.query(ts));
        let add_bid = to_f32_3(self.add_bid.query(ts));
        let add_ask = to_f32_3(self.add_ask.query(ts));
        let event_intensity = to_f32_3(self.event_count.query(ts));
        let trade_intensity = to_f32_3(self.trade_count.query(ts));
        let ofi = to_f32_3(self.ofi.query(ts));

        FlowState {
            ts,
            trade_flow,
            cancel_bid,
            cancel_ask,
            add_bid,
            add_ask,
            event_intensity,
            trade_intensity,
            ofi,
            bbo_change_cause: cause,
        }
    }
}

/// Compute one side's OFI contribution.
///
/// Per Cont, Kukanov, Stoikov (2014):
/// - If price improved (bid up or ask down): contribution = new size
/// - If price unchanged: contribution = size_change (new - old)
/// - If price deteriorated (bid down or ask up): contribution = -old_size
fn compute_side_ofi(
    prev_price: Option<i64>,
    prev_size: u32,
    cur_price: Option<i64>,
    cur_size: u32,
) -> f64 {
    match (prev_price, cur_price) {
        (Some(prev), Some(cur)) => {
            if cur > prev {
                // Price improved (bid up) — new level
                cur_size as f64
            } else if cur == prev {
                // Same price — queue size change
                cur_size as f64 - prev_size as f64
            } else {
                // Price deteriorated (bid down) — level consumed
                -(prev_size as f64)
            }
        }
        (None, Some(_)) => {
            // No previous level, new level appeared
            cur_size as f64
        }
        (Some(_), None) => {
            // Level disappeared
            -(prev_size as f64)
        }
        (None, None) => 0.0,
    }
}

/// Classify what caused the BBO change from pending actions.
fn classify_bbo_cause(actions: &[(char, char)]) -> BboChangeCause {
    if actions.is_empty() {
        return BboChangeCause::None;
    }

    let mut has_trade = false;
    let mut has_cancel = false;
    let mut has_add = false;
    let mut has_modify = false;

    for &(action, _side) in actions {
        match action {
            'T' | 'F' => has_trade = true,
            'C' => has_cancel = true,
            'A' => has_add = true,
            'M' => has_modify = true,
            _ => {}
        }
    }

    let count = has_trade as u8 + has_cancel as u8 + has_add as u8 + has_modify as u8;
    if count > 1 {
        return BboChangeCause::Multiple;
    }

    if has_trade {
        BboChangeCause::AggressiveTrade
    } else if has_cancel {
        BboChangeCause::Cancel
    } else if has_add {
        BboChangeCause::NewLevel
    } else if has_modify {
        BboChangeCause::Modify
    } else {
        BboChangeCause::None
    }
}

/// Compact snapshot of flow state at a commit point.
///
/// Fixed-size, Copy, no heap allocations. Parallel to CommittedState.
#[derive(Debug, Clone, Copy)]
pub struct FlowState {
    pub ts: u64,
    /// Signed trade volume (buyer+ / seller-) at each timescale.
    pub trade_flow: [f32; NUM_SCALES],
    /// Bid-side cancel volume at each timescale.
    pub cancel_bid: [f32; NUM_SCALES],
    /// Ask-side cancel volume at each timescale.
    pub cancel_ask: [f32; NUM_SCALES],
    /// Bid-side add volume at each timescale.
    pub add_bid: [f32; NUM_SCALES],
    /// Ask-side add volume at each timescale.
    pub add_ask: [f32; NUM_SCALES],
    /// Event intensity (decayed count) at each timescale.
    pub event_intensity: [f32; NUM_SCALES],
    /// Trade intensity (decayed count) at each timescale.
    pub trade_intensity: [f32; NUM_SCALES],
    /// Order flow imbalance (Cont et al. 2014) at each timescale.
    pub ofi: [f32; NUM_SCALES],
    /// What caused the BBO change at this commit.
    pub bbo_change_cause: BboChangeCause,
}

#[inline]
fn to_f32_3(v: [f64; NUM_SCALES]) -> [f32; NUM_SCALES] {
    [v[0] as f32, v[1] as f32, v[2] as f32]
}

/// Feature names for flow state, in order.
/// Used by downstream crates (event-export) for Parquet column naming.
pub const FLOW_FEATURE_NAMES: &[&str] = &[
    // trade_flow × 3 scales
    "trade_flow_fast", "trade_flow_med", "trade_flow_slow",
    // cancel_bid × 3
    "cancel_bid_fast", "cancel_bid_med", "cancel_bid_slow",
    // cancel_ask × 3
    "cancel_ask_fast", "cancel_ask_med", "cancel_ask_slow",
    // add_bid × 3
    "add_bid_fast", "add_bid_med", "add_bid_slow",
    // add_ask × 3
    "add_ask_fast", "add_ask_med", "add_ask_slow",
    // event_intensity × 3
    "event_intensity_fast", "event_intensity_med", "event_intensity_slow",
    // trade_intensity × 3
    "trade_intensity_fast", "trade_intensity_med", "trade_intensity_slow",
    // ofi × 3
    "ofi_fast", "ofi_med", "ofi_slow",
    // bbo_change_cause (categorical, encoded as u8)
    "bbo_change_cause",
];

/// Number of flow features (8 accumulators × 3 scales + 1 cause = 25).
pub const NUM_FLOW_FEATURES: usize = 25;

impl FlowState {
    /// Extract flow features as a flat f32 array for model input.
    pub fn to_features(&self) -> [f32; NUM_FLOW_FEATURES] {
        let mut out = [0.0f32; NUM_FLOW_FEATURES];
        let mut i = 0;

        for &v in &self.trade_flow { out[i] = v; i += 1; }
        for &v in &self.cancel_bid { out[i] = v; i += 1; }
        for &v in &self.cancel_ask { out[i] = v; i += 1; }
        for &v in &self.add_bid { out[i] = v; i += 1; }
        for &v in &self.add_ask { out[i] = v; i += 1; }
        for &v in &self.event_intensity { out[i] = v; i += 1; }
        for &v in &self.trade_intensity { out[i] = v; i += 1; }
        for &v in &self.ofi { out[i] = v; i += 1; }
        out[i] = self.bbo_change_cause as u8 as f32;

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ema_single_update() {
        let mut ema = EmaAccumulator::new(DEFAULT_HALFLIVES_NS);
        ema.update(1_000_000_000, 10.0); // 1s
        let vals = ema.query(1_000_000_000);
        assert!((vals[0] - 10.0).abs() < 1e-6);
        assert!((vals[1] - 10.0).abs() < 1e-6);
        assert!((vals[2] - 10.0).abs() < 1e-6);
    }

    #[test]
    fn test_ema_decay() {
        let mut ema = EmaAccumulator::new(DEFAULT_HALFLIVES_NS);
        ema.update(0, 100.0);

        // After one halflife, value should be ~50
        let vals_fast = ema.query(DEFAULT_HALFLIVES_NS[0]);
        assert!(
            (vals_fast[0] - 50.0).abs() < 1.0,
            "Expected ~50 after one fast halflife, got {:.1}",
            vals_fast[0]
        );

        // Slow scale should barely decay
        let vals_slow = ema.query(DEFAULT_HALFLIVES_NS[0]);
        assert!(
            vals_slow[2] > 90.0,
            "Slow scale should barely decay after fast halflife, got {:.1}",
            vals_slow[2]
        );
    }

    #[test]
    fn test_ema_multiple_updates() {
        let mut ema = EmaAccumulator::new(DEFAULT_HALFLIVES_NS);
        // Two events at different times
        ema.update(0, 100.0);
        ema.update(250_000_000, 100.0); // 250ms later (one fast halflife)

        let vals = ema.query(250_000_000);
        // Fast scale: 100 * 0.5 + 100 = 150
        assert!(
            (vals[0] - 150.0).abs() < 1.0,
            "Expected ~150, got {:.1}",
            vals[0]
        );
    }

    #[test]
    fn test_flow_accumulators_trade() {
        let mut accums = FlowAccumulators::with_defaults();
        // Buyer-initiated trade
        accums.on_event(1_000_000_000, 'T', 'B', 5);
        // Seller-initiated trade
        accums.on_event(1_000_000_000, 'T', 'A', 3);

        let state = accums.snapshot(1_000_000_000, false, Some(100), Some(101), 10, 10);
        // Net trade flow: +5 - 3 = +2
        assert!(
            (state.trade_flow[0] - 2.0).abs() < 0.1,
            "Expected trade_flow ~2.0, got {:.1}",
            state.trade_flow[0]
        );
        // Two trades
        assert!(
            (state.trade_intensity[0] - 2.0).abs() < 0.1,
            "Expected 2 trades, got {:.1}",
            state.trade_intensity[0]
        );
    }

    #[test]
    fn test_flow_accumulators_cancel_asymmetry() {
        let mut accums = FlowAccumulators::with_defaults();
        // More bid cancels than ask cancels
        accums.on_event(1_000_000_000, 'C', 'B', 20);
        accums.on_event(1_000_000_000, 'C', 'B', 15);
        accums.on_event(1_000_000_000, 'C', 'A', 5);

        let state = accums.snapshot(1_000_000_000, false, Some(100), Some(101), 10, 10);
        // cancel_bid should be larger than cancel_ask
        assert!(state.cancel_bid[0] > state.cancel_ask[0]);
        assert!((state.cancel_bid[0] - 35.0).abs() < 0.1);
        assert!((state.cancel_ask[0] - 5.0).abs() < 0.1);
    }

    #[test]
    fn test_ofi_bid_improvement() {
        let mut accums = FlowAccumulators::with_defaults();
        // Set initial state
        accums.snapshot(0, false, Some(100), Some(102), 10, 10);
        // Bid improves: price goes up → OFI positive (buying pressure)
        let state = accums.snapshot(1_000_000, true, Some(101), Some(102), 8, 10);
        // bid_ofi = 8 (new level), ask_ofi = 0 (unchanged)
        // OFI = 8 - 0 = 8
        assert!(
            state.ofi[0] > 0.0,
            "OFI should be positive on bid improvement, got {:.1}",
            state.ofi[0]
        );
    }

    #[test]
    fn test_ofi_bid_consumed() {
        let mut accums = FlowAccumulators::with_defaults();
        // Set initial state
        accums.snapshot(0, false, Some(100), Some(102), 50, 10);
        // Bid deteriorates: price drops → OFI negative
        let state = accums.snapshot(1_000_000, true, Some(99), Some(102), 5, 10);
        // bid_ofi = -50 (old level consumed), ask_ofi = 0
        // OFI = -50 - 0 = -50
        assert!(
            state.ofi[0] < 0.0,
            "OFI should be negative when bid consumed, got {:.1}",
            state.ofi[0]
        );
    }

    #[test]
    fn test_ofi_queue_change_same_price() {
        let mut accums = FlowAccumulators::with_defaults();
        // Set initial state
        accums.snapshot(0, false, Some(100), Some(102), 10, 10);
        // Same price, bid queue grows
        let state = accums.snapshot(1_000_000, false, Some(100), Some(102), 15, 10);
        // bid_ofi = 15 - 10 = 5, ask_ofi = 0
        // OFI = 5
        assert!(
            (state.ofi[0] - 5.0).abs() < 0.1,
            "OFI should be +5 from queue growth, got {:.1}",
            state.ofi[0]
        );
    }

    #[test]
    fn test_bbo_change_cause_aggressive_trade() {
        let mut accums = FlowAccumulators::with_defaults();
        accums.record_bbo_action('T', 'B');
        let state = accums.snapshot(1_000_000, true, Some(100), Some(102), 10, 10);
        assert_eq!(state.bbo_change_cause, BboChangeCause::AggressiveTrade);
    }

    #[test]
    fn test_bbo_change_cause_cancel() {
        let mut accums = FlowAccumulators::with_defaults();
        accums.record_bbo_action('C', 'B');
        let state = accums.snapshot(1_000_000, true, Some(100), Some(102), 10, 10);
        assert_eq!(state.bbo_change_cause, BboChangeCause::Cancel);
    }

    #[test]
    fn test_bbo_change_cause_multiple() {
        let mut accums = FlowAccumulators::with_defaults();
        accums.record_bbo_action('C', 'B');
        accums.record_bbo_action('A', 'A');
        let state = accums.snapshot(1_000_000, true, Some(100), Some(102), 10, 10);
        assert_eq!(state.bbo_change_cause, BboChangeCause::Multiple);
    }

    #[test]
    fn test_bbo_change_cause_none_when_no_change() {
        let mut accums = FlowAccumulators::with_defaults();
        accums.record_bbo_action('C', 'B'); // This would be cleared
        let state = accums.snapshot(1_000_000, false, Some(100), Some(102), 10, 10);
        assert_eq!(state.bbo_change_cause, BboChangeCause::None);
    }

    #[test]
    fn test_flow_state_to_features() {
        let accums = FlowAccumulators::with_defaults();
        let state = FlowState {
            ts: 0,
            trade_flow: [1.0, 2.0, 3.0],
            cancel_bid: [4.0, 5.0, 6.0],
            cancel_ask: [7.0, 8.0, 9.0],
            add_bid: [10.0, 11.0, 12.0],
            add_ask: [13.0, 14.0, 15.0],
            event_intensity: [16.0, 17.0, 18.0],
            trade_intensity: [19.0, 20.0, 21.0],
            ofi: [22.0, 23.0, 24.0],
            bbo_change_cause: BboChangeCause::Cancel,
        };
        let features = state.to_features();
        assert_eq!(features.len(), NUM_FLOW_FEATURES);
        assert!((features[0] - 1.0).abs() < 1e-6); // trade_flow_fast
        assert!((features[23] - 24.0).abs() < 1e-6); // ofi_slow
        assert!((features[24] - 2.0).abs() < 1e-6); // bbo_change_cause = Cancel = 2
        let _ = accums; // suppress unused warning
    }

    #[test]
    fn test_num_flow_features_matches_names() {
        assert_eq!(NUM_FLOW_FEATURES, FLOW_FEATURE_NAMES.len());
    }
}
