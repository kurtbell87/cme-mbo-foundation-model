# Research Questions — MBO Tokenization

## 1. Goal

Can a transformer/SSM learn the generative grammar of MBO event sequences, and does that grammar encode directional information about future price movement?

**Success looks like:** A pretrained sequence model whose frozen hidden states, read out by a linear probe, predict sign(future price change) above chance under CPCV with proper purging and embargo. Failure is conclusive: linear probe on pretrained representations ≤ linear probe on raw embeddings.

---

## 2. Constraints

| Constraint | Decision |
|------------|----------|
| Framework  | PyTorch (training), Rust (tokenization + data pipeline) |
| Compute    | Local first (M-series Mac). EC2 only after local signal. |
| Data       | 2022 MES MBO, 312 .dbn.zst files, 49.2 GB (S3) |
| Baseline   | Order-5 Markov: ppl 1.98 (0.99 bits/token) |

---

## 3. Non-Goals (This Phase)

- Live trading integration
- Multi-instrument cross-signal (deferred until single-instrument grammar is understood)
- Hyperparameter sweeps (architecture search is premature before Gate 2)
- BPE / hierarchical tokenization (don't compress until you know what to preserve)

---

## 4. Open Questions

| Priority | Question | Status | Parent | Blocker | Decision Gate | Experiment(s) |
|----------|----------|--------|--------|---------|---------------|---------------|
| P0 | Does MBO event grammar have compositional structure beyond 5-gram? | In progress | — | — | Transformer ppl < 1.98 (Markov-5) | exp-001-grammar-gate |
| P0 | Do pretrained representations encode directional information? | Not started | P0 Q1 | Gate 1 must pass | Linear probe > raw embeddings under CPCV | exp-002-direction-gate |
| P1 | What is the characteristic dependency length of LOB grammar? | Partially answered | P0 Q1 | — | Markov sweep curve shape; attention pattern analysis | markov-baseline (done), exp-001 attention viz |
| P1 | Where in the token stream does directional information concentrate? | Not started | P0 Q2 | Gate 2 | Per-position probe accuracy breakdown | exp-002 extension |
| P2 | Mamba vs sparse attention: which architecture better fits LOB memory structure? | Not started | P1 Q1 | Dependency length answer | Compare perplexity at matched param count | exp-003 |

---

## 5. Answered Questions

| Question | Answer Type | Answer | Evidence |
|----------|-------------|--------|----------|
| Is there learnable structure in MBO token sequences? | CONFIRMED | Yes — ppl 1.98 with only 5-gram context on 126-token vocab. 94.5% of prices in ±50 tick range. Entropy ~1 bit/token (86% redundant). | markov-baseline on 74.5M MES tokens |
| Does Markov perplexity improve beyond order 5? | REFUTED | No — perplexity rises after order 5 due to data sparsity (18.9M unique contexts at order 20 for 59.6M training tokens). Does NOT mean no long-range structure — means Markov can't test it. | markov-baseline sweep |

---

## 6. Working Hypotheses

- **H1 (Grammar):** MBO event sequences follow a learnable grammar with compositional structure. Evidence: Markov-5 achieves ppl 1.98, stream is 86% redundant, clear "phrases" visible in token output (TRADE-FILL-CANCEL, sweep patterns).
- **H2 (Directional encoding):** The grammar is asymmetric around future price moves — certain event patterns systematically precede directional movement. Evidence: NONE. Three prior research threads (bar-level, tick-level, event-LOB) found zero directional signal in static LOB features. The open question is whether sequential structure encodes what snapshots don't.
- **H3 (Anomaly detection):** A model that learns "normal" LOB grammar can detect anomalies (deviations from expected patterns), and those anomalies correlate with informed trading / directional intent. Evidence: Theoretical only. Unfalsifiable until tested.

---

## 7. Gate 2 Specification — Directional Encoding

### Setup

1. **Pretrain** a small transformer (4 layers, 128 dim, 4 heads) on next-token prediction over full 2022 MES dataset. Training objective: autoregressive cross-entropy on the token stream. Context window: 512 tokens.

2. **Freeze** all pretrained weights.

3. **Extract** hidden state vectors at each COMMIT token (these mark book-consistent boundaries where direction prediction is meaningful).

4. **Label** each extraction point with sign(mid_price[t+N] - mid_price[t]) for horizon N ∈ {50, 200, 1000} events.

5. **Linear probe**: fit logistic regression (no hidden layers) from frozen hidden state → direction label. Use L2 regularization (no feature selection tricks).

6. **CPCV evaluation** with purging and embargo:
   - 6 groups, 2 test groups per split → 15 folds
   - Purge: remove train samples within 5000 events of any test boundary
   - Embargo: 1000 events after each purge boundary
   - Report: mean accuracy, Sharpe-equivalent metric, per-fold distribution

### Kill Criteria

| Outcome | Interpretation | Action |
|---------|---------------|--------|
| Linear probe accuracy ≤ raw embedding probe accuracy across all horizons | Pretrained representations do NOT encode direction. Grammar is syntactically rich but directionally symmetric. | **STOP.** The compositional structure hypothesis for alpha is dead. Document and move on. |
| Linear probe accuracy > raw embedding probe by ≥ 2% on any horizon, AND positive in >60% of CPCV folds | Directional information IS encoded in the grammar. The representation captures something snapshots miss. | **PROCEED** to architecture comparison (Mamba vs sparse attention) and execution-aware evaluation. |
| Probe accuracy > raw by 1-2%, inconsistent across folds | Weak/ambiguous signal. Possibly real but not robust. | **Investigate**: per-position breakdown, regime conditioning, alternative horizons. One more experiment, then decide. |

### Control: Raw Embedding Probe

Run the identical linear probe on the *untrained* model's hidden states (random initialization, no pretraining). This controls for the possibility that the token embedding structure itself carries directional information (which would mean the tokenization, not the grammar learning, is doing the work).

Actually run THREE probes:
1. **Random init** — untrained transformer hidden states
2. **Raw one-hot** — just the token ID at the COMMIT position (no context)
3. **Pretrained** — frozen pretrained hidden states

Comparison: Pretrained must beat both controls. If raw one-hot beats pretrained, the sequential context is hurting, not helping — abort.
