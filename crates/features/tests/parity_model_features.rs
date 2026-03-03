//! Parity Validation Harness — RED Phase Test Suite
//!
//! Tests the 20 model feature computation contracts that the parity-test tool
//! must validate between C++ and Rust pipelines.
//!
//! Spec: .kit/docs/parity-validation-harness.md
//!
//! Test Plan Coverage:
//!   T1: CLI arg parsing            → parity_tool_integration.rs
//!   T2: Reference Parquet loading  → parity_tool_integration.rs
//!   T3: Bar count matching         → parity_tool_integration.rs
//!   T4: Feature comparison         → THIS FILE (Sections 2-9)
//!   T5: Zero-volume bar handling   → THIS FILE (Section 3)
//!   T6: Early bar fixup parity     → THIS FILE (Section 6)
//!   T7: Summary report             → parity_tool_integration.rs
//!
//! Known Risk Areas (FEATURE_PARITY_SPEC 12.2):
//!   - volatility_20/50: Population vs sample std      → Section 5
//!   - vwap_distance: Zero-volume edge case            → Section 3
//!   - cancel_add_ratio, message_rate: MBO recount     → Section 8
//!   - trade_count: Event count method                 → Section 2
//!   - high_low_range_50: Guard condition n > 50       → Section 7
//!   - message_rate: Excludes trades                   → Section 8
//!   - Bars 2-50: fixup_rolling_features()             → Section 6

use common::bar::Bar;
use common::book::BOOK_DEPTH;
use features::{BarFeatureComputer, BarFeatureRow};

const TICK_SIZE: f32 = 0.25;
const EPS: f32 = 1e-8;

/// The 20 model features from the spec, in XGBoost feature order.
const MODEL_FEATURES: [&str; 20] = [
    "weighted_imbalance",
    "spread",
    "net_volume",
    "volume_imbalance",
    "trade_count",
    "avg_trade_size",
    "vwap_distance",
    "return_1",
    "return_5",
    "return_20",
    "volatility_20",
    "volatility_50",
    "high_low_range_50",
    "close_position",
    "cancel_add_ratio",
    "message_rate",
    "modify_fraction",
    "time_sin",
    "time_cos",
    "minutes_since_open",
];

// =====================================================================
// Test Helpers
// =====================================================================

/// Extract the 20 model feature values from a BarFeatureRow, in spec order.
fn extract_model_features(row: &BarFeatureRow) -> [f64; 20] {
    [
        row.weighted_imbalance as f64,
        row.spread as f64,
        row.net_volume as f64,
        row.volume_imbalance as f64,
        row.trade_count as f64,
        row.avg_trade_size as f64,
        row.vwap_distance as f64,
        row.return_1 as f64,
        row.return_5 as f64,
        row.return_20 as f64,
        row.volatility_20 as f64,
        row.volatility_50 as f64,
        row.high_low_range_50 as f64,
        row.close_position as f64,
        row.cancel_add_ratio as f64,
        row.message_rate as f64,
        row.modify_fraction as f64,
        row.time_sin as f64,
        row.time_cos as f64,
        row.minutes_since_open as f64,
    ]
}

/// Create a bar with full control over model-relevant fields.
fn make_bar_full(
    close_ts: u64,
    close_mid: f32,
    open_mid: f32,
    high_mid: f32,
    low_mid: f32,
    volume: u32,
    buy_volume: f32,
    sell_volume: f32,
    vwap: f32,
    bar_duration_s: f32,
    time_of_day: f32,
    trade_event_count: u32,
    add_count: u32,
    cancel_count: u32,
    modify_count: u32,
    spread: f32,
    bids: [[f32; 2]; BOOK_DEPTH],
    asks: [[f32; 2]; BOOK_DEPTH],
) -> Bar {
    Bar {
        close_ts,
        close_mid,
        open_mid,
        high_mid,
        low_mid,
        volume,
        buy_volume,
        sell_volume,
        vwap,
        bar_duration_s,
        time_of_day,
        trade_event_count,
        add_count,
        cancel_count,
        modify_count,
        spread,
        bids,
        asks,
        ..Default::default()
    }
}

/// Make a uniform bar with equal bid/ask sizes at all levels.
fn make_uniform_bar(idx: u64, close_mid: f32, volume: u32) -> Bar {
    let mut bids = [[0.0f32; 2]; BOOK_DEPTH];
    let mut asks = [[0.0f32; 2]; BOOK_DEPTH];
    for i in 0..BOOK_DEPTH {
        bids[i] = [close_mid - TICK_SIZE * (i + 1) as f32, 10.0];
        asks[i] = [close_mid + TICK_SIZE * (i + 1) as f32, 10.0];
    }
    Bar {
        close_ts: idx * 5_000_000_000,
        close_mid,
        open_mid: close_mid,
        high_mid: close_mid + TICK_SIZE,
        low_mid: close_mid - TICK_SIZE,
        volume,
        buy_volume: volume as f32 * 0.6,
        sell_volume: volume as f32 * 0.4,
        vwap: close_mid,
        bar_duration_s: 5.0,
        time_of_day: 10.0,
        trade_event_count: volume.min(5),
        add_count: 100,
        cancel_count: 30,
        modify_count: 10,
        spread: TICK_SIZE,
        bids,
        asks,
        ..Default::default()
    }
}

/// Create N bars with linearly increasing close_mid prices.
fn make_price_ramp(n: usize, start: f32, step: f32) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let mid = start + i as f32 * step;
            let mut bar = make_uniform_bar(i as u64, mid, 100);
            bar.high_mid = mid + TICK_SIZE;
            bar.low_mid = mid - TICK_SIZE;
            bar
        })
        .collect()
}


// =====================================================================
// Section 1: Model Feature Names (T2 prerequisite)
//
// The parity tool must extract these 20 columns from the reference
// Parquet. Verify they exist in BarFeatureRow::feature_names().
// =====================================================================

#[test]
fn all_20_model_features_present_in_feature_names() {
    let all_names = BarFeatureRow::feature_names();
    for &model_feat in &MODEL_FEATURES {
        assert!(
            all_names.contains(&model_feat),
            "Model feature '{}' not found in BarFeatureRow::feature_names(). \
             The parity tool needs to extract this column from reference Parquet.",
            model_feat,
        );
    }
}

#[test]
fn model_feature_count_is_20() {
    // Exactly 20 model features are used by XGBoost.
    assert_eq!(MODEL_FEATURES.len(), 20);
}

#[test]
fn model_features_extractable_from_row() {
    // The extract_model_features helper must return exactly 20 values.
    let bar = make_uniform_bar(0, 4500.0, 100);
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    let feats = extract_model_features(&row);
    assert_eq!(feats.len(), 20);
}

// =====================================================================
// Section 2: Instant Feature Contracts (T4 — zero lookback)
// =====================================================================

// --- weighted_imbalance ---

#[test]
fn weighted_imbalance_equal_books_yields_zero() {
    let bar = make_uniform_bar(0, 4500.0, 100);
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        row.weighted_imbalance.abs() < 1e-6,
        "Equal bid/ask sizes → weighted_imbalance ≈ 0, got {}",
        row.weighted_imbalance,
    );
}

#[test]
fn weighted_imbalance_bid_heavy_positive() {
    let mut bids = [[0.0f32; 2]; BOOK_DEPTH];
    let mut asks = [[0.0f32; 2]; BOOK_DEPTH];
    for i in 0..BOOK_DEPTH {
        bids[i] = [4500.0 - TICK_SIZE * (i + 1) as f32, 20.0];
        asks[i] = [4500.0 + TICK_SIZE * (i + 1) as f32, 10.0];
    }
    let bar = make_bar_full(
        1_000_000_000, 4500.0, 4500.0, 4500.25, 4499.75,
        100, 60.0, 40.0, 4500.0, 5.0, 10.0, 5, 100, 30, 10,
        TICK_SIZE, bids, asks,
    );
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);

    // w[i] = 1/(i+1), bid_size=20, ask_size=10 at all levels
    // w_bid = 20 * Σ(1/(i+1), i=0..9) = 20 * H(10)
    // w_ask = 10 * H(10)
    // result = (20*H10 - 10*H10) / (20*H10 + 10*H10 + eps) = 10*H10 / (30*H10 + eps) ≈ 1/3
    let expected: f32 = 1.0 / 3.0;
    assert!(
        (row.weighted_imbalance - expected).abs() < 1e-4,
        "Bid-heavy: expected ≈ {:.6}, got {:.6}",
        expected, row.weighted_imbalance,
    );
}

#[test]
fn weighted_imbalance_ask_heavy_negative() {
    let mut bids = [[0.0f32; 2]; BOOK_DEPTH];
    let mut asks = [[0.0f32; 2]; BOOK_DEPTH];
    for i in 0..BOOK_DEPTH {
        bids[i] = [4500.0 - TICK_SIZE * (i + 1) as f32, 5.0];
        asks[i] = [4500.0 + TICK_SIZE * (i + 1) as f32, 15.0];
    }
    let bar = make_bar_full(
        1_000_000_000, 4500.0, 4500.0, 4500.25, 4499.75,
        100, 60.0, 40.0, 4500.0, 5.0, 10.0, 5, 100, 30, 10,
        TICK_SIZE, bids, asks,
    );
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        row.weighted_imbalance < -0.1,
        "Ask-heavy book should yield negative weighted_imbalance, got {}",
        row.weighted_imbalance,
    );
}

// --- spread ---

#[test]
fn spread_normalized_by_tick_size() {
    let bar = make_uniform_bar(0, 4500.0, 100);
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // bar.spread = 0.25, normalized = 0.25 / 0.25 = 1.0
    assert!(
        (row.spread - 1.0).abs() < 1e-6,
        "spread = bar.spread / tick_size = 1.0, got {}",
        row.spread,
    );
}

#[test]
fn spread_two_ticks_wide() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.spread = 0.50;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        (row.spread - 2.0).abs() < 1e-6,
        "spread = 0.50 / 0.25 = 2.0, got {}",
        row.spread,
    );
}

// --- net_volume ---

#[test]
fn net_volume_is_buy_minus_sell() {
    // Use exact f32 values to avoid 0.6f32 rounding.
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.buy_volume = 60.0;
    bar.sell_volume = 40.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        (row.net_volume - 20.0).abs() < 1e-6,
        "net_volume = buy(60) - sell(40) = 20, got {}",
        row.net_volume,
    );
}

#[test]
fn net_volume_negative_when_sell_dominant() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.buy_volume = 30.0;
    bar.sell_volume = 70.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        (row.net_volume - (-40.0)).abs() < 1e-6,
        "net_volume = 30 - 70 = -40, got {}",
        row.net_volume,
    );
}

// --- volume_imbalance ---

#[test]
fn volume_imbalance_formula() {
    let bar = make_uniform_bar(0, 4500.0, 100);
    // net_volume = 20, volume = 100
    // volume_imbalance = 20 / (100 + eps) ≈ 0.2
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    let expected = 20.0 / (100.0 + EPS);
    assert!(
        (row.volume_imbalance - expected).abs() < 1e-6,
        "volume_imbalance = net_vol / (vol + eps), expected {}, got {}",
        expected, row.volume_imbalance,
    );
}

#[test]
fn volume_imbalance_zero_volume_yields_zero() {
    let mut bar = make_uniform_bar(0, 4500.0, 0);
    bar.buy_volume = 0.0;
    bar.sell_volume = 0.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        row.volume_imbalance.abs() < 1e-6,
        "Zero volume → volume_imbalance = 0, got {}",
        row.volume_imbalance,
    );
}

// --- trade_count ---

#[test]
fn trade_count_uses_trade_event_count_field() {
    // KNOWN RISK: Must use trade_event_count from MBO recount, not snapshot trade buffer
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.trade_event_count = 42;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert_eq!(
        row.trade_count, 42,
        "trade_count must use bar.trade_event_count (MBO recount), got {}",
        row.trade_count,
    );
}

// --- avg_trade_size ---

#[test]
fn avg_trade_size_volume_over_trade_count() {
    let mut bar = make_uniform_bar(0, 4500.0, 200);
    bar.trade_event_count = 10;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // avg = 200 / 10 = 20.0
    assert!(
        (row.avg_trade_size - 20.0).abs() < 1e-6,
        "avg_trade_size = volume / trade_event_count = 20, got {}",
        row.avg_trade_size,
    );
}

#[test]
fn avg_trade_size_zero_trades_yields_zero() {
    let mut bar = make_uniform_bar(0, 4500.0, 0);
    bar.trade_event_count = 0;
    bar.buy_volume = 0.0;
    bar.sell_volume = 0.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        row.avg_trade_size.abs() < 1e-6,
        "Zero trades → avg_trade_size = 0, got {}",
        row.avg_trade_size,
    );
}

// --- vwap_distance ---

#[test]
fn vwap_distance_formula() {
    let mut bar = make_uniform_bar(0, 4500.50, 100);
    bar.vwap = 4500.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // (4500.50 - 4500.0) / 0.25 = 2.0
    assert!(
        (row.vwap_distance - 2.0).abs() < 1e-4,
        "vwap_distance = (close_mid - vwap) / tick = 2.0, got {}",
        row.vwap_distance,
    );
}

// =====================================================================
// Section 3: Zero-Volume Edge Cases (T5)
//
// KNOWN RISK: When volume == 0, VWAP must be 0.0 (not NaN),
// producing vwap_distance = close_mid / 0.25.
// =====================================================================

#[test]
fn vwap_distance_zero_volume_not_nan() {
    let mut bar = make_uniform_bar(0, 4500.0, 0);
    bar.buy_volume = 0.0;
    bar.sell_volume = 0.0;
    bar.vwap = 0.0; // Spec: VWAP must be 0.0 when volume == 0
    bar.trade_event_count = 0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        !row.vwap_distance.is_nan(),
        "vwap_distance must NOT be NaN when volume == 0 (vwap == 0.0)",
    );
}

#[test]
fn vwap_distance_zero_volume_equals_close_mid_over_tick() {
    let close_mid = 4500.0f32;
    let mut bar = make_uniform_bar(0, close_mid, 0);
    bar.buy_volume = 0.0;
    bar.sell_volume = 0.0;
    bar.vwap = 0.0;
    bar.trade_event_count = 0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    let expected = close_mid / TICK_SIZE; // 4500.0 / 0.25 = 18000.0
    assert!(
        (row.vwap_distance - expected).abs() < 1e-2,
        "Zero volume: vwap_distance = close_mid / tick = {}, got {}",
        expected, row.vwap_distance,
    );
}

// =====================================================================
// Section 4: Lookback Feature Contracts (T4 — require multiple bars)
// =====================================================================

// --- return_1 ---

#[test]
fn return_1_one_bar_lookback() {
    let bars = make_price_ramp(3, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // return_1 at bar 1: (4500.25 - 4500.0) / 0.25 = 1.0
    assert!(
        (rows[1].return_1 - 1.0).abs() < 1e-4,
        "return_1 = (mid[1] - mid[0]) / tick = 1.0, got {}",
        rows[1].return_1,
    );
    // return_1 at bar 2: (4500.50 - 4500.25) / 0.25 = 1.0
    assert!(
        (rows[2].return_1 - 1.0).abs() < 1e-4,
        "return_1 = (mid[2] - mid[1]) / tick = 1.0, got {}",
        rows[2].return_1,
    );
}

#[test]
fn return_1_first_bar_is_nan() {
    let bar = make_uniform_bar(0, 4500.0, 100);
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        row.return_1.is_nan(),
        "return_1 at bar 0 must be NaN (no prior bar), got {}",
        row.return_1,
    );
}

// --- return_5 ---

#[test]
fn return_5_five_bar_lookback() {
    let bars = make_price_ramp(7, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // return_5 at bar 5: (close_mid[5] - close_mid[0]) / tick
    // close_mid[5] = 4500 + 5*0.25 = 4501.25
    // close_mid[0] = 4500.0
    // (4501.25 - 4500.0) / 0.25 = 5.0
    assert!(
        (rows[5].return_5 - 5.0).abs() < 1e-4,
        "return_5 = (mid[5] - mid[0]) / tick = 5.0, got {}",
        rows[5].return_5,
    );
}

#[test]
fn return_5_nan_before_6_bars() {
    let bars = make_price_ramp(5, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // At bar 4 we only have 5 close_mids (indices 0-4), need 6 for return_5
    assert!(
        rows[4].return_5.is_nan(),
        "return_5 at bar 4 must be NaN (need 6 bars), got {}",
        rows[4].return_5,
    );
}

// --- return_20 ---

#[test]
fn return_20_twenty_bar_lookback() {
    let bars = make_price_ramp(22, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // return_20 at bar 20: (mid[20] - mid[0]) / tick = 20.0
    assert!(
        (rows[20].return_20 - 20.0).abs() < 1e-4,
        "return_20 = (mid[20] - mid[0]) / tick = 20.0, got {}",
        rows[20].return_20,
    );
}

// =====================================================================
// Section 5: Volatility — Population Std (Known Risk Area)
//
// KNOWN RISK: Must use population std: var = sum_sq/n - mean^2
// NOT sample std (n-1 denominator).
// =====================================================================

#[test]
fn volatility_20_population_std_constant_returns() {
    // All returns are the same → population std = 0
    let bars = make_price_ramp(22, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // All 1-bar returns are 1.0 (each bar steps up by TICK_SIZE)
    // pop_std of [1.0; 20] = 0.0
    assert!(
        rows[20].volatility_20.abs() < 1e-6,
        "Constant returns → volatility_20 (pop std) = 0, got {}",
        rows[20].volatility_20,
    );
}

#[test]
fn volatility_20_population_std_known_value() {
    // Construct bars where returns alternate between +2 and -2 ticks.
    // Pop std of [+2, -2, +2, -2, ...] (20 values):
    //   mean = 0, mean(r^2) = 4, var = 4, std = 2.0
    let n = 22;
    let mut bars = Vec::with_capacity(n);
    let mut price = 4500.0f32;
    for i in 0..n {
        let mut bar = make_uniform_bar(i as u64, price, 100);
        bar.high_mid = price + TICK_SIZE;
        bar.low_mid = price - TICK_SIZE;
        bars.push(bar);
        if i % 2 == 0 {
            price += TICK_SIZE * 2.0; // +2 ticks
        } else {
            price -= TICK_SIZE * 2.0; // -2 ticks
        }
    }
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // Returns: [+2, -2, +2, -2, ...] in ticks
    // pop_std = sqrt(mean(r^2) - mean(r)^2) = sqrt(4 - 0) = 2.0
    assert!(
        (rows[21].volatility_20 - 2.0).abs() < 1e-4,
        "Alternating ±2 returns → pop std = 2.0, got {}",
        rows[21].volatility_20,
    );
}

#[test]
fn volatility_20_is_not_sample_std() {
    // Distinguish population std (n divisor) from sample std (n-1 divisor).
    // Construct bars where one return is a spike in the last-20 window.
    let n2 = 22;
    let mut bars2 = Vec::with_capacity(n2);
    let mut price2 = 4500.0f32;
    for i in 0..n2 {
        let mut bar = make_uniform_bar(i as u64, price2, 100);
        bar.high_mid = price2 + TICK_SIZE;
        bar.low_mid = price2 - TICK_SIZE;
        bars2.push(bar);
        // One spike return in the middle of the last-20 window
        if i == 11 {
            price2 += TICK_SIZE * 20.0; // +20 ticks
        }
        // (all other bars same price, so returns are 0)
    }
    let mut computer2 = BarFeatureComputer::new();
    let rows2: Vec<_> = bars2.iter().map(|b| computer2.update(b)).collect();

    // Returns at bars 1-21 (21 returns, 0-indexed):
    // r[0]=0 (bar0→1), ..., r[10]=0 (bar10→11), r[11]=20 (bar11→12),
    // r[12]=0 (bar12→13), ..., r[20]=0 (bar20→21)
    // Last 20 returns at bar 21: r[1]..r[20] = [0,0,...,20,...,0,0]
    // with the 20 at position r[11] (index 10 in the last-20 window)
    //
    // Pop std: mean = 20/20 = 1, mean_sq = 400/20 = 20, var = 20 - 1 = 19, std = sqrt(19)
    // Sample std (n-1): var = 20*20/(20*19) = 400/19, std = 20/sqrt(19)... no wait.
    // Actually: sample_var = sum((x - mean)^2) / (n-1)
    // sum((x - mean)^2) = 19*(0-1)^2 + 1*(20-1)^2 = 19 + 361 = 380
    // sample_var = 380 / 19 = 20, sample_std = sqrt(20)
    // pop_var = 380 / 20 = 19, pop_std = sqrt(19)
    let pop_std = (19.0f32).sqrt();
    let sample_std = (20.0f32).sqrt();
    let actual = rows2[21].volatility_20;

    assert!(
        (actual - pop_std).abs() < 1e-4,
        "volatility_20 must use POPULATION std = {:.4}, got {:.4} (sample std would be {:.4})",
        pop_std, actual, sample_std,
    );
    assert!(
        (actual - sample_std).abs() > 0.01,
        "volatility_20 must NOT equal sample std {:.4}, but got {:.4}",
        sample_std, actual,
    );
}

#[test]
fn volatility_50_requires_50_returns() {
    // With only 30 bars (29 returns), volatility_50 should be NaN during incremental.
    let bars = make_price_ramp(30, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    assert!(
        rows[29].volatility_50.is_nan(),
        "volatility_50 at bar 29 (only 29 returns) should be NaN, got {}",
        rows[29].volatility_50,
    );
}

#[test]
fn volatility_50_computed_at_51_bars() {
    let bars = make_price_ramp(52, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // Bar 50 has 50 returns → volatility_50 computed (returns.len() >= 50)
    assert!(
        !rows[50].volatility_50.is_nan(),
        "volatility_50 at bar 50 (50 returns) should be computed, got NaN",
    );
}

// =====================================================================
// Section 6: Early Bar Fixup — Partial Lookback (T6)
//
// When lookback is incomplete, fixup_rolling_features() must use
// available data. E.g., volatility_20 at bar 10 uses 10 returns.
// =====================================================================

#[test]
fn volatility_20_at_bar_10_uses_partial_lookback() {
    let bars = make_price_ramp(60, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);

    // After compute_all, fixup should fill volatility_20 at bar 10.
    // It uses min(10, 20) = 10 returns.
    assert!(
        !rows[10].volatility_20.is_nan(),
        "After compute_all, volatility_20 at bar 10 must NOT be NaN (uses 10 returns via fixup)",
    );
    // With constant returns of 1.0 (price ramp step = TICK_SIZE), pop std = 0
    assert!(
        rows[10].volatility_20.abs() < 1e-6,
        "Constant returns → fixup volatility_20 at bar 10 = 0, got {}",
        rows[10].volatility_20,
    );
}

#[test]
fn volatility_50_at_bar_20_uses_partial_lookback() {
    let bars = make_price_ramp(60, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);

    // fixup uses min(20, 50) = 20 returns for volatility_50 at bar 20
    assert!(
        !rows[20].volatility_50.is_nan(),
        "After compute_all, volatility_50 at bar 20 must NOT be NaN (uses 20 returns via fixup)",
    );
}

#[test]
fn fixup_volatility_at_bar_2_uses_2_returns() {
    let bars = make_price_ramp(60, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);

    // Bar 2: fixup with min(2, 20) = 2 returns. Constant returns → std=0.
    assert!(
        !rows[2].volatility_20.is_nan(),
        "After compute_all, volatility_20 at bar 2 must NOT be NaN (uses 2 returns via fixup)",
    );
}

#[test]
fn fixup_does_not_touch_bar_0_or_bar_1() {
    let bars = make_price_ramp(60, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);

    // Bar 0: no returns available (i=0, i >= 2 is false), volatility stays NaN
    // Bar 1: i=1, i >= 2 is false, volatility stays NaN
    // But return_1 at bar 1 should be valid.
    assert!(
        rows[0].return_1.is_nan(),
        "Bar 0 return_1 should remain NaN after compute_all",
    );
}

#[test]
fn fixup_high_low_range_50_at_bar_30() {
    let bars = make_price_ramp(60, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);

    // At bar 30, n=31 which is <= 50, so incremental gives NaN.
    // fixup: count = min(31, 50) = 31 bars for high_low_range_50.
    assert!(
        !rows[30].high_low_range_50.is_nan(),
        "After compute_all, high_low_range_50 at bar 30 must NOT be NaN (fixup with 31 bars)",
    );
}

// =====================================================================
// Section 7: Guard Conditions (Known Risk Area)
//
// high_low_range_50 triggers at n > 50 (strictly greater).
// C++ uses n > 50, NOT n >= 50.
// =====================================================================

#[test]
fn high_low_range_50_nan_during_incremental_at_bar_49() {
    // At bar 49, close_mids.len() = 50, which is NOT > 50.
    let bars = make_price_ramp(51, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let mut rows = Vec::new();
    for bar in &bars {
        rows.push(computer.update(bar));
    }
    assert!(
        rows[49].high_low_range_50.is_nan(),
        "During incremental: bar 49 (n=50) should have NaN high_low_range_50 \
         (guard is n > 50, not n >= 50), got {}",
        rows[49].high_low_range_50,
    );
}

#[test]
fn high_low_range_50_computed_during_incremental_at_bar_50() {
    // At bar 50, close_mids.len() = 51, which IS > 50.
    let bars = make_price_ramp(52, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let mut rows = Vec::new();
    for bar in &bars {
        rows.push(computer.update(bar));
    }
    assert!(
        !rows[50].high_low_range_50.is_nan(),
        "During incremental: bar 50 (n=51) should have computed high_low_range_50, got NaN",
    );
}

#[test]
fn high_low_range_50_value_correct() {
    // Price ramp: bar i has close_mid = 4500 + i * 0.25
    // high_mid = close_mid + 0.25, low_mid = close_mid - 0.25
    // At bar 50 (n=51 > 50):
    //   Last 50 highs: bars 1-50 → high_mid = (4500.25+0.25)...(4512.5+0.25) = 4500.5...4512.75
    //   max(high[-50:]) = bar 50 high = 4500 + 50*0.25 + 0.25 = 4512.75
    //   Last 50 lows: bars 1-50 → low_mid = (4500.25-0.25)...(4512.5-0.25) = 4500.0...4512.25
    //   min(low[-50:]) = bar 1 low = 4500.0 + 1*0.25 - 0.25 = 4500.0
    //   range = (4512.75 - 4500.0) / 0.25 = 12.75 / 0.25 = 51.0
    let bars = make_price_ramp(52, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let mut rows = Vec::new();
    for bar in &bars {
        rows.push(computer.update(bar));
    }
    let expected = 51.0f32;
    assert!(
        (rows[50].high_low_range_50 - expected).abs() < 1e-2,
        "high_low_range_50 at bar 50 should be {} ticks, got {}",
        expected, rows[50].high_low_range_50,
    );
}

// =====================================================================
// Section 8: Message Microstructure (Known Risk Area)
//
// KNOWN RISK: message_rate = (add + cancel + modify) / duration
//             EXCLUDES trades. Model trained without trade count.
// =====================================================================

#[test]
fn message_rate_excludes_trades() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.add_count = 100;
    bar.cancel_count = 30;
    bar.modify_count = 10;
    bar.trade_event_count = 500; // Many trades — must NOT affect message_rate
    bar.bar_duration_s = 5.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);

    // message_rate = (100 + 30 + 10) / 5.0 = 28.0
    let expected = (100.0 + 30.0 + 10.0) / 5.0;
    assert!(
        (row.message_rate - expected).abs() < 1e-4,
        "message_rate = (add + cancel + modify) / duration = {}, got {} \
         (trade_event_count={} must NOT be included)",
        expected, row.message_rate, bar.trade_event_count,
    );
}

#[test]
fn message_rate_zero_duration_yields_zero() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.bar_duration_s = 0.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    assert!(
        row.message_rate.abs() < 1e-6,
        "Zero duration → message_rate = 0 (not infinity), got {}",
        row.message_rate,
    );
}

// --- cancel_add_ratio ---

#[test]
fn cancel_add_ratio_formula() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.cancel_count = 30;
    bar.add_count = 100;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // cancel_count / (add_count + eps) = 30 / (100 + eps) ≈ 0.3
    let expected = 30.0 / (100.0 + EPS);
    assert!(
        (row.cancel_add_ratio - expected).abs() < 1e-5,
        "cancel_add_ratio = cancel / (add + eps) = {}, got {}",
        expected, row.cancel_add_ratio,
    );
}

#[test]
fn cancel_add_ratio_zero_adds() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.add_count = 0;
    bar.cancel_count = 50;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // 50 / (0 + eps) is a large number, but should not be NaN or Inf
    assert!(
        !row.cancel_add_ratio.is_nan() && !row.cancel_add_ratio.is_infinite(),
        "cancel_add_ratio with zero adds must not be NaN/Inf, got {}",
        row.cancel_add_ratio,
    );
}

// --- modify_fraction ---

#[test]
fn modify_fraction_formula() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.add_count = 100;
    bar.cancel_count = 30;
    bar.modify_count = 10;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // modify / (add + cancel + modify + eps) = 10 / (140 + eps) ≈ 0.07143
    let total = 100.0 + 30.0 + 10.0;
    let expected = 10.0 / (total + EPS);
    assert!(
        (row.modify_fraction - expected).abs() < 1e-5,
        "modify_fraction = modify / (total + eps) = {}, got {}",
        expected, row.modify_fraction,
    );
}

// =====================================================================
// Section 9: Time Context Features
// =====================================================================

#[test]
fn time_sin_at_noon() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.time_of_day = 12.0; // noon
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // sin(2π × 12/24) = sin(π) ≈ 0
    assert!(
        row.time_sin.abs() < 1e-5,
        "time_sin at noon (12h) = sin(π) ≈ 0, got {}",
        row.time_sin,
    );
}

#[test]
fn time_cos_at_noon() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.time_of_day = 12.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // cos(2π × 12/24) = cos(π) = -1
    assert!(
        (row.time_cos - (-1.0)).abs() < 1e-5,
        "time_cos at noon (12h) = cos(π) = -1, got {}",
        row.time_cos,
    );
}

#[test]
fn time_sin_at_6am() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.time_of_day = 6.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // sin(2π × 6/24) = sin(π/2) = 1.0
    assert!(
        (row.time_sin - 1.0).abs() < 1e-5,
        "time_sin at 6h = sin(π/2) = 1.0, got {}",
        row.time_sin,
    );
}

#[test]
fn time_cos_at_6am() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.time_of_day = 6.0;
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // cos(2π × 6/24) = cos(π/2) ≈ 0
    assert!(
        row.time_cos.abs() < 1e-5,
        "time_cos at 6h = cos(π/2) ≈ 0, got {}",
        row.time_cos,
    );
}

#[test]
fn minutes_since_open_after_rth_open() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.time_of_day = 10.0; // 10:00 AM, 30 min after RTH open (9:30)
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // max(0, (10.0 - 9.5) * 60) = 30.0
    assert!(
        (row.minutes_since_open - 30.0).abs() < 1e-4,
        "minutes_since_open at 10:00 = 30 min after 9:30 open, got {}",
        row.minutes_since_open,
    );
}

#[test]
fn minutes_since_open_before_rth_clamps_to_zero() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.time_of_day = 9.0; // 9:00 AM, before RTH open
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // max(0, (9.0 - 9.5) * 60) = max(0, -30) = 0
    assert!(
        row.minutes_since_open.abs() < 1e-6,
        "minutes_since_open before RTH open should be 0, got {}",
        row.minutes_since_open,
    );
}

#[test]
fn minutes_since_open_at_close() {
    let mut bar = make_uniform_bar(0, 4500.0, 100);
    bar.time_of_day = 16.0; // 4:00 PM RTH close
    let mut computer = BarFeatureComputer::new();
    let row = computer.update(&bar);
    // max(0, (16.0 - 9.5) * 60) = 390.0
    assert!(
        (row.minutes_since_open - 390.0).abs() < 1e-4,
        "minutes_since_open at RTH close (16:00) = 390, got {}",
        row.minutes_since_open,
    );
}

// =====================================================================
// Section 10: Warmup Bars
//
// First 50 bars (indices 0-49) are warmup. Parity tool skips them.
// =====================================================================

#[test]
fn first_50_bars_are_warmup() {
    let bars = make_price_ramp(55, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    for i in 0..50 {
        assert!(
            rows[i].is_warmup,
            "Bar {} must be warmup (is_warmup=true), got false",
            i,
        );
    }
}

#[test]
fn bar_50_is_not_warmup() {
    let bars = make_price_ramp(55, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    assert!(
        !rows[50].is_warmup,
        "Bar 50 must NOT be warmup (first non-warmup bar), got is_warmup=true",
    );
}

// =====================================================================
// Section 11: close_position Feature Contract
// =====================================================================

#[test]
fn close_position_mid_range_value() {
    // With a uniform price ramp, close_position should be close to 1.0
    // at the latest bar (close_mid is at the top of recent range).
    let bars = make_price_ramp(25, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows: Vec<_> = bars.iter().map(|b| computer.update(b)).collect();
    // At bar 24 (n=25 > 20): uses last 20 bars
    // close_mid = 4500 + 24*0.25 = 4506.0
    // max(high[-20:]) = bar 24 high = 4506.25
    // min(low[-20:]) = bar 5 low = 4500 + 5*0.25 - 0.25 = 4501.0
    // close_position = (4506.0 - 4501.0) / (4506.25 - 4501.0 + eps) = 5.0 / 5.25 ≈ 0.952
    let actual = rows[24].close_position;
    assert!(
        actual > 0.9 && actual <= 1.0,
        "close_position at top of range should be > 0.9, got {}",
        actual,
    );
}

// =====================================================================
// Section 12: Batch Mode (compute_all) Contract
//
// compute_all runs incremental + fixup + forward returns.
// The parity tool should use compute_all for correct behavior.
// =====================================================================

#[test]
fn compute_all_fills_forward_returns() {
    let bars = make_price_ramp(10, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);
    // fwd_return_1 at bar 0 should be valid
    assert!(
        !rows[0].fwd_return_1.is_nan(),
        "compute_all should fill fwd_return_1 at bar 0",
    );
    // fwd_return_1 at last bar should be NaN (no bar ahead)
    assert!(
        rows[9].fwd_return_1.is_nan(),
        "fwd_return_1 at last bar must be NaN",
    );
}

#[test]
fn compute_all_fixup_makes_early_volatility_non_nan() {
    let bars = make_price_ramp(60, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);
    // Bars 2-19 should have volatility_20 filled by fixup (not NaN)
    for i in 2..20 {
        assert!(
            !rows[i].volatility_20.is_nan(),
            "After compute_all, volatility_20 at bar {} must NOT be NaN (fixup required)",
            i,
        );
    }
}

#[test]
fn compute_all_fixup_high_low_range_50_early_bars() {
    let bars = make_price_ramp(60, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);
    // Bars 1-49 should have high_low_range_50 filled by fixup
    for i in 1..50 {
        assert!(
            !rows[i].high_low_range_50.is_nan(),
            "After compute_all, high_low_range_50 at bar {} must NOT be NaN (fixup required)",
            i,
        );
    }
}

// =====================================================================
// Section 13: 20-Feature Extraction Round-Trip
//
// Verify that extracting the 20 model features from a fully-computed
// BarFeatureRow produces finite, non-NaN values (post-warmup).
// =====================================================================

#[test]
fn all_20_features_finite_after_warmup() {
    let bars = make_price_ramp(100, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);

    // Bar 60: well past warmup (bar 50), all lookbacks satisfied
    let feats = extract_model_features(&rows[60]);
    for (idx, &val) in feats.iter().enumerate() {
        assert!(
            val.is_finite(),
            "Model feature #{} ('{}') at bar 60 must be finite, got {}",
            idx, MODEL_FEATURES[idx], val,
        );
    }
}

#[test]
fn all_20_features_not_nan_after_warmup() {
    let bars = make_price_ramp(100, 4500.0, TICK_SIZE);
    let mut computer = BarFeatureComputer::new();
    let rows = computer.compute_all(&bars);

    // Check several post-warmup bars
    for bar_idx in [51, 60, 75, 99] {
        let feats = extract_model_features(&rows[bar_idx]);
        for (feat_idx, &val) in feats.iter().enumerate() {
            assert!(
                !val.is_nan(),
                "Model feature #{} ('{}') at bar {} must NOT be NaN, got NaN",
                feat_idx, MODEL_FEATURES[feat_idx], bar_idx,
            );
        }
    }
}
