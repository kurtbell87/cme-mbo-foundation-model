# Last Touch — Cold-Start Briefing

**Read this file first. Then read `CLAUDE.md` for the full protocol.**

---

## TL;DR — Where We Are

### Just Completed (2026-02-28) — Phase 0c Bar Count Parity Fix (TDD Tests)

Bar count parity TDD tests written on branch `tdd/parity-test-pipeline`. **262 tests pass, 15 ignored.**

| Phase | Spec | Result | What was built |
|-------|------|--------|---------------|
| **0** | `.kit/docs/parity-validation-harness.md` | **GREEN** (70 tests) | `tools/parity-test/` CLI + 56 feature parity + 14 tool integration tests |
| **0b** | `.kit/docs/parity-test-pipeline.md` | **GREEN** (38 tests) | Pipeline wiring: parquet loading, Rust pipeline execution, comparison logic |
| **0c** | `.kit/docs/bar-count-parity-fix.md` | **IN PROGRESS** (19 tests: 7 pass, 12 ignored) | Bar count parity tests: RTH boundary, snapshot count, warmup/fwd_return filtering |
| **1** | `.kit/docs/xgboost-ffi.md` | **GREEN** (27 tests) | `crates/xgboost-ffi/` — pure Rust XGBoost JSON inference, GbtModel, TwoStageModel |
| **2** | `.kit/docs/rithmic-protobuf-codegen.md` | **GREEN** (53 tests) | `crates/rithmic-client/` — proto types, template_id dispatch, RequestLogin builder |

**Key addition this cycle:** `tools/parity-test/tests/bar_count_parity_test.rs` — 19 TDD tests targeting bar count off-by-one (Rust 4631 vs C++ 4630). 7 unit tests pass, 12 ignored tests require real reference data.

### What Needs to Happen Next

**Bar count off-by-one is the BLOCKER.** The Rust pipeline produces 4631 bars vs the C++ reference's 4630. Root cause must be diagnosed and fixed before 251-day parity validation can proceed.

**Next steps in priority order:**

1. **Diagnose and fix bar count off-by-one** — Check RTH boundary filter (`<` vs `<=` on close), snapshot alignment (floor vs ceil), flush behavior. Files to examine: `tools/parity-test/src/lib.rs`, `crates/bars/src/time_bar_builder.rs`, `crates/book-builder/src/`. See spec: `.kit/docs/bar-count-parity-fix.md`.
2. **Run parity test against reference data** — Execute `tools/parity-test` against C++ Parquet files. Fix any deviations found in `crates/features/`.
3. **Phase 3: Rithmic WebSocket Client** — Depends on Phase 2 (DONE). Build 5-plant WSS client with TLS, heartbeats, message routing.
4. **Phase 4: Streaming Live Pipeline** — Depends on Phases 0+1+3. Real-time book -> bars -> features -> inference.
5. **Phase 5: Trading Engine** — Depends on Phase 3. Order management, risk controls.

### Phase Dependency Graph

```
Phase 0 (Parity) ───── DONE (tests pass, real validation pending)
Phase 0b (Pipeline) ── DONE (38 tests, pipeline wired)
Phase 0c (Bar Count) ─ IN PROGRESS (7/19 tests pass, off-by-one to fix)
Phase 1 (XGBoost) ──── DONE (27 tests, pure Rust)
Phase 2 (Protobuf) ─── DONE (53 tests, dispatch working)
        │
        v
Phase 3 (Rithmic WS) ── NEXT (after bar count fix)
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

- **Build verification** — 262 tests pass, build is clean (0 warnings)
- **Re-reading Phase 0-2 specs** — implementation is done
- **C++ codebase** — read FEATURE_PARITY_SPEC.md instead
- **Pipeline test structure** — already wired, focus on bar count root cause

---

Updated: 2026-02-28. Phases 0/0b/1/2 GREEN complete, 0c in progress. 262 tests. Branch: tdd/parity-test-pipeline.
