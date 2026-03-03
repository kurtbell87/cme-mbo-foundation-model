/// Time constants and utilities for ET (Eastern Time) nanosecond timestamps.

pub const NS_PER_SEC: u64 = 1_000_000_000;
pub const NS_PER_HOUR: u64 = 3600 * NS_PER_SEC;
pub const NS_PER_DAY: u64 = 24 * NS_PER_HOUR;

/// 2022-01-03 00:00:00 ET in UTC nanoseconds (reference epoch).
pub const REF_MIDNIGHT_ET_NS: u64 = 1_641_186_000 * NS_PER_SEC;

/// Compute midnight ET (nanoseconds) for the day containing `ts`.
///
/// Uses signed arithmetic to safely handle timestamps before the reference epoch.
///
/// **Warning**: This derives the calendar day from a UTC timestamp using a
/// fixed EST (UTC-5) offset. It does NOT handle DST and will mis-assign
/// the day for timestamps that fall in the 1-hour window between midnight
/// EDT and midnight EST during summer. For production pipelines, prefer
/// [`date_to_midnight_ns`] with an explicit date string.
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

/// Compute midnight ET (nanoseconds) for a specific date string (YYYYMMDD).
///
/// Matches the C++ `date_to_midnight_ns()` approach: compute the difference
/// in days from the reference date (2022-01-03), multiply by `NS_PER_DAY`.
///
/// This is the preferred method for computing RTH boundaries because it
/// avoids the bug where `midnight_et_ns(first_ts)` derives the wrong day
/// when the first event is from the previous evening's globex session.
///
/// **Note**: Does NOT account for DST transitions. Midnight ET is always
/// computed as UTC-5 (EST). This matches the C++ behavior for parity.
pub fn date_to_midnight_ns(date: &str) -> u64 {
    assert!(date.len() == 8, "date must be YYYYMMDD format, got: {}", date);
    let y: i32 = date[0..4].parse().expect("invalid year in date");
    let m: u32 = date[4..6].parse().expect("invalid month in date");
    let d: u32 = date[6..8].parse().expect("invalid day in date");

    let ref_jdn = julian_day_number(2022, 1, 3);
    let date_jdn = julian_day_number(y, m, d);
    let day_diff = (date_jdn - ref_jdn) as i64;

    (REF_MIDNIGHT_ET_NS as i64 + day_diff * NS_PER_DAY as i64) as u64
}

/// RTH open time (09:30 ET) for a specific date (YYYYMMDD).
pub fn rth_open_for_date(date: &str) -> u64 {
    date_to_midnight_ns(date) + 9 * NS_PER_HOUR + 30 * 60 * NS_PER_SEC
}

/// CME equity index futures (ES/MES) half-day early close schedule.
/// On these days, RTH closes at 13:15 ET (12:15 CT) instead of 16:00 ET.
///
/// Source: CME Group holiday calendar. For production, replace with
/// CME Reference Data API (Trading Schedules endpoint).
const CME_EQUITY_HALF_DAYS: &[&str] = &[
    "20221125", // Black Friday 2022
];

/// RTH close time for a specific date (YYYYMMDD).
///
/// Returns 13:15 ET for known CME equity half-days, 16:00 ET otherwise.
pub fn rth_close_for_date(date: &str) -> u64 {
    let midnight = date_to_midnight_ns(date);
    if CME_EQUITY_HALF_DAYS.contains(&date) {
        midnight + 13 * NS_PER_HOUR + 15 * 60 * NS_PER_SEC
    } else {
        midnight + 16 * NS_PER_HOUR
    }
}

/// Julian Day Number for a Gregorian calendar date.
///
/// Standard algorithm — returns an integer that increases by 1 for each
/// calendar day. Used only for computing day differences.
fn julian_day_number(y: i32, m: u32, d: u32) -> i32 {
    let a = (14 - m as i32) / 12;
    let y_adj = y + 4800 - a;
    let m_adj = m as i32 + 12 * a - 3;
    d as i32 + (153 * m_adj + 2) / 5 + 365 * y_adj + y_adj / 4 - y_adj / 100 + y_adj / 400 - 32045
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

    #[test]
    fn test_date_to_midnight_ns_reference_date() {
        // 2022-01-03 is the reference date — should equal REF_MIDNIGHT_ET_NS
        assert_eq!(date_to_midnight_ns("20220103"), REF_MIDNIGHT_ET_NS);
    }

    #[test]
    fn test_date_to_midnight_ns_next_day() {
        let next_day = date_to_midnight_ns("20220104");
        assert_eq!(next_day, REF_MIDNIGHT_ET_NS + NS_PER_DAY);
    }

    #[test]
    fn test_date_to_midnight_ns_prev_day() {
        let prev_day = date_to_midnight_ns("20220102");
        assert_eq!(prev_day, REF_MIDNIGHT_ET_NS - NS_PER_DAY);
    }

    #[test]
    fn test_date_to_midnight_matches_ts_based() {
        // For a timestamp during RTH (after midnight ET), both methods should agree
        let rth_ts = REF_MIDNIGHT_ET_NS + 10 * NS_PER_HOUR; // 10:00 ET Jan 3
        assert_eq!(midnight_et_ns(rth_ts), date_to_midnight_ns("20220103"));
    }

    #[test]
    fn test_rth_for_date() {
        let open = rth_open_for_date("20220103");
        let close = rth_close_for_date("20220103");
        assert_eq!(open, rth_open_ns(REF_MIDNIGHT_ET_NS));
        assert_eq!(close, rth_close_ns(REF_MIDNIGHT_ET_NS));
        assert_eq!(close - open, 6 * NS_PER_HOUR + 30 * 60 * NS_PER_SEC);
    }

    #[test]
    fn test_date_to_midnight_cross_month() {
        // Feb 1 vs Jan 31 should differ by exactly 1 day
        let jan31 = date_to_midnight_ns("20220131");
        let feb01 = date_to_midnight_ns("20220201");
        assert_eq!(feb01 - jan31, NS_PER_DAY);
    }

    #[test]
    fn test_date_to_midnight_leap_year() {
        // 2024 is a leap year: Feb 28 → Feb 29 → Mar 1
        let feb28 = date_to_midnight_ns("20240228");
        let feb29 = date_to_midnight_ns("20240229");
        let mar01 = date_to_midnight_ns("20240301");
        assert_eq!(feb29 - feb28, NS_PER_DAY);
        assert_eq!(mar01 - feb29, NS_PER_DAY);
    }

    #[test]
    fn test_half_day_close() {
        // Black Friday 2022: early close at 13:15 ET
        let close = rth_close_for_date("20221125");
        let open = rth_open_for_date("20221125");
        // 13:15 - 09:30 = 3h45m = 13,500 seconds = 2,700 bars at 5s
        assert_eq!(close - open, 3 * NS_PER_HOUR + 45 * 60 * NS_PER_SEC);
    }

    #[test]
    fn test_normal_day_close_unchanged() {
        // Normal day: 16:00 - 09:30 = 6.5h
        let close = rth_close_for_date("20220103");
        let open = rth_open_for_date("20220103");
        assert_eq!(close - open, 6 * NS_PER_HOUR + 30 * 60 * NS_PER_SEC);
    }
}
