# Research Questions

Last updated: 2026-03-11

---

## 1. Goal

Find a positive-expectancy trading strategy on CME E-mini/Micro futures using MBO (L3) order book data, validated through rigorous CPCV with serial execution.

**Success looks like:** At least one geometry with >60% of CPCV folds positive, expectancy CI lower bound > 0, DSR significant at p < 0.05, profit factor > 1.0 after costs.

---

## 2. Constraints

| Constraint | Decision |
|------------|----------|
| Data | 1 year MES MBO 2022 (312 days, 49.2 GB .dbn.zst on S3) |
| Compute | Local Mac for prototyping; EC2 spot (c7a) for CPU work, RunPod (RTX 4090/H200) for GPU training |
| Budget | Minimize EC2/data spend until local experiments show signal |
| Execution | Serial only (one position at a time), entry at bid/ask |
| Validation | CPCV 45-fold, DSR gate, calibration, Ljung-Box |

---

## 3. Non-Goals (This Phase)

- Multi-day holding periods
- Options / volatility strategies
- HFT (sub-millisecond) — we are event-level but not co-located
- New data purchases until local experiments show promise

---

## 4. Open Questions

| Priority | Question | Status | Blocker | Decision Gate |
|----------|----------|--------|---------|---------------|
| **P0** | **Does pretrained transformer + book state recon find directional signal?** | **Phase 3 READY.** Phase 1 (ppl 1.864) and Phase 2 (Gate 1.5) passed. Code+configs ready: `phase3_signal_check.py`, `cloud-run-phase3.toml`. | Phase 3 launch on RunPod | B beats majority by >= 2pp in >= 60% of CPCV folds on next BBO change direction |
| P1 | Does cross-instrument signal exist (NQ imbalance → ES price)? | Not started. `tools/lead-lag` built but no experiment run. | Need 1 day ES+NQ MBO | If lead/lag > 50ms and significant, new feature class |
| P1 | Does interarrival time encoding improve LM perplexity? | Not started — add `[TIME_DELTA]` sub-token (16 log-scale bins, vocab 126→142, tokens/event 4.88→5.88). All three major papers encode timing; we don't. | Needs `.timestamps` sidecar | Ablate after Phase 3. If ppl improves, adopt permanently. |
| P2 | Is signal regime-dependent (appears in high-vol, averages to zero overall)? | Not started | — | If any regime shows >5pp lift, condition all models on regime |
| P2 | Does continuous-time RoPE improve over learned positional embeddings for irregular MBO events? | Not started — needs `.timestamps` sidecar first (~2 hrs Rust), then custom attention integration (~1 day). LOBERT approach: `phi_i(t) = t * theta_i`. | `.timestamps` sidecar | Ablate post-Phase 3. If ppl improves by >1%, adopt. |
| P2 | Does bps-normalized pricing enable cross-instrument pretraining (MES + ES + NQ)? | Not started — TradeFM approach. Replace tick-relative price with `(order_price - mid) / mid` in bps. | Multi-instrument data, Phase 7+ | If cross-instrument model beats single-instrument by >2pp on direction, adopt. |

---

## 5. Answered Questions

| Question | Answer | Evidence |
|----------|--------|----------|
| Can 5s bar features predict short-term direction? | REFUTED | Thread 01-02: 45/45 folds negative, win rate = null |
| Does higher resolution (event-level) fix bar-level failures? | REFUTED (for static features) | Thread 03: 0/45 folds positive with 44 LOB features |
| Are OFI/trade flow/cancel asymmetry predictive univariately? | REFUTED | Gate test: near-null lift across all geometries |
| Does XGBoost find nonlinear combinations of LOB features? | REFUTED | Thread 03: 35M training rows, 0/45 positive folds |
| Do sequential BBO features + cooldown fix the autocorrelation problem? | REFUTED | Thread 06: 45/45 negative across 7 cooldown values (0–422) |
| Can XGBoost find signal in *any* hand-engineered MBO features? | REFUTED | Threads 03, 04, 06: static LOB, flow EMA, seq-features all 0/45 |
| Is the BookBuilder correct? | CONFIRMED | book-verify: 99.91% match vs Databento MBP-10 (54.3M checks) |
| Do event *sequences* predict better than static snapshots (via frozen probing)? | REFUTED (frozen probe) | Thread 05 Gate 2: raw one-hot beats pretrained at all horizons (K=50,200,1000). Grammar is structural, not directional. Fine-tuning (Phase 3) still untested. |
| Do deeper sequence models find signal XGBoost misses? | XGBoost EXHAUSTED | 4 feature sets, 0/45 positive folds. Tree models on tabular features are dead. Transformer fine-tuning is the remaining path. |
| Can the model reconstruct pre-batch book state from ~105 events of context? | CONFIRMED | Phase 2 Gate 1.5: pre-batch size acc 67.0%, spread NM 86.5%, imb MAE 0.0656. Outperforms post-batch on spread/imbalance. LOBS5 risk did not materialize. |
| Does the transformer learn MBO grammar beyond n-gram statistics? | CONFIRMED | Phase 1: ppl 1.864 vs Markov-5 1.984 (6.1% improvement), 6.5M params on 3.93B tokens |

---

## 6. Working Hypotheses

- **H1:** The predictive content in MBO data is in the *sequence* of events, not the instantaneous book state. Static snapshots are necessary but not sufficient. **Partially supported:** transformer learns real grammar (Phase 1) and encodes book state (Phase 2), but frozen representations showed no directional advantage over raw tokens (Gate 2). Fine-tuning (Phase 3) is the remaining test.
- **H2:** Cross-instrument microstructure (ES/NQ lead-lag) may contain signal invisible to single-instrument analysis. **Untested.**
- **H3:** Signal may be regime-dependent — averaging to zero unconditionally but present in specific volatility/trend states. **Untested.**
- **H4:** Simpler targets (next BBO direction) may be more predictable than triple-barrier. **Phase 3 tests this directly** — "next BBO change direction" with 15-fold CPCV.
