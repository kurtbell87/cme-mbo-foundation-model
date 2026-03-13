# Deep Learning for Limit Order Books & Market Microstructure
## A Literature Review for Kenoma Labs -- March 2026

---

## 1. Landscape Overview

The field has moved fast. As recently as 2023, the dominant paradigm was still discriminative models (CNNs, LSTMs) trained on LOB snapshots to predict mid-price direction. By early 2026, the frontier has shifted decisively toward foundation models trained on raw message/event streams, using autoregressive generative objectives. The field now has three distinct waves, and our work sits at the intersection of waves 2 and 3.

---

## 2. Wave 1: Discriminative Models on LOB Snapshots (2018-2024)

### 2.1 DeepLOB -- The Baseline Everyone Benchmarks Against
- **Paper:** Zhang, Zohren & Roberts (2019). "DeepLOB: Deep Convolutional Neural Networks for Limit Order Books." arXiv:1808.03668
- **Architecture:** CNN + LSTM. Convolutional filters capture spatial structure across price levels; LSTM captures temporal dependencies.
- **Data:** 10-level LOB snapshots (price + volume, bid + ask = 40 features per timestep). LOBSTER dataset (NASDAQ equities).
- **Task:** Mid-price movement classification (up/down/stationary) at horizons of 10, 20, 50, 100 events.
- **Key insight:** The model transfers across instruments it wasn't trained on, suggesting universal features exist in LOB structure.
- **Relevance:** Canonical baseline. Any new architecture must beat DeepLOB on the same data to be taken seriously. Operates on aggregated LOB snapshots, not raw MBO.

### 2.2 LOBCAST -- The Benchmark Framework
- **Paper:** Briola et al. (2024). "LOB-based Deep Learning Models for Stock Price Trend Prediction: A Benchmark Study." Artificial Intelligence Review. arXiv:2308.01915
- **What it does:** Reimplements and benchmarks 15 state-of-the-art DL models on LOBSTER data. Releases an open-source framework for data preprocessing, training, evaluation, and profit analysis.
- **Key finding:** All models exhibit significant performance degradation on new data. Raises serious questions about real-world applicability of academic results.
- **Relevance:** The "nothing actually works in production" paper. Any reproduction harness should be validated against LOBCAST's results.

### 2.3 LOBFrame -- Microstructure-Aware Evaluation
- **Paper:** Briola, Turiel & Aste (2024). "Deep Limit Order Book Forecasting: A Microstructural Guide." Quantitative Finance. arXiv:2403.09267
- **Key contribution:** Demonstrates that stocks' microstructural characteristics (tick size, spread, liquidity) fundamentally determine which models work. Proposes that traditional ML metrics are inadequate -- evaluates whether predictions correspond to actionable trading signals.
- **Open source:** LOBFrame framework released. ~960 GPU-hours of experiments on V100s.
- **Relevance:** The finding that microstructural properties determine model efficacy is critical for futures. Futures have very different tick structures than equities. Don't assume equity LOB results transfer.

### 2.4 HLOB -- Heterogeneous LOB Representation
- **Paper:** Briola et al. (2024). "HLOB -- Information Persistence and Structure in Limit Order Books." Expert Systems with Applications.
- **Key insight:** Standard LOB representation assumes homogeneous spacing between price levels. This fails when tick sizes create irregular spacing -- especially problematic for CNNs. HLOB proposes an alternative representation that accounts for this heterogeneity.
- **Relevance:** Futures instruments (ES, CL, ZC, etc.) have very different tick structures. A representation optimal for ES won't work for ZC.

### 2.5 LiT -- Limit Order Book Transformer
- **Paper:** Xiao et al. (2025). "LiT: Limit Order Book Transformer." Frontiers in AI.
- **Architecture:** Patch-based, convolution-free transformer. Uses structured patches and self-attention to model spatial-temporal features. No CNN layers.
- **Key finding:** Consistently outperforms ML and CNN baselines (including DeepLOB) across prediction horizons (300ms-1000ms). Maintains performance under distributional shift via fine-tuning.
- **Data:** Cryptocurrency LOB data (24-hour markets, very granular).
- **Relevance:** Demonstrates pure attention architectures work for LOB data. Patch-based tokenization approach is relevant to tokenization design choices.

### 2.6 TLOB -- Dual Attention Transformer
- **Paper:** Berti & Kasneci (2025). "TLOB: A Novel Transformer Model with Dual Attention for Stock Price Trend Prediction with Limit Order Book Data." arXiv:2502.15757
- **Key finding:** An MLP-based baseline, when properly adapted to LOB data, surprisingly surpasses many complex architectures. TLOB then improves further with dual spatial/temporal attention. Effective at longer horizons and in volatile conditions.
- **Relevance:** The MLP result suggests the representation and training procedure matter more than the architecture, at least at this scale.

---

## 3. Wave 2: Deep Learning on MBO (Market-by-Order) Data

### 3.1 Deep Learning for Market by Order Data -- The Foundational MBO Paper
- **Paper:** Zhang, Zohren & Roberts (2021). "Deep Learning for Market by Order Data." Applied Mathematical Finance. arXiv:2102.08811
- **Key contribution:** First predictive analysis on MBO data. Introduces normalization scheme for MBO that encodes price level information and enables multi-instrument training. Uses GRU-based attention model.
- **Critical finding:** MBO and LOB models provide similar individual performance, but **ensembles of both yield superior results**. MBO data provides orthogonal information to LOB snapshots -- it's not redundant.
- **Data format:** Each MBO event = (order ID, action type [add/cancel/execute], side, price, quantity). Normalized relative to current best bid/ask.
- **Relevance:** Validates the intuition that MBO data contains information LOB snapshots discard. Still the only MBO-specific deep learning paper as of March 2026.

### 3.2 Forecasting Liquidity Withdrawal from MBO Data
- **Paper:** Recent (2025). "Forecasting Liquidity Withdraw with Machine Learning Models." arXiv:2509.22985
- **Data:** Nasdaq MBO data via DataBento (our exact data source). HIMS, NBIS, RKLB, SNAP.
- **Key finding:** Horizon-dependent structure: 250ms is noise-dominated; linear models perform best at 1-2s; tree ensembles dominate at 5s. Targets liquidity withdrawal rather than mid-price direction.
- **Relevance:** Uses our data vendor. Provides a reproducible pipeline from raw MBO events to features. The liquidity withdrawal framing is more operationally useful than mid-price direction for actual trading.

---

## 4. Wave 3: Foundation Models & Generative Approaches (2023-2026)

The core idea: treat the order/message stream as a language, tokenize it, and train autoregressive or masked models at scale.

### 4.1 LOBS5 / Nagy et al. -- The First Autoregressive LOB Message Generator
- **Paper:** Nagy, Frey, Sapora et al. (2023). "Generative AI for End-to-End Limit Order Book Modelling: A Token-Level Autoregressive Generative Model of Message Flow Using a Deep State Space Network." ICAIF 2023. arXiv:2309.00638
- **Architecture:** S5 (Simplified Structured State Space) layers processing dual inputs: book state sequences + tokenized message sequences. Two-branch architecture that merges after initial processing.
- **Tokenization:** Custom tokenizer converts LOB messages to finite vocabulary. Groups of successive digits become tokens, similar to BPE in LLMs. Message fields: event type, direction, price, size, inter-arrival time. 22 tokens/message, 12,011 vocab.
- **Key design choice:** Uses a JAX-LOB simulator in the inference loop. Model generates a message token sequence -> decoded into a message -> fed to LOB simulator -> updated book state feeds back into the model.
- **Key finding:** Low perplexity on held-out data. Generated mid-price returns show significant correlation with real data. Book state as input "drastically improves" loss.
- **Code:** Open source at github.com/peernagy/LOBS5 (JAX/Flax).
- **Relevance:** Most directly relevant prior work for architecture. The finding that book state input improves loss is a RISK FLAG for our Phase 2 pre-batch reconstruction head.

### 4.2 LOB-Bench -- Evaluation Framework for Generative LOB Models
- **Paper:** Nagy, Frey et al. (2025). "LOB-Bench: Benchmarking Generative AI for Finance -- an Application to Limit Order Book Data." arXiv:2502.09172
- **What it does:** Standardized benchmark for evaluating generative message-by-order models. Measures distributional differences in conditional and unconditional statistics.
- **Models benchmarked:** LOBS5, RWKV (170M params, off-the-shelf BPE tokenizer), CGAN, parametric Cont model.
- **Key finding:** Autoregressive GenAI approaches beat traditional model classes. RWKV with generic BPE is the "scaling brute force" approach.
- **Code:** Open source at lobbench.github.io

### 4.3 MarS / Large Market Model (LMM) -- Microsoft's Order-Level Foundation Model
- **Paper:** Li et al. (2024/2025). "MarS: a Financial Market Simulation Engine Powered by Generative Foundation Model." ICLR 2025. arXiv:2409.07486
- **Architecture:** LLaMA2-based decoder-only transformer. 1 composite token per order: 49,152 vocab (3 types x 32 price bins x 32 volume bins x 16 interval bins). Embedding: `emb(order) + linear_proj(LOB_20d_volumes) + emb(mid_price_ticks)`.
- **Scaling laws:** 2M to 1.02B params on 32B tokens. Loss at 2M ~7.05, at 1.02B ~7.00 (modest ~0.7% per 10x params). All curves still falling steeply with more tokens -- data-limited, not param-limited.
- **Code:** github.com/microsoft/MarS (framework only). No weights. No training code.
- **Relevance:** Scaling laws validate our 8M model at 3.93B tokens. Additive book state conditioning is a candidate fallback for Gate 1.5.

### 4.4 LOBERT -- BERT for LOB Messages
- **Paper:** Linna et al. (2025). "LOBERT: Generative AI Foundation Model for Limit Order Book Messages." arXiv:2511.12563
- **Architecture:** Encoder-only (BERT-style) adapted for LOB messages. 1.1M params, 293-token composite vocab.
- **Key innovations:**
  - One token per complete multi-dimensional message (~20x context compression vs digit-level)
  - Continuous-time Rotary Position Embedding (RoPE): `phi_i(t) = t * theta_i` with standard 10000 base
  - Hybrid discrete-continuous decoding head (4 heads: token CE + 3 regression MSE)
  - Piecewise Linear-Geometric Scaling (PLGS) for continuous quantities
- **Hyperparameter gap:** d_model, layers, heads NOT specified despite stating 1.1M params. No code available.
- **Relevance:** Continuous-time RoPE solves irregular inter-arrival times. The hybrid decoding head is the right answer for tokenizing continuous financial quantities. Both are post-Phase 3 priorities.

### 4.5 TradeFM -- The 524M Parameter Microstructure Foundation Model
- **Paper:** Kawawa-Beaudan, Sood et al. (2026). "TradeFM: A Generative Foundation Model for Trade-flow and Market Microstructure." arXiv:2602.23784
- **Scale:** 524M parameters. 19 billion tokens, 9,000+ US equities, 1.9 million date-asset pairs. Decoder-only transformer (Llama-family, 32 layers, 32 heads, d=1024).
- **Key innovations:**
  - **Scale-invariant features:** Price as bps from EW-VWAP mid, log-volume, log-interarrival-time. Enables cross-asset generalization.
  - **Mixed-radix 16,384-token composite encoding:** 2x2x16x16x16 (action, side, depth, vol, time).
  - **Zero-shot to APAC markets** with moderate perplexity degradation -- trained only on US equities.
- **What it does NOT model:** No order IDs, no book state, no trade/fill events, no multi-event batches.
- **Relevance:** PRIMARY COMPETITIVE THREAT. JPMorgan resources could trivially extend to CME futures. However, their partial-observation (trade-level only) approach is strictly less information than our full L3 MBO reconstruction. Published Feb 2026.

### 4.6 Kronos -- Foundation Model for K-Line Data
- **Paper:** Shi et al. (2025). "Kronos: A Foundation Model for the Language of Financial Markets." AAAI 2026. arXiv:2508.02739
- **Scale:** 12 billion K-line records from 45 global exchanges. Decoder-only transformer.
- **Key finding:** 93% improvement in RankIC over leading TSFM in zero-shot price forecasting.
- **Code & weights:** Open source at github.com/shiyu-coder/Kronos.
- **Relevance:** Complementary (aggregated candlestick, not MBO), but tokenization and scaling laws are informative.

---

## 5. Architecture Comparison: Transformers vs. State-Space Models

### Transformers
- Mature ecosystem, well-understood training dynamics
- All largest/most successful models (TradeFM, MarS, Kronos) use transformers
- Flash Attention reduces O(n^2) pain

### SSMs (S4/S5/Mamba)
- O(n) sequence processing -- matters for millions of MBO events per day
- State-space formulation maps naturally to LOB dynamics (the book IS a state)
- LOBS5 achieved strong results with S5
- Linear-time inference for real-time deployment

### Pragmatic Assessment
No controlled comparison of transformers vs. Mamba at same scale on tick-level MBO data exists. Start with transformer (proven by TradeFM/MarS). Keep Mamba as potential second architecture. Tokenization and training infrastructure should be architecture-agnostic.

---

## 6. The Tokenization Design Space

| Paper | Granularity | Price Encoding | Volume Encoding | Time Encoding | Vocab Size |
|-------|------------|----------------|-----------------|---------------|------------|
| LOBS5 | 22 tokens/msg (digit-level) | Raw digits 0-999 ticks | Raw 0-9999 | 3-digit groups | 12,011 |
| LOBERT | 1 token/msg (composite) | Tick diff from opposing BBO, quantized to {0,1,2,3,5,10} + PLGS regression | Continuous regression | Continuous-time RoPE | 293 |
| TradeFM | 1 token/event (mixed-radix) | bps from EW-VWAP, 16 quantile bins | log(1+V), 16 equal-width | log interarrival, 16 bins | 16,384 |
| MarS/LMM | 1 token/order (composite) | 32 bins relative to mid | 32 bins | 16 interval bins | 49,152 |
| Ours | ~4.88 tokens/event (factored) | Integer ticks from exact L3 mid, ±50 range | log2 quantized, 9 bins | None (implicit) | 126 |

**Key tension:** Finer tokenization preserves more information but creates longer sequences. Coarser tokenization is more efficient but forces less learning per token. Our factored encoding enables compositional generalization; their composite encodings enable wider context windows.

---

## 7. What's NOT Been Done (Our Whitespace)

1. **No foundation model trained on CME futures MBO data.** Every paper uses NASDAQ equities (LOBSTER) or cryptocurrency. Futures microstructure is qualitatively different.
2. **No multi-asset futures model.** TradeFM does multi-asset for equities, but no one has jointly modeled cross-instrument futures dynamics from raw MBO.
3. **No Rithmic-native pipeline.** All published work uses LOBSTER or DataBento. Our Rithmic infrastructure gives a live data path -- engineering moat.
4. **No controlled Transformer vs. Mamba comparison on tick-level MBO.** Publishable result on its own.
5. **Tokenization for variable-tick instruments is unsolved.** LOBERT assumes single tick size. TradeFM's bps normalization is closest but designed for equities.
6. **Book state as reconstruction TARGET (not input) is novel.** LOBS5 and MarS both feed book state as input. Our dual-head reconstruction approach is untested in literature.

---

## 8. Open-Source Code & Frameworks

| Resource | What It Provides | Language | Link |
|----------|-----------------|----------|------|
| LOBCAST | 15-model benchmark on LOB data | Python/PyTorch | github (referenced in paper) |
| LOBFrame | Microstructure-aware evaluation | Python/PyTorch | github (referenced in paper) |
| LOBS5 | Autoregressive LOB generation with S5 | JAX/Flax | github.com/peernagy/LOBS5 |
| LOB-Bench | Generative model evaluation benchmark | Python | lobbench.github.io |
| MarS | Market simulation engine + LMM architecture | Python/PyTorch | github.com/microsoft/MarS |
| Kronos | K-line foundation model + tokenizer | Python/PyTorch | github.com/shiyu-coder/Kronos |

---

## 9. Reproduction Priority

1. **LOBS5** -- Open source, custom tokenizer, simulator-in-the-loop, autoregressive pipeline. Gives working tokenizer + training loop + evaluation.
2. **LOB-Bench** -- Standardized generative quality metrics.
3. **DeepLOB** -- Quick discriminative baseline.
4. **LOBERT** -- Study one-token-per-message and continuous-time RoPE design patterns.
5. **TradeFM** -- Scale-invariant features and universal tokenization for multi-instrument futures.
6. **MarS** -- Order state representation and LMM architecture.

---

## References (Chronological)

1. Zhang, Zohren & Roberts (2019). DeepLOB. arXiv:1808.03668
2. Zhang, Zohren & Roberts (2021). Deep Learning for MBO Data. arXiv:2102.08811
3. Nagy et al. (2023). LOBS5. ICAIF 2023. arXiv:2309.00638
4. Briola et al. (2024). LOBCAST. arXiv:2308.01915
5. Briola, Turiel & Aste (2024). LOBFrame. arXiv:2403.09267
6. Briola et al. (2024). HLOB. Expert Systems with Applications.
7. Li et al. (2024/2025). MarS / LMM. ICLR 2025. arXiv:2409.07486
8. Berti & Kasneci (2025). TLOB. arXiv:2502.15757
9. Nagy et al. (2025). LOB-Bench. arXiv:2502.09172
10. Xiao et al. (2025). LiT. Frontiers in AI.
11. Linna et al. (2025). LOBERT. arXiv:2511.12563
12. Shi et al. (2025). Kronos. AAAI 2026. arXiv:2508.02739
13. Kawawa-Beaudan et al. (2026). TradeFM. arXiv:2602.23784
