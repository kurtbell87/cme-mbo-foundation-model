//! Derived flow features from FlowState snapshots.
//!
//! Takes the raw 25-element FlowState and computes derived ratios, normalized
//! quantities, and cross-scale momentum features. These encode the microstructure
//! signals that XGBoost and the gate test consume.
//!
//! Feature groups:
//! - Raw (25): direct from FlowState::to_features()
//! - Per-scale derived (18): cancel/add asymmetry, OFI normalized, trade size, renewal rates
//! - Cross-scale momentum (3): fast-minus-slow for trade flow, OFI, cancel asymmetry

use book_builder::flow::{FlowState, BboChangeCause, NUM_SCALES};

/// Small epsilon to avoid division by zero.
const EPS: f32 = 1e-9;

// ── Feature counts ────────────────────────────────────────────

/// Raw features from FlowState (8 accumulators × 3 scales + 1 cause).
pub const NUM_RAW: usize = 25;

/// Per-scale derived features (6 ratios × 3 scales).
pub const NUM_DERIVED_PER_SCALE: usize = 18;

/// Cross-scale momentum features.
pub const NUM_CROSS_SCALE: usize = 3;

/// Total flow feature count.
pub const NUM_FLOW_FEATURES: usize = NUM_RAW + NUM_DERIVED_PER_SCALE + NUM_CROSS_SCALE; // 46

// ── Feature names ─────────────────────────────────────────────

pub const FLOW_FEATURE_NAMES: [&str; NUM_FLOW_FEATURES] = [
    // --- Raw (25) ---
    // trade_flow × 3
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
    // bbo_change_cause
    "bbo_change_cause",

    // --- Per-scale derived (18) ---
    // cancel_asymmetry: (cancel_bid - cancel_ask) / (cancel_bid + cancel_ask + ε)
    "cancel_asym_fast", "cancel_asym_med", "cancel_asym_slow",
    // add_asymmetry: (add_bid - add_ask) / (add_bid + add_ask + ε)
    "add_asym_fast", "add_asym_med", "add_asym_slow",
    // ofi_normalized: ofi / (event_intensity + ε)
    "ofi_norm_fast", "ofi_norm_med", "ofi_norm_slow",
    // trade_flow_per_trade: trade_flow / (trade_intensity + ε)
    "trade_size_fast", "trade_size_med", "trade_size_slow",
    // bid renewal: cancel_bid / (add_bid + ε)
    "bid_renewal_fast", "bid_renewal_med", "bid_renewal_slow",
    // ask renewal: cancel_ask / (add_ask + ε)
    "ask_renewal_fast", "ask_renewal_med", "ask_renewal_slow",

    // --- Cross-scale momentum (3) ---
    // fast - slow for key signals
    "trade_flow_accel",
    "ofi_accel",
    "cancel_asym_accel",
];

// ── Computation ───────────────────────────────────────────────

/// Compute the full 46-element flow feature vector from a FlowState.
pub fn compute_flow_features(state: &FlowState) -> [f32; NUM_FLOW_FEATURES] {
    let mut f = [0.0f32; NUM_FLOW_FEATURES];

    // --- Raw features (25) ---
    let raw = state.to_features();
    f[..NUM_RAW].copy_from_slice(&raw);
    let mut i = NUM_RAW;

    // --- Per-scale derived (18) ---

    // Cancel asymmetry: (cancel_bid - cancel_ask) / (cancel_bid + cancel_ask + ε)
    let cancel_asym = per_scale_ratio(
        &state.cancel_bid, &state.cancel_ask,
    );
    for s in 0..NUM_SCALES { f[i] = cancel_asym[s]; i += 1; }

    // Add asymmetry: (add_bid - add_ask) / (add_bid + add_ask + ε)
    let add_asym = per_scale_ratio(
        &state.add_bid, &state.add_ask,
    );
    for s in 0..NUM_SCALES { f[i] = add_asym[s]; i += 1; }

    // OFI normalized: ofi / (event_intensity + ε)
    for s in 0..NUM_SCALES {
        f[i] = state.ofi[s] / (state.event_intensity[s] + EPS);
        i += 1;
    }

    // Trade flow per trade: trade_flow / (trade_intensity + ε)
    for s in 0..NUM_SCALES {
        f[i] = state.trade_flow[s] / (state.trade_intensity[s] + EPS);
        i += 1;
    }

    // Bid renewal: cancel_bid / (add_bid + ε)
    for s in 0..NUM_SCALES {
        f[i] = state.cancel_bid[s] / (state.add_bid[s] + EPS);
        i += 1;
    }

    // Ask renewal: cancel_ask / (add_ask + ε)
    for s in 0..NUM_SCALES {
        f[i] = state.cancel_ask[s] / (state.add_ask[s] + EPS);
        i += 1;
    }

    // --- Cross-scale momentum (3) ---

    // Trade flow acceleration: fast - slow
    f[i] = state.trade_flow[0] - state.trade_flow[2];
    i += 1;

    // OFI acceleration: fast - slow
    f[i] = state.ofi[0] - state.ofi[2];
    i += 1;

    // Cancel asymmetry acceleration: fast - slow
    f[i] = cancel_asym[0] - cancel_asym[2];

    debug_assert_eq!(i, NUM_FLOW_FEATURES - 1);
    f
}

/// Compute asymmetry ratio: (a - b) / (a + b + ε) per scale.
/// Returns values in [-1, 1].
#[inline]
fn per_scale_ratio(a: &[f32; NUM_SCALES], b: &[f32; NUM_SCALES]) -> [f32; NUM_SCALES] {
    let mut out = [0.0f32; NUM_SCALES];
    for s in 0..NUM_SCALES {
        out[s] = (a[s] - b[s]) / (a[s] + b[s] + EPS);
    }
    out
}

/// Subset of features for the univariate gate test (step 4).
///
/// Returns OFI at 3 scales + cancel asymmetry at 3 scales + bbo_change_cause.
/// These are the primary signals to test before investing in full XGBoost.
#[derive(Debug, Clone, Copy)]
pub struct GateFeatures {
    pub ofi: [f32; NUM_SCALES],
    pub ofi_norm: [f32; NUM_SCALES],
    pub cancel_asym: [f32; NUM_SCALES],
    pub bbo_change_cause: BboChangeCause,
}

/// Extract just the gate-test features from a FlowState.
pub fn compute_gate_features(state: &FlowState) -> GateFeatures {
    let mut ofi_norm = [0.0f32; NUM_SCALES];
    let cancel_asym = per_scale_ratio(&state.cancel_bid, &state.cancel_ask);

    for s in 0..NUM_SCALES {
        ofi_norm[s] = state.ofi[s] / (state.event_intensity[s] + EPS);
    }

    GateFeatures {
        ofi: state.ofi,
        ofi_norm,
        cancel_asym,
        bbo_change_cause: state.bbo_change_cause,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_flow_state(
        trade_flow: [f32; 3],
        cancel_bid: [f32; 3],
        cancel_ask: [f32; 3],
        add_bid: [f32; 3],
        add_ask: [f32; 3],
        event_intensity: [f32; 3],
        trade_intensity: [f32; 3],
        ofi: [f32; 3],
        cause: BboChangeCause,
    ) -> FlowState {
        FlowState {
            ts: 1_000_000_000,
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

    #[test]
    fn test_feature_count_matches_names() {
        assert_eq!(FLOW_FEATURE_NAMES.len(), NUM_FLOW_FEATURES);
        assert_eq!(NUM_FLOW_FEATURES, 46);
    }

    #[test]
    fn test_raw_features_passthrough() {
        let state = make_flow_state(
            [1.0, 2.0, 3.0],
            [4.0, 5.0, 6.0],
            [7.0, 8.0, 9.0],
            [10.0, 11.0, 12.0],
            [13.0, 14.0, 15.0],
            [16.0, 17.0, 18.0],
            [19.0, 20.0, 21.0],
            [22.0, 23.0, 24.0],
            BboChangeCause::Cancel,
        );
        let f = compute_flow_features(&state);
        // First 25 should match FlowState::to_features()
        let raw = state.to_features();
        for i in 0..NUM_RAW {
            assert!(
                (f[i] - raw[i]).abs() < 1e-6,
                "Mismatch at raw feature {}: {} vs {}",
                i, f[i], raw[i]
            );
        }
    }

    #[test]
    fn test_cancel_asymmetry_balanced() {
        // Equal cancel volumes → asymmetry = 0
        let state = make_flow_state(
            [0.0; 3],
            [10.0, 10.0, 10.0], // cancel_bid
            [10.0, 10.0, 10.0], // cancel_ask
            [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3],
            BboChangeCause::None,
        );
        let f = compute_flow_features(&state);
        // cancel_asym starts at index 25
        for s in 0..NUM_SCALES {
            assert!(
                f[25 + s].abs() < 0.01,
                "cancel_asym[{}] should be ~0, got {}",
                s, f[25 + s]
            );
        }
    }

    #[test]
    fn test_cancel_asymmetry_bid_heavy() {
        // More bid cancels → positive asymmetry (bearish signal)
        let state = make_flow_state(
            [0.0; 3],
            [30.0, 30.0, 30.0], // cancel_bid
            [10.0, 10.0, 10.0], // cancel_ask
            [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3],
            BboChangeCause::None,
        );
        let f = compute_flow_features(&state);
        // (30-10)/(30+10) = 20/40 = 0.5
        for s in 0..NUM_SCALES {
            assert!(
                (f[25 + s] - 0.5).abs() < 0.01,
                "cancel_asym[{}] should be ~0.5, got {}",
                s, f[25 + s]
            );
        }
    }

    #[test]
    fn test_ofi_normalized() {
        let state = make_flow_state(
            [0.0; 3],
            [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3],
            [100.0, 200.0, 300.0], // event_intensity
            [0.0; 3],
            [10.0, 20.0, 30.0],    // ofi
            BboChangeCause::None,
        );
        let f = compute_flow_features(&state);
        // ofi_norm starts at index 31
        assert!((f[31] - 0.1).abs() < 0.01, "ofi_norm_fast: {}", f[31]);   // 10/100
        assert!((f[32] - 0.1).abs() < 0.01, "ofi_norm_med: {}", f[32]);    // 20/200
        assert!((f[33] - 0.1).abs() < 0.01, "ofi_norm_slow: {}", f[33]);   // 30/300
    }

    #[test]
    fn test_trade_size_per_trade() {
        let state = make_flow_state(
            [50.0, 100.0, 150.0],   // trade_flow (signed volume)
            [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3],
            [0.0; 3],
            [10.0, 20.0, 30.0],     // trade_intensity (count)
            [0.0; 3],
            BboChangeCause::None,
        );
        let f = compute_flow_features(&state);
        // trade_size starts at index 34
        assert!((f[34] - 5.0).abs() < 0.01, "trade_size_fast: {}", f[34]);  // 50/10
        assert!((f[35] - 5.0).abs() < 0.01, "trade_size_med: {}", f[35]);   // 100/20
        assert!((f[36] - 5.0).abs() < 0.01, "trade_size_slow: {}", f[36]);  // 150/30
    }

    #[test]
    fn test_renewal_rates() {
        let state = make_flow_state(
            [0.0; 3],
            [15.0, 15.0, 15.0],  // cancel_bid
            [20.0, 20.0, 20.0],  // cancel_ask
            [30.0, 30.0, 30.0],  // add_bid
            [10.0, 10.0, 10.0],  // add_ask
            [0.0; 3], [0.0; 3], [0.0; 3],
            BboChangeCause::None,
        );
        let f = compute_flow_features(&state);
        // bid_renewal starts at index 37: cancel_bid / add_bid = 15/30 = 0.5
        assert!((f[37] - 0.5).abs() < 0.01, "bid_renewal_fast: {}", f[37]);
        // ask_renewal starts at index 40: cancel_ask / add_ask = 20/10 = 2.0
        assert!((f[40] - 2.0).abs() < 0.01, "ask_renewal_fast: {}", f[40]);
    }

    #[test]
    fn test_cross_scale_momentum() {
        let state = make_flow_state(
            [10.0, 5.0, 2.0],    // trade_flow: fast > slow
            [20.0, 15.0, 10.0],  // cancel_bid
            [10.0, 15.0, 20.0],  // cancel_ask: asymmetry flips across scales
            [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3],
            [8.0, 4.0, 1.0],     // ofi: fast > slow
            BboChangeCause::None,
        );
        let f = compute_flow_features(&state);
        // trade_flow_accel at index 43: fast - slow = 10 - 2 = 8
        assert!((f[43] - 8.0).abs() < 0.01, "trade_flow_accel: {}", f[43]);
        // ofi_accel at index 44: fast - slow = 8 - 1 = 7
        assert!((f[44] - 7.0).abs() < 0.01, "ofi_accel: {}", f[44]);
        // cancel_asym_accel at index 45:
        // fast: (20-10)/(20+10) = 0.333, slow: (10-20)/(10+20) = -0.333
        // accel = 0.333 - (-0.333) = 0.667
        assert!((f[45] - 0.667).abs() < 0.02, "cancel_asym_accel: {}", f[45]);
    }

    #[test]
    fn test_gate_features() {
        let state = make_flow_state(
            [0.0; 3],
            [30.0, 20.0, 10.0],  // cancel_bid
            [10.0, 20.0, 30.0],  // cancel_ask
            [0.0; 3], [0.0; 3],
            [100.0, 200.0, 300.0], // event_intensity
            [0.0; 3],
            [5.0, 10.0, 15.0],    // ofi
            BboChangeCause::AggressiveTrade,
        );
        let gate = compute_gate_features(&state);
        assert_eq!(gate.ofi, [5.0, 10.0, 15.0]);
        assert!((gate.ofi_norm[0] - 0.05).abs() < 0.01); // 5/100
        assert!(gate.cancel_asym[0] > 0.0); // more bid cancels
        assert!(gate.cancel_asym[2] < 0.0); // more ask cancels at slow scale
        assert_eq!(gate.bbo_change_cause, BboChangeCause::AggressiveTrade);
    }

    #[test]
    fn test_all_features_finite() {
        // Zero state should produce all finite values (no NaN/Inf)
        let state = make_flow_state(
            [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3], [0.0; 3],
            [0.0; 3], [0.0; 3], [0.0; 3],
            BboChangeCause::None,
        );
        let f = compute_flow_features(&state);
        for (i, &v) in f.iter().enumerate() {
            assert!(v.is_finite(), "feature {} ({}) is not finite: {}", i, FLOW_FEATURE_NAMES[i], v);
        }
    }

    #[test]
    fn test_feature_names_unique() {
        let mut seen = std::collections::HashSet::new();
        for name in &FLOW_FEATURE_NAMES {
            assert!(seen.insert(name), "Duplicate feature name: {}", name);
        }
    }
}
