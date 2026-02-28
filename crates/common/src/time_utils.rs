/// Time constants and utilities for ET (Eastern Time) nanosecond timestamps.

pub const NS_PER_SEC: u64 = 1_000_000_000;
pub const NS_PER_HOUR: u64 = 3600 * NS_PER_SEC;
pub const NS_PER_DAY: u64 = 24 * NS_PER_HOUR;

/// 2022-01-03 00:00:00 ET in UTC nanoseconds (reference epoch).
pub const REF_MIDNIGHT_ET_NS: u64 = 1_641_186_000 * NS_PER_SEC;

/// Compute midnight ET (nanoseconds) for the day containing `ts`.
///
/// Uses signed arithmetic to safely handle timestamps before the reference epoch.
pub fn midnight_et_ns(ts: u64) -> u64 {
    let diff = ts as i64 - REF_MIDNIGHT_ET_NS as i64;
    let ns_per_day = NS_PER_DAY as i64;
    // Floor division for negative values
    let day_offset = if diff >= 0 {
        diff / ns_per_day
    } else {
        (diff - ns_per_day + 1) / ns_per_day
    };
    (REF_MIDNIGHT_ET_NS as i64 + day_offset * ns_per_day) as u64
}

/// RTH open time (09:30 ET) in nanoseconds for the day containing `ts`.
pub fn rth_open_ns(ts: u64) -> u64 {
    midnight_et_ns(ts) + 9 * NS_PER_HOUR + 30 * 60 * NS_PER_SEC
}

/// RTH close time (16:00 ET) in nanoseconds for the day containing `ts`.
pub fn rth_close_ns(ts: u64) -> u64 {
    midnight_et_ns(ts) + 16 * NS_PER_HOUR
}

/// Fractional hours since midnight ET.
pub fn compute_time_of_day(ts: u64) -> f32 {
    let midnight = midnight_et_ns(ts);
    let ns_since_midnight = ts as i64 - midnight as i64;
    (ns_since_midnight as f64 / NS_PER_HOUR as f64) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ref_midnight() {
        // REF_MIDNIGHT_ET_NS should be 2022-01-03 00:00:00 ET
        assert_eq!(midnight_et_ns(REF_MIDNIGHT_ET_NS), REF_MIDNIGHT_ET_NS);
        // One hour into the day
        assert_eq!(midnight_et_ns(REF_MIDNIGHT_ET_NS + NS_PER_HOUR), REF_MIDNIGHT_ET_NS);
    }

    #[test]
    fn test_next_day_midnight() {
        let next_day = REF_MIDNIGHT_ET_NS + NS_PER_DAY;
        assert_eq!(midnight_et_ns(next_day), next_day);
        assert_eq!(midnight_et_ns(next_day + NS_PER_HOUR), next_day);
    }

    #[test]
    fn test_rth_open_close() {
        let open = rth_open_ns(REF_MIDNIGHT_ET_NS);
        let close = rth_close_ns(REF_MIDNIGHT_ET_NS);
        // RTH is 6.5 hours = 23400 seconds
        assert_eq!(close - open, 6 * NS_PER_HOUR + 30 * 60 * NS_PER_SEC);
    }

    #[test]
    fn test_time_of_day() {
        // At midnight → 0.0
        let tod = compute_time_of_day(REF_MIDNIGHT_ET_NS);
        assert!((tod - 0.0).abs() < 1e-6);

        // At noon → 12.0
        let noon = REF_MIDNIGHT_ET_NS + 12 * NS_PER_HOUR;
        let tod_noon = compute_time_of_day(noon);
        assert!((tod_noon - 12.0).abs() < 1e-4);

        // At 09:30 → 9.5
        let rth_open = rth_open_ns(REF_MIDNIGHT_ET_NS);
        let tod_open = compute_time_of_day(rth_open);
        assert!((tod_open - 9.5).abs() < 1e-4);
    }
}
