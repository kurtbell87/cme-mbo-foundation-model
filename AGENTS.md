# Agents — Current State

**Updated:** 2026-02-28
**Branch:** `feat/migration-phase0-2`

---

## Build & Test Status

- **Build:** GREEN (2 warnings — unused imports in `tools/parity-test/tests/pipeline_test.rs`)
- **Tests:** 255 passed, 0 failed, 3 ignored (`cargo test --workspace`)
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

## What Changed This Cycle (Phase 0b — Pipeline Wiring)

| File | Change |
|------|--------|
| `tools/parity-test/Cargo.toml` | Added pipeline deps (features, bars, book-builder, databento-ingest, parquet, arrow) |
| `tools/parity-test/src/lib.rs` | NEW — Pipeline library: parquet loading, Rust pipeline execution, bar-by-bar comparison |
| `tools/parity-test/tests/pipeline_test.rs` | NEW — 38 tests covering comparison logic, parquet loading, day matching, pipeline execution |
| `crates/features/src/bar_features.rs` | Feature computation updates |
| `crates/xgboost-ffi/src/lib.rs` | XGBoost inference updates |
| `tests/xgboost_ffi_integration.rs` | Integration test updates |
| `Cargo.lock` | Dependency resolution |

## Next Actions

1. **Run 251-day parity validation** — Execute `tools/parity-test` against C++ reference Parquet at `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/`. Fix any deviations in `crates/features/`.
2. **Phase 3: Rithmic WebSocket Client** — 5-plant WSS client with TLS, heartbeats, message routing.
3. **Phase 4: Streaming Live Pipeline** — Blocked on Phases 0+1+3.
