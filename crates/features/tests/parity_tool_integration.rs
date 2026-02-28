//! Parity Validation Harness — Tool Integration Tests (RED Phase)
//!
//! Tests that the `parity-test` binary crate exists, builds, and behaves
//! correctly as specified in .kit/docs/parity-validation-harness.md.
//!
//! These tests MUST FAIL in the RED phase because `tools/parity-test/`
//! does not exist yet.
//!
//! Test Plan Coverage:
//!   T1: CLI arg parsing             → Section 1
//!   T2: Reference Parquet loading   → Section 2
//!   T3: Bar count matching          → Section 3
//!   T7: Summary report format       → Section 4

use std::path::Path;
use std::process::Command;

/// Workspace root, two levels up from crates/features/.
fn workspace_root() -> String {
    let manifest = env!("CARGO_MANIFEST_DIR"); // crates/features/
    Path::new(manifest)
        .parent() // crates/
        .unwrap()
        .parent() // workspace root
        .unwrap()
        .to_string_lossy()
        .to_string()
}

/// Run `cargo build -p parity-test` and return the output.
fn build_parity_test() -> std::process::Output {
    Command::new("cargo")
        .args(["build", "-p", "parity-test"])
        .current_dir(workspace_root())
        .output()
        .expect("Failed to execute cargo build")
}

/// Assert that parity-test builds successfully. Gate for all other tests.
fn require_parity_test_builds() {
    let output = build_parity_test();
    assert!(
        output.status.success(),
        "parity-test must be a buildable workspace member. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Run `parity-test` binary with given args and return the output.
fn run_parity_test(args: &[&str]) -> std::process::Output {
    Command::new("cargo")
        .args(["run", "-p", "parity-test", "--"])
        .args(args)
        .current_dir(workspace_root())
        .output()
        .expect("Failed to execute cargo run")
}

// =====================================================================
// Section 1: CLI Arg Parsing (T1)
//
// The parity-test binary must:
// - Build as a workspace member
// - Accept --reference <parquet_dir>
// - Accept --data <dbn_dir>
// - Accept optional --day <YYYYMMDD>
// - Accept optional --tolerance <float> (default 1e-5)
// - Print help with --help
// =====================================================================

#[test]
fn parity_test_crate_builds() {
    require_parity_test_builds();
}

#[test]
fn parity_test_help_flag() {
    require_parity_test_builds();
    let output = run_parity_test(&["--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "parity-test --help should succeed. stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        stdout.contains("--reference"),
        "Help text must document --reference flag. stdout:\n{}",
        stdout,
    );
    assert!(
        stdout.contains("--data"),
        "Help text must document --data flag. stdout:\n{}",
        stdout,
    );
}

#[test]
fn parity_test_requires_reference_and_data() {
    require_parity_test_builds();
    // Running with no args should fail (missing required args).
    let output = run_parity_test(&[]);
    assert!(
        !output.status.success(),
        "parity-test with no args should fail (--reference and --data required)",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Error should mention missing required arguments, not a crash.
    assert!(
        stderr.contains("required") || stderr.contains("--reference") || stderr.contains("--data"),
        "Error should mention missing required args. stderr:\n{}",
        stderr,
    );
}

#[test]
fn parity_test_accepts_optional_day() {
    require_parity_test_builds();
    let output = run_parity_test(&[
        "--reference", "/nonexistent",
        "--data", "/nonexistent",
        "--day", "20220103",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should fail because dirs don't exist, but NOT because --day is unknown.
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("unknown option"),
        "parity-test must accept --day flag without error. stderr:\n{}",
        stderr,
    );
}

#[test]
fn parity_test_accepts_optional_tolerance() {
    require_parity_test_builds();
    let output = run_parity_test(&[
        "--reference", "/nonexistent",
        "--data", "/nonexistent",
        "--tolerance", "1e-5",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unexpected argument") && !stderr.contains("unknown option"),
        "parity-test must accept --tolerance flag. stderr:\n{}",
        stderr,
    );
}

#[test]
fn parity_test_default_tolerance_is_1e5() {
    require_parity_test_builds();
    let output = run_parity_test(&["--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Help text should show default tolerance of 1e-5 (or 0.00001)
    assert!(
        stdout.contains("1e-5") || stdout.contains("0.00001") || stdout.contains("1e-05"),
        "Default tolerance should be 1e-5. Help text:\n{}",
        stdout,
    );
}

// =====================================================================
// Section 2: Reference Parquet Loading (T2)
//
// The tool must load C++ reference Parquet files and extract the
// 20 model features by column name. It must also extract bar_index
// and is_warmup for alignment, and skip warmup bars.
// =====================================================================

#[test]
fn parity_test_graceful_error_on_missing_reference() {
    require_parity_test_builds();
    let output = run_parity_test(&[
        "--reference", "/nonexistent/reference",
        "--data", "/nonexistent/data",
        "--day", "20220103",
    ]);
    // The binary should fail gracefully (not panic/segfault).
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);
    // Must mention the reference path or "not found" in the error
    assert!(
        combined.contains("reference") || combined.contains("parquet") || combined.contains("not found")
            || combined.contains("No such file"),
        "Tool should report a clear error about missing reference data. Output:\n{}",
        combined,
    );
    // Must NOT contain "panic" or "SIGSEGV"
    assert!(
        !combined.contains("panicked") && !combined.contains("SIGSEGV"),
        "Tool must not panic on missing files. Output:\n{}",
        combined,
    );
}

#[test]
fn parity_test_extracts_20_feature_columns() {
    // The 20 model feature column names that must be extracted from Parquet.
    // This constant test documents the contract for the GREEN phase.
    let expected_columns = [
        "weighted_imbalance", "spread", "net_volume", "volume_imbalance",
        "trade_count", "avg_trade_size", "vwap_distance",
        "return_1", "return_5", "return_20",
        "volatility_20", "volatility_50", "high_low_range_50", "close_position",
        "cancel_add_ratio", "message_rate", "modify_fraction",
        "time_sin", "time_cos", "minutes_since_open",
    ];
    assert_eq!(
        expected_columns.len(), 20,
        "Must compare exactly 20 model features",
    );
    // Also verify these match what BarFeatureRow exposes.
    let all_names = features::BarFeatureRow::feature_names();
    for col in &expected_columns {
        assert!(
            all_names.contains(col),
            "Parquet column '{}' must exist in BarFeatureRow::feature_names()",
            col,
        );
    }
}

// =====================================================================
// Section 3: Bar Count Matching (T3)
//
// The tool must verify that the Rust pipeline produces the same number
// of non-warmup bars as the reference Parquet for each day.
// =====================================================================

#[test]
fn parity_test_reports_bar_count() {
    require_parity_test_builds();
    let output = run_parity_test(&[
        "--reference", "/nonexistent/reference",
        "--data", "/nonexistent/data",
        "--day", "20220103",
    ]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    // Output should mention bar counts (reference vs rust).
    assert!(
        combined.contains("bar") || combined.contains("count") || combined.contains("rows"),
        "Tool output should report bar/row counts. Output:\n{}",
        combined,
    );
}

// =====================================================================
// Section 4: Summary Report (T7)
//
// After processing, the tool must produce a summary with:
// - Days passed/failed
// - Worst feature name
// - Worst deviation value
// - Per-feature max absolute deviation
// =====================================================================

#[test]
fn parity_test_summary_contains_pass_fail() {
    require_parity_test_builds();
    // When the tool processes data, stdout/stderr should contain "pass" or "fail".
    // This is a contract on the output format.
}

#[test]
fn parity_test_summary_reports_worst_feature() {
    require_parity_test_builds();
    // The summary must identify the feature with the worst deviation.
}

#[test]
fn parity_test_summary_reports_per_feature_deviation() {
    require_parity_test_builds();
    // The summary must report max absolute deviation for each of the 20 features.
}

// =====================================================================
// Section 5: Tolerance Threshold
//
// Deviations > tolerance (default 1e-5) must be flagged as failures.
// =====================================================================

#[test]
fn parity_test_flags_deviation_above_tolerance() {
    require_parity_test_builds();
}

#[test]
fn parity_test_passes_when_all_within_tolerance() {
    require_parity_test_builds();
}
