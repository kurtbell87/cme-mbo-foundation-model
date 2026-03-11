# Research Log

Cumulative findings from all experiments. Read this file FIRST when starting any new research task.

Last updated: 2026-03-11

---

## What We Learned

### 1. Time-aggregated features are information-destroying

5-second bars discard 12-31% of barrier-relevant price excursions. Labels computed on bar close_mid miss intra-bar barrier breaches entirely. This is not a fixable problem — it is fundamental to any time-aggregated approach on MES.

### 2. Mid-price entry is a fiction

All Thread 01-02 results assumed entry/exit at theoretical midpoint. Real execution happens at bid (shorts) or ask (longs), eating the spread on every trade. This alone can flip marginal strategies from positive to negative.

### 3. Overlapping position metrics are meaningless

Thread 01 reported Sharpe 14.21 with ~3,750 "trades"/day. These were overlapping entries sharing 99.9% of their forward window. Under serial execution (one position at a time), the same model produced Sharpe -18.26. **Any backtest that does not enforce serial execution is not informative.**

### 4. Static LOB snapshots have no predictive power for short-term direction

Thread 03 tested 44 LOB features (book depth profile, imbalance at multiple depths, spread, HHI, slope, cancel/add rates, OFI, trade flow) across 287 days and 897M rows. Results:

- **Gate baseline test:** All 6 univariate signals (OFI fast/med/slow, cancel asymmetry, trade flow) showed near-null lift across all 11 geometries and time-of-day buckets.
- **Full CPCV (10:5 geometry, 45 folds):** **0/45 positive folds.** Mean expectancy -$1.37/trade. Mean win rate 32.8% vs null 33.3%. XGBoost with 35M training rows per fold found zero signal.

The features describe the book's *state* but not the *dynamics* that cause price to move.

---

## What Does NOT Work

| Approach | Why It Fails | Evidence |
|----------|-------------|----------|
| 5-second bar features → triple barrier | Bars destroy intra-bar dynamics; 12-31% label flip rate | Thread 02: 45/45 CPCV folds negative |
| 2-stage XGBoost classification (direction filter + predictor) | No directional edge at execution resolution | Thread 02: win rate matches null (26.6% vs 26.9%) |
| Asymmetric barriers (19:7) with tight stops | 73% base stop rate from random walk alone; noise overwhelms any small edge | Thread 02: barrier sweep confirms null |
| Static LOB snapshot features → P(target) regression | Book state is not predictive; all features near-null individually and combined | Thread 03: 0/45 CPCV folds positive, gate test null |
| OFI / trade flow / cancel asymmetry as univariate signals | Near-zero lift across all geometries and time-of-day | Thread 03: gate-test-report.json |
| Per-bar overlapping evaluation | Inflates metrics by 99.9% correlation between consecutive "trades" | Thread 01: Sharpe 14.21 → -18.26 serial |
| Slow signal on fast evaluation grid | EMA-smoothed OFI evaluated at every BBO change produces hundreds of correlated trades per regime | Thread 04: 99K trades, Ljung-Box Q=184K, 0/45 negative folds is artifact of autocorrelation |
| Pretrained MBO token transformer → linear probe for direction | Transformer learns event grammar (ppl 1.93) but representations encode syntax, not directional semantics; raw one-hot beats pretrained at all horizons | Thread 05: K=50 pretrained 71.7% vs one-hot 72.0%; K=200 59.9% vs 60.9%; K=1000 51.8% vs 53.5% |
| Sequential BBO features + XGBoost (any cooldown) | 22 inter-episode dynamics features over 20 BBO transitions have zero predictive power regardless of cooldown spacing | Thread 06: 45/45 folds negative at all 7 cooldown values (0–422), expectancy ~$-1.75/trade |
| **XGBoost on any hand-engineered MBO features** | **Four feature sets exhausted (static LOB, flow EMA, seq-features). Tree models on tabular features cannot find directional signal in MES MBO data.** | Threads 03, 04, 06: 0/45 positive folds across all attempts |

---

## What We Did In Response

1. **Moved from bars to events.** Replaced 5s aggregation with committed-state (event-level) processing. Every book update is an evaluation point. Eliminates label flip problem entirely.

2. **Moved from mid to bid/ask entry.** Spread cost is now implicit in execution — no theoretical mid-price fiction.

3. **Moved from classification to probability regression.** Model predicts P(target | LOB state, T, S) instead of direction. Decision rule: trade when P > S/(T+S) + margin.

4. **Built multi-geometry training.** Single model sees 10 (T,S) pairs per evaluation point. Geometry is a feature, not a hyperparameter.

5. **Built rigorous validation.** CPCV (45 folds), serial execution only, tick-level barrier simulation, calibration curves, DSR gating.

6. **Verified book correctness.** BookBuilder validated against Databento MBP-10 ground truth: 99.91% exact match across 54.3M comparisons. Pipeline is not the problem.

7. **Built multi-instrument live pipeline.** Rithmic client supports N symbols on one connection with per-instrument thread isolation. Live-tested with ES+NQ.

8. **Pivoted to MBO grammar learning.** After exhausting XGBoost + hand-engineered features (Threads 03-04, 06), built an MBO tokenizer (126-token vocabulary: action/side/price/size per event) and trained a decoder-only transformer on 3.93B tokens from full-year MES 2022. Phase 1 (LM gate) and Phase 2 (book state reconstruction gate) both passed — the model learns real market grammar and encodes meaningful book state representations.

---

## What Remains True (Useful Infrastructure)

- **BookBuilder:** Verified correct. Cache-friendly sorted-Vec + FxHashMap. Production-grade.
- **Flow features:** 48 event-count EMA features (27 raw + 18 derived + 3 cross-scale). Clean implementation but features themselves showed no signal.
- **Event export pipeline:** Can re-export any feature set from raw .dbn.zst to Parquet at ~3M events/sec.
- **CPCV infrastructure:** Distributed fold sharding, serial backtest, validation gates. Ready for any new feature hypothesis.
- **Rithmic live pipeline:** Multi-instrument, per-instrument threads, BBO health monitoring. Ready for Phase 5 when we have a strategy.
- **Raw data:** 312 .dbn.zst files on S3 (49.2 GB), full year 2022 MES MBO.

---

## The Path Forward: MBO Grammar Foundation Model

### The core insight

Six threads confirmed: **hand-engineered LOB features + XGBoost cannot predict short-term direction on MES MBO data.** Static snapshots, flow EMAs, and sequential BBO dynamics all produced 0/45 positive CPCV folds. The tree-based approach on tabular features is exhaustively dead.

The pivot: let a transformer learn directly from raw MBO event token sequences what patterns matter. The model learns *grammar* (how events relate to each other) and *state* (what the book looks like) — both prerequisites for any directional prediction.

### Where we are

| Phase | Status | Result |
|-------|--------|--------|
| Phase 0: MBO tokenizer + sidecars | DONE | 126-token vocab, 3.93B tokens from 312 files, book_state/mids sidecars on S3 |
| Phase 1: Language model gate | PASSED | ppl 1.864 vs Markov-5 1.984 (6.1% improvement). 6.5M params, 8 layers. |
| Phase 2: Book state reconstruction (Gate 1.5) | PASSED | Size acc 67.4% (vs 23.6% baseline), spread NM 84.4%, imb MAE 0.0665 |
| **Phase 3: Directional signal check** | **READY** | B (pretrained+recon) vs C (random) on next BBO change direction, 15-fold CPCV |

### Concrete next step

**Phase 3** — the critical test. Does pretraining + book state reconstruction give the model a directional advantage over a randomly initialized baseline?

- Code ready: `phase3_signal_check.py`, `cloud-run-phase3.toml`, `run_phase3.sh` in `../mbo-tokenization/research/04-mbo-grammar/`
- Kill gate: B >= majority+2pp in >= 60% of CPCV folds
- Estimated: ~7-15 hours on RTX 4090, ~$5-9

### Future options (contingent on Phase 3)

If Phase 3 passes:
- **Interarrival time encoding** — `[TIME_DELTA]` sub-token (16 log-scale bins). All three major papers encode timing; we don't.
- **Continuous-time RoPE** — replace discrete position index with cumulative time. Needs `.timestamps` sidecar.
- **Cross-instrument pretraining** — bps-normalized pricing (TradeFM approach) to enable MES+ES+NQ joint training.

If Phase 3 fails:
- Re-evaluate whether MBO grammar encodes any tradeable signal at all. Consider cross-instrument approaches or different target formulations (signed move, time-to-fill).

---

## Thread 04: Seq-Features CPCV (2026-03-06)

### Setup

22 hand-crafted sequence features (inter-episode dynamics: price velocity, trade clustering, spread regime changes, volume acceleration across last 20 BBO-change episodes). OFI-directed bilateral labels (ofi_fast >= 0 → long@ask, ofi_fast < 0 → short@bid). Single geometry 10:5. 310 days, 150M rows, 45-fold CPCV on EC2 (c6i.32xlarge spot, 128 vCPU).

### Raw Results

- **0/45 negative folds.** Mean expectancy $9.25/trade, 99,925 total trades, $875K total PnL.
- Every fold profitable. Profit factor 2.86. Win rate 45.7%.

### Why These Results Are Wrong

**Massive trade autocorrelation.** Ljung-Box Q = 184,922 (p=0). The 99,925 "trades" are not independent — they cluster in bursts where the OFI EMA maintains a regime and the model fires on every consecutive BBO change within that regime.

**Root cause is upstream, not in the evaluation layer.** OFI fast is an EMA with a characteristic timescale τ. Meaningful regime changes happen at frequency ~1/τ. But the evaluation grid is every BBO change (millisecond-scale). The model evaluates a slow signal on a fast grid, producing hundreds of redundant, correlated predictions per regime. Differencing the signal doesn't fix this — it just turns a persistent signal into near-zero-with-spikes, and the model learns the spikes, which is the same regime detection one derivative removed.

### Key Insight: Evaluation Grid Must Match Signal Timescale

The evaluation grid should be defined by the *signal's information content*, not by the *data feed*. Evaluating at every BBO change when the direction signal refreshes at ~1/τ produces N/τ redundant samples per meaningful update. This inflates trade count and creates artificial autocorrelation regardless of what the evaluation-layer does to correct it (clustering, N_eff adjustment, etc.).

**Three parallel options with different risk/reward profiles:**
1. *(Today)* Cluster correlated trades in evaluation, pick best entry per cluster. Extracts whatever's salvageable from the current pipeline. Masks the upstream problem.
2. *(Weeks)* Event-driven evaluation in signal space — evaluate only when OFI has moved by threshold δ. Eliminates redundant samples but δ is a hyperparameter encoding our assumption about the signal timescale.
3. *(Research program)* Learned evaluation grid — a sequence model (transformer) over raw MBO events that implicitly learns *when* to evaluate, not just *what* to predict. The attention mechanism is a learned evaluation grid. Sidesteps the hand-engineering problem entirely but may take a year and may not work.

**Provenance note:** The insight that trade clustering is *information about the feature* (not just a statistical nuisance to correct in the evaluation layer), and that the evaluation grid should be defined by the signal's information content rather than the data feed, came from a conversation about the philosophy of statistics and the limits of agent-assisted research — not from staring at ACF plots or running diagnostics. The connection to tokenization/transformers as a structural solution to the grid problem (attention = learned evaluation grid) emerged from the same conversation. This matters because the insight is conceptual, not empirical, and future experiments should be designed to test it rather than assuming it.

### What This Means for the Pipeline

The CPCV infrastructure, serial backtest, and feature export pipeline are all mechanically correct. The bug is conceptual: evaluating a slow signal on a fast grid. Any future feature set evaluated at BBO-change frequency will have this problem unless the features themselves refresh at that frequency (which EMA-smoothed features by construction do not).

**The pipeline needs an autocorrelation diagnostic that is prominent and structural, not a pass/fail gate.** Surface the full ACF with magnitudes. Let it inform interpretation rather than gatekeep reports. At large N, Ljung-Box rejects for trivially small autocorrelations, so a binary threshold invites gaming rather than understanding.

---

## Thread 05: MBO Grammar — Transformer Pretraining + Directional Probe (2026-03-07)

### Hypothesis

If a transformer trained on MBO token sequences (126-token vocabulary: action/side/price/size per event) learns meaningful market microstructure grammar, its internal representations may encode directional information that static LOB features cannot capture. This tests the "learned evaluation grid" idea from Thread 04 — can attention over raw event sequences discover when and what to predict?

### Setup

**Gate 1 (Language Model):** 4-layer transformer (128-dim, 4 heads, 512 FF), 875K params. Trained on 74.5M MBO tokens from MES 2022. Context length 512, stride 256. 10 epochs, batch size 128. Pass criterion: perplexity < 1.984 (Markov-5 baseline).

**Gate 2 (Directional Probe):** Extract hidden states at COMMIT tokens (book update boundaries). Train logistic regression probe to predict future mid-price direction at K={50, 200, 1000} events. CPCV with 6 groups, 14 folds. Three baselines: pretrained transformer, random-init transformer (same architecture, no training), raw one-hot tokens.

**Infrastructure:** Ran via `cloud-run` tool on g4dn.2xlarge (Tesla T4, 32 GB RAM). Docker containerized. Total wall-clock ~2 hours including Gate 1 training (~90 min) and Gate 2 probe (~30 min). Instance self-terminated after uploading results to S3.

### Results

**Gate 1: PASSED.** Transformer perplexity 1.934 vs Markov-5 baseline 1.984. The model learned MBO grammar beyond 5-gram statistics.

Per-token-class perplexity:
- SIDE: 1.52 (most predictable — side is highly constrained by action context)
- SIZE: 1.67
- PRICE: 2.22
- ACTION: 2.48
- SPECIAL/COMMIT: 40,945 (extremely hard — timing of F_LAST is structurally noisy)

**Gate 2: NO directional signal in learned representations.**

| Horizon | Pretrained | Random Init | Raw One-Hot |
|---------|-----------|-------------|-------------|
| K=50    | 71.7%     | 71.5%       | **72.0%**   |
| K=200   | 59.9%     | 58.2%       | **60.9%**   |
| K=1000  | 51.8%     | 50.5%       | **53.5%**   |

- Raw one-hot beats pretrained at all horizons. The predictability comes from the token content itself (especially price-relative tokens, which encode current price level), not from learned representations.
- Pretrained vs random-init delta is tiny (+0.14% to +1.7%) — the learned grammar adds negligible directional information.
- K=1000 is near coin-flip for all representations, confirming longer-horizon unpredictability.

### Interpretation

The transformer successfully learns MBO event grammar — it can predict what kind of event follows what. But this grammatical knowledge does not encode *directional* information. The "grammar" is structural (e.g., ADD_BID is likely followed by specific price/size patterns) rather than predictive of future price direction.

This is a meaningful negative result: even when we move from static snapshots (Threads 01-03) to sequential representations (Thread 05), the linear probe finds no directional signal beyond what raw tokens provide. The attention mechanism learned *syntax*, not *semantics* relevant to price prediction.

### What This Rules Out

- Simple "pretrain LM on events, probe for direction" does not work for MBO sequences. The grammar is real but directionally uninformative.
- A linear probe over single-layer hidden states may be too weak to extract nonlinear directional features. A deeper probe or fine-tuning approach might differ, but the raw one-hot baseline already winning is a strong signal against this.

### What Remains Open

- **Fine-tuning** the transformer on a directional objective (rather than probing frozen representations) is fundamentally different and untested.
- **Cross-instrument sequences** (interleaving ES and NQ events) might carry lead/lag signal invisible in single-instrument grammar.
- **Larger models / more data** — 875K params on 74.5M tokens is small. But raw one-hot winning suggests scale won't help.

### Provenance

Results: `s3://kenoma-labs-research/runs/mbo-grammar-gates-20260307T151445Z/`
Code: `../mbo-tokenization/research/04-mbo-grammar/`
Cloud-run tool: `tools/cloud-run/` (first successful cloud run after fixing 4 bugs in the tool)

---

## Thread 05 Follow-up: Phase 0 — Book State Sidecar + Snapshot Filtering (2026-03-07)

### What Was Done

Implemented Phase 0 of the dual-head reconstruction spec (`.kit/spec-dual-head-reconstruction.md`):

1. **Book state sidecar** (`.book_state`): 12-field post-batch book state vector emitted at every COMMIT. Fields: best_bid_rel, best_ask_rel (ticks from mid), 5 bid sizes, 5 ask sizes. Raw f32 LE, 48 bytes/row, no header. Row-aligned with `.mids` sidecar.

2. **Snapshot filtering in tokenizer**: CLEAR events now enter `in_snapshot` mode. All snapshot events (CLEAR + ADDs until F_LAST) are fed to BookBuilder for correct book rebuild but produce zero tokens, zero mids, zero book_state rows. This is a deviation from the spec (which said to filter in the Python data loader) — filtering at the tokenizer level is cleaner because it prevents contaminated data from ever entering the token stream or sidecars.

### Validation (single-day, Oct 14 2022 MES)

- **Snapshot filtering:** 7,008 events skipped (1 CLEAR + 7,007 snapshot ADDs). CLEAR count = 0 in tokenized output. `price_no_ref = 0` (book always two-sided for real events).
- **Sidecar alignment:** `mids_rows == book_state_rows == 15,653,879`. File size exact (48 * rows).
- **Invariants:** `bid_rel <= 0` and `ask_rel >= 0` hold for 99.994% of rows. Spread == 1 tick for 91.5%.

### Remaining Edge Case

937 crossed-book rows (0.006%) in a single contiguous cluster at rows 5,740,218–5,741,154. This is NOT a snapshot — it's a 635-event batch (mostly cancels/trades/fills, only 89 ADDs) during a session transition. No CLEAR precedes it. The data loader should filter windows where `spread < 0`.

### Files Modified

- `../mbo-tokenization/crates/mbo-tokenizer/src/lib.rs` — `book_snapshot()` method, `in_snapshot` field, snapshot filtering in `feed_event()`, 32/32 tests pass
- `../mbo-tokenization/tools/mbo-tokenize/src/main.rs` — `.book_state` sidecar writing, `--no-book-state` flag

### Next Step

Run full 312-file tokenization to produce final `.book_state` sidecar (~13.49M rows, ~647 MB), upload to S3, then proceed to Phase 1 (Gate 1 retrain at 8M params).

---

## Thread 05 Follow-up: Phase 0b — Full Tokenization + Phase 1 Gate 1 Retrain (2026-03-08)

### Snapshot Filter Fix

The original CLEAR-based snapshot state machine was fundamentally broken. Databento snapshot sequences come in multiple batches, each with F_LAST set. The state machine exited on the first batch's F_LAST, then the `in_snapshot` flag persisted across files, eating all subsequent data. This is why the original tokenization produced only 74.5M tokens (only ~2 months before the filter swallowed everything).

**Fix:** Replaced the state machine with a simple flag check. Databento marks ALL snapshot records with `F_SNAPSHOT` (0x20). The tokenizer now checks `flags & F_SNAPSHOT != 0` — events with this flag are fed to BookBuilder (for correct book rebuild) but produce zero tokens. Non-snapshot CLEAR events (e.g., trading halts) are tokenized normally as ACT_CLEAR.

### Full 312-File Tokenization

Ran `mbo-tokenize` on the full MES 2022 dataset (312 .dbn.zst files, instrument_id=13615) with the corrected snapshot filter:

- **809M events processed**, 517K snapshot events skipped
- **3,929,781,116 tokens** (3.93B) — 53× more than the broken 74.5M
- **695,555,487 COMMIT entries** (mids sidecar: 10.4 GB)
- Tokens/event: 4.86
- tokens.bin: 7.3 GiB (uploaded to `s3://kenoma-labs-research/cloud-runs/mbo-grammar/tokens.bin`)
- book_state sidecar: truncated at 584M/695M entries (ran out of local disk at 97%). Not needed for Phase 1. Will regenerate for Phase 2.

### Phase 1: Gate 1 Retrain (8M Model) — PASSED

**Model:** 6.5M params (d_model=256, 8 layers, 8 heads, dim_ff=1024, weight-tied). Trained on RTX 4090 via RunPod ($0.59/hr).

**Training config:** 10 epochs, batch 128, context 512, stride 2048, lr 3e-4, cosine schedule, AdamW (wd=0.01).

**Results:**

| Metric | Value |
|--------|-------|
| Transformer ppl | **1.864** |
| Markov-5 baseline | 1.984 |
| Improvement | **6.1%** |
| Best epoch | 9/10 |

Per-token-class perplexity:
- SIDE: 1.459 (best — bid/ask highly predictable from context)
- PRICE: 1.848 (relative tick position is learnable)
- SIZE: 1.944 (noisiest token class)
- ACTION: 2.302
- SPECIAL: 15,378 (expected — BOS/EOS/COMMIT timing is structurally hard)

**Comparison to 875K model (Thread 05):** The 8M model on 3.93B tokens (1.864) beats the 875K model on 74.5M tokens (1.934) by a comfortable margin. Both beat Markov-5. The grammar is real and the larger model exploits it better — but the model is undersized for the data by Chinchilla standards (~30×), so there's room for more capacity.

### Infrastructure Fixes

Fixed 5 bugs in `cloud-run` RunPod backend during this session:
1. GraphQL env var escaping — literal `\"` outside strings → use raw quotes, let serde_json handle JSON layer
2. `cloudType: "SECURE"` → unquoted enum `cloudType: SECURE`
3. Datacenter pinning caused supply constraint failures → omit `dataCenterId` to let `podFindAndDeployOnDemand` auto-select
4. PyTorch container missing `curl` → bake `awscli` via pip in Dockerfile
5. `mkdir -p` on file path created directory → use `$(dirname ...)` for parent
6. Added server-side TTL watchdog to RunPod bootstrap (parity with EC2 user-data)
7. Self-stop still broken (uses `curl` which isn't in container) — needs fix to use python urllib or install curl

**Pod idle cost:** ~$5.30 (9 hours idle after experiment completed because self-stop curl failed). TTL watchdog will prevent this on future runs.

### Provenance

- Results: `s3://kenoma-labs-research/runs/mbo-grammar-phase1-20260308T023435Z/`
- Model checkpoint: `best_model.pt` (78 MB)
- Code: `research/04-mbo-grammar/`

### Next Step

~~Phase 2: dual-head book state reconstruction.~~ DONE — Gate 1.5 PASSED. Proceed to Phase 3.

---

## Thread 05 Follow-up: Phase 0c — Book State Sidecar Regeneration (2026-03-08)

### Problem

The `.book_state` sidecar from Phase 0b was truncated at 584M/695M entries (local disk ran out). Phase 2 requires the full sidecar.

### Streaming Rewrite of mbo-tokenize

The original `mbo-tokenize` CLI accumulated ALL output in memory (`Vec<u16>` for tokens, `Vec<(u64,i64)>` for mids, `Vec<[f32;12]>` for book_state) before writing to disk. For the full dataset this is ~49 GiB in RAM — on a c7a.2xlarge with 16 GiB, the process swap-thrashed for 4+ hours before being killed.

**Fix:** Rewrote main loop to stream all output through `BufWriter` (1 MiB buffer each). Tokens, mids, and book_state are written incrementally as events are processed. Memory usage dropped from ~49 GiB to <10 MiB.

### cloud-run Infrastructure Fixes

1. **Heartbeat decoupled from result sync** — heartbeat was in a loop that also did `aws s3 sync /results/`. A growing 33 GiB file blocked the heartbeat indefinitely. Split into two background processes: heartbeat every 60s (lightweight), result sync every 300s (independent).
2. **Crash resilience** — `cleanup_and_die()` now syncs `/results/` to S3 before shutdown, preserving partial results.
3. **SIGPIPE fix** — `ls | head` with `set -eo pipefail` returns exit 141. Added `|| true`.
4. **S3 path fix** — data was in a subdirectory (`GLBX-20260207-L953CAPU5B/`) that the initial config missed.

### Results

Ran on EC2 c7a.large (2 vCPU, 4 GiB RAM) spot instance. Total wall-clock: **~40 min** (vs 4+ hours swap-thrashing on c7a.2xlarge). Cost: ~$0.03.

- **book_state:** 33,386,663,376 bytes, 695,555,487 rows, remainder=0. Validated.
- **mids:** 10.4 GiB, 695,555,487 entries. Row-aligned with book_state.
- **meta.json:** Matches Phase 0b stats exactly (3.93B tokens, 695M commits, 312 files).
- All sidecars uploaded to canonical S3 location: `s3://kenoma-labs-research/cloud-runs/mbo-grammar/`

### Observation: .mids Sidecar is Dead Weight for Phase 2

The `.mids` file (10.4 GiB) stores `(u64 commit_position, i64 mid_price)` pairs. Phase 2 (`train_finetune.py`) only uses the `pos` field as a COMMIT position index — mid price values are loaded and immediately discarded. The 5.18 GiB of mid price data serves zero purpose. COMMIT positions could be derived by scanning tokens.bin for COMMIT tokens (~5 sec at startup), eliminating the sidecar entirely. Low priority but noted for future optimization.

### Provenance

- Run: `mbo-tokenize-bookstate-20260308T172517Z`
- Instance: i-0b459e41b8a5421f6 (c7a.large, spot, auto-terminated)
- Code: `../mbo-tokenization/tools/mbo-tokenize/src/main.rs` (streaming rewrite)
- cloud-run fix: `tools/cloud-run/src/userdata.rs` (heartbeat decoupling)

---

## Thread 06: Cooldown Sweep — Sequential Features CPCV (2026-03-08)

### Hypothesis

Thread 04 showed seq-features had massive trade autocorrelation (Ljung-Box Q=184K) due to evaluating a slow OFI signal on a fast grid. Adding cooldown between trades might decouple entries enough to reveal genuine signal. Sweep tests cooldown values 0, 51, 52, 104, 210, 422 + best-in-window@210.

### Setup

22 seq-features (inter-episode BBO dynamics over 20-event windows). Single geometry 10:5. 230 days, 45-fold CPCV. Subsample 15%. XGBoost: max_depth=6, eta=0.01, min_child_weight=100, 3000 trees, early stopping 100. 6 parallel folds on c7a.16xlarge spot (64 vCPU).

### Results

| Cooldown | Mean Expectancy | Neg Folds | Sharpe | Trades |
|----------|----------------|-----------|--------|--------|
| 0        | $-1.66 ± $0.46 | 45/45     | -0.11  | 32,128 |
| 51       | $-1.76 ± $0.44 | 45/45     | -0.12  | 31,775 |
| 52       | $-1.76 ± $0.45 | 45/45     | -0.12  | 31,775 |
| 104      | $-1.76 ± $0.44 | 45/45     | -0.12  | 31,682 |
| 210      | $-1.75 ± $0.44 | 45/45     | -0.12  | 31,494 |
| 422      | $-1.74 ± $0.44 | 45/45     | -0.12  | 31,081 |
| 210+BIW  | $-1.71 ± $0.44 | 45/45     | -0.12  | 31,452 |

**All 45/45 folds negative across every cooldown value.** Cooldown makes no difference — expectancy is ~$-1.75/trade regardless. Win rate ~32.8% vs 33.3% null.

### Interpretation

The Thread 04 autocorrelation was a symptom, not the disease. Removing trade clustering via cooldown confirms there was never real signal — the seq-features (BBO transition dynamics over 20-event windows + XGBoost) simply do not predict direction.

**This exhausts XGBoost as a model class for this problem.** Four feature sets tested — static LOB (Thread 03), slow flow features (Thread 04), and sequential BBO dynamics (Threads 04 + 06) — all produce 0/45 positive folds. The tree-based approach on hand-engineered tabular features is dead.

### Provenance

Results: `s3://kenoma-labs-research/runs/cooldown-sweep-20260308T034027Z/results/`
Config: `docker/backtest/cloud-run.toml`
Instance: c7a.16xlarge spot, ~7 hours total

---

## Competitive Landscape Assessment (2026-03-08)

### Position Summary

Literature review of 13 papers across three waves (discriminative LOB models, MBO deep learning, foundation models) confirms an **unoccupied whitespace**: no published foundation model exists for CME futures MBO data. All published work uses NASDAQ equities (LOBSTER/ITCH), cryptocurrency, or Chinese equities. Futures microstructure is qualitatively different — no opening auction, different participant composition, calendar spread dynamics, different fee structures — so equity results should not be assumed to transfer.

Full review: `.kit/lit-review-lob-dl.md`
Deep-dive technical notes: `.kit/lit-deep-dives.md`

### TradeFM Threat Assessment (HIGH)

TradeFM (Kawawa-Beaudan et al., JPMorgan AI, arXiv:2602.23784, Feb 2026) is the primary competitive threat: 524M params, 19B tokens, 9,000+ US equities, decoder-only transformer. Uses mixed-radix 16,384-token composite encoding with scale-invariant bps normalization. Zero-shot generalization to APAC markets.

**However:** TradeFM models only add/cancel events (no trades/fills/modify/clear), has no book state reconstruction, no multi-event batch boundaries, and uses EW-VWAP reference price (noisy vs our exact L3 mid). Our full MBO reconstruction is strictly more information. JPMorgan could extend to futures in 6-12 months with their data access — speed matters.

### Architectural Validations from Literature

- **8M model size:** MarS scaling laws (2M-1.02B params on 32B tokens) show our 8M model is well-calibrated for 3.93B tokens (data-limited regime). Going to 50M+ requires more data.
- **Factored tokenization:** At 8M params, compositionality (shared embeddings, 126 vocab) beats composite encoding (16K+ vocab). TradeFM's composite wins at 500M+ where the model can memorize all token semantics and wider context dominates.
- **Decoder-only transformer:** All successful large models (TradeFM, MarS, Kronos) use decoder-only transformers. Our architecture choice is validated.

### LOBS5 Risk Flag for Phase 2

LOBS5 (Nagy et al., Oxford, arXiv:2309.00638) explicitly found that "training and validation loss drastically improves when using book data in addition to message sequences" — i.e., the model cannot learn book state from messages alone without explicit input. This is a **direct risk flag for our Phase 2 Gate 1.5 pre-batch reconstruction head**, which must recover book state from ~105 events of context without book state input.

The post-batch head (predicting book state after processing the batch's events) is a local computation and should be easier. The pre-batch head is the risky one.

### Extractable Components (Post-Phase 2)

Three high-value technical components identified for future integration:

1. **Continuous-time RoPE (LOBERT):** Replace discrete position index with cumulative time `phi_i(t) = t * theta_i`. Needs `.timestamps` sidecar (~2 hrs Rust). Integration ~2-3 days. Priority: post-Phase 3.
2. **Interarrival time encoding (TradeFM/LOBS5/LOBERT):** Add `[TIME_DELTA]` sub-token with 16 log-scale bins. Increases tokens/event from ~4.88 to ~5.88 (~17% context reduction). Quick ablation after Phase 2 Gate 1.
3. **MarS-style additive conditioning (fallback):** If Gate 1.5 pre-batch fails, add `h_commit += linear_proj(book_state_prev)` (2-4 hrs). Keeps post-batch reconstruction target. LOBS5 and MarS both feed book state as input — our reconstruction-as-target approach is novel but unproven.

### Conclusion

Phase 2 (dual-head book state reconstruction) is confirmed as the correct next step. The literature validates every major design choice (model size, tokenization, architecture, reconstruction objective) while surfacing one specific risk (pre-batch head difficulty) with a concrete fallback plan.

---

## Thread 05 Follow-up: Phase 2 — Gate 1.5 Dual-Head Book State Reconstruction (2026-03-10)

### Hypothesis

If the transformer can reconstruct book state (BBO sizes, spread, imbalance) from ~105 events of context, its internal representations encode meaningful market state information. This is a necessary condition for directional prediction — if the model can't even recover what the book looks like, it won't predict what price does next.

### Setup

Loaded Phase 1 checkpoint (8M params, ppl 1.864), added two reconstruction heads (pre-batch and post-batch, ~112K params each). Trained with combined loss: `L = 1.0*L_lm + 0.1*L_pre_recon + 0.1*L_post_recon`. RunPod RTX 4090 ($0.59/hr).

**Training config:** 10 epochs, batch 256, context 512, stride 2048, lr 3e-4, cosine schedule, AdamW (wd=0.01), AMP bf16.

**Architecture note:** Replaced `nn.TransformerEncoder` with custom `CausalBlock` calling `F.scaled_dot_product_attention(is_causal=True)` directly. This bypasses PyTorch 2.5.1's three-level block on FlashAttention during training (nn.TransformerEncoder requires explicit attn_mask, nn.MultiheadAttention blocks fast path with dropout > 0, and multi_head_attention_forward's standard path also requires attn_mask with is_causal). The custom block also required checkpoint key remapping from Phase 1's nn.TransformerEncoder parameter names. VOCAB_SIZE padded 126→128 for tensor core alignment.

### Speed Optimization Journey

| Version | Changes | Time/epoch |
|---------|---------|-----------|
| v1 (fp32) | Baseline | 3210s |
| v4 (AMP bf16 + compile) | torch.compile with float causal mask | 1203s |
| v6 (+ streaming) | StreamingPackedDataset | 1191s |
| v7-v9 (crashes) | Attempted FlashAttention via nn.TransformerEncoderLayer, torch.compile + CausalBlock | crashed |
| **v10 (FlashAttention)** | **Custom CausalBlock + direct SDPA, no compile** | **840s** |

Key insight: `torch.compile` was counterproductive because it forced the float causal mask code path, blocking FlashAttention. The 25% speedup from v6→v10 came entirely from FlashAttention dispatch via direct `F.scaled_dot_product_attention(is_causal=True)`.

### Results

**Run:** `mbo-grammar-phase2-20260310T020658Z` | **Best epoch:** 4 | **Cost:** ~$1.50

Training progression (8 of 10 epochs captured before pod restart overwrote log):

| Epoch | LM loss | Pre recon | Post recon | Size L1 acc | Spread NM | Imb MAE | Time |
|-------|---------|-----------|------------|-------------|-----------|---------|------|
| 1 | 0.5925 | 0.7692 | 0.7775 | 59.7% | 83.4% | 0.0734 | 842s |
| 2 | 0.5900 | 0.6983 | 0.7087 | 59.9% | 75.6% | 0.0774 | 839s |
| 3 | 0.5884 | 0.7000 | 0.7102 | 62.2% | 82.5% | 0.0744 | 840s |
| 4 | 0.5903 | 0.6955 | 0.7062 | 67.7% | 80.6% | 0.0668 | 839s |
| 5 | 0.5876 | 0.6818 | 0.6927 | 64.7% | 83.2% | 0.0711 | 838s |
| 6 | 0.5863 | 0.6601 | 0.6714 | 62.7% | 88.6% | 0.0709 | 840s |
| 7 | 0.5872 | 0.6640 | 0.6753 | 66.5% | 80.7% | 0.0687 | 839s |
| 8 | 0.5854 | 0.6535 | 0.6648 | 63.7% | 82.7% | 0.0724 | 839s |

### Gate 1.5 Verdict: PASS (all three criteria)

| Criterion | Baseline | Gate | Result | Margin |
|-----------|----------|------|--------|--------|
| Size acc (level 1) | 23.6% | 33.6% (+10pp) | **67.4%** | +43.8pp |
| Spread non-modal acc | — | >40% | **84.4%** | +44.4pp |
| Imbalance MAE | 0.2273 | <0.15 | **0.0665** | 3.4x better |

### Pre-batch vs Post-batch Comparison

| Metric | Pre-batch | Post-batch |
|--------|-----------|------------|
| Size acc (level 1) | 67.0% | 67.4% |
| Size acc (overall) | 64.3% | 65.5% |
| Spread non-modal | 86.5% | 84.4% |
| Imbalance MAE | 0.0656 | 0.0665 |

Pre-batch slightly outperforms post-batch on spread and imbalance. **The LOBS5 risk flag (pre-batch head might fail) did NOT materialize.** The model successfully recovers book state from ~105 events of context without explicit book state input.

### Interpretation

The model encodes meaningful book state representations in its hidden states. This is a necessary (but not sufficient) condition for directional prediction. The reconstruction heads show that:

1. **Size prediction is strong** (+43.8pp over unconditional mode). The model knows what the book depth looks like.
2. **Spread is nearly solved** (97.8% overall, 84.4% on the hard non-modal cases). The model tracks BBO dynamics.
3. **Imbalance is well-calibrated** (0.0665 MAE vs 0.2273 baseline). The model understands buy/sell pressure distribution.
4. **Pre-batch works**: the model carries a running book state representation across positions, not just computing locally from the current batch.

### Infrastructure Notes

- Pod self-stop still broken (uses curl which isn't in container). Two pods were manually terminated after training completed.
- Pod auto-restarts caused the experiment to re-run, overwriting the log. Full 10-epoch results captured in `gate1_5_results.json` before the overwrite.

### Provenance

- Results: `s3://kenoma-labs-research/runs/mbo-grammar-phase2-20260310T020658Z/results/`
- Checkpoint: `best_model.pt` (77 MB, epoch 4)
- Code: `../mbo-tokenization/research/04-mbo-grammar/` (model.py, train_finetune.py)

### Next Step

Phase 3: Signal Check. Run conditions B (pretrained + recon + direction) and C (random init + direction only) on "next BBO change direction" target. 15-fold CPCV. Kill gate: B beats majority class by >= 2pp in >= 60% of folds. Code and configs ready: `phase3_signal_check.py`, `cloud-run-phase3.toml`.
