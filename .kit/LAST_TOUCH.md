# Last Touch — Cold-Start Briefing

**Read this file first. Then read `CLAUDE.md` for the full protocol.**

---

## TL;DR — Where We Are

### Just Completed (2026-02-28) — Phase 0b Pipeline Wiring GREEN

Parity test pipeline wired end-to-end on branch `feat/migration-phase0-2`. **255 tests pass, 3 ignored.**

| Phase | Spec | Result | What was built |
|-------|------|--------|---------------|
| **0** | `.kit/docs/parity-validation-harness.md` | **GREEN** (70 tests) | `tools/parity-test/` CLI + 56 feature parity + 14 tool integration tests |
| **0b** | `.kit/docs/parity-test-pipeline.md` | **GREEN** (38 tests) | Pipeline wiring: parquet loading, Rust pipeline execution, comparison logic |
| **1** | `.kit/docs/xgboost-ffi.md` | **GREEN** (27 tests) | `crates/xgboost-ffi/` — pure Rust XGBoost JSON inference, GbtModel, TwoStageModel |
| **2** | `.kit/docs/rithmic-protobuf-codegen.md` | **GREEN** (53 tests) | `crates/rithmic-client/` — proto types, template_id dispatch, RequestLogin builder |

**Key addition this cycle:** `tools/parity-test/src/lib.rs` now contains the full pipeline: load reference Parquet, run `.dbn.zst` through Rust pipeline (ingest -> book -> bars -> features), compare bar-by-bar with per-feature deviation tracking.

### What Needs to Happen Next

**Phase 0 real validation is the critical gate.** The pipeline is wired but hasn't been run against real C++ reference data. The 251-day parity validation must pass before downstream phases can proceed.

**Next steps in priority order:**

1. **Run parity test against reference data** — Execute `tools/parity-test` against C++ Parquet files at `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/.kit/results/full-year-export/` with DBN data at `/Users/brandonbell/LOCAL_DEV/MBO-DL-02152026/DATA/GLBX-20260207-L953CAPU5B/`. Fix any deviations found in `crates/features/`.
2. **Phase 3: Rithmic WebSocket Client** — Depends on Phase 2 (DONE). Build 5-plant WSS client with TLS, heartbeats, message routing.
3. **Phase 4: Streaming Live Pipeline** — Depends on Phases 0+1+3. Real-time book -> bars -> features -> inference.
4. **Phase 5: Trading Engine** — Depends on Phase 3. Order management, risk controls.

### Phase Dependency Graph

```
Phase 0 (Parity) ───── DONE (tests pass, real validation pending)
Phase 0b (Pipeline) ── DONE (38 tests, pipeline wired)
Phase 1 (XGBoost) ──── DONE (27 tests, pure Rust)
Phase 2 (Protobuf) ─── DONE (53 tests, dispatch working)
        │
        v
Phase 3 (Rithmic WS) ── NEXT
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
| `tools/parity-test/src/lib.rs` | Pipeline library: parquet load, Rust pipeline, comparison |
| `tools/parity-test/src/main.rs` | Parity validation CLI |
| `tools/parity-test/tests/pipeline_test.rs` | 38 pipeline tests (35 pass, 3 ignored for real data) |
| `crates/xgboost-ffi/src/lib.rs` | Pure Rust XGBoost inference |
| `crates/rithmic-client/src/lib.rs` | Proto dispatch, message types |
| `crates/features/src/bar_features.rs` | Feature computer (audit vs parity spec) |
| `.kit/docs/parity-test-pipeline.md` | Phase 0b spec |

---

## Don't Waste Time On

- **Build verification** — 255 tests pass, GREEN phase verified
- **Re-reading Phase 0-2 specs** — implementation is done
- **C++ codebase** — read FEATURE_PARITY_SPEC.md instead
- **Pipeline test warnings** — 2 unused import warnings in pipeline_test.rs, cosmetic only

---

Updated: 2026-02-28. Phases 0/0b/1/2 GREEN complete. 255 tests. Branch: feat/migration-phase0-2.
