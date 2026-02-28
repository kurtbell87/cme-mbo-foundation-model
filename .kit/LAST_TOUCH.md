# Last Touch — Cold-Start Briefing

**Read this file first. Then read `CLAUDE.md` for the full protocol.**

---

## TL;DR — Where We Are

### In Progress (2026-02-28) — C++ to Rust Migration, Phases 0-2

Three parallel TDD cycles running on branch `feat/migration-phase0-2`:

| Phase | Spec | Status | What it does |
|-------|------|--------|-------------|
| **0** | `.kit/docs/parity-validation-harness.md` | **TDD RUNNING** | `tools/parity-test/` — compares Rust features vs C++ reference Parquet |
| **1** | `.kit/docs/xgboost-ffi.md` | **TDD RUNNING** | `crates/xgboost-ffi/` — XGBoost C API FFI for model inference |
| **2** | `.kit/docs/rithmic-protobuf-codegen.md` | **TDD RUNNING** | Proto compilation + template ID routing in `crates/rithmic-client/` |

### Context

This is the production Rust port of the C++ MBO-DL research pipeline. The C++ repo (`../MBO-DL-02152026/`) is the correctness reference. `../MBO-DL-02152026/FEATURE_PARITY_SPEC.md` (1008 lines) is the definitive contract for feature computation.

### What's Already Built (3 commits on main)

- 10-crate workspace: `common`, `book-builder`, `bars`, `features`, `labels`, `backtest`, `analysis`, `models`, `databento-ingest`, `rithmic-client`
- Batch export tool: `tools/bar-feature-export/` (733 LOC) — full pipeline from `.dbn.zst` → Parquet
- Additional tools: `oracle-expectancy`, `subordination-test`, `info-decomposition`
- `models` and `rithmic-client` are stubs awaiting Phase 1 and Phase 2/3

### Phase Dependency Graph

```
Phase 0 (Parity) ──────┐
                        v
Phase 1 (XGBoost) ──────┤
                        v
Phase 2 (Protobuf) ─────┤
        │               v
        v        Phase 4 (Live Pipeline)
Phase 3 (Rithmic WS) ──┤
                        v
                 Phase 5 (Trading Engine)
                        │
                        v
                 Phase 6 (Paper Trading)
```

### After Phases 0-2 Complete

1. **Phase 3:** Rithmic WebSocket client (depends on Phase 2)
2. **Phase 4:** Streaming live pipeline (depends on Phases 0, 1, 3)
3. **Phase 5:** Trading engine (depends on Phase 3)
4. **Phase 6:** Integration & paper trading

---

## Key Files

| File | Role |
|------|------|
| `.kit/docs/parity-validation-harness.md` | Phase 0 TDD spec |
| `.kit/docs/xgboost-ffi.md` | Phase 1 TDD spec |
| `.kit/docs/rithmic-protobuf-codegen.md` | Phase 2 TDD spec |
| `../MBO-DL-02152026/FEATURE_PARITY_SPEC.md` | Correctness contract (1008 lines) |
| `crates/features/src/bar_features.rs` | Rust feature computer (audit vs parity spec) |
| `tools/bar-feature-export/src/main.rs` | Batch pipeline reference for streaming |

---

## Don't Waste Time On

- **Build verification** — TDD sub-agents handle all builds and tests
- **C++ codebase exploration** — read FEATURE_PARITY_SPEC.md instead
- **Proto file analysis** — Phase 2 TDD agent copies and compiles them

---

Updated: 2026-02-28. Three parallel TDD cycles launching (Phases 0, 1, 2).
