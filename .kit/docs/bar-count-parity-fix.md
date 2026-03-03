# TDD Spec: Bar Count Parity Fix

**Phase:** 0c (Parity Validation — Critical Gate)
**Priority:** BLOCKER — parity validation cannot proceed until bar counts match exactly.

---

## Context

Running parity tests against real C++ reference data (2022-01-03) reveals a bar count mismatch. Tests have been written in `tools/parity-test/tests/bar_count_parity_test.rs`. The tests correctly identify the bug but the GREEN phase has not yet fixed the implementation.

**Observed failures (from running `cargo test --package parity-test -- --ignored`):**

| Test | Result | Observation |
|------|--------|-------------|
| Total snapshot count = 234,000 | **PASS** | Correct |
| No bar at/past RTH close | **PASS** | Boundary respected |
| All bars have 50 snapshots | **PASS** | Most bars are full |
| Pipeline features and bars agree | **PASS** | |
| Total bar count = 4680 | **FAIL** | Got **4681** (off by 1) |
| Non-warmup bar count = 4630 | **FAIL** | Got **4631** (off by 1) |
| Last bar close_ts | **FAIL** | Got `1641157199900000000`, expected `1641243599900000000` (24h off!) |
| First bar opens at RTH open | **FAIL** | Timestamp mismatch |
| Bar count Rust == reference | **FAIL** | 4631 vs 4630 |
| No partial bar at end | **FAIL** | Spurious partial bar exists |

### Critical Diagnostic — ROOT CAUSE CONFIRMED

**Bug #1: Date offset is exactly 1 day off.** The Rust code computes midnight ET for January 2 instead of January 3.

Evidence:
- First bar opens at `1641133800000000000` ns = RTH open for **Jan 2** (not Jan 3)
- Expected first bar: `1641220200000000000` ns = RTH open for **Jan 3**
- Difference: exactly `86,400,000,000,000` ns = 24 hours
- The midnight_et_ns computed by the Rust code is `1641099600000000000` (Jan 2 00:00 ET)
- The correct midnight_et_ns should be `1641186000000000000` (Jan 3 00:00 ET)

**Bug #2: Spurious flush bar with 0 snapshots.** 234,000 snapshots / 50 per bar = exactly 4,680 bars. But the pipeline produces 4,681. The last bar has **0 snapshots** (confirmed by test). This means `flush()` creates an empty bar even when `snapshot_count == 0`. Fix: only flush if `snapshot_count > 0`.

**Despite the wrong date, the pipeline still produces ~4680 bars** because the Databento file `glbx-mdp3-20220103.mbo.dbn.zst` contains data spanning from Jan 2 evening through Jan 3 evening (UTC boundaries). The Jan 2 RTH window (09:30-16:00 ET) has no market data (Jan 2 is Sunday), but Jan 3 RTH does. The pipeline finds events that happen to fall within the Jan 2 RTH timestamp window due to a coincidence of the data range — but this is WRONG and will cause issues on other days.

### C++ Parity Spec (from FEATURE_PARITY_SPEC.md)

**RTH boundaries:**
- RTH open: 09:30:00.000 ET (midnight_et_ns + 9.5 * 3600 * 1e9)
- RTH close: 16:00:00.000 ET (midnight_et_ns + 16 * 3600 * 1e9)
- ET = UTC-5 in winter (EST). Midnight ET = 05:00 UTC.
- `REF_MIDNIGHT_ET_NS` = 2022-01-03 00:00:00 ET = 2022-01-03 05:00:00 UTC = `1641186000 * 1e9` nanoseconds

**For 2022-01-03:**
- midnight_et = 1641186000000000000 ns
- rth_open = 1641186000000000000 + 34200000000000 = 1641220200000000000 ns (09:30 ET = 14:30 UTC)
- rth_close = 1641186000000000000 + 57600000000000 = 1641243600000000000 ns (16:00 ET = 21:00 UTC)

**Event filtering:** `ts_event >= rth_open && ts_event < rth_close` — events AT rth_close are EXCLUDED (half-open interval)

**Snapshot emission:** Aligned to `rth_open + N * 100ms`. Total snapshots = (rth_close - rth_open) / 100ms = 234,000.

**Bar emission:** Every 50 snapshots → emit bar. 234,000 / 50 = 4,680 bars exactly (no remainder).

**Flush:** C++ `flush()` only emits a partial bar if `snapshot_count_ > 0`. Since 234,000 is evenly divisible by 50, flush should emit nothing.

**Warmup:** First 50 bars are warmup. 4,680 - 50 = 4,630 non-warmup bars. The reference Parquet has 4,630 rows.

---

## What to Fix

### Root Cause #1: Spurious bar from flush()

The bar builder's `flush()` is creating a 4,681st bar when it should not. Fix: only flush if `snapshot_count_ > 0` (matching C++ behavior).

Look at:
- `crates/bars/src/time_bar_builder.rs` (or wherever TimeBarBuilder::flush is)
- The flush condition: it must check `self.snapshot_count > 0` before emitting

### Root Cause #2: Date/timestamp offset

The last bar's close_ts being 24 hours early suggests the midnight reference is wrong. Check:
- How the parity-test pipeline computes `midnight_et_ns` from the input date string `"20220103"`
- It should be: parse YYYYMMDD → compute UTC midnight → add 5 hours (EST offset) → that's midnight ET in UTC nanoseconds
- OR: use the reference formula from the spec: midnight_et = REF_MIDNIGHT_ET_NS + (days_since_ref * 86400 * 1e9)

The correct midnight_et for 2022-01-03:
- 2022-01-03 00:00:00 ET = 2022-01-03 05:00:00 UTC = 1641186000 seconds = 1641186000000000000 ns

### Files to Examine

1. **`tools/parity-test/src/lib.rs`** — Pipeline execution, date computation, bar counting
2. **`crates/bars/src/`** — TimeBarBuilder, flush() logic
3. **`crates/book-builder/src/`** — Snapshot emission, RTH filtering
4. **`crates/common/src/`** — Time utilities, midnight computation
5. **`tools/bar-feature-export/src/main.rs`** — Reference batch pipeline (known correct for bar counting via tests)

### Data Paths

- Reference Parquet: `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/2022-01-03.parquet`
- Raw .dbn.zst: `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/glbx-mdp3-20220103.mbo.dbn.zst`

---

## Exit Criteria

- [ ] Root cause of bar count mismatch (4681 vs 4680 total) identified and fixed
- [ ] Root cause of 24-hour timestamp offset identified and fixed (if real, not test bug)
- [ ] `cargo test --package parity-test -- --ignored` — all bar_count_parity_test tests pass
- [ ] `cargo test --package parity-test -- --ignored` — pipeline_test `single_day_2022_01_03_end_to_end` and `single_day_2022_01_03_bar_count_matches_reference` pass
- [ ] `cargo test` (non-ignored) — all 262+ tests still pass
- [ ] Rust pipeline produces exactly 4,630 non-warmup bars for 2022-01-03 (matching reference Parquet)

---

## Test Plan

### Tests Already Written

The RED phase has already written 19 tests in `tools/parity-test/tests/bar_count_parity_test.rs`. 7 pass (unit tests), 12 fail (require real data and contain the bar count assertions).

Additionally, `tools/parity-test/tests/pipeline_test.rs` has 3 ignored tests that also verify bar counts.

### GREEN Phase Instructions

**CRITICAL: The tests that verify the fix are marked `#[ignore]` because they require real data files.** You MUST run them explicitly to verify your fix works.

1. First, read the failing test assertions in `tools/parity-test/tests/bar_count_parity_test.rs` and `tools/parity-test/tests/pipeline_test.rs`.
2. Read the implementation code that processes data: `tools/parity-test/src/lib.rs`, `crates/bars/src/`, `crates/book-builder/src/`, `crates/common/src/`.
3. **Diagnose**: The date offset (24h) and spurious flush are the two bugs. Find where midnight_et_ns is computed from the date string, and find the flush() logic.
4. **Fix both bugs**: (a) correct the midnight_et calculation, (b) add `snapshot_count > 0` guard to flush.
5. **Verify with ignored tests**: Run `cargo test --package parity-test -- --ignored --nocapture 2>&1` — ALL tests must pass including bar count and timestamp checks.
6. **Check regressions**: Run `cargo test 2>&1` — all non-ignored tests must still pass.
7. **Ground truth**: The reference Parquet file has 4,630 rows. The Rust pipeline must produce exactly 4,630 non-warmup bars to match.

**IMPORTANT: If some test assertions seem wrong (e.g., expected timestamps don't match your analysis), you may need to fix the tests too. The ground truth is: Rust non-warmup bar count == reference Parquet row count (4630). Tests that assert wrong expected values should be corrected.**

### Known Test Names That Must Pass

```
bar_count_parity_test::t1_bar_count_rust_equals_reference_2022_01_03
bar_count_parity_test::t1_non_warmup_bar_count_is_4630
bar_count_parity_test::t2_first_non_warmup_bar_opens_at_expected_timestamp
bar_count_parity_test::t3_last_bar_closes_before_rth_close
bar_count_parity_test::t5_total_bar_count_including_warmup_is_4680
bar_count_parity_test::t5_no_partial_bar_at_end_of_session
bar_count_parity_test::first_bar_opens_at_rth_open
pipeline_test::single_day_2022_01_03_end_to_end
pipeline_test::single_day_2022_01_03_bar_count_matches_reference
```
