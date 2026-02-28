# TDD Spec: Parity Test Pipeline Wiring

**Phase:** 0b (Critical — completes Phase 0)
**Crate:** `tools/parity-test/` (existing — extend implementation)
**Priority:** HIGHEST — the parity validation gate cannot pass until this works.

---

## Context

The `tools/parity-test/` binary exists with CLI parsing and output formatting but the actual pipeline execution is a **stub** — it prints "pass/fail summary placeholder" instead of processing data. This spec extends it to wire up the real pipeline.

The `tools/bar-feature-export/` binary (733 LOC) already demonstrates how to wire the full pipeline: `.dbn.zst` → `databento-ingest` → `book-builder` → `bars` → `features` → Parquet. The parity-test tool needs to replicate this pipeline AND compare output against reference C++ Parquet files.

**IMPORTANT:** Do NOT rewrite the pipeline from scratch. Reuse the existing crate interfaces. Study `tools/bar-feature-export/src/main.rs` for the wiring pattern.

---

## What to Build

Extend `tools/parity-test/src/main.rs` to:

### 1. Load Reference Parquet

Given a directory of Parquet files (one per trading day, named `YYYY-MM-DD.parquet`):
- Read each file using the `parquet` and `arrow` crates
- Extract the 20 model feature columns by name (see list below)
- Extract `bar_index` and `is_warmup` columns for alignment
- Skip warmup bars (`is_warmup == true` or first 50 bars)
- Return a `Vec<[f64; 20]>` of reference feature vectors per day

**The 20 feature column names (in order):**
```
weighted_imbalance, spread, net_volume, volume_imbalance, trade_count,
avg_trade_size, vwap_distance, return_1, return_5, return_20,
volatility_20, volatility_50, high_low_range_50, close_position,
cancel_add_ratio, message_rate, modify_fraction, time_sin, time_cos,
minutes_since_open
```

### 2. Process .dbn.zst Through Rust Pipeline

Given a `.dbn.zst` file for one trading day:
- Use `databento-ingest` to read MBO events
- Use `book-builder` to reconstruct book and emit 100ms snapshots
- Use `bars` (TimeBarBuilder, 50 snapshots per bar) to emit 5-second bars
- Use `features` (BarFeatureComputer) to compute all features per bar
- Skip warmup bars (first 50)
- Return a `Vec<[f64; 20]>` of computed feature vectors

**Reference implementation:** Look at `tools/bar-feature-export/src/main.rs` for the exact wiring pattern. The parity test pipeline should be identical except it returns features in-memory instead of writing Parquet.

### 3. Compare Features Bar-by-Bar

For each non-warmup bar:
- Compare all 20 features between Rust and C++ reference
- Compute absolute deviation: `|rust_value - cpp_value|`
- Track per-feature max deviation, mean deviation
- Flag any deviation > tolerance

### 4. Output Format

When running against a single day:
```
Day 20220103: 4632 bars (reference: 4632)
  weighted_imbalance: max_dev=1.2e-7  PASS
  spread:             max_dev=0.0e+0  PASS
  ...
  minutes_since_open: max_dev=0.0e+0  PASS
RESULT: PASS (all 20 features within 1e-5)
```

When running against all days:
```
Day 20220103: PASS (worst: weighted_imbalance 1.2e-7)
Day 20220104: PASS (worst: volatility_20 3.4e-6)
...
Day 20221230: PASS (worst: vwap_distance 8.9e-8)
SUMMARY: 251/251 days PASS
```

On failure:
```
Day 20220103: FAIL
  volatility_20: max_dev=2.3e-2  FAIL (> 1e-5)
    Bar 42: rust=0.04521 cpp=0.02234 dev=0.02287
    Bar 43: rust=0.04498 cpp=0.02198 dev=0.02300
  ...
```

---

## Data Files for Testing

**Reference Parquet:** `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/` (S3 symlinks, hydrated)

**Raw .dbn.zst:** `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/`

**Filename convention:**
- Parquet: `2022-01-03.parquet`
- DBN: `glbx-mdp3-20220103.mbo.dbn.zst`

**Day matching:** Extract date from Parquet filename `YYYY-MM-DD` and match to DBN file `glbx-mdp3-YYYYMMDD.mbo.dbn.zst`.

---

## Dependencies to Add

```toml
# tools/parity-test/Cargo.toml
[dependencies]
clap = { version = "4", features = ["derive"] }
anyhow = "1"
parquet = "58"
arrow = "58"
features = { path = "../../crates/features" }
bars = { path = "../../crates/bars" }
book-builder = { path = "../../crates/book-builder" }
databento-ingest = { path = "../../crates/databento-ingest" }
common = { path = "../../crates/common" }
dbn = "0.50"
```

---

## Known Risk Areas (from FEATURE_PARITY_SPEC)

Study `FEATURE_PARITY_SPEC.md` (in the workspace root) Section 12.2 for the specific risk areas. The most likely sources of deviation:

1. **volatility_20/50** — Must use population std (`var = sum_sq/n - mean²`), NOT sample std
2. **vwap_distance** — When `volume == 0`, VWAP must be `0.0`, producing `close_mid / 0.25`
3. **cancel_add_ratio, message_rate, modify_fraction** — MBO event recount from actual events, not bar builder internals
4. **trade_count** — Must use `trade_event_count` from MBO recount
5. **high_low_range_50** — Guard at `n > 50` (strictly greater)
6. **Bars 2-50 (fixup)** — When lookback is incomplete, use available data
7. **message_rate** — EXCLUDES trades: `(add + cancel + modify) / bar_duration_s`

---

## Exit Criteria

- [ ] `tools/parity-test` loads reference Parquet files and extracts 20 feature columns
- [ ] `tools/parity-test` processes `.dbn.zst` through the full Rust pipeline (ingest → book → bars → features)
- [ ] Bar-by-bar comparison reports per-feature max deviation
- [ ] Successfully processes at least 1 real day (2022-01-03) with bar count match to reference
- [ ] Output shows per-feature deviations and pass/fail status
- [ ] Any deviation > 1e-5 is clearly flagged with failing bar indices and values

---

## Test Plan

### RED Phase Tests

Write tests in `tools/parity-test/tests/pipeline_test.rs` (or extend existing `crates/features/tests/`).

**T1: Parquet loading** — Load a reference Parquet file, verify 20 named columns are extracted, verify row count matches expected bar count.

**T2: Pipeline execution** — Process a .dbn.zst file through the Rust pipeline, verify non-zero number of bars produced, verify features are non-NaN after warmup.

**T3: Day matching** — Given reference dir and data dir, correctly match `2022-01-03.parquet` to `glbx-mdp3-20220103.mbo.dbn.zst`.

**T4: Comparison logic** — Given two identical feature vectors, report max deviation 0 and PASS. Given vectors with known differences, report correct deviation and FAIL.

**T5: Single-day execution** — Run end-to-end on 2022-01-03. This is the real integration test. (May be marked `#[ignore]` if CI doesn't have data, but MUST run locally.)

### GREEN Phase Implementation

1. Add pipeline dependencies to `tools/parity-test/Cargo.toml` (`features`, `bars`, `book-builder`, `databento-ingest`, `common`, `dbn`, `parquet`, `arrow`)
2. Implement `load_reference_parquet(path) -> Vec<[f64; 20]>` — read named columns, skip warmup
3. Implement `run_rust_pipeline(dbn_path) -> Vec<[f64; 20]>` — replicate `bar-feature-export` pipeline but return in-memory
4. Implement `compare_features(rust, reference, tolerance) -> ComparisonResult` — bar-by-bar comparison
5. Wire into CLI: parse args → match day → load reference → run pipeline → compare → report
6. Run against real 2022-01-03 data
