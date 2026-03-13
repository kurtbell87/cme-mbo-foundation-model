# Spec: Dual-Head Book Reconstruction + Directional Fine-Tuning

**Project:** MBO Grammar Transformer (Thread 05 follow-up)
**Author:** Brandon / Research Director
**Date:** 2026-03-07
**Status:** Proposed
**Depends on:** Thread 05 Technical Analysis, Gate 1 checkpoint (epoch 10, ppl 1.934)

---

## 1. Motivation

The Gate 2 probe found no directional signal in frozen transformer representations. However, the negative result is weak: the baseline was broken (one-hot at COMMIT is constant), the probe was linear-only, and no fine-tuning was tested.

More fundamentally, the LM objective never required the model to maintain a book state representation. The model can predict the next ACTION token from local transition statistics alone — it has no training signal to compress "what does the book look like right now" into its hidden state. The 88% single-event batch rate (15.26M events / 13.49M COMMITs = 1.13 events/batch) confirms this: for most COMMITs, predicting the next token requires almost no book awareness.

This spec adds two things: (1) an auxiliary dual-head reconstruction objective that forces explicit book state encoding, and (2) end-to-end directional fine-tuning — the strongest test not yet run.

It also provides Gate 1.5: if the model cannot recover book state from event context, no downstream directional head will find that information in the representations.

---

## 2. Book State Sidecar

### Data Generation

Emit a 12-field post-batch book state vector at every COMMIT position during tokenization. The tokenizer's BookBuilder already tracks full book state — this is an extraction step, not new computation. Mid-price is already in the existing `.mids` sidecar; spread is derivable from BBO fields. Neither is stored redundantly.

| Index | Field | Encoding | Notes |
|-------|-------|----------|-------|
| 0 | best_bid | (bid_price - mid) / tick_size as f32 | Always <= 0; typically -0.5 for 1-tick spread |
| 1 | best_ask | (ask_price - mid) / tick_size as f32 | Always >= 0; typically +0.5 for 1-tick spread |
| 2-6 | bid_size levels 1-5 | Raw u32 cast to f32 | Level 1 = best bid. 0 if fewer levels exist. |
| 7-11 | ask_size levels 1-5 | Raw u32 cast to f32 | Level 1 = best ask. 0 if fewer levels exist. |

**Total:** 12 x f32 per COMMIT = 48 bytes/row. At 13.49M COMMITs = ~647 MB. Generated once (CPU-only), reused across all experiments.

Sizes are stored **raw** (not log-normalized). The data loader discretizes into the tokenizer's 9 log2 buckets (0, 1, 2-3, 4-7, 8-15, 16-31, 32-63, 64-127, 128+) for the classification targets. Storing raw preserves the option of regression targets in future experiments.

Spread is computed by the data loader as `sidecar[1] - sidecar[0]` (best_ask_rel - best_bid_rel), always positive.

Imbalance is computed by the data loader as `sidecar[2] / (sidecar[2] + sidecar[7])` (bid_size_1 / (bid_size_1 + ask_size_1)), clamped to [0, 1]. If both are 0, imbalance = 0.5.

**Critical detail:** The pre-batch reconstruction target at COMMIT N is the post-batch sidecar row at COMMIT N-1. One sidecar file, shifted index in the data loader. No duplicate data.

### Snapshot Handling

**UPDATE (Phase 0 implementation):** Snapshot filtering is now done in the tokenizer, not the data loader. CLEAR events set `in_snapshot = true`. All snapshot events (CLEAR + ADDs until F_LAST) are fed to BookBuilder for correct book rebuild but produce zero tokens, zero mids, zero book_state rows. The token stream and sidecars contain only real market events.

**Remaining edge case:** ~937 crossed-book rows (0.006%) from a session transition that lacks a CLEAR. The data loader should filter windows where `spread < 0` (i.e., `sidecar[1] - sidecar[0] < 0`).

---

## 3. Architecture

### Dual Reconstruction Heads

At each COMMIT position, two heads predict book state at different points in time:

| Head | Target at COMMIT N | What it forces the model to learn |
|------|-------------------|-----------------------------------|
| Pre-batch | Book state at COMMIT N-1 | State maintenance — carry a running book representation |
| Post-batch | Book state at COMMIT N | State transition — understand what the current event(s) did |
| Delta (implicit) | Post minus Pre | Event impact — the informational content of the batch |

Both heads share the same architecture but have independent parameters:

```
hidden_state[COMMIT] (d_model)
    |
    |---> Linear(d_model, d_model/2) -> GELU -> Linear(d_model/2, 9x10) -> reshape [10, 9]
    |        +-- 10 size fields: 5 bid levels + 5 ask levels, each 9-class CE (log2 buckets)
    |
    |---> Linear(d_model, d_model/2) -> GELU -> Linear(d_model/2, 11)
    |        +-- spread: 11-class CE (ticks 0-10, where class 10 = ">=10"; class 0 = crossed/invalid)
    |
    +---> Linear(d_model, d_model/2) -> GELU -> Linear(d_model/2, 1) -> sigmoid
             +-- imbalance: bid_size_1 / (bid_size_1 + ask_size_1), MSE on [0, 1]
```

At d_model=256, each head adds ~100K parameters (x2 = ~200K total). Negligible relative to model size (8M).

### Reconstruction Target Progression

Start with BBO-only (indices 0-1 prices + indices 2,7 level-1 sizes = 4 values). The full 12-field sidecar is already generated, so scaling up to all 5 depth levels is a config flag, not a data regeneration step.

| Scope | Sidecar indices | Recon targets | Rationale |
|-------|----------------|---------------|-----------|
| BBO-only | [0,1,2,7] | 2 sizes (9-class CE) + spread (11-class CE) + imbalance (MSE) | Validates architecture. Easiest reconstruction task. |
| Full depth | [0-11] | 10 sizes (9-class CE each) + spread (11-class CE) + imbalance (MSE) | Book shape — thin ask + fat bid is directionally different from symmetric depth, even at identical BBO. |

### Directional Head

```
hidden_state[COMMIT] (d_model) -> Linear(d_model, d_model/2) -> GELU -> Linear(d_model/2, 1) -> sigmoid
```

- Target: sign(mid[i+K] - mid[i]) with binary cross-entropy
- Cost-sensitive loss: weight minority class inversely to frequency
- K as an input feature (concatenated before first linear) for multi-horizon training

### Combined Loss

```
L = L_direction + a*L_pre_recon + b*L_post_recon + g*L_lm
```

| Term | Default | Role |
|------|---------|------|
| L_direction | 1.0 | Primary objective |
| a (pre-recon) | 0.1 | State maintenance regularizer |
| b (post-recon) | 0.1 | State transition regularizer |
| g (LM) | 0.01 | Light grammar regularizer; prevents catastrophic forgetting of pretrained representations |

Sweep a, b in {0.05, 0.1, 0.5} and g in {0.0, 0.01, 0.1}.

---

## 4. Gate 1.5: Reconstruction Validation

Before running directional experiments, validate that the reconstruction heads actually work. Train with LM + reconstruction losses only (no directional head) and measure:

| Metric | Pass Threshold | Baseline | Notes |
|--------|---------------|----------|-------|
| Best bid/ask size accuracy (top-1 bucket) | > baseline + 10pp | Unconditional mode (~30-40%) | Must beat marginal distribution by a meaningful margin |
| Spread accuracy on non-modal samples (spread > 1 tick) | > 40% | Random (~10% over 10 classes) | MES spread is 1 tick ~90%+ of RTH; modal accuracy is trivial. Measure on the hard cases. |
| Imbalance MAE | < 0.15 | Predict 0.5 always (~0.25 MAE) | |

**Step 0 (before evaluating thresholds):** Compute unconditional baselines from the validation split of the sidecar. Report the marginal class distribution for each size bucket and spread class. The pass thresholds above are relative to these baselines, not absolute.

If Gate 1.5 fails, the model cannot recover book state from 105 events of context. This would indicate either a context length problem (book state depends on older history) or a capacity problem (128-dim is too small). Either way, the directional experiment is pointless — skip it and go to scale-up.

---

## 5. Experimental Conditions

### Matrix

All transformer conditions (A-D) use the **8M primary model** (d=256, 8 layers). Conditions A* and B* replicate A and B at 875K as a capacity diagnostic.

| Condition | Model | LM Pretrain | Recon Heads | Directional Head | Tests |
|-----------|:-----:|:-----------:|:-----------:|:----------------:|-------|
| A | 8M | Y | N | Y | Does grammar pretraining help direction? |
| B | 8M | Y | Y | Y | Does pretraining + reconstruction help direction? |
| C | 8M | N | N | Y | Tabula rasa baseline — is sequence modeling useful at all? |
| D | 8M | N | Y | Y | Does reconstruction alone (no grammar) help direction? |
| A* | 875K | Y | N | Y | Capacity diagnostic: does 8M beat 875K? |
| B* | 875K | Y | Y | Y | Capacity diagnostic: does reconstruction scale with model size? |
| E | — | — | — | — | XGBoost on static window features (OFI, imbalance, etc.) |
| F | — | — | — | — | Majority class (true base rate) |

Key comparisons:
- **B vs A** -> value of reconstruction auxiliary loss
- **D vs C** -> value of reconstruction without grammar pretraining
- **A vs C** -> value of grammar pretraining without reconstruction
- **B vs F** -> does anything work at all?
- **B vs E** -> does sequence modeling beat static features?
- **A vs A*** -> value of model scale (is 875K a capacity bottleneck?)
- **B vs B*** -> does reconstruction benefit more from capacity?

### Directional Targets

| Target | Type | Rationale |
|--------|------|-----------|
| Next BBO change direction | Binary CE | Tightest, most tradeable, avoids trend bias (BBO changes are roughly symmetric) |
| sign(mid[i+K] - mid[i]) for K in {50, 200, 1000} | Binary CE | Comparable to Gate 2 but with fine-tuning |
| Signed tick change at COMMIT+K | Regression (MSE) | Avoids classification threshold artifacts |
| Significant move: mid moves >= 2 ticks in next K COMMITs, which direction? | 3-class (down/flat/up) | Filters noise, focuses on tradeable moves |

### Model Scale

The primary model is 8M parameters (d=256, 8 layers). The 875K model from Gate 1 is retained as a capacity diagnostic — if both fail, the cause is not model size. The large model is contingent on positive signal at 8M.

| Model | Params | d_model | Layers | Heads | FF dim | Data/Param Ratio | Role |
|-------|--------|---------|--------|-------|--------|-----------------|------|
| Small | 875K | 128 | 4 | 4 | 512 | 85:1 | Capacity diagnostic (Gate 1 checkpoint exists) |
| **Primary** | **8M** | **256** | **8** | **8** | **1024** | **9:1** | **Main experiment for all conditions** |
| Large | 50M | 512 | 12 | 8 | 2048 | 1.5:1 | Contingent on positive signal at 8M |

**Why 8M:** Chinchilla scaling suggests ~3.7M params optimal for 74.5M tokens. Our tokens are lower-entropy (~1 bit vs ~10 for language), so effective information per token is lower, but the reconstruction task is denser than pure LM. 8M is slightly over Chinchilla-optimal for language, approximately right for our setting. d=256 provides 2x the representational headroom for book state encoding. 8 layers doubles the depth for implicit state tracking across positions.

**Gate 1 retraining:** The 8M model requires a new Gate 1 pretraining run (the existing checkpoint is 875K/d=128). This adds ~4 GPU hours to the compute budget. The 875K Gate 1 checkpoint is reused for the small model conditions.

The large model needs more data for robust training. Multi-instrument (ES, NQ, MNQ) and multi-year would push toward 1-2B tokens. Only pursue if the 8M model shows a positive signal worth amplifying.

---

## 6. Kill Criteria

### Primary gate (the only one that matters)

> If **no condition** (A-D) beats majority class (F) by >= 2pp across >= 60% of CPCV folds on **any** target formulation, the MBO-sequence-to-direction pipeline is dead on MES. Stop.

### Diagnostic gates (inform next steps if primary passes)

> **Pretraining value:** If A does not beat C by >= 2pp, grammar pretraining doesn't help. Future work should skip pretraining and train end-to-end from scratch.

> **Reconstruction value:** If B does not beat A by >= 1pp, the reconstruction auxiliary loss isn't the bottleneck. The constraint is elsewhere (context length, data volume, or the signal simply isn't there at this latency).

> **Capacity value:** If A does not beat A* by >= 1pp, scaling from 875K to 8M didn't help. The bottleneck is not representational capacity. Don't pursue the 50M model.

> **Sequence vs static:** If no transformer condition beats XGBoost (E), static features capture whatever's there. The tokenization approach adds complexity without value.

---

## 7. Execution Phases

Each phase is a single `cloud-run` invocation (except Phase 0 which is local). Phases are strictly sequential — each phase has a go/no-go gate before the next phase starts. This minimizes wasted compute: the experiment stops at the first failure instead of burning the full budget.

### Phase 0: Sidecar Generation
**Local CPU, ~30 min, $0**

Extend tokenizer to emit 12-field book state at each COMMIT (see 10b). Validate: `book_state_rows == mids_rows == meta.json["commits"]` (13.49M). Upload `.book_state` to S3 alongside existing `tokens.bin` and `.mids`.

### Phase 1: Gate 1 Retrain (8M)
**Single GPU, ~30 min on H100 / ~2 hrs on T4, ~$2-3**

Train 8M model (d=256, 8 layers, 8 heads, FF=1024) on LM objective. Same setup as original Gate 1 but with larger model. Save checkpoint.

**Gate:** ppl must be < 1.934 (875K baseline). More capacity should help. If not, something is wrong with the training — debug before continuing.

### Phase 2: Gate 1.5 (Reconstruction)
**Single GPU, ~30 min on H100 / ~2 hrs on T4, ~$2-3**

Train LM + dual reconstruction (no directional head) from 8M Gate 1 checkpoint. Validate reconstruction accuracy against unconditional baselines.

**Gate:** Must beat unconditional baselines per Section 4 thresholds. If fail, the model cannot recover book state from ~105 events of context. The directional experiment is pointless — investigate context length or data volume before continuing. **Stop. Total spend: ~$5.**

### Phase 3: Signal Check (Minimum Viable Test)
**Single GPU, ~4 hrs on H100 / ~12 hrs on T4, ~$5-10**

Run only the two most informative conditions on the single tightest target:

| Condition | Target | Folds | Runs |
|-----------|--------|-------|------|
| B (8M, pretrained + recon) | Next BBO change direction | 15 | 15 |
| C (8M, random init, no recon) | Next BBO change direction | 15 | 15 |
| **Total** | | | **30 runs** |

Compute majority-class baseline F from each fold's test set.

**Gate (primary kill):** B must beat F by >= 2pp across >= 60% of folds. If not, **stop. MBO sequences do not predict direction on MES. Total spend: ~$9.**

**Diagnostic:** Compare B vs C.
- B >> C (>= 2pp): Pretraining + reconstruction both help. Expand.
- B ≈ C, both > F: Sequence modeling helps but pretraining doesn't. The LM stage is wasted work. Consider dropping it.
- B ≈ C ≈ F: Nothing works. Stop.

### Phase 4: Targeted Expansion
**Single GPU, ~8-16 hrs on H100, ~$10-20**

Only reached if Phase 3 shows signal. Scope depends on Phase 3 diagnostics:

**If B > C > F (pretraining + recon both help):**
- Run A (pretrained, no recon) to isolate reconstruction value (B vs A)
- Run D (random init + recon) to isolate pretraining value (B vs D)
- Expand to remaining targets: sign(mid[i+K]) for K={50,200,1000}, signed tick change, significant move
- Run A*, B* (875K) on best target to measure capacity value
- Total: up to 4 conditions × 4 targets × 15 folds = 240 runs

**If B ≈ C > F (sequence modeling helps, pretraining doesn't):**
- Drop pretraining. Focus on C + D (random init ± reconstruction)
- Test remaining targets
- Total: 2 conditions × 4 targets × 15 folds = 120 runs

### Phase 5: Full CPCV
**Single GPU, ~4-6 hrs on H100, ~$5-8**

Full 45-fold validation (10 groups, 2 test) on the winning condition/target pair from Phase 4.

### Phase 6: XGBoost Baseline
**CPU (local or cheap instance), ~2 hrs, ~$1**

15 features (see 10i), 500 trees, CPCV with same fold structure. Tests whether sequence modeling adds value over static features.

**Gate:** If XGBoost matches the best transformer condition, the tokenization approach adds complexity without value. The signal (if any) is in static book state, not in event sequences.

### Phase 7: Cross-Instrument (Contingent)
**Single GPU, ~12-20 hrs on H100, ~$15-25**

Only if Phase 5 shows robust signal. Tokenize ES + NQ MBO data, interleave sequences, test whether cross-instrument context improves predictions.

---

## 8. Cost Summary

| Outcome | Phases Run | Total Spend | Wall-clock (H100) |
|---------|-----------|-------------|-------------------|
| Gate 1.5 fails | 0-2 | ~$5 | ~1.5 hrs |
| Phase 3: no signal (B ≈ F) | 0-3 | ~$9 | ~5 hrs |
| Phase 3: signal but pretraining useless (B ≈ C > F) | 0-4 | ~$20 | ~12 hrs |
| Phase 3: full signal (B > C > F), through CPCV | 0-6 | ~$30-40 | ~24 hrs |
| Everything works, including cross-instrument | 0-7 | ~$50-65 | ~36 hrs |

**Compute platform:** The 8M model fits on any modern GPU (32MB params). Use **RunPod** via the `cloud-run` CLI (RunPod backend merged to main: `15c799a`). Config auto-detects backend from `[runpod]` section presence. Recommended GPU: `NVIDIA H200 SXM` (~$3/hr) or `NVIDIA GeForce RTX 4090` (~$0.70/hr). No multi-GPU nodes needed. No data/tensor/pipeline parallelism — the model is too small. Each phase is one `cloud-run` invocation with a self-contained Python entry point.

Example `cloud-run.toml` for RunPod:

```toml
[experiment]
name = "mbo-grammar-phase1"

[container]
dockerfile = "Dockerfile"
context = "."
dockerhub_repo = "kurtbell87/mbo-dl"

[instance]
region = "us-east-1"  # for S3 access
gpu = true

[runpod]
gpu_type = "NVIDIA GeForce RTX 4090"
container_disk_gb = 40
gpu_count = 1

[data]
sources = [
    { s3 = "s3://kenoma-labs-research/cloud-runs/mbo-grammar/tokens.bin", path = "/data/tokens.bin" },
    { s3 = "s3://kenoma-labs-research/cloud-runs/mbo-grammar/tokens.bin.mids", path = "/data/tokens.bin.mids" },
    { s3 = "s3://kenoma-labs-research/cloud-runs/mbo-grammar/tokens.bin.book_state", path = "/data/tokens.bin.book_state" },
    { s3 = "s3://kenoma-labs-research/cloud-runs/mbo-grammar/tokens.bin.meta.json", path = "/data/tokens.bin.meta.json" },
]

[results]
s3_prefix = "s3://kenoma-labs-research/runs"

[run]
command = "python train_finetune.py --phase 1 --results-dir /results"

[heartbeat]
interval_seconds = 60
```

Swap `gpu_type` to `"NVIDIA H200 SXM"` for faster phases. Update `[run] command` per phase.

---

## 9. Files to Modify

Superseded by the expanded file manifest in Section 10j. Retained here as a quick reference:

| File | Phase | Change |
|------|-------|--------|
| `crates/mbo-tokenizer/src/lib.rs` | 0 | Add `book_snapshot()` method (see 10b) |
| `tools/mbo-tokenize/src/main.rs` | 0 | Write `.book_state` sidecar (see 10b) |
| `research/04-mbo-grammar/model.py` | 1-4 | 8M model config, ReconHead, DirectionalHead (see 10f) |
| `research/04-mbo-grammar/data.py` | 1-4 | BookStateDataset, snapshot filtering (see 10c, 10d) |
| `research/04-mbo-grammar/train_finetune.py` | 1-4 | New: per-phase entry point with combined loss |
| `research/04-mbo-grammar/evaluate.py` | 3-5 | New: CPCV evaluation, kill criterion checks |
| `research/04-mbo-grammar/baseline_xgboost.py` | 6 | New: static features + XGBoost (see 10i) |

---

## 10. Implementation Details

This section resolves all ambiguities. An implementing agent should not need to make design decisions beyond what is specified here.

### 10a. Sidecar Binary Format

**File extension:** `.book_state`
**Format:** Raw f32, little-endian, contiguous. No header. 12 values per row, 48 bytes per row.
**Row count:** Must equal the number of COMMIT tokens in `tokens.bin`. Validate: `file_size / 48 == commit_count` from `meta.json`.
**Python load:** `np.fromfile(path, dtype='<f32').reshape(-1, 12)`

The existing `.mids` sidecar stores `(u64 token_position, i64 mid_price_fixed)` per COMMIT. The `.book_state` sidecar is aligned row-for-row with `.mids` — row i of `.book_state` corresponds to row i of `.mids`. No position index is stored in `.book_state` because the alignment is implicit.

### 10b. Sidecar Emission (Rust)

Add a public method to `MboTokenizer`:

```rust
/// Returns the post-event book state as 12 f32 values.
/// Call after `feed_event`. Meaningful at COMMIT boundaries.
/// Returns None if the book is one-sided (no valid mid).
pub fn book_snapshot(&self) -> Option<[f32; 12]> {
    let bid = self.book.best_bid_price()?;
    let ask = self.book.best_ask_price()?;
    let mid = (bid + ask) / 2;
    let tick = self.tick_size_fixed;

    let bid_rel = (bid - mid) as f64 / tick as f64;
    let ask_rel = (ask - mid) as f64 / tick as f64;

    let bid_levels = self.book.bid_levels_raw();
    let ask_levels = self.book.ask_levels_raw();

    let mut out = [0.0f32; 12];
    out[0] = bid_rel as f32;
    out[1] = ask_rel as f32;

    // Bid sizes: levels 1-5 (best bid = last element of bid_levels)
    let n_bids = bid_levels.len();
    for i in 0..5 {
        if i < n_bids {
            out[2 + i] = bid_levels[n_bids - 1 - i].1 as f32;
        }
    }

    // Ask sizes: levels 1-5 (best ask = first element of ask_levels)
    let n_asks = ask_levels.len();
    for i in 0..5 {
        if i < n_asks {
            out[7 + i] = ask_levels[i].1 as f32;
        }
    }

    Some(out)
}
```

In `main.rs`, after the existing mids recording block:

```rust
// Record book state at each COMMIT position
if all_tokens.len() > pre_len && *all_tokens.last().unwrap() == COMMIT {
    if let Some(state) = tokenizer.book_snapshot() {
        book_states.push(state);
    }
}
```

Write with a new `write_book_state()` function following the same pattern as `write_mids()`:

```rust
fn write_book_state(path: &str, states: &[[f32; 12]]) -> Result<()> {
    let file = File::create(path).context("Failed to create book_state file")?;
    let mut writer = BufWriter::new(file);
    for state in states {
        for &val in state {
            writer.write_all(&val.to_le_bytes())?;
        }
    }
    writer.flush()?;
    let bytes = states.len() * 48;
    eprintln!("    {} entries, {} bytes ({:.1} MB)",
        states.len(), bytes, bytes as f64 / 1_048_576.0);
    Ok(())
}
```

### 10c. Size Discretization (Python data loader)

The 9 log2 buckets match the tokenizer vocabulary:

```python
SIZE_BUCKET_BOUNDARIES = [0, 1, 2, 4, 8, 16, 32, 64, 128]  # 9 classes

def discretize_size(raw_size: float) -> int:
    """Map raw order size to bucket index 0-8."""
    s = int(raw_size)
    if s == 0: return 0
    if s == 1: return 1
    if s <= 3: return 2
    if s <= 7: return 3
    if s <= 15: return 4
    if s <= 31: return 5
    if s <= 63: return 6
    if s <= 127: return 7
    return 8  # 128+
```

### 10d. Spread Discretization (Python data loader)

```python
def discretize_spread(best_bid_rel: float, best_ask_rel: float) -> int:
    """Map BBO relative prices to spread class 0-10."""
    spread_ticks = best_ask_rel - best_bid_rel  # always >= 0
    spread_int = int(round(spread_ticks))
    if spread_int <= 0: return 0     # crossed/invalid
    if spread_int >= 10: return 10   # wide spread
    return spread_int                # classes 1-9 = 1-9 ticks
```

### 10e. Training Hyperparameters

**Gate 1 retraining (8M model):**

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Architecture | d=256, 8 layers, 8 heads, FF=1024 | Primary model |
| Optimizer | AdamW | Same as original Gate 1 |
| Learning rate | 3e-4 | Same as original Gate 1 |
| LR schedule | Cosine annealing to 0 | |
| Weight decay | 0.01 | |
| Batch size | 128 | Fits in T4 16GB at context 512 |
| Epochs | 10 | |
| Loss | LM (next-token CE, SPECIAL tokens masked) | Same as original Gate 1 |
| Init | Random | Fresh 8M model |
| Pass criterion | ppl < 1.934 (875K baseline) | More capacity should help; if not, something is wrong |

**Gate 1.5 (reconstruction validation, 8M):**

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Optimizer | AdamW | |
| Learning rate | 3e-4 | Not fine-tuning; training new heads on frozen-then-unfrozen backbone |
| LR schedule | Cosine annealing to 0 | |
| Weight decay | 0.01 | |
| Batch size | 128 | |
| Epochs | 10 | |
| Loss | L_lm + 0.1 * L_pre_recon + 0.1 * L_post_recon | No directional head |
| Init | 8M Gate 1 checkpoint | Reconstruction heads randomly initialized |

**Directional fine-tuning (conditions A-D at 8M, A*-B* at 875K):**

| Parameter | Value | Rationale |
|-----------|-------|-----------|
| Optimizer | AdamW | |
| Learning rate | 5e-5 | Lower than pretraining; standard fine-tuning LR |
| LR schedule | Cosine annealing to 0 | |
| Weight decay | 0.01 | |
| Batch size | 128 | |
| Epochs | 10 | |
| Loss weights | See Section 3 combined loss | Sweep a, b, g |
| Init (A, B) | 8M Gate 1 checkpoint (Phase 1) | Recon heads from 8M Gate 1.5 (Phase 2) for B |
| Init (A*, B*) | 875K Gate 1 checkpoint (existing) | Recon heads trained in Phase 4 for B* (875K Gate 1.5 run inline) |
| Init (C, D) | Random (8M architecture) | |

### 10f. Horizon K Encoding

K is encoded as a single scalar: `k_normalized = K / 1000.0`. This is concatenated to the hidden state at COMMIT positions before the directional head's first Linear layer.

```python
# Directional head input: d_model + 1
self.dir_head = nn.Sequential(
    nn.Linear(d_model + 1, d_model // 2),
    nn.GELU(),
    nn.Linear(d_model // 2, 1),
)

# Forward:
h = hidden_states[commit_mask]           # [N, d_model]
k = k_values.unsqueeze(-1) / 1000.0     # [N, 1]
h_k = torch.cat([h, k], dim=-1)         # [N, d_model + 1]
logits = self.dir_head(h_k).squeeze(-1)  # [N]
```

**Batch construction:** Each sample gets one K, sampled uniformly from {50, 200, 1000}. During evaluation, evaluate each K separately (3 passes per fold). Approximately 1/3 of training samples see each K.

### 10g. CPCV Configuration

**Sweep phase (conditions A-D):** 6 temporal groups, 2 test groups per fold = C(6,2) = 15 folds. Purge = 5000 samples, embargo = 1000 samples. This matches Gate 2 for comparability.

**Full CPCV (best config):** 10 temporal groups, 2 test groups per fold = C(10,2) = 45 folds. Same purge/embargo. Only run on the condition/target pair with strongest signal from the sweep.

### 10h. Target Definitions

**sign(mid[i+K] - mid[i]):**
Using the `.mids` sidecar. Label = 1 if `mid[i+K] > mid[i]`, else 0. Exclude samples where `i+K` exceeds the sidecar length. Exclude ties (mid[i+K] == mid[i]) — do not assign them to either class.

**Next BBO change direction:**
Find the smallest j > i where `mid[j] != mid[i]`. Label = 1 if `mid[j] > mid[i]`, else 0. Exclude if no change within 2000 COMMITs (j > i + 2000). These "flat" samples are rare during RTH but common during low-activity periods; excluding them avoids contaminating the target with noise.

**Signed tick change at COMMIT+K:**
Target = `(mid[i+K] - mid[i]) / tick_size_fixed`. This is a regression target (MSE loss). No exclusions.

**Significant move (3-class):**
Within the next 200 COMMITs after COMMIT i, compute `max_up = max(mid[i+1..i+200] - mid[i]) / tick_size` and `max_down = min(mid[i+1..i+200] - mid[i]) / tick_size`. If `max_up >= 2`: class 2 (up). Elif `max_down <= -2`: class 0 (down). Else: class 1 (flat). If both thresholds are hit, use whichever was hit first (scan sequentially). Loss: 3-class CE.

### 10i. XGBoost Baseline (Condition E)

**Features:** Computed at each COMMIT position from the preceding 20 events in the token stream.

| Feature | Computation |
|---------|------------|
| bid_size_1 | From `.book_state` sidecar index 2 |
| ask_size_1 | From `.book_state` sidecar index 7 |
| bid_size_total_5 | Sum of sidecar indices 2-6 |
| ask_size_total_5 | Sum of sidecar indices 7-11 |
| imbalance_1 | bid_size_1 / (bid_size_1 + ask_size_1) |
| imbalance_5 | bid_size_total_5 / (bid_size_total_5 + ask_size_total_5) |
| spread | sidecar[1] - sidecar[0] |
| mid_change_20 | (mid[i] - mid[i-20]) / tick_size (from `.mids`) |
| mid_change_100 | (mid[i] - mid[i-100]) / tick_size |
| trade_count_20 | Count of TRADE tokens (ID 7) in last 20 events |
| cancel_count_20 | Count of CANCEL tokens (ID 5) in last 20 events |
| add_count_20 | Count of ADD tokens (ID 4) in last 20 events |
| trade_side_imbalance_20 | (buy_trades - sell_trades) / total_trades in last 20 events |
| size_imbalance_change | imbalance_1[i] - imbalance_1[i-20] |
| spread_change | spread[i] - spread[i-20] |

Total: 15 features.

**XGBoost config:** `n_estimators=500, max_depth=6, learning_rate=0.05, subsample=0.8, colsample_bytree=0.8, objective='binary:logistic'` (for binary targets) or `'multi:softmax', num_class=3` (for significant move).

**CPCV:** Same as transformer sweep: 6 groups, 2 test, purge 5000, embargo 1000.

### 10j. Files to Modify (expanded)

| File | Change | Notes |
|------|--------|-------|
| `crates/mbo-tokenizer/src/lib.rs` | Add `pub fn book_snapshot(&self) -> Option<[f32; 12]>` method | See 10b for implementation |
| `tools/mbo-tokenize/src/main.rs` | Accumulate `book_states: Vec<[f32; 12]>`, write `.book_state` file | Follow existing `.mids` pattern; see 10b |
| `tools/mbo-tokenize/src/main.rs` | Add `--book-state` CLI flag (default: on) | Parallel to existing `--text` flag |
| `research/04-mbo-grammar/data.py` | Add `BookStateDataset`: load `.book_state`, yield pre/post targets, snapshot filtering, size discretization | See 10c, 10d for discretization; see Section 2 for snapshot rule |
| `research/04-mbo-grammar/model.py` | Add `ReconHead` class (shared arch, independent params for pre/post), `DirectionalHead` class, `MBOTransformerFinetune` wrapper | See Section 3 for architecture; 10f for K encoding |
| `research/04-mbo-grammar/train_finetune.py` | New file: Gate 1.5 + conditions A-D training loop, combined loss, metrics logging | See 10e for hyperparams; keep `train.py` for Gate 1 |
| `research/04-mbo-grammar/evaluate.py` | New file: CPCV evaluation, per-fold accuracy, majority baseline, kill criterion checks | See 10g for CPCV config; 10h for target defs |
| `research/04-mbo-grammar/baseline_xgboost.py` | New file: feature extraction from sidecar + tokens, XGBoost CPCV | See 10i |
| `research/04-mbo-grammar/cloud-run.toml` | Update for new experiment (data sources, command, env) | Point to new train_finetune.py entry point |
