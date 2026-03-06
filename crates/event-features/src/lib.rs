//! Event-level LOB feature computation.
//!
//! Computes 42 LOB features from a `CommittedState` + recent event window,
//! plus 2 geometry inputs (T, S) for a total of 44 model dimensions.

use book_builder::{CommittedState, BOOK_DEPTH};
use common::event::MBOEvent;

/// Total number of LOB features (excluding T, S geometry inputs).
pub const NUM_LOB_FEATURES: usize = 42;

/// Total number of model inputs (LOB features + T + S).
pub const NUM_MODEL_INPUTS: usize = 44;

/// Feature names for the 42 LOB features, in order.
pub const LOB_FEATURE_NAMES: [&str; NUM_LOB_FEATURES] = [
    // Book depth profile: 10 bid sizes, 10 ask sizes (20)
    "bid_size_0", "bid_size_1", "bid_size_2", "bid_size_3", "bid_size_4",
    "bid_size_5", "bid_size_6", "bid_size_7", "bid_size_8", "bid_size_9",
    "ask_size_0", "ask_size_1", "ask_size_2", "ask_size_3", "ask_size_4",
    "ask_size_5", "ask_size_6", "ask_size_7", "ask_size_8", "ask_size_9",
    // Book imbalance at depths 1, 3, 5, 10 (4)
    "imbalance_d1", "imbalance_d3", "imbalance_d5", "imbalance_d10",
    // Weighted imbalance (1)
    "weighted_imbalance",
    // Spread in ticks (1)
    "spread_ticks",
    // HHI per side (2)
    "bid_hhi", "ask_hhi",
    // Book slope per side (2)
    "bid_slope", "ask_slope",
    // Active level count per side (2)
    "bid_levels", "ask_levels",
    // Cancel rate at inside bid/ask (2)
    "cancel_rate_bid", "cancel_rate_ask",
    // Add rate at inside bid/ask (2)
    "add_rate_bid", "add_rate_ask",
    // Trade aggression (1)
    "trade_aggression",
    // Message rate (1)
    "message_rate",
    // Cancel-add ratio (1)
    "cancel_add_ratio",
    // Inside quote changes (1)
    "bbo_change_count",
    // Price momentum (1)
    "price_momentum_ticks",
    // Order flow toxicity (1)
    "flow_toxicity",
];

/// Full model input names (42 LOB features + T + S).
pub const MODEL_INPUT_NAMES: [&str; NUM_MODEL_INPUTS] = [
    "bid_size_0", "bid_size_1", "bid_size_2", "bid_size_3", "bid_size_4",
    "bid_size_5", "bid_size_6", "bid_size_7", "bid_size_8", "bid_size_9",
    "ask_size_0", "ask_size_1", "ask_size_2", "ask_size_3", "ask_size_4",
    "ask_size_5", "ask_size_6", "ask_size_7", "ask_size_8", "ask_size_9",
    "imbalance_d1", "imbalance_d3", "imbalance_d5", "imbalance_d10",
    "weighted_imbalance",
    "spread_ticks",
    "bid_hhi", "ask_hhi",
    "bid_slope", "ask_slope",
    "bid_levels", "ask_levels",
    "cancel_rate_bid", "cancel_rate_ask",
    "add_rate_bid", "add_rate_ask",
    "trade_aggression",
    "message_rate",
    "cancel_add_ratio",
    "bbo_change_count",
    "price_momentum_ticks",
    "flow_toxicity",
    "target_ticks", "stop_ticks",
];

/// Configuration for the event window used to compute rolling features.
#[derive(Debug, Clone)]
pub struct EventWindowConfig {
    /// Number of recent events to look back over.
    pub lookback_events: usize,
    /// Tick size for the instrument (e.g., 0.25 for MES).
    pub tick_size: f32,
}

impl Default for EventWindowConfig {
    fn default() -> Self {
        Self {
            lookback_events: 200,
            tick_size: 0.25,
        }
    }
}

/// Output of the feature computer: 42 LOB features as a fixed-size array.
pub type LobFeatures = [f32; NUM_LOB_FEATURES];

/// Compute the full 42-element LOB feature vector from a committed state
/// and a recent event window.
///
/// `state`: The current committed book state.
/// `recent_events`: Slice of recent MBO events (up to lookback_events).
/// `best_bid_at_events`: Best bid price at each event's committed state (for inside detection).
/// `best_ask_at_events`: Best ask price at each event's committed state (for inside detection).
/// `cfg`: Event window configuration.
pub fn compute_lob_features(
    state: &CommittedState,
    recent_events: &[MBOEvent],
    cfg: &EventWindowConfig,
) -> LobFeatures {
    let mut f = [0.0f32; NUM_LOB_FEATURES];
    let mut idx = 0;

    // --- Book depth profile: 10 bid sizes, 10 ask sizes (20 features) ---
    for i in 0..BOOK_DEPTH {
        f[idx] = state.bids[i][1]; // bid size at level i
        idx += 1;
    }
    for i in 0..BOOK_DEPTH {
        f[idx] = state.asks[i][1]; // ask size at level i
        idx += 1;
    }

    // --- Book imbalance at depths 1, 3, 5, 10 (4 features) ---
    for &depth in &[1usize, 3, 5, 10] {
        f[idx] = book_imbalance(state, depth);
        idx += 1;
    }

    // --- Weighted imbalance (1 feature) ---
    f[idx] = weighted_imbalance(state);
    idx += 1;

    // --- Spread in ticks (1 feature) ---
    f[idx] = state.spread / cfg.tick_size;
    idx += 1;

    // --- HHI per side (2 features) ---
    f[idx] = depth_hhi_bid(state);
    idx += 1;
    f[idx] = depth_hhi_ask(state);
    idx += 1;

    // --- Book slope per side (2 features) ---
    let (bid_slope, ask_slope) = book_slopes(state);
    f[idx] = bid_slope;
    idx += 1;
    f[idx] = ask_slope;
    idx += 1;

    // --- Active level count per side (2 features) ---
    f[idx] = state.n_bids as f32;
    idx += 1;
    f[idx] = state.n_asks as f32;
    idx += 1;

    // --- Rolling event window features (10 features) ---
    let window = compute_event_window_features(state, recent_events, cfg);
    f[idx] = window.cancel_rate_bid;
    idx += 1;
    f[idx] = window.cancel_rate_ask;
    idx += 1;
    f[idx] = window.add_rate_bid;
    idx += 1;
    f[idx] = window.add_rate_ask;
    idx += 1;
    f[idx] = window.trade_aggression;
    idx += 1;
    f[idx] = window.message_rate;
    idx += 1;
    f[idx] = window.cancel_add_ratio;
    idx += 1;
    f[idx] = window.bbo_change_count;
    idx += 1;
    f[idx] = window.price_momentum_ticks;
    idx += 1;
    f[idx] = window.flow_toxicity;

    debug_assert_eq!(idx, NUM_LOB_FEATURES - 1);
    f
}

/// Compute the full 44-element model input vector (42 LOB features + T + S).
pub fn compute_model_inputs(
    state: &CommittedState,
    recent_events: &[MBOEvent],
    cfg: &EventWindowConfig,
    target_ticks: i32,
    stop_ticks: i32,
) -> [f32; NUM_MODEL_INPUTS] {
    let lob = compute_lob_features(state, recent_events, cfg);
    let mut out = [0.0f32; NUM_MODEL_INPUTS];
    out[..NUM_LOB_FEATURES].copy_from_slice(&lob);
    out[NUM_LOB_FEATURES] = target_ticks as f32;
    out[NUM_LOB_FEATURES + 1] = stop_ticks as f32;
    out
}

// --- Instantaneous book features ---

/// Book imbalance at a given depth: (sum_bid - sum_ask) / (sum_bid + sum_ask).
/// Returns 0.0 if both sides empty.
fn book_imbalance(state: &CommittedState, depth: usize) -> f32 {
    let d = depth.min(BOOK_DEPTH);
    let mut bid_sum = 0.0f32;
    let mut ask_sum = 0.0f32;
    for i in 0..d {
        bid_sum += state.bids[i][1];
        ask_sum += state.asks[i][1];
    }
    let total = bid_sum + ask_sum;
    if total == 0.0 {
        0.0
    } else {
        (bid_sum - ask_sum) / total
    }
}

/// Weighted imbalance: sum(bid_size_i / (i+1)) - sum(ask_size_i / (i+1)) normalized.
fn weighted_imbalance(state: &CommittedState) -> f32 {
    let mut bid_w = 0.0f32;
    let mut ask_w = 0.0f32;
    for i in 0..BOOK_DEPTH {
        let weight = 1.0 / (i as f32 + 1.0);
        bid_w += state.bids[i][1] * weight;
        ask_w += state.asks[i][1] * weight;
    }
    let total = bid_w + ask_w;
    if total == 0.0 {
        0.0
    } else {
        (bid_w - ask_w) / total
    }
}

/// Herfindahl-Hirschman Index for bid depth concentration.
/// HHI = sum((size_i / total)^2). 1.0 = all at one level. 1/N = uniform.
fn depth_hhi_bid(state: &CommittedState) -> f32 {
    depth_hhi(&state.bids, state.n_bids as usize)
}

fn depth_hhi_ask(state: &CommittedState) -> f32 {
    depth_hhi(&state.asks, state.n_asks as usize)
}

fn depth_hhi(levels: &[[f32; 2]; BOOK_DEPTH], n: usize) -> f32 {
    let total: f32 = levels[..n].iter().map(|l| l[1]).sum();
    if total == 0.0 {
        return 0.0;
    }
    let mut hhi = 0.0f32;
    for l in &levels[..n] {
        let share = l[1] / total;
        hhi += share * share;
    }
    hhi
}

/// Book slopes: linear regression of size vs level index.
/// Returns (bid_slope, ask_slope). Positive bid_slope means more size at deeper levels.
fn book_slopes(state: &CommittedState) -> (f32, f32) {
    (
        level_slope(&state.bids, state.n_bids as usize),
        level_slope(&state.asks, state.n_asks as usize),
    )
}

fn level_slope(levels: &[[f32; 2]; BOOK_DEPTH], n: usize) -> f32 {
    if n < 2 {
        return 0.0;
    }
    // Simple least-squares slope: y = size, x = level index
    let nf = n as f32;
    let mut sum_x = 0.0f32;
    let mut sum_y = 0.0f32;
    let mut sum_xy = 0.0f32;
    let mut sum_xx = 0.0f32;
    for i in 0..n {
        let x = i as f32;
        let y = levels[i][1];
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_xx += x * x;
    }
    let denom = nf * sum_xx - sum_x * sum_x;
    if denom.abs() < 1e-12 {
        return 0.0;
    }
    (nf * sum_xy - sum_x * sum_y) / denom
}

// --- Rolling event window features ---

struct EventWindowFeatures {
    cancel_rate_bid: f32,
    cancel_rate_ask: f32,
    add_rate_bid: f32,
    add_rate_ask: f32,
    trade_aggression: f32,
    message_rate: f32,
    cancel_add_ratio: f32,
    bbo_change_count: f32,
    price_momentum_ticks: f32,
    flow_toxicity: f32,
}

fn compute_event_window_features(
    state: &CommittedState,
    events: &[MBOEvent],
    cfg: &EventWindowConfig,
) -> EventWindowFeatures {
    if events.is_empty() {
        return EventWindowFeatures {
            cancel_rate_bid: 0.0,
            cancel_rate_ask: 0.0,
            add_rate_bid: 0.0,
            add_rate_ask: 0.0,
            trade_aggression: 0.0,
            message_rate: 0.0,
            cancel_add_ratio: 0.0,
            bbo_change_count: 0.0,
            price_momentum_ticks: 0.0,
            flow_toxicity: 0.0,
        };
    }

    let n = events.len() as f32;
    let best_bid = state.bids[0][0];
    let best_ask = state.asks[0][0];

    let mut cancel_bid = 0u32;
    let mut cancel_ask = 0u32;
    let mut add_bid = 0u32;
    let mut add_ask = 0u32;
    let mut buy_volume = 0.0f32;
    let mut sell_volume = 0.0f32;
    let mut total_cancels = 0u32;
    let mut total_adds = 0u32;
    let mut trade_count = 0u32;

    // Track BBO changes: count events where price at inside changed
    let mut bbo_changes = 0u32;
    let mut prev_event_price = 0.0f32;
    let mut prev_event_side = -1i32;

    let _first_mid = state.mid;

    for evt in events {
        match evt.action {
            0 => {
                // Add
                total_adds += 1;
                // Check if at inside level (within half tick of BBO)
                if evt.side == 0 && (evt.price - best_bid).abs() < cfg.tick_size * 0.6 {
                    add_bid += 1;
                } else if evt.side == 1 && (evt.price - best_ask).abs() < cfg.tick_size * 0.6 {
                    add_ask += 1;
                }
            }
            1 => {
                // Cancel
                total_cancels += 1;
                if evt.side == 0 && (evt.price - best_bid).abs() < cfg.tick_size * 0.6 {
                    cancel_bid += 1;
                } else if evt.side == 1 && (evt.price - best_ask).abs() < cfg.tick_size * 0.6 {
                    cancel_ask += 1;
                }
            }
            3 => {
                // Trade
                trade_count += 1;
                let vol = evt.size as f32;
                if evt.side == 0 {
                    buy_volume += vol; // buyer aggressor
                } else {
                    sell_volume += vol; // seller aggressor
                }
            }
            _ => {} // Modify, Clear
        }

        // Detect BBO-level price changes in event stream
        if evt.action == 0 || evt.action == 1 {
            if evt.side == prev_event_side
                && (evt.price - prev_event_price).abs() > cfg.tick_size * 0.1
                && (evt.side == 0 && (evt.price - best_bid).abs() < cfg.tick_size * 0.6
                    || evt.side == 1 && (evt.price - best_ask).abs() < cfg.tick_size * 0.6)
            {
                bbo_changes += 1;
            }
            prev_event_price = evt.price;
            prev_event_side = evt.side;
        }
    }

    // Time span of the window in seconds
    let time_span_s = if events.len() >= 2 {
        let dt = events.last().unwrap().ts_event.saturating_sub(events[0].ts_event);
        (dt as f64 / 1e9) as f32
    } else {
        1.0 // avoid division by zero
    };

    // Trade aggression: (buy_vol - sell_vol) / (buy_vol + sell_vol)
    let total_trade_vol = buy_volume + sell_volume;
    let trade_aggression = if total_trade_vol > 0.0 {
        (buy_volume - sell_volume) / total_trade_vol
    } else {
        0.0
    };

    // Cancel-add ratio
    let cancel_add_ratio = if total_adds > 0 {
        total_cancels as f32 / total_adds as f32
    } else if total_cancels > 0 {
        2.0 // cap at 2.0 when no adds
    } else {
        0.0
    };

    // Price momentum: mid change over window in ticks
    // Use first and last event to approximate
    let price_momentum_ticks = if events.len() >= 2 {
        let first = &events[0];
        let last = events.last().unwrap();
        // Use trade prices if available, otherwise use event prices
        let first_price = first.price;
        let last_price = last.price;
        (last_price - first_price) / cfg.tick_size
    } else {
        0.0
    };

    // Order flow toxicity: |price_move| / trade_count
    let flow_toxicity = if trade_count > 0 {
        let price_move = if events.len() >= 2 {
            (events.last().unwrap().price - events[0].price).abs()
        } else {
            0.0
        };
        (price_move / cfg.tick_size) / trade_count as f32
    } else {
        0.0
    };

    EventWindowFeatures {
        cancel_rate_bid: cancel_bid as f32 / n,
        cancel_rate_ask: cancel_ask as f32 / n,
        add_rate_bid: add_bid as f32 / n,
        add_rate_ask: add_ask as f32 / n,
        trade_aggression,
        message_rate: n / time_span_s.max(0.001),
        cancel_add_ratio,
        bbo_change_count: bbo_changes as f32,
        price_momentum_ticks,
        flow_toxicity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_two_sided_state() -> CommittedState {
        let mut bids = [[0.0f32; 2]; BOOK_DEPTH];
        let mut asks = [[0.0f32; 2]; BOOK_DEPTH];

        // 3 bid levels: 4500.00 (10), 4499.75 (20), 4499.50 (5)
        bids[0] = [4500.00, 10.0];
        bids[1] = [4499.75, 20.0];
        bids[2] = [4499.50, 5.0];

        // 3 ask levels: 4500.25 (8), 4500.50 (15), 4500.75 (3)
        asks[0] = [4500.25, 8.0];
        asks[1] = [4500.50, 15.0];
        asks[2] = [4500.75, 3.0];

        CommittedState {
            ts: 1000,
            has_bid: true,
            has_ask: true,
            bids,
            asks,
            mid: 4500.125,
            spread: 0.25,
            n_bids: 3,
            n_asks: 3,
            bbo_changed: true,
        }
    }

    #[test]
    fn test_book_imbalance_d1() {
        let state = make_two_sided_state();
        let imb = book_imbalance(&state, 1);
        // bid=10, ask=8 → (10-8)/(10+8) = 2/18 ≈ 0.111
        assert!((imb - 0.111).abs() < 0.01, "got {}", imb);
    }

    #[test]
    fn test_book_imbalance_d3() {
        let state = make_two_sided_state();
        let imb = book_imbalance(&state, 3);
        // bid=35, ask=26 → (35-26)/(35+26) = 9/61 ≈ 0.148
        assert!((imb - 0.148).abs() < 0.01, "got {}", imb);
    }

    #[test]
    fn test_weighted_imbalance() {
        let state = make_two_sided_state();
        let wi = weighted_imbalance(&state);
        // bid: 10/1 + 20/2 + 5/3 = 10 + 10 + 1.667 = 21.667
        // ask: 8/1 + 15/2 + 3/3 = 8 + 7.5 + 1.0 = 16.5
        // (21.667 - 16.5) / (21.667 + 16.5) = 5.167/38.167 ≈ 0.135
        assert!((wi - 0.135).abs() < 0.02, "got {}", wi);
    }

    #[test]
    fn test_hhi_concentrated() {
        // All size at one level → HHI = 1.0
        let mut state = make_two_sided_state();
        state.bids[0] = [4500.0, 100.0];
        state.bids[1] = [4499.75, 0.0];
        state.bids[2] = [4499.50, 0.0];
        state.n_bids = 1;
        let hhi = depth_hhi_bid(&state);
        assert!((hhi - 1.0).abs() < 0.01, "got {}", hhi);
    }

    #[test]
    fn test_hhi_uniform() {
        // Equal size at 3 levels → HHI = 3 * (1/3)^2 = 1/3
        let mut state = make_two_sided_state();
        state.bids[0][1] = 10.0;
        state.bids[1][1] = 10.0;
        state.bids[2][1] = 10.0;
        let hhi = depth_hhi_bid(&state);
        assert!((hhi - 1.0 / 3.0).abs() < 0.01, "got {}", hhi);
    }

    #[test]
    fn test_level_slope() {
        let mut levels = [[0.0f32; 2]; BOOK_DEPTH];
        // Increasing size: 10, 20, 30
        levels[0] = [100.0, 10.0];
        levels[1] = [99.0, 20.0];
        levels[2] = [98.0, 30.0];
        let slope = level_slope(&levels, 3);
        // Positive slope = more size at deeper levels
        assert!(slope > 0.0, "got {}", slope);
        assert!((slope - 10.0).abs() < 0.01, "got {}", slope);
    }

    #[test]
    fn test_compute_lob_features_shape() {
        let state = make_two_sided_state();
        let cfg = EventWindowConfig::default();
        let features = compute_lob_features(&state, &[], &cfg);
        assert_eq!(features.len(), NUM_LOB_FEATURES);

        // First 10 should be bid sizes
        assert!((features[0] - 10.0).abs() < 0.01);
        assert!((features[1] - 20.0).abs() < 0.01);
        assert!((features[2] - 5.0).abs() < 0.01);

        // Next 10 should be ask sizes
        assert!((features[10] - 8.0).abs() < 0.01);
        assert!((features[11] - 15.0).abs() < 0.01);
        assert!((features[12] - 3.0).abs() < 0.01);

        // Spread in ticks = 0.25 / 0.25 = 1.0
        assert!((features[25] - 1.0).abs() < 0.01, "spread_ticks = {}", features[25]);
    }

    #[test]
    fn test_compute_model_inputs_includes_geometry() {
        let state = make_two_sided_state();
        let cfg = EventWindowConfig::default();
        let inputs = compute_model_inputs(&state, &[], &cfg, 10, 5);
        assert_eq!(inputs.len(), NUM_MODEL_INPUTS);
        assert!((inputs[42] - 10.0).abs() < 0.01); // target_ticks
        assert!((inputs[43] - 5.0).abs() < 0.01); // stop_ticks
    }

    #[test]
    fn test_empty_book_features() {
        let state = CommittedState {
            ts: 1000,
            has_bid: false,
            has_ask: false,
            bids: [[0.0f32; 2]; BOOK_DEPTH],
            asks: [[0.0f32; 2]; BOOK_DEPTH],
            mid: 0.0,
            spread: 0.0,
            n_bids: 0,
            n_asks: 0,
            bbo_changed: false,
        };
        let cfg = EventWindowConfig::default();
        let features = compute_lob_features(&state, &[], &cfg);
        // All features should be 0 or well-defined
        for (i, &f) in features.iter().enumerate() {
            assert!(f.is_finite(), "feature {} is not finite: {}", i, f);
        }
    }

    #[test]
    fn test_event_window_with_events() {
        let state = make_two_sided_state();
        let cfg = EventWindowConfig::default();

        let events = vec![
            MBOEvent { action: 0, price: 4500.00, size: 5, side: 0, ts_event: 100 },  // add bid
            MBOEvent { action: 0, price: 4500.25, size: 3, side: 1, ts_event: 200 },  // add ask
            MBOEvent { action: 1, price: 4500.00, size: 5, side: 0, ts_event: 300 },  // cancel bid
            MBOEvent { action: 3, price: 4500.25, size: 2, side: 0, ts_event: 400 },  // trade buy
            MBOEvent { action: 3, price: 4500.00, size: 1, side: 1, ts_event: 500 },  // trade sell
        ];

        let features = compute_lob_features(&state, &events, &cfg);

        // cancel_rate_bid = 1/5 = 0.2
        assert!((features[32] - 0.2).abs() < 0.01, "cancel_rate_bid = {}", features[32]);
        // add_rate_bid = 1/5 = 0.2
        assert!((features[34] - 0.2).abs() < 0.01, "add_rate_bid = {}", features[34]);
        // trade_aggression: buy=2, sell=1 → (2-1)/(2+1) = 0.333
        assert!((features[36] - 0.333).abs() < 0.05, "trade_aggression = {}", features[36]);
    }

    #[test]
    fn test_feature_names_count() {
        assert_eq!(LOB_FEATURE_NAMES.len(), NUM_LOB_FEATURES);
        assert_eq!(MODEL_INPUT_NAMES.len(), NUM_MODEL_INPUTS);
    }
}
