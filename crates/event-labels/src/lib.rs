//! Event-level label computation via tick-level barrier simulation.
//!
//! Simulates barrier outcomes (target hit, stop hit, horizon) from an entry
//! point using tick-level mid-price data. Entry at bid/ask (not mid).

/// Configuration for barrier simulation.
#[derive(Debug, Clone)]
pub struct EventLabelConfig {
    /// Target in ticks (positive).
    pub target_ticks: i32,
    /// Stop in ticks (positive).
    pub stop_ticks: i32,
    /// Tick size for the instrument (e.g., 0.25 for MES).
    pub tick_size: f64,
    /// Maximum horizon in nanoseconds (e.g., 3600 * 1e9 for 1 hour).
    pub max_horizon_ns: u64,
}

/// Result of a barrier simulation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarrierOutcome {
    /// Price hit the target barrier first.
    TargetHit {
        exit_ts: u64,
        ticks_pnl: f64,
    },
    /// Price hit the stop barrier first.
    StopHit {
        exit_ts: u64,
        ticks_pnl: f64,
    },
    /// Neither barrier was hit within the time horizon.
    Horizon {
        exit_ts: u64,
        ticks_pnl: f64,
    },
}

impl BarrierOutcome {
    /// Returns the exit timestamp.
    pub fn exit_ts(&self) -> u64 {
        match self {
            Self::TargetHit { exit_ts, .. } => *exit_ts,
            Self::StopHit { exit_ts, .. } => *exit_ts,
            Self::Horizon { exit_ts, .. } => *exit_ts,
        }
    }

    /// Returns the PnL in ticks.
    pub fn ticks_pnl(&self) -> f64 {
        match self {
            Self::TargetHit { ticks_pnl, .. } => *ticks_pnl,
            Self::StopHit { ticks_pnl, .. } => *ticks_pnl,
            Self::Horizon { ticks_pnl, .. } => *ticks_pnl,
        }
    }

    /// Returns the outcome as an i8: 1 (target), 0 (stop), -1 (horizon).
    pub fn outcome_code(&self) -> i8 {
        match self {
            Self::TargetHit { .. } => 1,
            Self::StopHit { .. } => 0,
            Self::Horizon { .. } => -1,
        }
    }

    /// Returns true if the target was hit.
    pub fn is_target(&self) -> bool {
        matches!(self, Self::TargetHit { .. })
    }

    /// Returns true if the stop was hit.
    pub fn is_stop(&self) -> bool {
        matches!(self, Self::StopHit { .. })
    }
}

/// Simulate a barrier outcome from an entry point using tick-level mid-price data.
///
/// # Arguments
/// * `tick_mids` - Sorted slice of (timestamp_ns, mid_price) pairs.
/// * `entry_ts` - Timestamp of entry (will scan forward from here).
/// * `entry_price` - Entry price (best ask for longs, best bid for shorts).
/// * `direction` - +1.0 for long, -1.0 for short.
/// * `cfg` - Barrier configuration (T, S, tick_size, max_horizon).
///
/// # Algorithm
/// 1. Binary search tick_mids for the first tick after entry_ts.
/// 2. Walk forward, checking each tick for barrier breach.
/// 3. If horizon exceeded, return terminal PnL.
pub fn simulate_barrier(
    tick_mids: &[(u64, f32)],
    entry_ts: u64,
    entry_price: f64,
    direction: f64,
    cfg: &EventLabelConfig,
) -> BarrierOutcome {
    let target_threshold = cfg.target_ticks as f64;
    let stop_threshold = cfg.stop_ticks as f64;
    let horizon_end = entry_ts.saturating_add(cfg.max_horizon_ns);

    // Binary search for the first tick after entry_ts
    let start_idx = tick_mids.partition_point(|&(ts, _)| ts <= entry_ts);

    for &(ts, mid) in &tick_mids[start_idx..] {
        // Check horizon first
        if ts > horizon_end {
            let move_ticks = (mid as f64 - entry_price) * direction / cfg.tick_size;
            return BarrierOutcome::Horizon {
                exit_ts: ts,
                ticks_pnl: move_ticks,
            };
        }

        let move_ticks = (mid as f64 - entry_price) * direction / cfg.tick_size;

        if move_ticks >= target_threshold {
            return BarrierOutcome::TargetHit {
                exit_ts: ts,
                ticks_pnl: move_ticks,
            };
        }

        if move_ticks <= -stop_threshold {
            return BarrierOutcome::StopHit {
                exit_ts: ts,
                ticks_pnl: move_ticks,
            };
        }
    }

    // Ran out of tick data before horizon — treat as horizon with last known PnL
    if let Some(&(ts, mid)) = tick_mids.last() {
        let move_ticks = (mid as f64 - entry_price) * direction / cfg.tick_size;
        BarrierOutcome::Horizon {
            exit_ts: ts,
            ticks_pnl: move_ticks,
        }
    } else {
        BarrierOutcome::Horizon {
            exit_ts: entry_ts,
            ticks_pnl: 0.0,
        }
    }
}

/// Default geometries to generate labels for.
/// Each pair is (target_ticks, stop_ticks).
pub const DEFAULT_GEOMETRIES: [(i32, i32); 11] = [
    (5, 5),
    (10, 5),
    (10, 10),
    (15, 5),
    (15, 10),
    (15, 15),
    (19, 7),
    (19, 19),
    (25, 10),
    (25, 25),
    (40, 20), // 10pt target / 5pt stop (4 ticks/point on MES)
];

/// Generate labels for a single evaluation point across multiple geometries.
///
/// Returns a Vec of (target_ticks, stop_ticks, BarrierOutcome) for each geometry.
pub fn generate_multi_geometry_labels(
    tick_mids: &[(u64, f32)],
    entry_ts: u64,
    entry_price: f64,
    direction: f64,
    tick_size: f64,
    max_horizon_ns: u64,
    geometries: &[(i32, i32)],
) -> Vec<(i32, i32, BarrierOutcome)> {
    geometries
        .iter()
        .map(|&(t, s)| {
            let cfg = EventLabelConfig {
                target_ticks: t,
                stop_ticks: s,
                tick_size,
                max_horizon_ns,
            };
            let outcome = simulate_barrier(tick_mids, entry_ts, entry_price, direction, &cfg);
            (t, s, outcome)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tick_series() -> Vec<(u64, f32)> {
        // Simulate a price path: starts at 4500.125, goes up, then down
        // Tick size = 0.25, so 1 tick = 0.25
        vec![
            (1000, 4500.125),  // mid at entry
            (2000, 4500.375),  // +1 tick from entry at ask
            (3000, 4500.625),  // +2 ticks
            (4000, 4500.875),  // +3 ticks
            (5000, 4501.125),  // +4 ticks
            (6000, 4501.375),  // +5 ticks (target hit for T=5 from ask=4500.25)
            (7000, 4501.125),  // back to +4
            (8000, 4500.875),  // +3
            (9000, 4500.125),  // back to 0
            (10000, 4499.875), // -1 tick
        ]
    }

    #[test]
    fn test_target_hit_long() {
        let ticks = make_tick_series();
        let cfg = EventLabelConfig {
            target_ticks: 4,
            stop_ticks: 5,
            tick_size: 0.25,
            max_horizon_ns: 100_000,
        };
        // Long entry at ask = 4500.25
        let result = simulate_barrier(&ticks, 0, 4500.25, 1.0, &cfg);
        assert!(result.is_target(), "expected target hit, got {:?}", result);
        // 4 ticks up from 4500.25 = 4501.25 → mid 4501.125 is (4501.125-4500.25)/0.25 = 3.5 ticks
        // Actually 4501.375 is (4501.375-4500.25)/0.25 = 4.5 ticks → target
        assert_eq!(result.exit_ts(), 6000);
    }

    #[test]
    fn test_stop_hit_long() {
        // Price goes down from entry
        let ticks = vec![
            (1000, 4500.125),
            (2000, 4500.00),    // -1 tick from ask=4500.25
            (3000, 4499.75),    // -2 ticks
            (4000, 4499.50),    // -3 ticks → stop hit for S=3
        ];
        let cfg = EventLabelConfig {
            target_ticks: 10,
            stop_ticks: 3,
            tick_size: 0.25,
            max_horizon_ns: 100_000,
        };
        let result = simulate_barrier(&ticks, 0, 4500.25, 1.0, &cfg);
        assert!(result.is_stop(), "expected stop hit, got {:?}", result);
        assert_eq!(result.exit_ts(), 4000);
    }

    #[test]
    fn test_horizon_no_barrier() {
        let ticks = vec![
            (1000, 4500.125),
            (2000, 4500.250),
            (3000, 4500.125),
        ];
        let cfg = EventLabelConfig {
            target_ticks: 100, // very far target
            stop_ticks: 100,   // very far stop
            tick_size: 0.25,
            max_horizon_ns: 500, // very short horizon
        };
        let result = simulate_barrier(&ticks, 0, 4500.25, 1.0, &cfg);
        match result {
            BarrierOutcome::Horizon { .. } => {} // expected
            _ => panic!("expected horizon, got {:?}", result),
        }
    }

    #[test]
    fn test_short_direction() {
        // For shorts, entry at best bid, profit when price goes down
        let ticks = vec![
            (1000, 4500.125),
            (2000, 4499.875),  // -1 tick from mid perspective
            (3000, 4499.625),  // -2 ticks
            (4000, 4499.375),  // -3 ticks
            (5000, 4499.125),  // -4 ticks
        ];
        let cfg = EventLabelConfig {
            target_ticks: 3,
            stop_ticks: 10,
            tick_size: 0.25,
            max_horizon_ns: 100_000,
        };
        // Short entry at bid = 4500.00
        // Price move for short: (4500.00 - mid) / tick_size * direction
        // At ts=4000: (4500.00 - 4499.375) * (-1) * (-1) / 0.25 = ...
        // direction=-1: (mid - entry) * direction = (4499.375 - 4500.00) * (-1.0) = 0.625
        // move_ticks = 0.625 / 0.25 = 2.5
        // At ts=5000: (4499.125 - 4500.00) * (-1.0) / 0.25 = 3.5 → target
        let result = simulate_barrier(&ticks, 0, 4500.00, -1.0, &cfg);
        assert!(result.is_target(), "expected target hit for short, got {:?}", result);
        assert_eq!(result.exit_ts(), 5000);
    }

    #[test]
    fn test_outcome_codes() {
        let target = BarrierOutcome::TargetHit { exit_ts: 100, ticks_pnl: 5.0 };
        let stop = BarrierOutcome::StopHit { exit_ts: 200, ticks_pnl: -3.0 };
        let horizon = BarrierOutcome::Horizon { exit_ts: 300, ticks_pnl: 1.0 };

        assert_eq!(target.outcome_code(), 1);
        assert_eq!(stop.outcome_code(), 0);
        assert_eq!(horizon.outcome_code(), -1);
    }

    #[test]
    fn test_multi_geometry_labels() {
        let ticks = make_tick_series();
        let results = generate_multi_geometry_labels(
            &ticks,
            0,
            4500.25,  // entry at ask
            1.0,       // long
            0.25,      // tick_size
            100_000,   // max_horizon
            &[(3, 3), (5, 5), (10, 10)],
        );
        assert_eq!(results.len(), 3);

        // (3,3): target should be hit
        assert_eq!(results[0].0, 3);
        assert_eq!(results[0].1, 3);
        assert!(results[0].2.is_target());

        // (5,5): max move is +4.5 ticks from ask, doesn't reach 5 → horizon
        assert_eq!(results[1].2.outcome_code(), -1);

        // (10,10): neither barrier → horizon
        assert_eq!(results[2].2.outcome_code(), -1);
    }

    #[test]
    fn test_binary_search_start() {
        // Entry at ts=5000, should skip ticks before
        let ticks = vec![
            (1000, 4500.125),
            (2000, 4500.375),
            (3000, 4500.625),
            (5000, 4500.875), // first tick after entry
            (6000, 4501.125),
            (7000, 4501.375),
        ];
        let cfg = EventLabelConfig {
            target_ticks: 3,
            stop_ticks: 10,
            tick_size: 0.25,
            max_horizon_ns: 100_000,
        };
        // Entry at ts=3500, price=4500.25 (long)
        let result = simulate_barrier(&ticks, 3500, 4500.25, 1.0, &cfg);
        // First tick checked is (5000, 4500.875) → (4500.875-4500.25)/0.25 = 2.5 ticks
        // (6000, 4501.125) → 3.5 ticks → target hit
        assert!(result.is_target());
        assert_eq!(result.exit_ts(), 6000);
    }

    #[test]
    fn test_empty_tick_data() {
        let ticks: Vec<(u64, f32)> = vec![];
        let cfg = EventLabelConfig {
            target_ticks: 5,
            stop_ticks: 5,
            tick_size: 0.25,
            max_horizon_ns: 100_000,
        };
        let result = simulate_barrier(&ticks, 0, 4500.25, 1.0, &cfg);
        assert_eq!(result.outcome_code(), -1); // horizon
        assert_eq!(result.ticks_pnl(), 0.0);
    }

    #[test]
    fn test_null_hypothesis_geometry() {
        // Under random walk, P(target) = S/(T+S)
        // For T=S, P(target) should be 50%
        // We can't test this statistically in a unit test, but we can verify
        // the default geometries are all valid
        for &(t, s) in &DEFAULT_GEOMETRIES {
            assert!(t > 0);
            assert!(s > 0);
            let p_null = s as f64 / (t as f64 + s as f64);
            assert!(p_null > 0.0 && p_null < 1.0);
        }
    }
}
