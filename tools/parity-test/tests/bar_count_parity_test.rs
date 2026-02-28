//! Bar Count Parity Tests
//!
//! Spec: .kit/docs/bar-count-parity-fix.md
//!
//! These tests verify that the Rust pipeline produces the same bar count as the
//! C++ reference Parquet export. The RTH boundary filter must use a half-open
//! interval `[rth_open, rth_close)` to avoid emitting an extra snapshot at the
//! close boundary, which would produce a partial bar via flush.
//!
//! ## Expected Constants (from FEATURE_PARITY_SPEC.md)
//!
//! - RTH: 09:30:00 – 16:00:00 ET = 23,400 seconds
//! - Snapshot interval: 100ms
//! - Snapshots per bar: 50 (= 5 seconds)
//! - Total snapshots per RTH: 234,000
//! - Total bars per RTH: 4,680 (exactly, no remainder)
//! - Warmup bars: 50
//! - Non-warmup bars: 4,630
//!
//! ## Test Plan Coverage
//!
//!   T1: Bar count match              → Section 2 (#[ignore], real data)
//!   T2: First bar timestamp          → Section 3 (#[ignore], real data)
//!   T3: Last bar timestamp           → Section 3 (#[ignore], real data)
//!   T4: RTH boundary filter          → Section 4 (#[ignore], real data)
//!   T5: Total snapshot count         → Section 4 (#[ignore], real data)
//!   Invariants: RTH math constants   → Section 1 (pure, no data)

use parity_test::{load_reference_parquet, run_rust_pipeline, run_rust_pipeline_all_bars};

use common::book::SNAPSHOT_INTERVAL_NS;
use common::time_utils::{self, NS_PER_SEC, REF_MIDNIGHT_ET_NS};

use std::path::Path;

// ---------------------------------------------------------------------------
// Reference data paths (2022-01-03)
// ---------------------------------------------------------------------------
const REF_PARQUET: &str = "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet";
const DBN_FILE: &str = "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst";
const INSTRUMENT_ID: u32 = 11355; // MES contract

// ---------------------------------------------------------------------------
// Expected constants derived from the spec
// ---------------------------------------------------------------------------

/// RTH duration in seconds: 16:00 - 09:30 = 6.5 hours = 23,400s.
const EXPECTED_RTH_SECONDS: u64 = 23_400;

/// Total 100ms snapshots during RTH: 23,400s / 0.1s = 234,000.
const EXPECTED_SNAPSHOTS_PER_RTH: u64 = 234_000;

/// Snapshots per bar: 5s / 0.1s = 50.
const SNAPS_PER_BAR: u64 = 50;

/// Total bars per full RTH day: 234,000 / 50 = 4,680.
const EXPECTED_TOTAL_BARS: usize = 4_680;

/// Number of warmup bars skipped.
const WARMUP_BARS: usize = 50;

/// Expected non-warmup bars: 4,680 - 50 = 4,630.
const EXPECTED_NON_WARMUP_BARS: usize = EXPECTED_TOTAL_BARS - WARMUP_BARS;

// =====================================================================
// Section 1: RTH Math Invariants
//
// Pure computation tests that document the expected constants.
// These verify the mathematical relationships from the spec.
// No external data required.
// =====================================================================

#[test]
fn rth_duration_is_23400_seconds() {
    let rth_open = time_utils::rth_open_ns(REF_MIDNIGHT_ET_NS);
    let rth_close = time_utils::rth_close_ns(REF_MIDNIGHT_ET_NS);
    let duration_ns = rth_close - rth_open;
    let duration_s = duration_ns / NS_PER_SEC;

    assert_eq!(
        duration_s, EXPECTED_RTH_SECONDS,
        "RTH duration must be 23,400 seconds (6.5 hours), got {} seconds",
        duration_s,
    );
}

#[test]
fn snapshot_interval_divides_rth_evenly() {
    let rth_open = time_utils::rth_open_ns(REF_MIDNIGHT_ET_NS);
    let rth_close = time_utils::rth_close_ns(REF_MIDNIGHT_ET_NS);
    let duration_ns = rth_close - rth_open;
    let remainder = duration_ns % SNAPSHOT_INTERVAL_NS;

    assert_eq!(
        remainder, 0,
        "RTH duration ({} ns) must be evenly divisible by snapshot interval ({} ns), remainder = {}",
        duration_ns, SNAPSHOT_INTERVAL_NS, remainder,
    );
}

#[test]
fn snapshots_per_rth_is_234000() {
    let rth_open = time_utils::rth_open_ns(REF_MIDNIGHT_ET_NS);
    let rth_close = time_utils::rth_close_ns(REF_MIDNIGHT_ET_NS);
    let n_snapshots = (rth_close - rth_open) / SNAPSHOT_INTERVAL_NS;

    assert_eq!(
        n_snapshots, EXPECTED_SNAPSHOTS_PER_RTH,
        "RTH must produce exactly 234,000 snapshots at 100ms intervals, got {}",
        n_snapshots,
    );
}

#[test]
fn bars_per_rth_is_4680() {
    // 234,000 snapshots / 50 per bar = 4,680 bars with no remainder.
    assert_eq!(
        EXPECTED_SNAPSHOTS_PER_RTH % SNAPS_PER_BAR,
        0,
        "234,000 snapshots must divide evenly into 50-snapshot bars (remainder = {})",
        EXPECTED_SNAPSHOTS_PER_RTH % SNAPS_PER_BAR,
    );
    assert_eq!(
        (EXPECTED_SNAPSHOTS_PER_RTH / SNAPS_PER_BAR) as usize,
        EXPECTED_TOTAL_BARS,
        "234,000 / 50 = {} bars, expected {}",
        EXPECTED_SNAPSHOTS_PER_RTH / SNAPS_PER_BAR,
        EXPECTED_TOTAL_BARS,
    );
}

#[test]
fn non_warmup_bars_is_4630() {
    assert_eq!(
        EXPECTED_NON_WARMUP_BARS, 4630,
        "4,680 total - 50 warmup = 4,630 non-warmup bars",
    );
}

#[test]
fn half_open_interval_excludes_rth_close() {
    // The RTH filter is [rth_open, rth_close) — half-open interval.
    // The last valid snapshot timestamp is rth_close - SNAPSHOT_INTERVAL_NS.
    let rth_open = time_utils::rth_open_ns(REF_MIDNIGHT_ET_NS);
    let rth_close = time_utils::rth_close_ns(REF_MIDNIGHT_ET_NS);

    let last_valid_snapshot_ts = rth_close - SNAPSHOT_INTERVAL_NS;
    let first_snapshot_ts = rth_open;

    // Number of snapshots in [rth_open, rth_close) at 100ms intervals:
    // first = rth_open, last = rth_close - 100ms
    // count = (last - first) / interval + 1 = (rth_close - 100ms - rth_open) / 100ms + 1
    //       = (234,000 - 1) * 100ms / 100ms + 1 = 234,000
    let n_snapshots =
        (last_valid_snapshot_ts - first_snapshot_ts) / SNAPSHOT_INTERVAL_NS + 1;

    assert_eq!(
        n_snapshots, EXPECTED_SNAPSHOTS_PER_RTH,
        "Half-open [open, close) must yield exactly 234,000 snapshots, got {}",
        n_snapshots,
    );
}

#[test]
fn snapshot_at_rth_close_would_create_extra_bar() {
    // If we incorrectly use <= instead of < for rth_close, we get 234,001
    // snapshots. 234,001 / 50 = 4,680 full bars + 1 leftover snapshot,
    // which flush() emits as a partial bar → 4,681 total bars.
    let wrong_snapshot_count = EXPECTED_SNAPSHOTS_PER_RTH + 1; // 234,001
    let full_bars = wrong_snapshot_count / SNAPS_PER_BAR;
    let leftover = wrong_snapshot_count % SNAPS_PER_BAR;

    assert_eq!(full_bars, 4680, "234,001 / 50 = 4,680 full bars");
    assert_eq!(leftover, 1, "234,001 % 50 = 1 leftover snapshot");

    // The leftover triggers a flush → 4,681 total bars → 4,631 after warmup.
    let total_with_flush = full_bars as usize + 1; // partial bar from flush
    let non_warmup_wrong = total_with_flush - WARMUP_BARS;

    assert_eq!(
        non_warmup_wrong, 4631,
        "Off-by-one produces 4,631 non-warmup bars (the observed Rust bug)",
    );
}

// =====================================================================
// Section 2: Bar Count Match (T1)
//
// T1: Run Rust pipeline against 2022-01-03 .dbn.zst. Count total bars
// produced. Count reference Parquet rows. They must be equal.
// =====================================================================

#[test]
#[ignore] // requires real reference data
fn t1_bar_count_rust_equals_reference_2022_01_03() {
    let ref_path = Path::new(REF_PARQUET);
    let dbn_path = Path::new(DBN_FILE);

    let reference = load_reference_parquet(ref_path)
        .expect("Should load 2022-01-03 reference Parquet");
    let rust = run_rust_pipeline(dbn_path, INSTRUMENT_ID)
        .expect("Should process 2022-01-03 DBN through pipeline");

    assert_eq!(
        rust.len(),
        reference.len(),
        "Rust bar count ({}) must exactly match reference bar count ({}). \
         Difference of {} suggests an off-by-one in RTH boundary filtering.",
        rust.len(),
        reference.len(),
        (rust.len() as i64 - reference.len() as i64).abs(),
    );
}

#[test]
#[ignore] // requires real reference data
fn t1_non_warmup_bar_count_is_4630() {
    let dbn_path = Path::new(DBN_FILE);

    let rust = run_rust_pipeline(dbn_path, INSTRUMENT_ID)
        .expect("Should process 2022-01-03 DBN through pipeline");

    assert_eq!(
        rust.len(),
        EXPECTED_NON_WARMUP_BARS,
        "Rust pipeline must produce exactly {} non-warmup bars for a full RTH day \
         (234,000 snapshots / 50 per bar = 4,680 total - 50 warmup = 4,630). Got {}.",
        EXPECTED_NON_WARMUP_BARS,
        rust.len(),
    );
}

// =====================================================================
// Section 3: Bar Timestamp Validation (T2, T3)
//
// T2: The first non-warmup bar must have open_ts at RTH open + 250s
//     (= 50 warmup bars × 5s each = 09:34:10 ET).
//
// T3: The last bar must have close_ts strictly less than rth_close.
// =====================================================================

#[test]
#[ignore] // requires real data
fn t2_first_non_warmup_bar_opens_at_expected_timestamp() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars including warmup");

    assert!(
        all_bars.len() > WARMUP_BARS,
        "Must have more than {} bars to have non-warmup bars, got {}",
        WARMUP_BARS,
        all_bars.len(),
    );

    let first_non_warmup = &all_bars[WARMUP_BARS];

    // Expected: RTH open + 50 warmup bars × 50 snapshots × 100ms
    //         = rth_open + 50 × 5s = rth_open + 250s
    let rth_open = time_utils::rth_open_ns(REF_MIDNIGHT_ET_NS);
    let expected_open_ts = rth_open + (WARMUP_BARS as u64) * SNAPS_PER_BAR * SNAPSHOT_INTERVAL_NS;

    assert_eq!(
        first_non_warmup.open_ts, expected_open_ts,
        "First non-warmup bar (bar {}) must open at RTH + 250s = {} ns, got {} ns. \
         Delta = {} ns ({:.3} ms)",
        WARMUP_BARS,
        expected_open_ts,
        first_non_warmup.open_ts,
        (first_non_warmup.open_ts as i64 - expected_open_ts as i64).abs(),
        (first_non_warmup.open_ts as i64 - expected_open_ts as i64).abs() as f64 / 1e6,
    );
}

#[test]
#[ignore] // requires real data
fn t3_last_bar_closes_before_rth_close() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    assert!(
        !all_bars.is_empty(),
        "Pipeline must produce at least one bar",
    );

    let last_bar = all_bars.last().unwrap();
    let rth_close = time_utils::rth_close_ns(REF_MIDNIGHT_ET_NS);

    // The last bar's close_ts must be strictly less than rth_close.
    // Expected: rth_close - SNAPSHOT_INTERVAL_NS (the last 100ms snapshot
    // before the close boundary).
    assert!(
        last_bar.close_ts < rth_close,
        "Last bar close_ts ({}) must be strictly less than rth_close ({}). \
         Overrun by {} ns ({:.1} ms). This indicates snapshots were emitted \
         at or past the RTH close boundary.",
        last_bar.close_ts,
        rth_close,
        last_bar.close_ts.saturating_sub(rth_close),
        last_bar.close_ts.saturating_sub(rth_close) as f64 / 1e6,
    );

    // More precise: last bar's close_ts should be exactly rth_close - 100ms
    let expected_last_close = rth_close - SNAPSHOT_INTERVAL_NS;
    assert_eq!(
        last_bar.close_ts, expected_last_close,
        "Last bar close_ts should be exactly {} ns (rth_close - 100ms), got {} ns",
        expected_last_close,
        last_bar.close_ts,
    );
}

// =====================================================================
// Section 4: RTH Boundary Enforcement (T4) & Snapshot Count (T5)
//
// T4: No snapshots are emitted with timestamps >= rth_close.
//     Tested indirectly: no bar should have any timestamp >= rth_close.
//
// T5: Total snapshot count across all bars == 234,000.
//     Since 234,000 / 50 = 4,680 exactly, there should be NO partial
//     bar from flush().
// =====================================================================

#[test]
#[ignore] // requires real data
fn t4_no_bar_timestamp_at_or_past_rth_close() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    let rth_close = time_utils::rth_close_ns(REF_MIDNIGHT_ET_NS);

    for (i, bar) in all_bars.iter().enumerate() {
        assert!(
            bar.open_ts < rth_close,
            "Bar {} open_ts ({}) must be < rth_close ({}). \
             A bar opening at or after RTH close means snapshots leaked past the boundary.",
            i, bar.open_ts, rth_close,
        );
        assert!(
            bar.close_ts < rth_close,
            "Bar {} close_ts ({}) must be < rth_close ({}). \
             A bar closing at or after RTH close means the half-open interval \
             [open, close) is not enforced.",
            i, bar.close_ts, rth_close,
        );
    }
}

#[test]
#[ignore] // requires real data
fn t5_total_snapshot_count_is_234000() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    let total_snapshots: u64 = all_bars
        .iter()
        .map(|bar| bar.snapshot_count as u64)
        .sum();

    assert_eq!(
        total_snapshots, EXPECTED_SNAPSHOTS_PER_RTH,
        "Total snapshot count must be exactly {} (= 23,400s / 100ms). Got {}. \
         Delta = {} snapshots. If {} > {}, the RTH boundary is too loose (>= vs <).",
        EXPECTED_SNAPSHOTS_PER_RTH,
        total_snapshots,
        (total_snapshots as i64 - EXPECTED_SNAPSHOTS_PER_RTH as i64).abs(),
        total_snapshots,
        EXPECTED_SNAPSHOTS_PER_RTH,
    );
}

#[test]
#[ignore] // requires real data
fn t5_no_partial_bar_at_end_of_session() {
    // Since 234,000 / 50 = 4,680 exactly, there should be no partial bar.
    // A partial bar (snapshot_count < 50) at the end means too many or too
    // few snapshots were emitted.
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    let last_bar = all_bars.last().expect("Must have at least one bar");

    assert_eq!(
        last_bar.snapshot_count, SNAPS_PER_BAR as u32,
        "Last bar should have exactly {} snapshots (full bar). Got {} snapshots, \
         indicating a partial flush. This suggests the snapshot count is not \
         exactly 234,000 (off by {} snapshots).",
        SNAPS_PER_BAR,
        last_bar.snapshot_count,
        (last_bar.snapshot_count as i64 - SNAPS_PER_BAR as i64).abs(),
    );
}

#[test]
#[ignore] // requires real data
fn t5_total_bar_count_including_warmup_is_4680() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    assert_eq!(
        all_bars.len(),
        EXPECTED_TOTAL_BARS,
        "Total bar count (including warmup) must be exactly {} \
         (234,000 snapshots / 50 per bar). Got {}. \
         If {}, the off-by-one in RTH boundary adds an extra flush bar.",
        EXPECTED_TOTAL_BARS,
        all_bars.len(),
        if all_bars.len() > EXPECTED_TOTAL_BARS { "greater" } else { "less" },
    );
}

// =====================================================================
// Section 5: Cross-Validation
//
// Additional tests that cross-check the pipeline output against
// expected properties. These use the existing API where possible.
// =====================================================================

#[test]
#[ignore] // requires real data
fn all_bars_have_50_snapshots_except_possibly_last() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    // All bars except possibly the last must have exactly 50 snapshots.
    // (After the fix, even the last bar should have 50.)
    for (i, bar) in all_bars.iter().enumerate() {
        if i < all_bars.len() - 1 {
            assert_eq!(
                bar.snapshot_count, SNAPS_PER_BAR as u32,
                "Bar {} must have exactly {} snapshots, got {}. \
                 Non-final bars with fewer snapshots indicate a bar builder bug.",
                i, SNAPS_PER_BAR, bar.snapshot_count,
            );
        }
    }
}

#[test]
#[ignore] // requires real data
fn bar_timestamps_are_monotonically_increasing() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    for i in 1..all_bars.len() {
        assert!(
            all_bars[i].open_ts > all_bars[i - 1].open_ts,
            "Bar {} open_ts ({}) must be > bar {} open_ts ({})",
            i, all_bars[i].open_ts, i - 1, all_bars[i - 1].open_ts,
        );
        assert!(
            all_bars[i].open_ts >= all_bars[i - 1].close_ts,
            "Bar {} open_ts ({}) must be >= bar {} close_ts ({}) (no overlap)",
            i, all_bars[i].open_ts, i - 1, all_bars[i - 1].close_ts,
        );
    }
}

#[test]
#[ignore] // requires real data
fn first_bar_opens_at_rth_open() {
    let dbn_path = Path::new(DBN_FILE);

    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should return all bars");

    let rth_open = time_utils::rth_open_ns(REF_MIDNIGHT_ET_NS);

    assert_eq!(
        all_bars[0].open_ts, rth_open,
        "First bar (bar 0, warmup) must open at RTH open ({} ns), got {} ns",
        rth_open,
        all_bars[0].open_ts,
    );
}

#[test]
#[ignore] // requires real data
fn pipeline_features_and_bars_agree_on_count() {
    // Cross-check: run_rust_pipeline (features) and run_rust_pipeline_all_bars
    // must agree on the number of non-warmup bars.
    let dbn_path = Path::new(DBN_FILE);

    let features = run_rust_pipeline(dbn_path, INSTRUMENT_ID)
        .expect("Should produce features");
    let all_bars = run_rust_pipeline_all_bars(dbn_path, INSTRUMENT_ID)
        .expect("Should produce bars");

    let non_warmup_bar_count = all_bars.len().saturating_sub(WARMUP_BARS);

    assert_eq!(
        features.len(),
        non_warmup_bar_count,
        "Feature count ({}) must equal non-warmup bar count ({} total - {} warmup = {})",
        features.len(),
        all_bars.len(),
        WARMUP_BARS,
        non_warmup_bar_count,
    );
}
