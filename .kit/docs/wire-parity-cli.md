# TDD Spec: Wire Parity-Test CLI

**Phase:** 0d (Parity Validation — CLI Wiring)
**Priority:** BLOCKER — the binary is a stub that prints "placeholder" and never exercises the pipeline.

---

## Context

The library code in `tools/parity-test/src/lib.rs` is complete:
- `match_day_files()` — pairs reference Parquet with raw DBN files
- `run_rust_pipeline()` — processes DBN through Rust bar construction + feature computation
- `load_reference_parquet()` — loads C++ reference Parquet into memory
- `compare_features()` — bar-by-bar comparison of 20 features with tolerance checking

But `tools/parity-test/src/main.rs` is a stub that parses CLI args and prints "pass/fail summary placeholder". It never calls any library functions.

The bar count parity fix (commit e3777ee) has already been applied — Rust produces 4630 non-warmup bars for 2022-01-03, matching the C++ reference.

---

## What to Build

Wire `main.rs` to perform a complete parity validation:

### CLI Interface

```
parity-test --reference <dir> --data <dir> [--day YYYYMMDD] [--tolerance <float>]
```

- `--reference`: Directory containing C++ reference Parquet files (e.g., `2022-01-03.parquet`)
- `--data`: Directory containing raw `.dbn.zst` files (e.g., `glbx-mdp3-20220103.mbo.dbn.zst`)
- `--day`: Optional. If provided, validate only this day. If omitted, validate all days found in both directories.
- `--tolerance`: Maximum acceptable deviation per feature (default: `1e-5`)

### Execution Flow

1. **Parse CLI args** (already done — keep existing arg parsing, just wire the functions)
2. **Match day files**: Call `match_day_files()` (or equivalent) to find (reference_parquet, dbn_file) pairs. If `--day` is specified, filter to just that day.
3. **For each day**:
   a. Call `run_rust_pipeline()` with the DBN file path to produce Rust-computed features
   b. Call `load_reference_parquet()` with the reference Parquet path
   c. Call `compare_features()` with Rust features, reference features, and tolerance
   d. Collect per-feature results
4. **Print deviation summary**: For each feature, print:
   - Feature name
   - Max absolute deviation across all bars
   - Mean absolute deviation
   - PASS/FAIL based on tolerance
5. **Exit code**: Exit 0 if ALL features on ALL days pass tolerance. Exit 1 if any fail.

### Output Format

```
=== Parity Test: 2022-01-03 ===
Bars: Rust=4630, Reference=4630

Feature                  Max Dev     Mean Dev    Status
weighted_imbalance       1.23e-06    4.56e-07    PASS
spread                   0.00e+00    0.00e+00    PASS
net_volume               5.67e+01    1.23e+01    FAIL
...

Summary: 17/20 PASS, 3/20 FAIL
Overall: FAIL (exit 1)
```

### The 20 Features to Compare

These are the non-spatial features used by XGBoost (column names in Parquet):

```
weighted_imbalance, spread, net_volume, volume_imbalance, trade_count,
avg_trade_size, vwap_distance, return_1, return_5, return_20,
volatility_20, volatility_50, high_low_range_50, close_position,
cancel_add_ratio, message_rate, modify_fraction,
time_sin, time_cos, minutes_since_open
```

### Error Handling

- If a day file is missing from either directory, skip it and print a warning
- If bar counts don't match (Rust vs reference), report the mismatch and mark all features as FAIL for that day
- If the reference Parquet can't be read, skip with error message

---

## Files to Modify

1. **`tools/parity-test/src/main.rs`** — Replace the stub body with actual pipeline calls
2. **`tools/parity-test/src/lib.rs`** — Adjust function signatures if needed for the CLI integration (e.g., return types, error handling). Do NOT rewrite the pipeline logic — it's already correct.

---

## Exit Criteria

- [ ] `cargo build --release` succeeds with no errors
- [ ] `cargo test` — all existing non-ignored tests still pass (262+)
- [ ] Running `./target/release/parity-test --reference /Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/ --data /Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/ --day 20220103` produces meaningful per-feature output (not "placeholder")
- [ ] The output shows bar count for both Rust and reference
- [ ] The output shows per-feature max deviation and PASS/FAIL status
- [ ] Exit code is 0 if all features pass, 1 if any fail

---

## GREEN Phase Instructions

1. Read `tools/parity-test/src/main.rs` to understand the existing stub structure and CLI arg parsing.
2. Read `tools/parity-test/src/lib.rs` to understand the available functions and their signatures.
3. Wire `main.rs` to call the library functions in the order described above.
4. The library already has `match_day_files`, `run_rust_pipeline`, `load_reference_parquet`, and `compare_features` — use them. If their signatures don't quite fit the CLI flow, make minimal adjustments to lib.rs.
5. **Do NOT write new test files.** The parity-test binary comparing against C++ Parquet IS the validation.
6. Build with `cargo build --release` and verify existing tests pass with `cargo test`.
7. Run the binary against real data to verify it produces meaningful output.

**Data paths for verification:**
- Reference: `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/`
- Raw data: `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/`
- Test day: `20220103`

**IMPORTANT:** The binary will likely show feature deviations — that's EXPECTED. The goal of this spec is just to wire the CLI so it runs and produces output. Fixing the deviations is a separate step.
