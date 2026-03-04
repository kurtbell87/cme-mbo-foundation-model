# Agents — Current State

**Updated:** 2026-03-04
**Branch:** main (research/03 work on main)

---

## Build & Test Status

- **Build:** GREEN (0 warnings — clean)
- **Tests:** All passing (`cargo test --workspace`)

## Completed Phases

| Phase | Spec | Tests | Status |
|-------|------|-------|--------|
| **0** | `.kit/docs/parity-validation-harness.md` | 70 | GREEN |
| **0b** | `.kit/docs/parity-test-pipeline.md` | 38 (35+3 ignored) | GREEN |
| **1** | `.kit/docs/xgboost-ffi.md` | 27 | GREEN |
| **2** | `.kit/docs/rithmic-protobuf-codegen.md` | 53 | GREEN |
| **3** | Rithmic WebSocket Client | — | DONE |

## What Changed This Cycle (Distributed Imbalance CPCV)

| File | Change |
|------|--------|
| `tools/event-backtest/src/main.rs` | Added `--fold-range`, `--mode aggregate`, `--ofi-threshold`, `--geometry` CLI flags |
| `tools/event-backtest/src/fold_runner.rs` | `run_imbalance_fold()` — OFI-filtered fold execution |
| `tools/event-backtest/src/data.rs` | `load_day_imbalance()` — streaming OFI filter + geometry filter |
| `tools/event-backtest/src/statistics.rs` | Aggregation logic for merging partial fold results |
| `research/03-event-lob-probability/scripts/ec2-launch-imbalance-cpcv-distributed.sh` | NEW — distributed launch across N× c7a instances |
| `research/03-event-lob-probability/scripts/ec2-launch-imbalance-cpcv.sh` | NEW — single-instance imbalance CPCV |

## Next Actions

1. **Implement `--holdout-pct`** — 80/20 chronological day-level holdout to reduce per-fold memory and provide out-of-sample validation
2. **Spot vCPU quota increase** — AWS support ticket: 128 → 512 vCPU for c7a family
3. **Relaunch distributed imbalance CPCV** — 8× c7a.16xlarge, ~2-3 hours, ~$22
4. **Validation pipeline** — DSR gate, expectancy CI, negative fold fraction, Ljung-Box, profit factor
5. **Phase 4: Streaming Live Pipeline** — Blocked on Phase 3 live test passing
