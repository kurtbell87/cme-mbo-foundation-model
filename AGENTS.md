# Agents — Current State

**Updated:** 2026-02-28
**Branch:** `tdd/parity-test-pipeline`

---

## Build & Test Status

- **Build:** GREEN (0 warnings — clean)
- **Tests:** 262 passed, 0 failed, 15 ignored (`cargo test --workspace`)
- **Test breakdown:**
  - `analysis`: 6
  - `backtest`: 5
  - `bars`: 15
  - `book-builder`: 10
  - `common`: 8
  - `databento-ingest`: 11
  - `features`: 7
  - `parity_model_features` (features integration): 56
  - `parity_tool_integration` (features integration): 14
  - `bar_count_parity_test` (parity-test): 7 + 12 ignored
  - `pipeline_test` (parity-test): 35 + 3 ignored
  - `protobuf_codegen` (rithmic-client): 53
  - `xgboost_ffi_integration` (root): 27
  - `labels`: 8

## Completed Phases

| Phase | Spec | Tests | Status |
|-------|------|-------|--------|
| **0** | `.kit/docs/parity-validation-harness.md` | 70 | GREEN |
| **0b** | `.kit/docs/parity-test-pipeline.md` | 38 (35+3 ignored) | GREEN |
| **1** | `.kit/docs/xgboost-ffi.md` | 27 | GREEN |
| **2** | `.kit/docs/rithmic-protobuf-codegen.md` | 53 | GREEN |

## What Changed This Cycle (Phase 0c — Bar Count Parity Fix)

| File | Change |
|------|--------|
| `tools/parity-test/src/lib.rs` | Bar count parity fix — pipeline adjustments for off-by-one (Rust 4631 vs C++ 4630) |
| `tools/parity-test/tests/pipeline_test.rs` | Updated pipeline tests |
| `tools/parity-test/tests/bar_count_parity_test.rs` | NEW — 19 tests (7 pass, 12 ignored) for bar count validation, RTH boundaries, snapshot counts |
| `.kit/docs/bar-count-parity-fix.md` | NEW — TDD spec for bar count parity fix |

## Next Actions

1. **Diagnose and fix bar count off-by-one** — Root cause the Rust 4631 vs C++ 4630 mismatch. Check RTH boundary filter (`<` vs `<=`), snapshot alignment, flush behavior. Spec: `.kit/docs/bar-count-parity-fix.md`.
2. **Run 251-day parity validation** — Execute `tools/parity-test` against C++ reference Parquet. Fix any deviations in `crates/features/`.
3. **Phase 3: Rithmic WebSocket Client** — 5-plant WSS client with TLS, heartbeats, message routing.
4. **Phase 4: Streaming Live Pipeline** — Blocked on Phases 0+1+3.
