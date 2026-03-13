# MBO-DL: A Foundation Model for CME Futures Microstructure

End-to-end research pipeline for CME futures microstructure: raw L3 (market-by-order) data ingestion, full limit order book reconstruction, feature engineering, strategy backtesting, live market data, and foundation model training. Rust for performance-critical infrastructure (book reconstruction, feature extraction, backtesting at ~3M events/sec), Python/PyTorch for transformer training on GPU.

**Core thesis:** Static LOB snapshots and hand-engineered features cannot predict short-term price direction in MES futures. Six systematic research threads -- spanning 112 engineered features, multiple model classes (XGBoost, logistic regression, linear probes), and 897M+ rows of tick data -- confirmed this with rigorous cross-validation. The alternative: let a transformer learn directly from tokenized MBO event sequences what patterns matter, treating the order book as a language to be modeled rather than a feature vector to be engineered.

## Research Program

### The Negative Results (Threads 01-04, 06)

Before building the foundation model, I exhaustively tested conventional approaches. Every thread used combinatorially purged cross-validation (CPCV, 45 folds), serial execution (one position at a time), and tradeable prices (bid/ask entry, not mid-price fiction).

| Thread | Approach | Result |
|--------|----------|--------|
| 01 | 5s bar features + XGBoost + triple barrier | Overlapping Sharpe 14.21 collapsed to **-18.26** under serial execution. 45/45 folds negative. |
| 02 | Tick-level re-simulation of Thread 01 signals | Win rate 26.6% matches null (26.9%). Barrier sweep across 3 geometries confirms no edge. |
| 03 | 44 LOB features (depth, imbalance, OFI, trade flow, cancel rates) + XGBoost | Gate test: all univariate signals near-null. Full CPCV: **0/45 positive folds**. Mean expectancy -$1.37/trade across 35M training rows per fold. |
| 04 | 22 inter-episode BBO dynamics features + OFI-directed labels | Ljung-Box Q=184,922 -- massive trade autocorrelation from evaluating slow EMA signal on fast BBO-change grid. Apparent 0/45 negative folds is artifact. |
| 06 | Cooldown sweep on Thread 04 (7 values, 0-422 events) | 45/45 folds negative at every cooldown. Confirms autocorrelation was symptom, not disease. **Exhausts XGBoost on hand-engineered MBO features.** |

**Key methodological insight from Thread 04:** The evaluation grid must match the signal's information timescale. Evaluating a slow feature on a fast grid produces redundant correlated predictions, inflating trade counts and creating artificial autocorrelation regardless of downstream corrections. This is a structural problem with any EMA-smoothed signal evaluated at event frequency.

### The Foundation Model (Thread 05 -- Active)

#### Tokenization

126-token factored vocabulary over raw MBO events:
- **4 special tokens:** BOS, EOS, PAD, COMMIT (marks book update boundaries at Databento's `F_LAST`)
- **6 action tokens:** ADD, CANCEL, MODIFY, TRADE, FILL, CLEAR
- **3 side tokens:** BID, ASK, NONE
- **104 value tokens:** 95 relative price bins (integer ticks from exact L3 mid, +/-50 range + FAR + NO_REF) + 9 log2-quantized size bins

Each MBO event becomes ~4.88 tokens. A full year of MES 2022 (312 trading days, 809M events) produces **3.93 billion tokens** with 695M commit boundaries. Snapshot events (Databento `F_SNAPSHOT` flag) are fed to the book builder for correct state tracking but excluded from the token stream.

The factored encoding is a deliberate design choice at this model scale. At 8M parameters, compositionality (shared embeddings, 126 vocab) outperforms composite encoding (16K+ vocab). TradeFM's mixed-radix 16,384-token composite wins at 500M+ where the model can memorize all token semantics. MarS scaling laws (2M-1.02B params, 32B tokens) confirm our model is data-limited, not parameter-limited -- the right regime for factored tokens.

#### Phase Results

| Phase | Gate | Result |
|-------|------|--------|
| **Phase 0** | Tokenizer + sidecars | 3.93B tokens, .book_state (12-field post-COMMIT book vector), .mids sidecar. Streaming pipeline (<10 MiB RAM). |
| **Phase 1** | LM perplexity < Markov-5 | **PASSED.** ppl 1.864 vs Markov-5 1.984 (6.1% improvement). 6.5M params, d=256, 8 layers, weight-tied. |
| **Gate 2** | Frozen linear probe for direction | **NEGATIVE.** Raw one-hot beats pretrained at all horizons (K=50: 72.0% vs 71.7%). Grammar is structural, not directional. |
| **Phase 2** | Book state reconstruction | **PASSED.** Dual-head (pre-batch + post-batch): size acc 67.4% (vs 23.6% baseline), spread non-modal 84.4%, imbalance MAE 0.0665. |
| **Phase 3** | Directional signal from pretrained model | **NEXT.** B (pretrained+recon) vs C (random init) on next BBO change direction. 15-fold CPCV, kill gate: B >= majority+2pp in >= 60% of folds. |

**Phase 2 detail:** The model reconstructs what the order book looks like from ~105 events of token context alone -- no book state input. This is novel: LOBS5 and MarS both feed book state as input. LOBS5 explicitly found that "training and validation loss drastically improves when using book data in addition to message sequences," flagging our reconstruction-as-target approach as risky. The pre-batch head (predict book state *before* processing the current batch) was the concern -- it passed comfortably (spread NM 86.5%), meaning the model carries a running book state representation across positions.

**Phase 3 is the critical test:** Does encoding meaningful book state representations translate to directional predictive power? Statistical power is not an issue: 244K independent labeled test samples per fold, SE = 0.1pp for a 2pp effect (20-sigma).

### Competitive Position

A literature review of 13 papers across three waves (discriminative LOB models, MBO deep learning, foundation models) confirms **no published foundation model exists for CME futures MBO data.** All published work uses NASDAQ equities (LOBSTER/ITCH), cryptocurrency, or Chinese equities.

- **TradeFM** (JPMorgan, Feb 2026, 524M params): Primary threat. US equities only, add/cancel events only (no trades/fills/modify/clear), no book state reconstruction. Could extend to futures with their data access.
- **MarS** (Microsoft/ICLR 2025): Validates decoder-only architecture and provides scaling laws. Additive book state conditioning as potential fallback.
- **LOBS5** (Oxford, ICAIF 2023): Most architecturally relevant prior work. S5 (state-space) with simulator-in-the-loop.
- **LOBERT** (2025): Continuous-time RoPE for irregular inter-arrival times -- queued for post-Phase 3 integration.

Futures microstructure is qualitatively different from equities: no opening auction, different participant composition, calendar spread dynamics, different fee structures. Equity results should not be assumed to transfer.

## Infrastructure

The pipeline is split by what each language does best. Rust handles latency-sensitive, data-heavy work: order book reconstruction, feature extraction from 809M events, tick-level backtesting with serial execution, and live market data over WebSocket. Python handles GPU training: transformer pretraining, dual-head reconstruction fine-tuning, and CPCV fold evaluation. Data flows from Rust (raw .dbn.zst -> tokenized sequences + sidecars on S3) to Python (S3 -> DataLoader -> model).

### Rust Workspace

| Crate | Purpose |
|-------|---------|
| `book-builder` | L2 order book from L3 MBO events. Sorted-Vec + FxHashMap. **99.91% verified** vs Databento MBP-10 across 54.3M comparisons. |
| `event-features` | 42 instantaneous LOB features from committed book state |
| `flow-features` | 48 event-count EMA flow features (OFI, trade flow, cancel rates) |
| `seq-features` | 22 inter-episode BBO dynamics features |
| `event-labels` | Tick-level triple-barrier simulation (multi-geometry) |
| `backtest` | Triple-barrier backtest engine with serial execution enforcement |
| `databento-ingest` | .dbn.zst ingestion at ~3M events/sec |
| `rithmic-client` | Rithmic protobuf WebSocket client (live CME market data) |
| `xgboost-ffi` | Pure Rust XGBoost JSON inference (no C FFI in hot path) |

**Book builder internals:** Price levels use sorted `Vec<(i64, u32)>` with `binary_search_by_key` (replaced BTreeMap for cache locality). Per-order tracking via `FxHashMap<u64, OrderInfo>` where `OrderInfo` is 16 bytes (price: i64, size: u32, side: char), field-ordered to eliminate padding. Flow accumulators use `u8` bitmask for BBO change cause tracking.

### Tools

| Tool | Purpose |
|------|---------|
| `rithmic-live` | Multi-instrument live pipeline (Rithmic -> BookBuilder -> features). Per-instrument tokio tasks with independent book state and BBO health monitoring. |
| `event-export` | Export any feature set from raw .dbn.zst to Parquet |
| `event-backtest` | CPCV + serial PnL backtest with distributed fold sharding |
| `cloud-run` | Multi-backend cloud compute orchestration (AWS EC2 + RunPod GPU). Heartbeat monitoring, idle detection, TTL enforcement, automatic termination. |
| `book-verify` | Validate BookBuilder against Databento MBP-10 ground truth |

### Validation Infrastructure

- **CPCV:** Combinatorially purged cross-validation (10 groups, 5 test = 45 folds or 6 groups, 2 test = 15 folds). Eliminates train/test leakage from autocorrelated financial data.
- **Serial execution:** One position at a time. Overlapping evaluation inflated Sharpe from -18.26 to +14.21 in Thread 01 -- a 32x distortion.
- **Tradeable prices:** Entry at bid (shorts) or ask (longs). Mid-price entry is a fiction that absorbs spread cost.
- **Multi-geometry training:** Single model sees multiple (take-profit, stop-loss) pairs per evaluation point. Geometry is a feature, not a hyperparameter.

## Data

- **Source:** Databento CME MBO (L3), full year 2022 MES (Micro E-mini S&P 500)
- **Volume:** 312 trading days, 809M events, 49.2 GB raw (.dbn.zst)
- **Tokenized:** 3.93B tokens (7.3 GiB), 695M commit boundaries, book state sidecar (31.1 GiB)
- **Live:** Rithmic WebSocket client for real-time CME data (paper trading validated with MES + MNQ)

## Build

```bash
cargo build --release          # full workspace
cargo test -p book-builder     # run tests for a crate
cargo run --release -p rithmic-live -- --help
```

## Future Directions (Contingent on Phase 3)

If Phase 3 passes (pretrained model shows directional advantage):
- **Interarrival time encoding:** `[TIME_DELTA]` sub-token with 16 log-scale bins. All three major foundation model papers encode timing; we currently don't.
- **Continuous-time RoPE:** Replace discrete position index with cumulative time (LOBERT approach). Handles irregular inter-arrival times natively.
- **Cross-instrument pretraining:** bps-normalized pricing (TradeFM approach) to enable MES + ES + NQ joint training. Futures lead/lag dynamics are a known signal source.
- **Architecture comparison:** Controlled Transformer vs. Mamba experiment on tick-level MBO data. No published comparison exists -- publishable result on its own.

If Phase 3 fails:
- Re-evaluate whether MBO grammar encodes any tradeable signal. Consider alternative target formulations (signed move magnitude, time-to-fill, liquidity withdrawal) or cross-instrument approaches.

## Related Repository

The MBO tokenizer (Rust), transformer training code (Python/PyTorch), and experiment configs live in a separate repository ([mbo-tokenization](https://github.com/kurtbell87/mbo-tokenization)). This repo contains the core infrastructure: book reconstruction, feature extraction, backtesting, live pipeline, and cloud orchestration.
