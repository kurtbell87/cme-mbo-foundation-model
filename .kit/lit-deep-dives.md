# Literature Deep Dives — Actionable Technical Notes
## March 2026

Three deep-dive analyses on papers most relevant to Phase 2 and beyond. For the full 13-paper landscape review, see `lit-review-lob-dl.md`.

---

## 1. TradeFM (JPMorgan, arXiv:2602.23784)

### Mixed-Radix 16,384-Token Composite Encoding

Single token per event. Five fields packed via mixed-radix positional notation:

```
i_trade = (i_a * n_s * n_dp * n_v * n_dt)
        + (i_s * n_dp * n_v * n_dt)
        + (i_dp * n_v * n_dt)
        + (i_v * n_dt)
        + i_dt
```

Cardinalities: n_a=2, n_s=2, n_dp=16, n_v=16, n_dt=16. Total: 16,384 tokens.

### Scale-Invariant Features

| Field | Transform | Bins | Binning |
|-------|-----------|------|---------|
| Price depth | `(p_order - p_mid_hat) / p_mid_hat` (bps) | 16 | Quantile (equal-frequency) |
| Volume | `log(1 + V_t)` | 16 | Equal-width on log scale |
| Interarrival time | `w_t - w_{t-1}` (seconds) | 16 | Equal-width (appears log-scale) |
| Action | raw | 2 | add / cancel only |
| Side | raw | 2 | buy / sell |

`p_mid_hat` is an EW-VWAP estimate, NOT true book mid. Half-life value NOT SPECIFIED (reproducibility gap).

### Context Width Comparison

| | Ours (126-vocab factored) | TradeFM (16,384-vocab composite) |
|--|---------------------------|----------------------------------|
| Tokens/event | ~4.88 | 1.0 |
| Events in 1024-token context | ~105 (~2-5 sec MES) | 1024 (~2-5 min liquid equity) |
| Price reference | Exact L3 mid from book reconstruction | EW-VWAP estimate (noisy) |
| Price unit | Integer ticks (instrument-specific) | Basis points (instrument-agnostic) |
| Actions modeled | 6 (add/cancel/modify/trade/fill/clear) | 2 (add/cancel only) |
| Compositionality | High (shared embeddings) | Low (each combination unique) |
| Book state | Available via sidecar | Not modeled |

**Key trade-off:** TradeFM sees 10x more events in the same context window (massive advantage for long-range dependencies). Our factored encoding enables compositional generalization but compresses temporal horizon.

**At 8M params:** Our factored encoding wins (compositionality > context width for small models). At 500M+ params, their composite encoding likely wins (model can memorize all 16K token semantics, and wider context dominates).

### What TradeFM Gets Wrong / Doesn't Model

- No order IDs (no queue position tracking)
- No book state reconstruction
- No trade/fill events (fills emerge from external simulator)
- No modify/clear events
- No multi-event atomic batches (no COMMIT concept)
- Partial observation only
- EW-VWAP half-life unspecified
- No futures evaluation

### Implications for Multi-Instrument Futures Tokenization

Three options for supporting ES + CL + ZC etc:

- **Option A (bps normalization):** Replace tick-relative price with `(order_price - mid) / mid` in bps. Quantile-bin across instruments. Proven to generalize (TradeFM Figure 7). Loses exact tick-level precision.
- **Option B (tick-relative + instrument conditioning):** Keep tick encoding, normalize by tick size. All instruments use same ±50 tick range. Preserves tick structure but "1 tick of MES" != "1 tick of CL" economically.
- **Option C (hybrid):** Pretrain with bps (TradeFM approach), fine-tune with ticks per instrument. Most flexible, adds complexity.

**Priority:** Phase 7+ (single-instrument MES first).

---

## 2. LOBERT Continuous-Time RoPE (Tampere, arXiv:2511.12563)

### The Math

Standard RoPE replaces discrete position index `n` with cumulative time `t`:

```
theta_i = 10000^(-2i/d_h)     for i in {0, 1, ..., d_h/2 - 1}
phi_i(t) = t * theta_i

q'_{2i}   = q_{2i} * cos(phi_i(t)) - q_{2i+1} * sin(phi_i(t))
q'_{2i+1} = q_{2i} * sin(phi_i(t)) + q_{2i+1} * cos(phi_i(t))
```

Same rotation applied to key vectors. Attention dot product becomes `f(q_m, k_n, t_m - t_n)` — relative time encoding emerges naturally.

**Time unit NOT specified in paper** — critical reproducibility gap. Best guess: cumulative milliseconds from sequence start (keeps t in ~0..500 range like standard position indices).

**LOBERT uses BOTH learned `pos_emb` AND continuous-time RoPE simultaneously.**

### Hybrid Decoding Head

4-head design:
1. Token classification (293-class CE)
2. Price regression (MSE, dual-input: `concat(token_logits, hidden_state)`)
3. Volume regression (same structure)
4. Time regression (same structure)

Combined inference: token class bounds the regressor. Gives best distributional fidelity.

PLGS scaling parameters: Price: tau_start=10, tau_max=20, tau_clip=1000 ticks. Volume: 200/400/1500. Time: 1/50/250 ms.

### Implementation Cost

- **`.timestamps` sidecar:** ~2 hrs Rust work. Emit `ts_event` (u64) at each COMMIT position. Follow `.mids` pattern. The tokenizer's `feed_event()` already receives this but discards it.
- **RoPE integration:** ~1 day. Must replace `nn.TransformerEncoder` with custom attention that applies rotation. Cannot use PyTorch's built-in transformer with standard RoPE — need to modify `_scaled_dot_product_attention`.
- **Total:** 2-3 days end-to-end.

### Priority

**AFTER Phase 3.** Continuous-time RoPE is an optimization, not a prerequisite for Phase 2. The positional encoding issue only matters when event spacing is highly irregular — which it is for MBO data, but the baseline learned embeddings may be sufficient for the reconstruction task.

---

## 3. LOBS5 Dual-Branch + MarS Additive Conditioning

### LOBS5: Book State as INPUT (arXiv:2309.00638)

Architecture: three-stage pipeline.
1. **Message S5 branch:** tokenized messages (22n tokens) → embedding → S5 layers
2. **Book S5 branch:** L2 "volume images" (P+1 sparse vector per message) → S5 layers → Dense projection
3. **Merge:** `jnp.repeat` book along time axis (1 book state → 22 tokens), concat on feature axis → fusion S5 layers → logits

**Critical finding from the paper:** "Training and validation loss drastically improves when using book data in addition to message sequences."

**Translation for Phase 2:** This means reconstructing book state from messages alone (our pre-batch head) may require very long contexts. LOBS5 uses 500 messages × 22 tokens = 11,000 tokens of context plus explicit book state input. Our pre-batch head sees ~105 events in a 512-token context window — significantly less.

**RISK FLAG:** The pre-batch reconstruction head (predicting book state BEFORE processing the current batch, purely from prior context) is the harder of our two heads. LOBS5 explicitly confirms this is hard. The post-batch head (predicting book state AFTER processing the batch) should be easier — it's a local computation over the batch's events.

### MarS/LMM: Additive Book Conditioning (arXiv:2409.07486)

Embedding at each position:
```
h = emb(order_token) + linear_proj(LOB_20d_volumes) + emb(mid_price_ticks)
```

- `LOB_20d_volumes`: 20 continuous values (10 bid + 10 ask level volumes) → `nn.Linear(20, d_model)`
- `mid_price_ticks`: integer tick changes since market open → embedding lookup
- Additive (not concatenative) — book information blends into the same representation space

### MarS Scaling Laws

| Model size | Loss (32B tokens) |
|------------|-------------------|
| 2M | ~7.05 |
| 5M | ~7.01 |
| 10M | ~6.98 |
| 18M | ~6.96 |
| 44M | ~6.94 |
| 101M | ~6.92 |
| 221M | ~6.91 |
| 1.02B | ~7.00 |

All curves still falling steeply with more tokens — data-limited, not parameter-limited. **Our 8M model on 3.93B tokens is well-calibrated.** Going to 50M+ would require more data.

### Phase 2 Fallback: MarS-Style Additive Conditioning

If Gate 1.5 pre-batch reconstruction head fails (cannot recover book state from ~105 events of context):

**Fallback implementation (2-4 hours):**
```python
# At each COMMIT position:
h_commit += nn.Linear(12, d_model)(book_state_prev)
```

Where `book_state_prev` is the 12-field sidecar from the previous COMMIT. This gives the model explicit access to the prior book state — exactly what LOBS5 and MarS do.

**Keep the post-batch reconstruction head even with input conditioning.** The post-batch head tests whether the model can track state changes within a batch — a different (and easier) capability than recovering state from long context.

### LOBS5 vs MarS Conditioning: Which Is Simpler?

| | LOBS5 (separate branch) | MarS (additive) |
|--|------------------------|-----------------|
| Integration effort | 1-2 days (new branch, concat) | 2-4 hours (one linear layer) |
| Representation impact | Book in separate subspace until fusion | Book blended into event representations |
| Parameter cost | ~d_model × d_book + S5 layers | d_model × 12 (single linear layer) |

**MarS-style is simpler and sufficient for Phase 2.** LOBS5-style separate branch is more expressive but overkill for an auxiliary objective.

---

## 4. Interarrival Time Encoding (Cross-Paper Synthesis)

All three major papers (TradeFM, LOBERT, LOBS5) encode inter-event timing. We don't.

### Proposed: [TIME_DELTA] Sub-Token

Add a 17th token class: 16 log-scale bins for interarrival time.

| Bin | Range |
|-----|-------|
| 0 | 0 (simultaneous) |
| 1 | 1-10 µs |
| 2 | 10-100 µs |
| 3 | 100 µs - 1 ms |
| ... | (geometric progression) |
| 14 | 10-100 s |
| 15 | > 100 s |

**Impact:** Tokens/event increases from ~4.88 to ~5.88 (~17% context reduction). Vocab increases from 126 to 142.

**Priority:** Quick ablation AFTER Phase 2 Gate 1. If perplexity improves, adopt permanently. If not, the model already captures timing implicitly through event patterns.

**Prerequisite:** `.timestamps` sidecar (same as RoPE prerequisite).

---

## 5. Summary: Extractable Components by Priority

| Component | Source | Phase 2 Needed? | Effort | Priority |
|-----------|--------|-----------------|--------|----------|
| MarS-style additive conditioning | MarS | Only if Gate 1.5 fails | 2-4 hrs | Contingency |
| `.timestamps` sidecar | LOBERT/all | No | 2 hrs Rust | Post-Phase 2 |
| `[TIME_DELTA]` sub-token | TradeFM/LOBS5 | No | 4 hrs Rust + ablation | Post-Phase 2 Gate 1 |
| Continuous-time RoPE | LOBERT | No | 2-3 days | Post-Phase 3 |
| bps-normalized pricing | TradeFM | No | 1 day Rust | Phase 7+ (multi-instrument) |
