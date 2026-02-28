# TDD Spec: Numerical Parity Validation Harness

**Phase:** 0 (Critical Gate)
**Crate:** `tools/parity-test/` (new binary crate)
**Priority:** HIGHEST — nothing downstream is valid until this passes.

---

## Context

The Rust workspace (`mbo-dl-rust`) ports the C++ MBO-DL research pipeline. A validated two-stage XGBoost model was trained on C++ pipeline output (152-column Parquet files). The Rust pipeline must produce **identical** feature values, or the model is invalid on Rust output.

**Reference data:** C++ Parquet files from the full-year export (251 RTH trading days, 2022, MES). These live in `../MBO-DL-02152026/.kit/results/full-year-export/` and are also in S3 (`s3://kenoma-labs-research/results/bidirectional-reexport/`). Each file has 152 columns, ~4,600 bars per day.

**Raw data:** 312 daily `.dbn.zst` files in `DATA/GLBX-20260207-L953CAPU5B/` (relative to `../MBO-DL-02152026/`). The Rust pipeline reads these same files.

---

## What to Build

A binary `tools/parity-test/` that:

1. Takes CLI args: `--reference <parquet_dir> --data <dbn_dir> [--day <YYYYMMDD>] [--tolerance <float>]`
2. For each day (or a single specified day):
   a. Loads the reference C++ Parquet file
   b. Processes the corresponding `.dbn.zst` through the Rust pipeline (using existing crates: `databento-ingest`, `book-builder`, `bars`, `features`)
   c. Compares the 20 model features bar-by-bar
   d. Reports per-feature max absolute deviation
   e. Flags any deviation > tolerance (default 1e-5)
3. Produces a summary: days passed/failed, worst feature, worst deviation

### The 20 Model Features to Compare

These are the exact features used by XGBoost, in order:

| # | Feature Name | Formula | Lookback |
|---|-------------|---------|----------|
| 0 | `weighted_imbalance` | `(Σ bid[i]·w[i] - Σ ask[i]·w[i]) / (Σ bid[i]·w[i] + Σ ask[i]·w[i] + ε)`, w[i]=1/(i+1) | 0 bars |
| 1 | `spread` | `bar.spread / 0.25` | 0 bars |
| 2 | `net_volume` | `buy_volume - sell_volume` | 0 bars |
| 3 | `volume_imbalance` | `net_volume / (volume + ε)` | 0 bars |
| 4 | `trade_count` | Count of action='T' MBO events in bar | 0 bars |
| 5 | `avg_trade_size` | `volume / trade_event_count` (0 if no trades) | 0 bars |
| 6 | `vwap_distance` | `(close_mid - vwap) / 0.25` | 0 bars |
| 7 | `return_1` | `(close_mid[t] - close_mid[t-1]) / 0.25` | 1 bar |
| 8 | `return_5` | `(close_mid[t] - close_mid[t-5]) / 0.25` | 5 bars |
| 9 | `return_20` | `(close_mid[t] - close_mid[t-20]) / 0.25` | 20 bars |
| 10 | `volatility_20` | `sqrt(mean(r²) - mean(r)²)` over last 20 1-bar returns (POPULATION std) | 20 bars |
| 11 | `volatility_50` | `sqrt(mean(r²) - mean(r)²)` over last 50 1-bar returns (POPULATION std) | 50 bars |
| 12 | `high_low_range_50` | `(max(high_mid[-50:]) - min(low_mid[-50:])) / 0.25` — guard: `n > 50` strictly | 50 bars |
| 13 | `close_position` | `(close_mid - min(low[-20:])) / (max(high[-20:]) - min(low[-20:]) + ε)` | 20 bars |
| 14 | `cancel_add_ratio` | `cancel_count / (add_count + ε)` | 0 bars |
| 15 | `message_rate` | `(add + cancel + modify) / bar_duration_s` — EXCLUDES trades | 0 bars |
| 16 | `modify_fraction` | `modify / (add + cancel + modify + ε)` | 0 bars |
| 17 | `time_sin` | `sin(2π × time_of_day / 24)` | 0 bars |
| 18 | `time_cos` | `cos(2π × time_of_day / 24)` | 0 bars |
| 19 | `minutes_since_open` | `max(0, (time_of_day - 9.5) × 60)` | 0 bars |

**Constants:** `tick_size = 0.25`, `ε = 1e-8`

---

## Known Risk Areas (from FEATURE_PARITY_SPEC Section 12.2)

These are the features most likely to have parity bugs. Test these first and most carefully.

| Feature | Risk | What to check |
|---------|------|---------------|
| `volatility_20/50` | Population vs sample std | Must use `var = sum_sq/n - mean^2`, NOT `n-1` denominator |
| `vwap_distance` | Zero-volume edge case | When `volume == 0`, VWAP must be `0.0` (not NaN), producing `vwap_distance = close_mid / 0.25` |
| `cancel_add_ratio`, `message_rate`, `modify_fraction` | MBO event recount | C++ recounts from actual MBO events assigned to bars by timestamp, not bar builder internals |
| `trade_count` | Event count method | Must use `trade_event_count` from MBO recount, not snapshot trade buffer |
| Bars 2-50 (batch fixup) | `fixup_rolling_features()` | When lookback is incomplete, use available data (e.g., volatility_20 at bar 10 uses 10 returns) |
| `high_low_range_50` | Guard condition | C++ triggers at `n > 50` (strictly greater), not `n >= 50` |
| `message_rate` | Excludes trades | Model trained on `(add + cancel + modify) / duration`, NOT `(add + cancel + modify + trade) / duration` |

---

## Architecture

### New Crate: `tools/parity-test/`

```
tools/parity-test/
├── Cargo.toml
└── src/
    └── main.rs
```

**Dependencies:** `features`, `bars`, `book-builder`, `databento-ingest`, `common`, `parquet`, `arrow`, `clap`, `anyhow`

### Reference Parquet Loading

The C++ Parquet files have 152 columns. The parity tool needs to extract the 20 model feature columns by name:

```
weighted_imbalance, spread, net_volume, volume_imbalance, trade_count,
avg_trade_size, vwap_distance, return_1, return_5, return_20,
volatility_20, volatility_50, high_low_range_50, close_position,
cancel_add_ratio, message_rate, modify_fraction, time_sin, time_cos,
minutes_since_open
```

Also extract `bar_index` and `is_warmup` for alignment. Skip warmup bars (first 50 per session).

### Rust Pipeline Execution

Use the existing crates in the workspace. The `tools/bar-feature-export/` binary is the reference for how to wire up the pipeline:
1. `databento-ingest` reads `.dbn.zst`
2. `book-builder` processes MBO events → snapshots
3. `bars` accumulates snapshots → bars
4. `features` computes bar features

The parity test should use the same pipeline but instead of writing Parquet, it compares against reference.

### Comparison Logic

For each bar (after warmup):
- Compare each of the 20 features
- Compute absolute deviation: `|rust_value - cpp_value|`
- Track max deviation per feature per day
- Track total bar count vs reference bar count (must match exactly)

---

## Exit Criteria

- [ ] `tools/parity-test/` crate added to workspace, compiles
- [ ] CLI accepts `--reference`, `--data`, `--day`, `--tolerance` arguments
- [ ] Loads C++ reference Parquet and extracts 20 model features
- [ ] Processes `.dbn.zst` through Rust pipeline to produce features
- [ ] Compares bar-by-bar, reports per-feature max deviation
- [ ] Processes at least 1 reference day with bar count match
- [ ] All deviations < 1e-5 on the tested day(s) — OR — deviations are identified and documented with root cause

**Note:** If deviations are found, the TDD cycle should fix them in the relevant crates (`features`, `bars`, `book-builder`) as part of the GREEN phase. This may require multiple red-green iterations.

---

## Test Plan

### RED Phase Tests

**T1: CLI arg parsing** — Tool parses `--reference`, `--data`, `--day`, `--tolerance` correctly.

**T2: Reference Parquet loading** — Given a small synthetic Parquet with known values for the 20 features, loads and extracts them correctly.

**T3: Bar count matching** — Process a single day, verify Rust produces the same number of non-warmup bars as the reference.

**T4: Feature comparison** — Process a single day, all 20 features within tolerance.

**T5: Zero-volume bar handling** — Verify `vwap_distance` when volume is 0: must produce `close_mid / 0.25`, not NaN.

**T6: Early bar fixup parity** — Bars 2-50 must use partial lookback (e.g., volatility_20 at bar index 10 uses 10 available returns).

**T7: Summary report** — After processing, prints summary with pass/fail per day, worst feature, worst deviation.

### GREEN Phase Implementation

1. Create `tools/parity-test/Cargo.toml` with dependencies
2. Add to workspace `Cargo.toml` members list
3. Implement CLI parsing (clap)
4. Implement reference Parquet loader (arrow/parquet crates)
5. Wire up Rust pipeline: dbn → book → bars → features
6. Implement bar-by-bar comparison with tolerance checking
7. Implement summary reporting
8. Run against real data — fix any parity bugs found in upstream crates

### Data Locations

- Reference Parquet: `../MBO-DL-02152026/.kit/results/full-year-export/` (251 files, one per RTH day)
- Raw .dbn.zst: `../MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/` (312 files)
- S3 fallback for reference: `s3://kenoma-labs-research/results/bidirectional-reexport/`

The reference Parquet files may be S3 symlinks. If so, hydrate first: `orchestration-kit/tools/artifact-store hydrate` in the C++ repo.
