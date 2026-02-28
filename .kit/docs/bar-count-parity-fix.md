# TDD Spec: Bar Count Parity Fix

**Phase:** 0 (Parity Validation — Critical Gate)
**Priority:** BLOCKER — parity validation cannot proceed until bar counts match exactly.

---

## Context

Running the parity test against real C++ reference data for 2022-01-03 reveals a bar count mismatch:

- **Rust pipeline:** 4631 bars
- **C++ reference Parquet:** 4630 bars (rows in the Parquet file)
- **Difference:** Rust produces exactly 1 extra bar

The C++ reference Parquet (`/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet`) was produced by `bar_feature_export` with these post-processing steps:
1. Build ALL bars from RTH session
2. **Skip warmup bars** (first 50 bars of session)
3. **Skip bars without valid fwd_return_1** (bars near end of session)
4. Write remaining bars to Parquet

The Rust parity test (`tools/parity-test/`) loads the reference Parquet and runs `.dbn.zst` through the Rust pipeline (ingest → book → bars → features), then compares bar counts.

### Root Cause Hypotheses

The off-by-one is almost certainly one of:

1. **RTH boundary filter difference**: C++ uses `ts_event >= rth_open && ts_event < rth_close` (strict less-than on close). If Rust uses `<=` instead of `<`, one extra snapshot/bar could be emitted.

2. **Snapshot emission boundary**: C++ aligns snapshots to `rth_open + N * 100ms`. The number of 100ms windows in RTH is `23400 / 0.1 = 234,000`. If Rust counts one extra window (e.g., 234,001 snapshots → 4681 bars instead of 4680 → one extra bar after warmup filtering), this would cause the mismatch.

3. **Warmup or fwd_return filtering mismatch**: The Rust parity test may not be applying the same filtering as the C++ export (skip first 50 bars, skip bars without fwd_return_1), causing the count comparison to be apples-to-oranges.

4. **Flush behavior**: C++ `flush()` emits a partial bar if <50 snapshots remain. If Rust does/doesn't flush differently, this could add/remove one bar.

### C++ Parity Spec (from FEATURE_PARITY_SPEC.md)

**RTH boundaries:**
- RTH open: 09:30:00.000 ET (midnight_ns + 9.5 hours in nanoseconds)
- RTH close: 16:00:00.000 ET (midnight_ns + 16 hours in nanoseconds)
- RTH duration: 6.5 hours = 23,400 seconds
- Snapshot interval: 100ms (100,000,000 ns)
- Snapshots per bar: 50 (= 5 seconds)
- Event filtering: `ts_event >= rth_open && ts_event < rth_close` — events AT rth_close are EXCLUDED

**Midnight reference:** `time_utils::REF_MIDNIGHT_ET_NS` = 2022-01-03 00:00:00 ET in UTC nanoseconds = `1641186000 * 1e9`. Note: ET is UTC-5 during winter (EST). So midnight ET = 05:00 UTC.

**Bar emission:** `snapshot_count_ >= snaps_per_bar_` (50 snapshots triggers bar emission).

**Flush at end of session:** `flush()` emits partial bar with <50 snapshots — this IS included in the export.

**Warmup bars:** First 50 bars of each session are skipped in the Parquet export (but they ARE computed for rolling feature state).

**fwd_return filtering:** Bars near the end of session that don't have a valid 1-bar forward return are also skipped.

---

## What to Fix

### Investigation Steps

1. **Count total bars** from the Rust pipeline (before any warmup or fwd_return filtering). Compare to theoretical maximum (RTH duration / bar duration ≈ 4680).

2. **Check if the reference Parquet count (4630) accounts for warmup filtering**. Load the reference Parquet and check if there's an `is_warmup` column or if the first bar's `open_ts` is ~4.2 minutes after RTH open (09:34:10 ET).

3. **Check RTH boundary in Rust code**. The filter MUST be `ts_event >= rth_open && ts_event < rth_close` (strict less-than on close). If `<=` is used, fix to `<`.

4. **Check snapshot alignment**. First snapshot at `rth_open`, last valid snapshot before `rth_close`. Total snapshots = floor((rth_close - rth_open) / snapshot_interval). Not ceil. Not round.

5. **Check flush behavior**. At the end of RTH, if there are remaining snapshots in the bar builder, `flush()` should emit them as a partial bar. Both C++ and Rust should flush.

6. **Align comparison**. The parity test must compare like-for-like. If the reference Parquet has warmup bars stripped, the Rust pipeline output must also strip warmup bars before comparing counts.

### Likely Fix

The most common cause of off-by-one in snapshot-based systems is a boundary condition:

```
// WRONG — includes event exactly at close
if ts_event >= rth_open && ts_event <= rth_close { ... }

// CORRECT — excludes event at close (half-open interval)
if ts_event >= rth_open && ts_event < rth_close { ... }
```

Or in snapshot counting:

```
// WRONG — emits one extra snapshot window
let n_snapshots = (rth_close - rth_open) / interval + 1;

// CORRECT — half-open interval, no +1
let n_snapshots = (rth_close - rth_open) / interval;
```

### Files to Examine and Fix

Look at these files in the Rust codebase (in priority order):

1. **`tools/parity-test/src/lib.rs`** — The pipeline execution function. Check how bars are counted and compared to reference.
2. **`crates/bars/src/time_bar_builder.rs`** (or similar) — The bar builder. Check snapshot counting and bar emission trigger.
3. **`crates/book-builder/src/`** — Check RTH filtering and snapshot emission. Look for `>=` vs `>` and `<=` vs `<` on RTH close boundary.
4. **`crates/databento-ingest/src/`** — Check if event filtering happens here.
5. **`tools/bar-feature-export/src/main.rs`** — Reference Rust batch pipeline (733 LOC). This is the known-working wiring pattern.

Also check the reference Rust batch pipeline (`tools/bar-feature-export/`) to see if it has the same bar count issue — if `bar-feature-export` produces 4680 total bars and `parity-test` produces 4681, the bug is in parity-test's pipeline wiring, not in the bar builder.

### Data Paths for Testing

- **Reference Parquet:** `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet`
- **Raw .dbn.zst:** `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst`

---

## Exit Criteria

- [ ] Root cause of bar count mismatch identified and documented
- [ ] Fix applied to Rust codebase (bar builder, book builder, or parity test pipeline — wherever the bug is)
- [ ] `parity-test` for 2022-01-03 reports matching bar counts (Rust == reference)
- [ ] Fix does not break any existing tests (255 tests still pass)
- [ ] The parity test's ignored tests (`single_day_2022_01_03_end_to_end` and `single_day_2022_01_03_bar_count_matches_reference`) now pass

---

## Test Plan

### RED Phase Tests

**T1: Bar count match** — Run Rust pipeline against 2022-01-03 .dbn.zst. Count total bars produced. Count reference Parquet rows. They must be equal.

**T2: First bar timestamp validation** — The first bar in the Rust pipeline output (after warmup filtering, if applicable) must have `open_ts` matching the first row's timestamp in the reference Parquet.

**T3: Last bar timestamp validation** — The last bar in the Rust pipeline output must have `close_ts` matching the last row's timestamp in the reference Parquet.

**T4: RTH boundary filter** — Verify no snapshots are emitted with timestamps >= rth_close (16:00:00.000 ET in UTC nanoseconds for 2022-01-03).

**T5: Total snapshot count** — Count total snapshots emitted during RTH. Must be <= 234,000 (= 23,400s / 0.1s). Should NOT be 234,001.

### GREEN Phase Implementation

1. Diagnose the root cause by adding logging/assertions
2. Fix the boundary condition
3. Verify all existing tests still pass
4. Verify the parity test's ignored tests now pass with real data

### Notes

- The Rust codebase is a Cargo workspace at `/Users/brandonbell/LOCAL_DEV/mbo-dl-rust/`
- Build with: `cargo build` or `cargo test`
- The existing batch pipeline in `tools/bar-feature-export/src/main.rs` (733 LOC) is a useful reference for correct pipeline wiring
- Do NOT change the C++ reference data or the reference Parquet files
