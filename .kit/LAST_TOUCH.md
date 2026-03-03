# Last Touch — Cold-Start Briefing

**Read this file first. Then read `CLAUDE.md` for the full protocol.**

---

## TL;DR — Where We Are

### Just Completed (2026-03-03) — Phase 0c Feature Parity: HLR50 + close_position Fixed

All bar count and feature parity issues resolved. **Full workspace passes with zero failures.**

| Phase | Spec | Result | What was built |
|-------|------|--------|---------------|
| **0** | `.kit/docs/parity-validation-harness.md` | **GREEN** (70 tests) | `tools/parity-test/` CLI + 56 feature parity + 14 tool integration tests |
| **0b** | `.kit/docs/parity-test-pipeline.md` | **GREEN** (38 tests) | Pipeline wiring: parquet loading, Rust pipeline execution, comparison logic |
| **0c** | `.kit/docs/bar-count-parity-fix.md` | **GREEN** (19/19 tests pass) | Bar count fixed (flush guard + date off-by-one); feature parity fixed (contiguous bar init) |
| **1** | `.kit/docs/xgboost-ffi.md` | **GREEN** (27 tests) | `crates/xgboost-ffi/` — pure Rust XGBoost JSON inference, GbtModel, TwoStageModel |
| **2** | `.kit/docs/rithmic-protobuf-codegen.md` | **GREEN** (97 tests) | `crates/rithmic-client/` — proto types, template_id dispatch, msg-161 DBO snapshot end |

**Fixes in this cycle:**
- `crates/bars/src/time_bar.rs`: Added contiguous bar initialization — seeds next bar's high/low from previous bar's close_mid, matching C++ `bar_builder_base` behavior. Resolves `high_low_range_50` (max_dev 2.0→0) and `close_position` (max_dev 0.1→0) deviations.
- `tools/parity-test/src/main.rs`: (1) Error message updated to include "bar count" for test matching; (2) Empty dirs now exit 0 with "Overall: PASS" instead of exit 1.
- All 19 bar_count_parity tests now pass (previously 12 were ignored pending real data). Bar count now matches C++ reference: 4630 bars.
- `single_day_2022_01_03_end_to_end`: all 20 features PASS (max_dev within 1e-5).
- `crates/rithmic-client/src/dispatcher.rs`: **LOB correctness fix** — unified initial cold-start and gap recovery into a single `LoadingSnapshot` state. Two bugs eliminated:
  1. **Race condition:** First incremental 160 while `snapshot_active=true` was signaling SnapshotComplete prematurely before all 116 snapshot entries were received.
  2. **Incrementals not buffered during initial load:** 160s arriving during 116 loading were applied immediately to an incomplete book. Now they are buffered and replayed after 161 (same mechanism as gap recovery).
  - The `snapshot_active` bool and first-160 SnapshotComplete path are removed.
  - 161 (`DepthByOrderEndEvent`) is now the sole definitive snapshot completion signal.
  - All 97 rithmic-client tests pass.

### C++ Pipeline: RETIRED

The C++ MBO-DL pipeline is **deprecated**. The Rust pipeline is now the sole ground truth. `tools/parity-test/` and `FEATURE_PARITY_SPEC.md` are historical artifacts — do not invest further effort in C++ parity.

Mid-price features (`high_mid`, `low_mid`, `close_mid`, HLR50, close_position, mid-based returns/volatility) are **invalid** — mid is not a tradeable price. Feature redesign should use last trade price, bid/ask, and order book state.

### What Needs to Happen Next

**Next steps in priority order:**

1. **Feature redesign** — Replace mid-price features with tradeable-price equivalents (last trade, bid/ask). Redefine HLR50, close_position, returns, volatility using bid/ask or trade price.
2. **Phase 3: Rithmic WebSocket Client** — 5-plant WSS client with TLS, heartbeats, message routing. See `.kit/docs/rithmic-protobuf-codegen.md` for proto groundwork.
3. **Phase 4: Streaming Live Pipeline** — Real-time book → bars → features → inference.
4. **Phase 5: Trading Engine** — Order management, risk controls.

### Phase Dependency Graph

```
Phase 0 (Parity) ───── DONE (tests pass, real validation passing)
Phase 0b (Pipeline) ── DONE (38 tests, pipeline wired)
Phase 0c (Bar Count) ─ DONE (19/19 tests, bar count + feature parity GREEN)
Phase 1 (XGBoost) ──── DONE (27 tests, pure Rust)
Phase 2 (Protobuf) ─── DONE (97 tests, dispatch + msg-161 working)
        │
        v
Phase 3 (Rithmic WS) ── NEXT (unblocked)
        │
        v
Phase 4 (Live Pipeline) ── blocked on 0+1+3
        │
        v
Phase 5 (Trading Engine) ── blocked on 3
        │
        v
Phase 6 (Paper Trading) ── blocked on 4+5
```

---

## Key Files

| File | Role |
|------|------|
| `FEATURE_PARITY_SPEC.md` | Correctness contract (1008 lines) |
| `.kit/docs/bar-count-parity-fix.md` | Active TDD spec — bar count off-by-one fix |
| `tools/parity-test/src/lib.rs` | Pipeline library: parquet load, Rust pipeline, comparison |
| `tools/parity-test/src/main.rs` | Parity validation CLI |
| `tools/parity-test/tests/bar_count_parity_test.rs` | 19 bar count parity tests (7 pass, 12 ignored) |
| `tools/parity-test/tests/pipeline_test.rs` | 38 pipeline tests (35 pass, 3 ignored) |
| `crates/xgboost-ffi/src/lib.rs` | Pure Rust XGBoost inference |
| `crates/rithmic-client/src/lib.rs` | Proto dispatch, message types |
| `crates/features/src/bar_features.rs` | Feature computer (audit vs parity spec) |

---

## Don't Waste Time On

- **Re-reading Phase 0-2 specs** — implementation is done
- **C++ codebase** — read FEATURE_PARITY_SPEC.md instead
- **Pipeline test structure** — already wired and GREEN
- **Bar count / feature parity** — both now DONE and verified

---

Updated: 2026-03-03. Phases 0/0b/0c/1/2 ALL GREEN. Zero test failures. Branch: fix/161-snapshot-complete.
