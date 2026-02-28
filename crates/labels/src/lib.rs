use common::bar::Bar;

/// Triple barrier configuration.
#[derive(Debug, Clone)]
pub struct TripleBarrierConfig {
    pub target_ticks: i32,
    pub stop_ticks: i32,
    pub volume_horizon: u32,
    pub min_return_ticks: i32,
    pub max_time_horizon_s: u32,
    pub tick_size: f32,
    pub bidirectional: bool,
}

impl Default for TripleBarrierConfig {
    fn default() -> Self {
        Self {
            target_ticks: 10,
            stop_ticks: 5,
            volume_horizon: 50000,
            min_return_ticks: 2,
            max_time_horizon_s: 3600,
            tick_size: 0.25,
            bidirectional: true,
        }
    }
}

/// Result of a unidirectional triple barrier label computation.
#[derive(Debug, Clone)]
pub struct TripleBarrierResult {
    /// Label: -1 (short), 0 (hold), +1 (long).
    pub label: i32,
    /// Exit type: "target", "stop", "expiry", "timeout".
    pub exit_type: String,
    /// Bars held from entry to exit.
    pub bars_held: i32,
}

/// Result of a bidirectional triple barrier label computation.
#[derive(Debug, Clone)]
pub struct BidirectionalTBResult {
    /// Label: -1 (short), 0 (hold), +1 (long).
    pub label: i32,
    /// Exit type: "long_target", "short_target", "both", "neither", etc.
    pub exit_type: String,
    /// Bars held from entry to exit.
    pub bars_held: i32,
    pub long_triggered: bool,
    pub short_triggered: bool,
    pub both_triggered: bool,
}

impl Default for BidirectionalTBResult {
    fn default() -> Self {
        Self {
            label: 0,
            exit_type: String::new(),
            bars_held: 0,
            long_triggered: false,
            short_triggered: false,
            both_triggered: false,
        }
    }
}

/// Position-independent triple barrier label for a single bar.
///
/// At bar `idx`, asks: "if we entered long here, would target or stop hit first?"
/// Scans forward accumulating volume until volume_horizon or max_time_horizon_s.
pub fn compute_tb_label(bars: &[Bar], idx: usize, cfg: &TripleBarrierConfig) -> TripleBarrierResult {
    let n = bars.len();
    let entry_mid = bars[idx].close_mid;
    let target_dist = cfg.target_ticks as f32 * cfg.tick_size;
    let stop_dist = cfg.stop_ticks as f32 * cfg.tick_size;
    let min_return_dist = cfg.min_return_ticks as f32 * cfg.tick_size;

    let mut cum_volume: u32 = 0;

    for j in (idx + 1)..n {
        cum_volume += bars[j].volume;
        let diff = bars[j].close_mid - entry_mid;
        let held = (j - idx) as i32;

        let elapsed_s = (bars[j].close_ts - bars[idx].close_ts) as f32 / 1.0e9;

        // Time cap (hard safety limit)
        if elapsed_s >= cfg.max_time_horizon_s as f32 {
            if diff.abs() >= min_return_dist {
                return TripleBarrierResult {
                    label: if diff > 0.0 { 1 } else { -1 },
                    exit_type: "timeout".to_string(),
                    bars_held: held,
                };
            }
            return TripleBarrierResult {
                label: 0,
                exit_type: "timeout".to_string(),
                bars_held: held,
            };
        }

        // Upper barrier: target hit
        if diff >= target_dist {
            return TripleBarrierResult {
                label: 1,
                exit_type: "target".to_string(),
                bars_held: held,
            };
        }
        // Lower barrier: stop hit
        if -diff >= stop_dist {
            return TripleBarrierResult {
                label: -1,
                exit_type: "stop".to_string(),
                bars_held: held,
            };
        }

        // Volume expiry
        if cum_volume >= cfg.volume_horizon {
            if diff.abs() >= min_return_dist {
                return TripleBarrierResult {
                    label: if diff > 0.0 { 1 } else { -1 },
                    exit_type: "expiry".to_string(),
                    bars_held: held,
                };
            }
            return TripleBarrierResult {
                label: 0,
                exit_type: "expiry".to_string(),
                bars_held: held,
            };
        }
    }

    // Ran out of bars (end of day)
    let held = if n > idx + 1 { (n - 1 - idx) as i32 } else { 0 };
    if n > idx + 1 {
        let final_diff = bars[n - 1].close_mid - entry_mid;
        if final_diff.abs() >= min_return_dist {
            return TripleBarrierResult {
                label: if final_diff > 0.0 { 1 } else { -1 },
                exit_type: "expiry".to_string(),
                bars_held: held,
            };
        }
    }
    TripleBarrierResult {
        label: 0,
        exit_type: "expiry".to_string(),
        bars_held: held,
    }
}

/// Check if a V-reversal completes: after one race triggered, does the opposite
/// race's target get reached without continuation past target or re-stop?
fn v_reversal_target_reached(
    bars: &[Bar],
    idx: usize,
    scan_end: usize,
    entry_mid: f32,
    target_dist: f32,
    stop_dist: f32,
    max_time_s: f32,
    dir: f32,
) -> bool {
    let n = bars.len();

    // Phase 1: find the target bar
    let mut target_bar: Option<usize> = None;
    for j in (scan_end + 1)..n {
        let elapsed_s = (bars[j].close_ts - bars[idx].close_ts) as f32 / 1.0e9;
        if elapsed_s >= max_time_s {
            break;
        }
        let diff = bars[j].close_mid - entry_mid;
        if dir * diff >= target_dist {
            target_bar = Some(j);
            break;
        }
    }
    let target_bar = match target_bar {
        Some(tb) => tb,
        None => return false,
    };

    // Phase 2: validate no continuation past target or re-stop
    for j in (target_bar + 1)..n {
        let elapsed_s = (bars[j].close_ts - bars[idx].close_ts) as f32 / 1.0e9;
        if elapsed_s >= max_time_s {
            break;
        }
        let diff = bars[j].close_mid - entry_mid;
        if dir * diff > target_dist {
            return false; // continuation past target
        }
        if -dir * diff >= stop_dist {
            return false; // re-stop
        }
    }
    true
}

/// Bidirectional triple barrier label for a single bar.
///
/// Runs two independent races:
///   Long race:  does price hit +target_dist before -stop_dist?
///   Short race: does price hit -target_dist before +stop_dist?
/// Label = +1 if only long wins, -1 if only short wins, 0 if both or neither.
pub fn compute_bidirectional_tb_label(
    bars: &[Bar],
    idx: usize,
    cfg: &TripleBarrierConfig,
) -> BidirectionalTBResult {
    // Non-bidirectional: delegate for backward compatibility
    if !cfg.bidirectional {
        let old = compute_tb_label(bars, idx, cfg);
        return BidirectionalTBResult {
            label: old.label,
            exit_type: old.exit_type,
            bars_held: old.bars_held,
            ..Default::default()
        };
    }

    let n = bars.len();
    let entry_mid = bars[idx].close_mid;
    let target_dist = cfg.target_ticks as f32 * cfg.tick_size;
    let stop_dist = cfg.stop_ticks as f32 * cfg.tick_size;
    let max_time_s = cfg.max_time_horizon_s as f32;

    let mut long_resolved = false;
    let mut long_triggered = false;
    let mut short_resolved = false;
    let mut short_triggered = false;

    let mut cum_volume: u32 = 0;
    let mut held = 0i32;
    let mut scan_end = idx;

    for j in (idx + 1)..n {
        cum_volume += bars[j].volume;
        let diff = bars[j].close_mid - entry_mid;
        held = (j - idx) as i32;
        scan_end = j;

        let elapsed_s = (bars[j].close_ts - bars[idx].close_ts) as f32 / 1.0e9;

        if elapsed_s >= max_time_s {
            break;
        }

        // Long race
        if !long_resolved {
            if diff >= target_dist {
                long_triggered = true;
                long_resolved = true;
            } else if -diff >= stop_dist {
                long_resolved = true;
            }
        }

        // Short race
        if !short_resolved {
            if -diff >= target_dist {
                short_triggered = true;
                short_resolved = true;
            } else if diff >= stop_dist {
                short_resolved = true;
            }
        }

        if long_resolved && short_resolved {
            break;
        }

        if cum_volume >= cfg.volume_horizon {
            break;
        }
    }

    // V-reversal override
    if long_triggered && !short_triggered && short_resolved {
        if v_reversal_target_reached(
            bars, idx, scan_end, entry_mid, target_dist, stop_dist, max_time_s, -1.0,
        ) {
            short_triggered = true;
        }
    } else if short_triggered && !long_triggered && long_resolved {
        if v_reversal_target_reached(
            bars, idx, scan_end, entry_mid, target_dist, stop_dist, max_time_s, 1.0,
        ) {
            long_triggered = true;
        }
    }

    let mut result = BidirectionalTBResult::default();
    result.bars_held = held;
    result.long_triggered = long_triggered;
    result.short_triggered = short_triggered;
    result.both_triggered = long_triggered && short_triggered;

    if long_triggered && short_triggered {
        result.label = 0;
        result.exit_type = "both".to_string();
    } else if long_triggered {
        result.label = 1;
        result.exit_type = "long_target".to_string();
    } else if short_triggered {
        result.label = -1;
        result.exit_type = "short_target".to_string();
    } else {
        result.label = 0;
        result.exit_type = "neither".to_string();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bar(close_ts: u64, close_mid: f32, volume: u32) -> Bar {
        Bar {
            close_ts,
            close_mid,
            volume,
            ..Default::default()
        }
    }

    fn default_cfg() -> TripleBarrierConfig {
        TripleBarrierConfig {
            target_ticks: 10,
            stop_ticks: 5,
            tick_size: 0.25,
            ..Default::default()
        }
    }

    #[test]
    fn test_target_hit_long() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4502.5, 100), // +2.5 = 10 ticks → target
        ];
        let result = compute_tb_label(&bars, 0, &default_cfg());
        assert_eq!(result.label, 1);
        assert_eq!(result.exit_type, "target");
    }

    #[test]
    fn test_stop_hit_short() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4498.75, 100), // -1.25 = 5 ticks → stop
        ];
        let result = compute_tb_label(&bars, 0, &default_cfg());
        assert_eq!(result.label, -1);
        assert_eq!(result.exit_type, "stop");
    }

    #[test]
    fn test_hold_no_movement() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4500.0, 100_000), // volume expiry, no movement
        ];
        let cfg = TripleBarrierConfig {
            volume_horizon: 100_000,
            ..default_cfg()
        };
        let result = compute_tb_label(&bars, 0, &cfg);
        assert_eq!(result.label, 0);
        assert_eq!(result.exit_type, "expiry");
    }

    #[test]
    fn test_bidirectional_long_only() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4502.5, 100), // +2.5 = 10 ticks → long target
        ];
        let result = compute_bidirectional_tb_label(&bars, 0, &default_cfg());
        assert_eq!(result.label, 1);
        assert_eq!(result.exit_type, "long_target");
        assert!(result.long_triggered);
        assert!(!result.short_triggered);
    }

    #[test]
    fn test_bidirectional_short_only() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4497.5, 100), // -2.5 = 10 ticks → short target
        ];
        let result = compute_bidirectional_tb_label(&bars, 0, &default_cfg());
        assert_eq!(result.label, -1);
        assert_eq!(result.exit_type, "short_target");
        assert!(!result.long_triggered);
        assert!(result.short_triggered);
    }

    #[test]
    fn test_bidirectional_neither() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4500.0, 100_000),
        ];
        let cfg = TripleBarrierConfig {
            volume_horizon: 100_000,
            ..default_cfg()
        };
        let result = compute_bidirectional_tb_label(&bars, 0, &cfg);
        assert_eq!(result.label, 0);
        assert_eq!(result.exit_type, "neither");
    }

    #[test]
    fn test_timeout() {
        let bars = vec![
            make_bar(0, 4500.0, 0),
            make_bar(3_600_000_000_000, 4500.0, 100), // 3600s elapsed
        ];
        let result = compute_tb_label(&bars, 0, &default_cfg());
        assert_eq!(result.label, 0);
        assert_eq!(result.exit_type, "timeout");
    }

    #[test]
    fn test_non_bidirectional_fallback() {
        let bars = vec![
            make_bar(1_000_000_000, 4500.0, 0),
            make_bar(2_000_000_000, 4502.5, 100),
        ];
        let cfg = TripleBarrierConfig {
            bidirectional: false,
            ..default_cfg()
        };
        let result = compute_bidirectional_tb_label(&bars, 0, &cfg);
        assert_eq!(result.label, 1);
        assert_eq!(result.exit_type, "target");
    }
}
