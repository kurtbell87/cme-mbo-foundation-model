# MBO Grammar: A Foundation Model for CME Futures Microstructure

A research project investigating whether transformer pretraining on tokenized Level 3 (Market-by-Order) event sequences from CME E-mini S&P 500 futures can learn representations that encode directional information — after exhaustively ruling out hand-engineered features.

**Status:** Phase 2 of 3 complete. The model learns real market grammar (perplexity 1.864 vs Markov-5 baseline 1.984) and recovers full book state from raw event sequences alone (67.4% size accuracy vs 23.6% baseline). Phase 3 — the critical directional signal test — is next.

---

## Thesis

Six independent research threads demonstrated that **no combination of hand-engineered LOB features and gradient-boosted trees can predict short-term price direction on MES MBO data.** Static book snapshots, flow EMAs, sequential BBO dynamics, and cooldown sweeps all produced 0/45 positive CPCV folds. The tree-based approach on tabular microstructure features is exhaustively dead.

The pivot: rather than engineer features that describe what the book *looks like*, train a transformer to learn directly from raw MBO event sequences what patterns *matter*. The model learns event grammar (how orders, cancels, trades, and fills relate to each other across atomic batch boundaries) and book state (what the order book looks like at any point), both prerequisites for any directional prediction. The question is whether these learned representations encode information that transfers to direction.

### Why MBO sequences, not snapshots

A limit order book snapshot is a lossy compression of the event stream that produced it. Two identical book states can have radically different microstructural histories — a stable book with occasional cancels vs. a rapidly churning book with high add/cancel ratios at the same levels. The *sequence* of events encodes participant behavior, urgency, and information arrival in ways that a snapshot cannot.

The tokenization preserves this: each MBO event becomes `[ACTION] [SIDE] [PRICE] [SIZE]`, with `[COMMIT]` tokens marking CME's atomic batch boundaries (the F_LAST flag). The transformer sees the full compositional structure of market activity.

### Why this hasn't been done

Published LOB foundation models (TradeFM, LOBERT, LOBS5, MarS) operate exclusively on equities data (LOBSTER/ITCH format, US and Chinese markets, or crypto). CME futures microstructure is qualitatively different: no opening auction, different participant composition (institutional hedgers vs. retail/HFT mix), calendar spread dynamics, different fee structures. Results from equities should not be assumed to transfer.

No published foundation model exists for CME futures MBO data. This project occupies that whitespace.

---

## Research Progression

### What doesn't work (Threads 01-04, 06)

| Thread | Approach | Result | Key insight |
|--------|----------|--------|-------------|
| 01 | 5s bar features, XGBoost, overlapping evaluation | Sharpe 14.2 overlapping, **-18.3 serial** | Overlapping positions inflate metrics by 99.9% |
| 02 | Tick-level, bid/ask execution, serial | 45/45 CPCV folds negative | Mid-price entry is fiction; spread cost kills marginal edges |
| 03 | 44 LOB features (imbalance, OFI, HHI, etc.) | **0/45 CPCV folds positive** | Static snapshots describe state, not dynamics |
| 04 | 22 sequential BBO dynamics features | 0/45 folds (Q=184,922 autocorrelation) | Slow signals on fast grids produce correlated noise |
| 06 | Cooldown sweep (0-422 events) on Thread 04 | **45/45 negative at all cooldowns** | Signal doesn't exist at any evaluation frequency |

These threads are not failures — they are systematic elimination. Each ruled out an entire feature class with rigorous CPCV validation (10:5 geometry, 45 folds, serial execution, 287+ days, 897M events). The infrastructure built along the way (verified order book, CPCV framework, cloud orchestration) remains in production use.

### What works so far (Thread 05 — MBO Grammar)

**Phase 0: Tokenization.** Built a 126-token vocabulary that encodes every MBO event as a compositional sub-token sequence:

```
Vocabulary (126 tokens):
  Special:  PAD  BOS  EOS  COMMIT           (4)
  Action:   ADD  CANCEL  MODIFY  TRADE  FILL  CLEAR    (6)
  Side:     BID  ASK  NONE                   (3)
  Price:    FAR_NEG  {-50..+50 ticks}  FAR_POS  NO_REF  (104)
  Size:     0  1  2-3  4-7  8-15  16-31  32-63  64-127  128+  (9, log2-quantized)
```

Each event becomes `[ACTION] [SIDE] [PRICE_REL] [SIZE]`, with `[COMMIT]` at CME's F_LAST boundary marking atomic batch completion. Price is relative to pre-event mid in integer ticks; size is log2-quantized. This multi-token encoding preserves compositional structure — `[ADD] [BID]` shares learned representations with `[ADD] [ASK]`, analogous to how natural language models share structure across related phrases.

Full-year 2022 MES dataset: 312 files, 809M events, **3.93 billion tokens**, 695M commit boundaries.

**Phase 1: Language model gate — PASSED.** A decoder-only transformer (6.5M params, d=256, 8 layers, 8 heads) trained on next-token prediction achieves **perplexity 1.864**, beating the order-5 Markov baseline of 1.984 by 6.1%. The model learns real MBO grammar beyond n-gram statistics.

Per-token-class perplexity reveals what the model finds easy and hard:
- SIDE: 1.46 (bid/ask is highly predictable from action context)
- PRICE: 1.85 (relative tick position is learnable from order flow patterns)
- SIZE: 1.94 (noisiest — order sizes have high entropy)
- ACTION: 2.30 (sequence of adds, cancels, trades is moderately predictable)

**Phase 2: Book state reconstruction — PASSED.** Added dual reconstruction heads (pre-batch and post-batch) to test whether the transformer's internal representations encode meaningful book state. The pre-batch head predicts what the order book looks like *before* a batch of events arrives — purely from the preceding ~105 events of context, with no explicit book state input.

| Metric | Baseline | Model | Improvement |
|--------|----------|-------|-------------|
| Size accuracy (level 1) | 23.6% | **67.4%** | +43.8pp |
| Spread (non-modal accuracy) | — | **84.4%** | — |
| Imbalance MAE | 0.2273 | **0.0665** | 3.4x better |

The pre-batch head slightly outperforms post-batch on spread and imbalance, meaning the model carries a *running representation of book state across positions* — it doesn't just compute locally from the current batch. This contradicts the LOBS5 finding (Nagy et al., 2023) that models require explicit book state input.

**Phase 3: Directional signal check — READY.** The critical test: does pretraining + book state reconstruction give the model a directional advantage?

- **Condition B:** Phase 2 pretrained checkpoint + reconstruction heads + directional head (next BBO change direction)
- **Condition C:** Random initialization + directional head only (control)
- **Kill gate:** B beats majority class by >= 2pp in >= 60% of 15-fold CPCV folds
- **Statistical power:** 244K independent labeled samples per fold; SE for 2pp effect is 0.1pp (20-sigma)

If Phase 3 fails, MBO sequences do not predict direction on MES. Stop.

---

## Architecture

### Tokenizer (Rust)

The tokenizer (`crates/mbo-tokenizer`) processes raw Databento `.dbn.zst` files through a verified L2 order book (`crates/book-builder`, 99.91% exact match vs Databento MBP-10 across 54.3M comparisons) and emits token sequences with aligned sidecars:

- **tokens.bin** — u16 token IDs, streaming output (~7.3 GiB for full year)
- **book_state** — 12 f32 fields at each COMMIT: BBO prices (relative to mid), 5 bid sizes, 5 ask sizes (48 bytes/row, 31.1 GiB)
- **mids** — (commit_position, mid_price) pairs for label computation

Snapshot events (Databento's `F_SNAPSHOT` flag) are fed to the BookBuilder for correct state reconstruction but produce zero tokens — they represent exchange-initiated book rebuilds, not organic market activity.

### Transformer (PyTorch)

```
MBOTransformer
  d_model=256, layers=8, heads=8, dim_ff=1024
  VOCAB_SIZE=128 (padded from 126 for tensor core alignment)
  Weight-tied embedding/output projection
  Custom CausalBlock with direct F.scaled_dot_product_attention(is_causal=True)
  ~6.5M parameters

ReconHead (x2: pre-batch, post-batch)
  10 size fields as 9-class classification (log2 buckets)
  Spread as 11-class classification (0=crossed, 1-9=ticks, 10=wide)
  Level-1 imbalance as regression [0, 1]
  ~112K parameters each

DirectionalHead
  Binary classification (next BBO change: up vs down)
  d_model -> d_model/2 -> 1, GELU activation
```

The custom `CausalBlock` was necessary because PyTorch 2.5.1's `nn.TransformerEncoder`, `nn.MultiheadAttention`, and the standard attention forward path all block FlashAttention during training when dropout > 0. Direct `F.scaled_dot_product_attention(is_causal=True)` bypasses this, yielding a 25% speedup (840s/epoch vs 1191s with `torch.compile`, which forced a float causal mask path).

### Order Book (Rust)

The BookBuilder (`crates/book-builder`) reconstructs L2 order books from L3 MBO events:

- Price levels: sorted `Vec<(i64, u32)>` with `binary_search_by_key` (cache-friendly, replaced BTreeMap)
- Per-order tracking: `FxHashMap<u64, OrderInfo>` (rustc-hash for fast integer hashing)
- `OrderInfo`: 16 bytes (price i64, size u32, side char), field-ordered to eliminate padding
- Verified against Databento MBP-10 ground truth: 99.91% exact match across 54.3M comparisons

---

## Workspace Layout

```
crates/
  book-builder/       L2 order book from MBO events (sorted-Vec + FxHashMap)
  mbo-tokenizer/      126-token vocabulary, streaming tokenization + sidecars
  flow-features/      48 event-count EMA flow features (OFI, trade flow, cancel rates)
  event-features/     42 instantaneous LOB features from committed state
  event-labels/       Tick-level triple-barrier simulation (multi-geometry)
  seq-features/       22 inter-episode BBO dynamics features
  backtest/           Triple-barrier backtest engine
  rithmic-client/     Rithmic protobuf WebSocket client (live CME data)
  databento-ingest/   Databento .dbn.zst ingestion
  xgboost-ffi/        Pure Rust XGBoost JSON inference
  common/             Shared types

tools/
  mbo-tokenize/       CLI: .dbn.zst -> tokens.bin + sidecars
  cloud-run/          Multi-backend cloud orchestration (EC2 + RunPod), TTL enforcement
  rithmic-live/       Live multi-instrument pipeline (Rithmic -> BookBuilder -> features)
  event-export/       Export features + labels to Parquet
  event-backtest/     CPCV + serial PnL backtest with distributed fold sharding
  book-verify/        Validate BookBuilder against Databento MBP-10

research/
  01-bar-level-cpcv/          DEAD - bar aggregation destroys signal
  02-tick-level-serial/       DEAD - null at execution resolution
  03-event-lob-probability/   DEAD - 0/45 CPCV folds with static LOB features
  04-mbo-grammar/             ACTIVE - transformer pretraining on tokenized MBO events
    model.py                  CausalBlock + MBOTransformer + ReconHead + DirectionalHead
    train.py                  Phase 1: language model pretraining
    train_finetune.py         Phase 2: dual-head book state reconstruction
    phase3_signal_check.py    Phase 3: directional signal check (B vs C, 15-fold CPCV)
    data.py                   Dataset utilities (sliding window, temporal split)
    precompute_phase3.py      Direction target precomputation from mids sidecar
```

---

## Validation Methodology

All experiments use **Combinatorially Purged Cross-Validation (CPCV)** with serial execution:

- **CPCV geometry:** 10 groups, 5 test groups per split = 45 folds (Threads 01-04, 06); 6 groups, 2 test = 15 folds (Phase 3)
- **Serial execution:** One position at a time. No overlapping trades. Thread 01 showed overlapping evaluation inflates Sharpe from -18.3 to +14.2 — any backtest without serial execution is uninformative.
- **Purging + embargo:** Training windows exclude data near test boundaries (30K token buffer in Phase 3)
- **Tradeable prices:** All entry/exit at bid/ask, never theoretical mid-price
- **Kill gates:** Each phase has pre-specified pass criteria. If the gate fails, the research direction is abandoned — not tweaked until it passes.

---

## Competitive Landscape

| Model | Params | Data | Vocab | Market | Book state |
|-------|--------|------|-------|--------|------------|
| **This work** | **6.5M** | **3.93B tokens, 1 yr MES** | **126 (factored)** | **CME futures** | **Reconstruction from events** |
| TradeFM (JPMorgan, 2026) | 524M | 19B tokens, 9K+ equities | 16,384 (composite) | US equities | None |
| MarS (2024) | 2M-1.02B | 32B tokens | — | NASDAQ equities | Input (L2 snapshots) |
| LOBS5 (Oxford, 2023) | — | — | — | NASDAQ equities | Input (required) |
| LOBERT (2025) | — | — | — | Crypto | Continuous-time RoPE |

**Key differentiators:**
- Only project using CME futures MBO data (qualitatively different microstructure from equities)
- Full MBO event coverage (adds, cancels, modifies, trades, fills, clears) vs. TradeFM's add/cancel only
- Book state reconstruction as learned objective, not explicit input (novel approach — LOBS5 found explicit input was required; our Phase 2 contradicts this)
- Factored 126-token vocabulary preserving compositional structure (appropriate at 6.5M params; TradeFM's 16K composite vocabulary requires 500M+ params)

**Primary threat:** TradeFM could extend to futures markets within 6-12 months given JPMorgan's data access and compute budget. Speed matters.

---

## Data

**Raw:** 312 Databento `.dbn.zst` files, 49.2 GB — full year 2022 CME Micro E-mini S&P 500 (MES) MBO data. 809M events across all trading sessions.

**Tokenized:**
| Artifact | Size | Description |
|----------|------|-------------|
| tokens.bin | 7.3 GiB | 3.93B u16 token IDs |
| book_state | 31.1 GiB | 695M rows x 12 f32 fields (BBO + depth 5) |
| direction_targets.bin | 0.7 GiB | Precomputed BBO change direction labels |
| best_model.pt | 77 MB | Phase 2 checkpoint (backbone + recon heads) |

---

## End-to-End Pipeline

This project covers the full research-to-production cycle: raw data ingestion, order book reconstruction, feature engineering, model training, rigorous backtesting, and a live market data pipeline ready for strategy deployment.

**Data processing (Rust):** Streaming ingestion of Databento `.dbn.zst` files through a verified L2 order book at ~3M events/sec. Tokenizer produces aligned token sequences and book state sidecars with <10 MiB memory footprint regardless of dataset size.

**Feature engineering:** 44 instantaneous LOB features (depth profile, imbalance, spread, HHI, slope), 48 event-count EMA flow features (OFI at multiple timescales, trade flow, cancel rates), 22 inter-episode BBO dynamics features. All at committed-state (event-level) resolution, never time-aggregated.

**Model training (PyTorch):** Transformer pretraining + multi-task fine-tuning on GPU. Custom FlashAttention integration (CausalBlock bypassing PyTorch 2.5.1's attention stack) was the single largest speedup — more impactful than AMP, torch.compile, or data pipeline improvements. 840s/epoch on RTX 4090.

**Backtesting:** Combinatorially Purged Cross-Validation with serial execution, tick-level triple-barrier simulation, multi-geometry training, and calibration diagnostics. All entry/exit at tradeable bid/ask prices.

**Live pipeline:** Multi-instrument Rithmic WebSocket client with per-instrument thread isolation, independent BookBuilder instances, BBO health monitoring, and real-time feature extraction. Tested on CME E-mini S&P 500 and Micro E-mini NASDAQ.

**Cloud orchestration:** Custom `cloud-run` CLI supporting AWS EC2 (spot instances, auto-termination) and RunPod GPU pods (RTX 4090 at $0.59/hr). Heartbeat monitoring, idle detection, TTL enforcement, automatic shutdown.

---

## Build

```bash
# Rust workspace (tokenizer, book builder, tools)
~/.cargo/bin/cargo build --release
~/.cargo/bin/cargo test -p book-builder
~/.cargo/bin/cargo test -p mbo-tokenizer

# Python (model training)
cd research/04-mbo-grammar
pip install -r requirements.txt
python train.py --tokens /path/to/tokens.bin --epochs 10
```

---

## What's Next

**If Phase 3 passes** (pretrained model shows directional advantage over random baseline):
- Interarrival time encoding — `[TIME_DELTA]` sub-token (16 log-scale bins). All major papers encode timing; we currently don't.
- Continuous-time RoPE (LOBERT approach) — replace discrete position index with cumulative timestamp
- Cross-instrument pretraining — bps-normalized pricing to enable joint MES+ES+NQ training

**If Phase 3 fails:**
- MBO grammar encodes market structure but not tradeable direction on MES. Consider cross-instrument lead/lag approaches or alternative targets (signed move magnitude, time-to-fill).

---

## Key References

- **TradeFM** — Kawawa-Beaudan et al. (JPMorgan AI, 2026). arXiv:2602.23784
- **LOBS5** — Nagy et al. (Oxford, 2023). arXiv:2309.00638
- **LOBERT** — arXiv:2511.12563
- **MarS** — Hallmann et al. (2024). arXiv:2409.07486
- **DeepLOB** — Zhang et al. (2018). arXiv:1808.03668
