//! CLI wire integration tests for parity-test binary (Phase 0d).
//!
//! These tests verify that `main.rs` is wired to call library functions
//! (match_day_files, run_rust_pipeline, load_reference_parquet, compare_features)
//! instead of printing "placeholder". Tests exercise the binary as a subprocess
//! and validate output format, exit codes, and error handling per the spec.
//!
//! Test categories:
//!   Section 1: Stub detection — output must not contain "placeholder"
//!   Section 2: Structured output — wired binary produces formatted summaries
//!   Section 3: Error handling — corrupt files handled gracefully
//!   Section 4: Day filtering — --day flag controls which days are processed
//!   Section 5: Exit code contract — 0 when all pass, 1 when any fail
//!   Section 6: Real-data integration (ignored — require C++ reference Parquet)

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the compiled parity-test binary (set by cargo test).
fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_parity-test"))
}

/// Create a unique temp directory for a test, cleaning up any prior run.
fn temp_dir(test_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("parity_cli_wire_{}", test_name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Create ref/data subdirectories inside a root temp dir.
fn make_dirs(root: &Path) -> (PathBuf, PathBuf) {
    let ref_dir = root.join("ref");
    let data_dir = root.join("data");
    fs::create_dir_all(&ref_dir).unwrap();
    fs::create_dir_all(&data_dir).unwrap();
    (ref_dir, data_dir)
}

/// Write a matched pair of dummy files that match_day_files() will discover.
/// File contents are garbage — pipeline will fail on them, which is intentional.
fn write_dummy_pair(ref_dir: &Path, data_dir: &Path, date_compact: &str, date_dashed: &str) {
    fs::write(
        ref_dir.join(format!("{}.parquet", date_dashed)),
        b"not a real parquet file",
    )
    .unwrap();
    fs::write(
        data_dir.join(format!("glbx-mdp3-{}.mbo.dbn.zst", date_compact)),
        b"not a real dbn file",
    )
    .unwrap();
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

// ═══════════════════════════════════════════════════════════════════════
// Section 1: Stub Detection
//
// The current main.rs prints "pass/fail summary placeholder" and returns.
// Every test here asserts the output does NOT contain "placeholder",
// which forces the implementer to remove the stub and wire real logic.
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn output_not_placeholder_with_empty_dirs() {
    let root = temp_dir("s1_empty");
    let (ref_dir, data_dir) = make_dirs(&root);

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.to_lowercase().contains("placeholder"),
        "main.rs is still a stub — output contains 'placeholder'.\nstdout: {}",
        stdout
    );

    cleanup(&root);
}

#[test]
fn output_not_placeholder_with_matched_files() {
    let root = temp_dir("s1_matched");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.to_lowercase().contains("placeholder"),
        "main.rs is still a stub — output contains 'placeholder' even with matched files.\nstdout: {}",
        stdout
    );

    cleanup(&root);
}

#[test]
fn output_not_placeholder_with_tolerance_flag() {
    let root = temp_dir("s1_tol");
    let (ref_dir, data_dir) = make_dirs(&root);

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
            "--tolerance",
            "0.001",
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.to_lowercase().contains("placeholder"),
        "tolerance flag should not affect placeholder removal.\nstdout: {}",
        stdout
    );

    cleanup(&root);
}

#[test]
fn output_not_placeholder_with_day_filter() {
    let root = temp_dir("s1_day");
    let (ref_dir, data_dir) = make_dirs(&root);

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
            "--day",
            "20220103",
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.to_lowercase().contains("placeholder"),
        "day filter should not affect placeholder removal.\nstdout: {}",
        stdout
    );

    cleanup(&root);
}

// ═══════════════════════════════════════════════════════════════════════
// Section 2: Structured Output Format
//
// Once wired, the binary must produce structured output with headers,
// summaries, and an "Overall:" result line — not raw text.
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn empty_dirs_produce_overall_result_line() {
    let root = temp_dir("s2_overall");
    let (ref_dir, data_dir) = make_dirs(&root);

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Overall:"),
        "empty dirs should still produce an 'Overall:' result line.\nstdout: {}",
        stdout
    );

    cleanup(&root);
}

#[test]
fn matched_files_trigger_day_header() {
    let root = temp_dir("s2_header");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The wired binary should mention the date being processed
    assert!(
        combined.contains("2022-01-03") || combined.contains("20220103"),
        "matched files should trigger processing that mentions the date.\ncombined: {}",
        combined
    );

    cleanup(&root);
}

#[test]
fn empty_dirs_exit_zero_with_no_placeholder() {
    let root = temp_dir("s2_exit0");
    let (ref_dir, data_dir) = make_dirs(&root);

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Must exit 0 (nothing to fail) AND not contain placeholder
    assert!(
        !stdout.to_lowercase().contains("placeholder"),
        "must not contain placeholder.\nstdout: {}",
        stdout
    );
    assert!(
        output.status.success(),
        "empty dirs (no days to validate) should exit 0.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    cleanup(&root);
}

// ═══════════════════════════════════════════════════════════════════════
// Section 3: Error Handling
//
// Corrupt files should be handled gracefully — no panics, no segfaults.
// The spec says: skip unreadable files with error message, mark features
// as FAIL for that day if bar counts mismatch.
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn corrupt_files_do_not_produce_placeholder() {
    let root = temp_dir("s3_no_ph");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.to_lowercase().contains("placeholder"),
        "corrupt files should trigger pipeline attempt, not placeholder.\nstdout: {}",
        stdout
    );

    cleanup(&root);
}

#[test]
fn corrupt_files_do_not_panic() {
    let root = temp_dir("s3_no_panic");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The process should exit normally — not killed by signal (panic/segfault)
    assert!(
        output.status.code().is_some(),
        "binary should exit normally (not killed by signal) on corrupt files.\nstderr: {}",
        stderr
    );
    // Should not contain Rust panic messages
    assert!(
        !stderr.contains("panicked at"),
        "binary should not panic on corrupt files.\nstderr: {}",
        stderr
    );

    cleanup(&root);
}

#[test]
fn corrupt_files_produce_error_or_fail_indication() {
    let root = temp_dir("s3_fail_ind");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // With corrupt files, the wired binary should produce SOME indication
    // of failure: FAIL status, error message, skip warning, or non-zero exit.
    let has_indication = combined.contains("FAIL")
        || combined.contains("Error")
        || combined.contains("error")
        || combined.contains("Skip")
        || combined.contains("skip")
        || combined.contains("Warning")
        || combined.contains("warning")
        || !output.status.success();

    assert!(
        has_indication,
        "corrupt files should produce error/fail/skip indication.\ncombined: {}",
        combined
    );

    cleanup(&root);
}

// ═══════════════════════════════════════════════════════════════════════
// Section 4: Day Filtering
//
// --day YYYYMMDD should filter to only that day. If the day doesn't
// match any files, no days are processed (but output is still structured).
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn day_filter_includes_specified_day_in_output() {
    let root = temp_dir("s4_include");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");
    write_dummy_pair(&ref_dir, &data_dir, "20220104", "2022-01-04");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
            "--day",
            "20220103",
        ])
        .output()
        .expect("failed to run binary");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The wired binary should process and mention the filtered day
    assert!(
        combined.contains("2022-01-03") || combined.contains("20220103"),
        "--day 20220103 should include that day in output.\ncombined: {}",
        combined
    );

    cleanup(&root);
}

#[test]
fn day_filter_excludes_other_days() {
    let root = temp_dir("s4_exclude");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");
    write_dummy_pair(&ref_dir, &data_dir, "20220104", "2022-01-04");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
            "--day",
            "20220103",
        ])
        .output()
        .expect("failed to run binary");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // First: must actually be wired (not placeholder)
    assert!(
        !combined.to_lowercase().contains("placeholder"),
        "must not be a stub.\ncombined: {}",
        combined
    );

    // Second: must NOT mention the excluded day
    assert!(
        !combined.contains("2022-01-04") && !combined.contains("20220104"),
        "--day 20220103 should exclude 2022-01-04 from output.\ncombined: {}",
        combined
    );

    cleanup(&root);
}

#[test]
fn day_filter_nonexistent_day_no_placeholder() {
    let root = temp_dir("s4_nomatch");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
            "--day",
            "99991231",
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Even with no matched files, should produce structured output (not placeholder)
    assert!(
        !stdout.to_lowercase().contains("placeholder"),
        "filtering to non-existent day should still produce structured output.\nstdout: {}",
        stdout
    );

    cleanup(&root);
}

// ═══════════════════════════════════════════════════════════════════════
// Section 5: Exit Code Contract
//
// Exit 0 when all features pass tolerance. Exit 1 when any fail.
// With no days to validate, exit 0 (nothing failed).
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn corrupt_files_exit_nonzero_or_report_failure() {
    let root = temp_dir("s5_corrupt_exit");
    let (ref_dir, data_dir) = make_dirs(&root);
    write_dummy_pair(&ref_dir, &data_dir, "20220103", "2022-01-03");

    let output = Command::new(bin())
        .args([
            "--reference",
            ref_dir.to_str().unwrap(),
            "--data",
            data_dir.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The wired binary should either exit 1 (day failed) or report failure in output.
    // The stub exits 0 with "placeholder" — neither condition is met.
    let exit_nonzero = !output.status.success();
    let reports_failure = stdout.contains("FAIL") || stdout.contains("Overall: FAIL");

    assert!(
        exit_nonzero || reports_failure,
        "corrupt files should cause exit 1 or report FAIL.\nexit={}, stdout: {}",
        output.status.code().unwrap_or(-1),
        stdout
    );

    cleanup(&root);
}

// ═══════════════════════════════════════════════════════════════════════
// Section 6: Real-Data Integration Tests
//
// These require C++ reference Parquet files at hardcoded paths.
// Marked #[ignore] — run with `cargo test -- --ignored` when data is present.
// ═══════════════════════════════════════════════════════════════════════

const REAL_REF_DIR: &str =
    "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/";
const REAL_DATA_DIR: &str =
    "/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/";

/// Helper: run the binary against real data for a single day.
fn run_real_single_day() -> std::process::Output {
    Command::new(bin())
        .args([
            "--reference",
            REAL_REF_DIR,
            "--data",
            REAL_DATA_DIR,
            "--day",
            "20220103",
        ])
        .output()
        .expect("failed to run binary")
}

#[test]
#[ignore]
fn real_data_output_contains_day_header() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("=== Parity Test:") && stdout.contains("2022-01-03"),
        "output should contain '=== Parity Test: 2022-01-03 ===' header.\nstdout: {}",
        stdout
    );
}

#[test]
#[ignore]
fn real_data_output_contains_bar_counts() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Bars:") && stdout.contains("Rust=") && stdout.contains("Reference="),
        "output should show 'Bars: Rust=NNNN, Reference=NNNN'.\nstdout: {}",
        stdout
    );
}

#[test]
#[ignore]
fn real_data_output_contains_all_20_features() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let expected_features = [
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

    for feature in &expected_features {
        assert!(
            stdout.contains(feature),
            "output should list feature '{}' in the table.\nstdout: {}",
            feature,
            stdout
        );
    }
}

#[test]
#[ignore]
fn real_data_output_has_pass_or_fail_per_feature() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Each feature line should contain either PASS or FAIL
    let pass_count = stdout.matches("PASS").count();
    let fail_count = stdout.matches("FAIL").count();

    assert!(
        pass_count + fail_count >= 20,
        "should have at least 20 PASS/FAIL statuses (one per feature). Found {} PASS + {} FAIL.\nstdout: {}",
        pass_count, fail_count, stdout
    );
}

#[test]
#[ignore]
fn real_data_output_contains_summary_counts() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Summary:") && stdout.contains("/20"),
        "output should contain 'Summary: X/20 PASS, Y/20 FAIL'.\nstdout: {}",
        stdout
    );
}

#[test]
#[ignore]
fn real_data_output_contains_overall_result() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Overall:"),
        "output should contain 'Overall:' result line.\nstdout: {}",
        stdout
    );
}

#[test]
#[ignore]
fn real_data_output_shows_deviation_values() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Deviation values should appear in scientific or decimal notation
    let has_numeric = stdout.contains("e-") || stdout.contains("e+") || stdout.contains("0.00");
    assert!(
        has_numeric,
        "output should contain numeric deviation values (e.g., 1.23e-06).\nstdout: {}",
        stdout
    );
}

#[test]
#[ignore]
fn real_data_feature_table_has_column_headers() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Feature")
            && stdout.contains("Max Dev")
            && stdout.contains("Mean Dev")
            && stdout.contains("Status"),
        "output should contain column headers: Feature, Max Dev, Mean Dev, Status.\nstdout: {}",
        stdout
    );
}

#[test]
#[ignore]
fn real_data_exit_code_matches_overall_line() {
    let output = run_real_single_day();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let exit_code = output.status.code().unwrap_or(-1);

    if stdout.contains("Overall: PASS") {
        assert_eq!(exit_code, 0, "Overall: PASS should exit 0");
    } else if stdout.contains("Overall: FAIL") {
        assert_eq!(exit_code, 1, "Overall: FAIL should exit 1");
    } else {
        panic!(
            "output must contain 'Overall: PASS' or 'Overall: FAIL'.\nstdout: {}",
            stdout
        );
    }
}

#[test]
#[ignore]
fn real_data_all_days_produces_multiple_headers() {
    let output = Command::new(bin())
        .args(["--reference", REAL_REF_DIR, "--data", REAL_DATA_DIR])
        .output()
        .expect("failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let header_count = stdout.matches("=== Parity Test:").count();

    assert!(
        header_count > 1,
        "running all days should produce multiple day headers (found {}).\nfirst 500 chars: {}",
        header_count,
        &stdout[..stdout.len().min(500)]
    );
}
