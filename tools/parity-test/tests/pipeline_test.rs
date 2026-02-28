//! Parity Test Pipeline Wiring — RED Phase Test Suite
//!
//! Spec: .kit/docs/parity-test-pipeline.md
//!
//! These tests MUST FAIL (RED) because the `parity_test` crate has no lib target
//! and the pipeline functions don't exist yet.
//!
//! The GREEN phase must:
//!   1. Create `tools/parity-test/src/lib.rs` exposing the types and functions below
//!   2. Add deps to Cargo.toml: parquet, arrow, features, bars, book-builder,
//!      databento-ingest, common, dbn, anyhow
//!   3. Implement all functions to make these tests pass
//!
//! ## Test Plan Coverage
//!
//!   T1: Parquet loading            → Section 1
//!   T2: Pipeline execution         → Section 2
//!   T3: Day matching               → Section 3
//!   T4: Comparison logic           → Section 4
//!   T5: Single-day end-to-end      → Section 5 (#[ignore])
//!
//! ## Expected Function Signatures (for GREEN phase)
//!
//! ```ignore
//! pub const FEATURE_NAMES: [&str; 20] = [ ... ];
//!
//! pub struct DayPair {
//!     pub date: String,               // "20220103"
//!     pub reference_path: PathBuf,    // .../2022-01-03.parquet
//!     pub dbn_path: PathBuf,          // .../glbx-mdp3-20220103.mbo.dbn.zst
//! }
//!
//! pub struct FeatureDeviation {
//!     pub name: String,
//!     pub max_dev: f64,
//!     pub mean_dev: f64,
//!     pub passed: bool,
//!     pub worst_bar: Option<usize>,
//!     pub worst_rust_val: Option<f64>,
//!     pub worst_ref_val: Option<f64>,
//! }
//!
//! pub struct ComparisonResult {
//!     pub passed: bool,
//!     pub bar_count_rust: usize,
//!     pub bar_count_ref: usize,
//!     pub per_feature: Vec<FeatureDeviation>,
//! }
//!
//! pub fn load_reference_parquet(path: &Path) -> Result<Vec<[f64; 20]>>;
//! pub fn run_rust_pipeline(dbn_path: &Path, instrument_id: u32) -> Result<Vec<[f64; 20]>>;
//! pub fn match_day_files(ref_dir: &Path, data_dir: &Path) -> Result<Vec<DayPair>>;
//! pub fn compare_features(
//!     rust: &[[f64; 20]], reference: &[[f64; 20]], tolerance: f64,
//! ) -> ComparisonResult;
//! ```

// ---------------------------------------------------------------------------
// These imports will fail to compile (RED) — parity_test has no lib.rs yet.
// ---------------------------------------------------------------------------
use parity_test::{
    compare_features, load_reference_parquet, match_day_files, run_rust_pipeline,
    ComparisonResult, DayPair, FeatureDeviation, FEATURE_NAMES,
};

use std::path::{Path, PathBuf};

// =====================================================================
// Section 0: Feature Name Constant Contract
// =====================================================================

/// The 20 feature column names from the spec, in canonical order.
const SPEC_FEATURE_NAMES: [&str; 20] = [
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

#[test]
fn feature_names_constant_has_20_entries() {
    assert_eq!(
        FEATURE_NAMES.len(),
        20,
        "FEATURE_NAMES must contain exactly 20 feature names",
    );
}

#[test]
fn feature_names_match_spec_order() {
    for (i, &expected) in SPEC_FEATURE_NAMES.iter().enumerate() {
        assert_eq!(
            FEATURE_NAMES[i], expected,
            "FEATURE_NAMES[{}] must be '{}', got '{}'",
            i, expected, FEATURE_NAMES[i],
        );
    }
}

// =====================================================================
// Section 1: Parquet Loading (T1)
//
// load_reference_parquet(path) must:
//   - Read a Parquet file using the parquet/arrow crates
//   - Extract the 20 model feature columns by name
//   - Extract bar_index and is_warmup for alignment
//   - Skip warmup bars (is_warmup == true or first 50)
//   - Return Vec<[f64; 20]> of reference feature vectors
// =====================================================================

#[test]
fn load_reference_parquet_returns_feature_vectors() {
    // A valid reference Parquet file with 100 bars (50 warmup + 50 real)
    // should return exactly 50 feature vectors (warmup skipped).
    let path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet",
    );
    if !path.exists() {
        // Skip if reference data not available locally
        eprintln!("SKIP: reference Parquet not found at {}", path.display());
        return;
    }
    let features = load_reference_parquet(path).expect("Should load valid Parquet file");
    assert!(
        !features.is_empty(),
        "Reference Parquet must contain at least one non-warmup bar",
    );
}

#[test]
fn load_reference_parquet_each_row_has_20_features() {
    let path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet",
    );
    if !path.exists() {
        eprintln!("SKIP: reference Parquet not found");
        return;
    }
    let features = load_reference_parquet(path).unwrap();
    // Each row is [f64; 20] — the type system enforces this,
    // but verify we can index all 20 features.
    for (bar_idx, row) in features.iter().enumerate() {
        for feat_idx in 0..20 {
            assert!(
                row[feat_idx].is_finite(),
                "Reference feature [bar={}, feat={}] ('{}') must be finite, got {}",
                bar_idx,
                feat_idx,
                SPEC_FEATURE_NAMES[feat_idx],
                row[feat_idx],
            );
        }
    }
}

#[test]
fn load_reference_parquet_skips_warmup_bars() {
    // When loading reference data, warmup bars (is_warmup == true or first 50)
    // must be excluded. The returned Vec should only contain non-warmup bars.
    let path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet",
    );
    if !path.exists() {
        eprintln!("SKIP: reference Parquet not found");
        return;
    }
    let features = load_reference_parquet(path).unwrap();
    // A typical trading day has ~4600+ total bars, ~50 warmup.
    // So non-warmup should be > 4000 bars.
    assert!(
        features.len() > 4000,
        "2022-01-03 should have > 4000 non-warmup bars, got {}",
        features.len(),
    );
}

#[test]
fn load_reference_parquet_error_on_nonexistent_file() {
    let result = load_reference_parquet(Path::new("/nonexistent/path/2022-01-03.parquet"));
    assert!(
        result.is_err(),
        "Loading nonexistent Parquet file must return Err",
    );
}

#[test]
fn load_reference_parquet_error_message_is_descriptive() {
    let result = load_reference_parquet(Path::new("/tmp/empty_test_file.parquet"));
    // Should return an error with a descriptive message (not panic).
    assert!(
        result.is_err(),
        "Loading invalid Parquet file must return Err, not panic",
    );
}

// =====================================================================
// Section 2: Pipeline Execution (T2)
//
// run_rust_pipeline(dbn_path, instrument_id) must:
//   - Read .dbn.zst file via databento-ingest
//   - Build book via book-builder → 100ms snapshots
//   - Build 5-second time bars (50 snapshots per bar)
//   - Compute features via BarFeatureComputer
//   - Reassign MBO events to bars (post-hoc)
//   - Skip warmup bars (first 50)
//   - Return Vec<[f64; 20]> of computed feature vectors
// =====================================================================

#[test]
fn run_rust_pipeline_produces_non_empty_result() {
    let dbn_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst",
    );
    if !dbn_path.exists() {
        eprintln!("SKIP: DBN file not found at {}", dbn_path.display());
        return;
    }
    let features =
        run_rust_pipeline(dbn_path, 11355).expect("Pipeline should process valid DBN file");
    assert!(
        !features.is_empty(),
        "Pipeline must produce at least one non-warmup bar from a real trading day",
    );
}

#[test]
fn run_rust_pipeline_features_are_finite_after_warmup() {
    let dbn_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst",
    );
    if !dbn_path.exists() {
        eprintln!("SKIP: DBN file not found");
        return;
    }
    let features = run_rust_pipeline(dbn_path, 11355).unwrap();
    // All 20 features of every non-warmup bar must be finite (not NaN, not Inf).
    for (bar_idx, row) in features.iter().enumerate() {
        for (feat_idx, &val) in row.iter().enumerate() {
            assert!(
                val.is_finite(),
                "Feature [bar={}, feat={}] ('{}') must be finite after warmup, got {}",
                bar_idx,
                feat_idx,
                SPEC_FEATURE_NAMES[feat_idx],
                val,
            );
        }
    }
}

#[test]
fn run_rust_pipeline_error_on_nonexistent_file() {
    let result = run_rust_pipeline(Path::new("/nonexistent/data.dbn.zst"), 11355);
    assert!(
        result.is_err(),
        "Pipeline must return Err for nonexistent DBN file",
    );
}

#[test]
fn run_rust_pipeline_uses_time_bars_5_second() {
    // The spec requires 5-second time bars (50 snapshots at 100ms intervals).
    // A full trading day (~6.5 hours) should produce ~4680 total bars.
    // After skipping 50 warmup: ~4630 non-warmup bars.
    let dbn_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst",
    );
    if !dbn_path.exists() {
        eprintln!("SKIP: DBN file not found");
        return;
    }
    let features = run_rust_pipeline(dbn_path, 11355).unwrap();
    // Sanity: a 6.5-hour trading day at 5s bars ≈ 4680 bars total, minus 50 warmup.
    assert!(
        features.len() > 4000 && features.len() < 6000,
        "5-second bars for a full day should produce 4000-6000 non-warmup bars, got {}",
        features.len(),
    );
}

// =====================================================================
// Section 3: Day Matching (T3)
//
// match_day_files(ref_dir, data_dir) must:
//   - Scan ref_dir for files named YYYY-MM-DD.parquet
//   - Scan data_dir for files named glbx-mdp3-YYYYMMDD.mbo.dbn.zst
//   - Match by date: 2022-01-03.parquet ↔ glbx-mdp3-20220103.mbo.dbn.zst
//   - Return Vec<DayPair> sorted by date
// =====================================================================

#[test]
fn match_day_files_pairs_parquet_to_dbn() {
    // Create temp dirs with known filenames and verify matching.
    let tmp = std::env::temp_dir().join("parity_test_t3_match");
    let ref_dir = tmp.join("reference");
    let data_dir = tmp.join("data");
    let _ = std::fs::create_dir_all(&ref_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    // Create dummy files
    std::fs::write(ref_dir.join("2022-01-03.parquet"), b"dummy").unwrap();
    std::fs::write(
        data_dir.join("glbx-mdp3-20220103.mbo.dbn.zst"),
        b"dummy",
    )
    .unwrap();

    let pairs = match_day_files(&ref_dir, &data_dir).expect("Should find matching day files");
    assert_eq!(pairs.len(), 1, "Should find exactly one matching pair");
    assert_eq!(pairs[0].date, "20220103");
    assert_eq!(
        pairs[0].reference_path,
        ref_dir.join("2022-01-03.parquet"),
    );
    assert_eq!(
        pairs[0].dbn_path,
        data_dir.join("glbx-mdp3-20220103.mbo.dbn.zst"),
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn match_day_files_multiple_days() {
    let tmp = std::env::temp_dir().join("parity_test_t3_multi");
    let ref_dir = tmp.join("reference");
    let data_dir = tmp.join("data");
    let _ = std::fs::create_dir_all(&ref_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    // Three trading days
    for (parquet_name, dbn_name) in &[
        ("2022-01-03.parquet", "glbx-mdp3-20220103.mbo.dbn.zst"),
        ("2022-01-04.parquet", "glbx-mdp3-20220104.mbo.dbn.zst"),
        ("2022-01-05.parquet", "glbx-mdp3-20220105.mbo.dbn.zst"),
    ] {
        std::fs::write(ref_dir.join(parquet_name), b"dummy").unwrap();
        std::fs::write(data_dir.join(dbn_name), b"dummy").unwrap();
    }

    let pairs = match_day_files(&ref_dir, &data_dir).unwrap();
    assert_eq!(pairs.len(), 3, "Should find three matching pairs");

    // Verify sorted by date
    assert_eq!(pairs[0].date, "20220103");
    assert_eq!(pairs[1].date, "20220104");
    assert_eq!(pairs[2].date, "20220105");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn match_day_files_unmatched_files_ignored() {
    let tmp = std::env::temp_dir().join("parity_test_t3_unmatched");
    let ref_dir = tmp.join("reference");
    let data_dir = tmp.join("data");
    let _ = std::fs::create_dir_all(&ref_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    // Reference has day 3 and 4, data only has day 3
    std::fs::write(ref_dir.join("2022-01-03.parquet"), b"dummy").unwrap();
    std::fs::write(ref_dir.join("2022-01-04.parquet"), b"dummy").unwrap();
    std::fs::write(
        data_dir.join("glbx-mdp3-20220103.mbo.dbn.zst"),
        b"dummy",
    )
    .unwrap();

    let pairs = match_day_files(&ref_dir, &data_dir).unwrap();
    assert_eq!(
        pairs.len(),
        1,
        "Only day 3 has both reference and data; day 4 should be excluded",
    );
    assert_eq!(pairs[0].date, "20220103");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn match_day_files_no_matches_returns_empty() {
    let tmp = std::env::temp_dir().join("parity_test_t3_empty");
    let ref_dir = tmp.join("reference");
    let data_dir = tmp.join("data");
    let _ = std::fs::create_dir_all(&ref_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    // Reference has day 3, data has day 4 — no overlap
    std::fs::write(ref_dir.join("2022-01-03.parquet"), b"dummy").unwrap();
    std::fs::write(
        data_dir.join("glbx-mdp3-20220104.mbo.dbn.zst"),
        b"dummy",
    )
    .unwrap();

    let pairs = match_day_files(&ref_dir, &data_dir).unwrap();
    assert!(
        pairs.is_empty(),
        "No matching dates should produce empty Vec, got {} pairs",
        pairs.len(),
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn match_day_files_ignores_non_parquet_and_non_dbn_files() {
    let tmp = std::env::temp_dir().join("parity_test_t3_junk");
    let ref_dir = tmp.join("reference");
    let data_dir = tmp.join("data");
    let _ = std::fs::create_dir_all(&ref_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    // Valid pair
    std::fs::write(ref_dir.join("2022-01-03.parquet"), b"dummy").unwrap();
    std::fs::write(
        data_dir.join("glbx-mdp3-20220103.mbo.dbn.zst"),
        b"dummy",
    )
    .unwrap();

    // Junk files that should be ignored
    std::fs::write(ref_dir.join("README.md"), b"ignore me").unwrap();
    std::fs::write(ref_dir.join("notes.txt"), b"ignore me").unwrap();
    std::fs::write(data_dir.join("config.toml"), b"ignore me").unwrap();

    let pairs = match_day_files(&ref_dir, &data_dir).unwrap();
    assert_eq!(
        pairs.len(),
        1,
        "Non-parquet and non-dbn files should be ignored",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn match_day_files_date_format_conversion() {
    // Parquet uses YYYY-MM-DD, DBN uses YYYYMMDD.
    // Verify the conversion handles all months correctly.
    let tmp = std::env::temp_dir().join("parity_test_t3_datefmt");
    let ref_dir = tmp.join("reference");
    let data_dir = tmp.join("data");
    let _ = std::fs::create_dir_all(&ref_dir);
    let _ = std::fs::create_dir_all(&data_dir);

    // December date to test two-digit month/day
    std::fs::write(ref_dir.join("2022-12-30.parquet"), b"dummy").unwrap();
    std::fs::write(
        data_dir.join("glbx-mdp3-20221230.mbo.dbn.zst"),
        b"dummy",
    )
    .unwrap();

    let pairs = match_day_files(&ref_dir, &data_dir).unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].date, "20221230");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn match_day_files_error_on_nonexistent_ref_dir() {
    let result = match_day_files(
        Path::new("/nonexistent/reference"),
        Path::new("/tmp"),
    );
    assert!(
        result.is_err(),
        "Nonexistent reference directory must return Err",
    );
}

#[test]
fn match_day_files_error_on_nonexistent_data_dir() {
    let result = match_day_files(
        Path::new("/tmp"),
        Path::new("/nonexistent/data"),
    );
    assert!(
        result.is_err(),
        "Nonexistent data directory must return Err",
    );
}

// =====================================================================
// Section 4: Comparison Logic (T4)
//
// compare_features(rust, reference, tolerance) must:
//   - Compare all 20 features bar-by-bar
//   - Compute absolute deviation: |rust_value - cpp_value|
//   - Track per-feature max deviation, mean deviation
//   - Flag any deviation > tolerance
//   - Report passed/failed status
//   - Identify worst bar index for each failing feature
// =====================================================================

#[test]
fn compare_identical_features_all_pass() {
    let features = vec![
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0,
         11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0],
    ];
    let result = compare_features(&features, &features, 1e-5);

    assert!(
        result.passed,
        "Identical features must produce PASS result",
    );
    assert_eq!(result.bar_count_rust, 1);
    assert_eq!(result.bar_count_ref, 1);

    for feat_dev in &result.per_feature {
        assert!(
            feat_dev.passed,
            "Feature '{}' must pass with identical inputs, max_dev={}",
            feat_dev.name, feat_dev.max_dev,
        );
        assert!(
            feat_dev.max_dev < 1e-15,
            "Feature '{}' max_dev should be ~0 for identical inputs, got {}",
            feat_dev.name, feat_dev.max_dev,
        );
        assert!(
            feat_dev.mean_dev < 1e-15,
            "Feature '{}' mean_dev should be ~0 for identical inputs, got {}",
            feat_dev.name, feat_dev.mean_dev,
        );
    }
}

#[test]
fn compare_identical_features_max_dev_is_zero() {
    let features = vec![
        [0.5; 20],
        [1.5; 20],
        [2.5; 20],
    ];
    let result = compare_features(&features, &features, 1e-5);
    assert!(result.passed);
    for feat_dev in &result.per_feature {
        assert_eq!(
            feat_dev.max_dev, 0.0,
            "Identical features must have exactly 0 max deviation",
        );
    }
}

#[test]
fn compare_known_deviation_reports_correct_max_dev() {
    let rust = vec![
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0,
         11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0],
    ];
    let mut reference = rust.clone();
    // Introduce known deviation in feature 0 (weighted_imbalance)
    reference[0][0] = 1.0 + 1e-7; // deviation = 1e-7 (within default tolerance)

    let result = compare_features(&rust, &reference, 1e-5);
    assert!(
        result.passed,
        "Deviation of 1e-7 should pass with tolerance 1e-5",
    );

    // Feature 0 should have max_dev ≈ 1e-7
    assert!(
        (result.per_feature[0].max_dev - 1e-7).abs() < 1e-12,
        "Feature 0 max_dev should be 1e-7, got {}",
        result.per_feature[0].max_dev,
    );
}

#[test]
fn compare_known_deviation_reports_correct_mean_dev() {
    let rust = vec![
        [1.0; 20],
        [2.0; 20],
    ];
    let mut reference = rust.clone();
    // Bar 0 feature 0: deviation = 0.1
    // Bar 1 feature 0: deviation = 0.3
    reference[0][0] = 1.1;
    reference[1][0] = 2.3;

    let result = compare_features(&rust, &reference, 1.0);

    // max_dev for feature 0 = max(0.1, 0.3) = 0.3
    assert!(
        (result.per_feature[0].max_dev - 0.3).abs() < 1e-12,
        "Feature 0 max_dev should be 0.3, got {}",
        result.per_feature[0].max_dev,
    );
    // mean_dev for feature 0 = (0.1 + 0.3) / 2 = 0.2
    assert!(
        (result.per_feature[0].mean_dev - 0.2).abs() < 1e-12,
        "Feature 0 mean_dev should be 0.2, got {}",
        result.per_feature[0].mean_dev,
    );
}

#[test]
fn compare_single_feature_above_tolerance_fails() {
    let rust = vec![[0.0; 20]];
    let mut reference = vec![[0.0; 20]];
    // Feature 6 (vwap_distance) exceeds tolerance
    reference[0][6] = 2e-5; // deviation = 2e-5 > 1e-5

    let result = compare_features(&rust, &reference, 1e-5);

    assert!(
        !result.passed,
        "Any feature exceeding tolerance must cause overall FAIL",
    );
    // Feature 6 should fail
    assert!(
        !result.per_feature[6].passed,
        "Feature 6 ('vwap_distance') should FAIL with deviation 2e-5 > tolerance 1e-5",
    );
    // Other features should still pass
    for (i, feat_dev) in result.per_feature.iter().enumerate() {
        if i != 6 {
            assert!(
                feat_dev.passed,
                "Feature {} ('{}') should PASS (deviation = 0), but reported FAIL",
                i, feat_dev.name,
            );
        }
    }
}

#[test]
fn compare_all_features_above_tolerance_all_fail() {
    let rust = vec![[0.0; 20]];
    let mut reference = vec![[0.0; 20]];
    // Every feature exceeds tolerance
    for i in 0..20 {
        reference[0][i] = 1e-4; // deviation = 1e-4 >> 1e-5
    }

    let result = compare_features(&rust, &reference, 1e-5);

    assert!(!result.passed, "All-exceeding must FAIL overall");
    for (i, feat_dev) in result.per_feature.iter().enumerate() {
        assert!(
            !feat_dev.passed,
            "Feature {} should FAIL when deviation exceeds tolerance",
            i,
        );
    }
}

#[test]
fn compare_reports_per_feature_has_20_entries() {
    let rust = vec![[0.0; 20]];
    let reference = vec![[0.0; 20]];
    let result = compare_features(&rust, &reference, 1e-5);
    assert_eq!(
        result.per_feature.len(),
        20,
        "ComparisonResult must report all 20 features",
    );
}

#[test]
fn compare_per_feature_names_match_spec() {
    let rust = vec![[0.0; 20]];
    let reference = vec![[0.0; 20]];
    let result = compare_features(&rust, &reference, 1e-5);
    for (i, feat_dev) in result.per_feature.iter().enumerate() {
        assert_eq!(
            feat_dev.name, SPEC_FEATURE_NAMES[i],
            "per_feature[{}].name should be '{}', got '{}'",
            i, SPEC_FEATURE_NAMES[i], feat_dev.name,
        );
    }
}

#[test]
fn compare_reports_worst_bar_index_on_failure() {
    let rust = vec![
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
    ];
    let mut reference = vec![
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
    ];
    // Bar 2, feature 10 (volatility_20) has largest deviation
    reference[2][10] = 5e-3;

    let result = compare_features(&rust, &reference, 1e-5);
    assert!(!result.passed);

    let vol20 = &result.per_feature[10];
    assert!(!vol20.passed);
    assert_eq!(
        vol20.worst_bar,
        Some(2),
        "Worst bar for volatility_20 should be bar 2, got {:?}",
        vol20.worst_bar,
    );
}

#[test]
fn compare_reports_worst_values_on_failure() {
    let rust = vec![[1.0; 20]];
    let mut reference = vec![[1.0; 20]];
    reference[0][3] = 1.5; // deviation = 0.5 for feature 3

    let result = compare_features(&rust, &reference, 1e-5);
    let feat3 = &result.per_feature[3];
    assert!(!feat3.passed);

    assert_eq!(
        feat3.worst_rust_val,
        Some(1.0),
        "Should report the Rust value at the worst bar",
    );
    assert_eq!(
        feat3.worst_ref_val,
        Some(1.5),
        "Should report the reference value at the worst bar",
    );
}

#[test]
fn compare_empty_vectors_passes() {
    let rust: Vec<[f64; 20]> = vec![];
    let reference: Vec<[f64; 20]> = vec![];
    let result = compare_features(&rust, &reference, 1e-5);
    assert!(
        result.passed,
        "Empty input (no bars) should vacuously pass",
    );
    assert_eq!(result.bar_count_rust, 0);
    assert_eq!(result.bar_count_ref, 0);
}

#[test]
fn compare_bar_count_mismatch_reported() {
    let rust = vec![[0.0; 20], [0.0; 20]]; // 2 bars
    let reference = vec![[0.0; 20]]; // 1 bar

    let result = compare_features(&rust, &reference, 1e-5);
    assert_eq!(result.bar_count_rust, 2);
    assert_eq!(result.bar_count_ref, 1);
    // Bar count mismatch should be reported (may or may not cause overall FAIL,
    // but the counts must be tracked for the summary output).
    assert_ne!(
        result.bar_count_rust, result.bar_count_ref,
        "Sanity: bar counts should differ in this test",
    );
}

#[test]
fn compare_deviation_at_exact_tolerance_passes() {
    let rust = vec![[0.0; 20]];
    let mut reference = vec![[0.0; 20]];
    // Deviation exactly at tolerance boundary
    reference[0][0] = 1e-5;

    let result = compare_features(&rust, &reference, 1e-5);
    // Spec: "deviation > tolerance" is FAIL, so exactly at tolerance should PASS.
    assert!(
        result.per_feature[0].passed,
        "Deviation exactly at tolerance (not strictly greater) should PASS",
    );
}

#[test]
fn compare_deviation_just_above_tolerance_fails() {
    let rust = vec![[0.0; 20]];
    let mut reference = vec![[0.0; 20]];
    // Deviation barely above tolerance
    reference[0][0] = 1e-5 + 1e-12;

    let result = compare_features(&rust, &reference, 1e-5);
    assert!(
        !result.per_feature[0].passed,
        "Deviation strictly above tolerance must FAIL",
    );
}

#[test]
fn compare_multiple_bars_tracks_worst_across_all() {
    let rust = vec![
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
    ];
    let mut reference = vec![
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
        [0.0; 20],
    ];
    // Scatter small deviations across bars for feature 0
    reference[0][0] = 1e-8;
    reference[1][0] = 5e-8;
    reference[2][0] = 3e-6; // This is the max
    reference[3][0] = 1e-7;
    reference[4][0] = 2e-8;

    let result = compare_features(&rust, &reference, 1e-5);
    let feat0 = &result.per_feature[0];
    assert!(
        (feat0.max_dev - 3e-6).abs() < 1e-12,
        "max_dev should be 3e-6 (from bar 2), got {}",
        feat0.max_dev,
    );
    // mean_dev = (1e-8 + 5e-8 + 3e-6 + 1e-7 + 2e-8) / 5
    let expected_mean = (1e-8 + 5e-8 + 3e-6 + 1e-7 + 2e-8) / 5.0;
    assert!(
        (feat0.mean_dev - expected_mean).abs() < 1e-15,
        "mean_dev should be {}, got {}",
        expected_mean, feat0.mean_dev,
    );
}

#[test]
fn compare_custom_tolerance_respected() {
    let rust = vec![[0.0; 20]];
    let mut reference = vec![[0.0; 20]];
    reference[0][0] = 0.1; // deviation = 0.1

    // With tight tolerance, this should fail
    let result_tight = compare_features(&rust, &reference, 1e-5);
    assert!(!result_tight.per_feature[0].passed);

    // With loose tolerance, this should pass
    let result_loose = compare_features(&rust, &reference, 1.0);
    assert!(result_loose.per_feature[0].passed);
}

// =====================================================================
// Section 5: Single-Day End-to-End (T5)
//
// Process 2022-01-03 through both pipelines and compare.
// This is the real integration test — #[ignore] for CI.
// =====================================================================

#[test]
#[ignore]
fn single_day_2022_01_03_end_to_end() {
    let ref_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet",
    );
    let dbn_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst",
    );

    let reference = load_reference_parquet(ref_path)
        .expect("Should load 2022-01-03 reference Parquet");
    let rust = run_rust_pipeline(dbn_path, 11355)
        .expect("Should process 2022-01-03 DBN through pipeline");

    // Bar count must match (spec exit criterion)
    assert_eq!(
        rust.len(),
        reference.len(),
        "Bar count mismatch: Rust produced {} bars, reference has {} bars",
        rust.len(),
        reference.len(),
    );

    let result = compare_features(&rust, &reference, 1e-5);

    // Report per-feature deviations for debugging
    for feat_dev in &result.per_feature {
        eprintln!(
            "  {:<25} max_dev={:.2e}  {}",
            feat_dev.name,
            feat_dev.max_dev,
            if feat_dev.passed { "PASS" } else { "FAIL" },
        );
    }

    assert!(
        result.passed,
        "All 20 features must be within 1e-5 tolerance for 2022-01-03",
    );
}

#[test]
#[ignore]
fn single_day_2022_01_03_bar_count_matches_reference() {
    let ref_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet",
    );
    let dbn_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst",
    );

    let reference = load_reference_parquet(ref_path).unwrap();
    let rust = run_rust_pipeline(dbn_path, 11355).unwrap();

    // Spec: "Successfully processes at least 1 real day (2022-01-03) with
    // bar count match to reference"
    assert_eq!(
        rust.len(),
        reference.len(),
        "Rust bar count ({}) must match reference bar count ({})",
        rust.len(),
        reference.len(),
    );
}

#[test]
#[ignore]
fn single_day_2022_01_03_deviation_above_tolerance_flagged() {
    let ref_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet",
    );
    let dbn_path = Path::new(
        "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst",
    );

    let reference = load_reference_parquet(ref_path).unwrap();
    let rust = run_rust_pipeline(dbn_path, 11355).unwrap();

    let result = compare_features(&rust, &reference, 1e-5);

    // Spec: "Any deviation > 1e-5 is clearly flagged with failing bar indices
    // and values"
    for feat_dev in &result.per_feature {
        if !feat_dev.passed {
            assert!(
                feat_dev.worst_bar.is_some(),
                "Failing feature '{}' must report worst_bar index",
                feat_dev.name,
            );
            assert!(
                feat_dev.worst_rust_val.is_some(),
                "Failing feature '{}' must report worst Rust value",
                feat_dev.name,
            );
            assert!(
                feat_dev.worst_ref_val.is_some(),
                "Failing feature '{}' must report worst reference value",
                feat_dev.name,
            );
        }
    }
}
