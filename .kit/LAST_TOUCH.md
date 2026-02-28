# Last Touch — Cold-Start Briefing

**Read this file first. Then read `CLAUDE.md` for the full protocol.**

---

## TL;DR — Where We Are

### Just Completed (2026-02-28) — Phases 0-2 GREEN

Three parallel TDD cycles completed on branch `feat/migration-phase0-2`. Commit `0c58c6e`. **150 tests pass.**

| Phase | Spec | Result | What was built |
|-------|------|--------|---------------|
| **0** | `.kit/docs/parity-validation-harness.md` | **GREEN** (70 tests) | `tools/parity-test/` CLI + 56 feature parity + 14 tool integration tests |
| **1** | `.kit/docs/xgboost-ffi.md` | **GREEN** (27 tests) | `crates/xgboost-ffi/` — pure Rust XGBoost JSON inference, GbtModel, TwoStageModel |
| **2** | `.kit/docs/rithmic-protobuf-codegen.md` | **GREEN** (53 tests) | `crates/rithmic-client/` — proto types, template_id dispatch, RequestLogin builder |

**Key architectural decision:** Phase 1 implemented XGBoost inference as **pure Rust** (tree traversal from JSON model format) instead of C FFI. No `libxgboost.dylib` dependency needed.

### What Needs to Happen Next

**Phase 0 is NOT fully validated yet.** The parity test tool has CLI and structure, but hasn't been run against real C++ reference Parquet data. The 251-day parity validation gate still needs to pass.

**Next steps in priority order:**

1. **Run parity test against reference data** — Execute `tools/parity-test` against the C++ Parquet files from the full-year export. Fix any deviations found in `crates/features/`.
2. **Phase 3: Rithmic WebSocket Client** — Depends on Phase 2 (DONE). Build 5-plant WSS client with TLS, heartbeats, message routing.
3. **Phase 4: Streaming Live Pipeline** — Depends on Phases 0+1+3. Real-time book → bars → features → inference.
4. **Phase 5: Trading Engine** — Depends on Phase 3. Order management, risk controls.

### Phase Dependency Graph

```
Phase 0 (Parity) ───── DONE (tests pass, real validation pending)
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
| `crates/xgboost-ffi/src/lib.rs` | Pure Rust XGBoost inference |
| `crates/rithmic-client/src/lib.rs` | Proto dispatch, message types |
| `crates/rithmic-client/src/rti.rs` | Generated protobuf Rust types |
| `tools/parity-test/src/main.rs` | Parity validation CLI |
| `crates/features/src/bar_features.rs` | Feature computer (audit vs parity spec) |

---

## Don't Waste Time On

- **Build verification** — 150 tests pass, GREEN phase verified
- **Re-reading Phase 0-2 specs** — implementation is done
- **C++ codebase** — read FEATURE_PARITY_SPEC.md instead

---

Updated: 2026-02-28. Phases 0-2 GREEN complete. 150 tests. Branch: feat/migration-phase0-2.
