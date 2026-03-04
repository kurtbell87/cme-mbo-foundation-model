# Thread 03: Event-Level LOB Probability Model

## Status: Active

## Hypothesis
A single regression model predicting P(price hits +T before -S | LOB state) from event-level microstructure features, with entry at bid/ask and tick-level barrier simulation, can generate positive serial expectancy on MES futures.

## Key Design Differences from Previous Threads

| Aspect | Thread 01–02 | Thread 03 |
|--------|-------------|-----------|
| Unit of analysis | 5s bars (~4630/day) | Committed states (~500K/day, ~75K BBO changes) |
| Features | 20 bar-level | 42 LOB + 2 geometry = 44 |
| Labels | close_mid barriers | tick-level barriers from bid/ask entry |
| Entry price | Theoretical mid | Best ask (longs) / best bid (shorts) |
| Model type | 2-stage classification | Single regression P(target) |
| Decision rule | Predict direction | P_model > S/(T+S) + margin |
| Geometries | Single (19:7) | 10 geometries per eval point |
| Resolution | Bar-level (~5s) | Event-level (~nanosecond) |

## Method

### Features (44 dimensions)
**Instantaneous book state (32 features):**
- Book depth profile: 10 bid sizes + 10 ask sizes
- Book imbalance at depths 1, 3, 5, 10
- Weighted imbalance
- Spread in ticks
- Depth concentration (HHI) per side
- Book slope per side
- Active level count per side

**Rolling event window (10 features):**
- Cancel/add rates at inside bid/ask (4)
- Trade aggression: buy vs sell volume
- Message rate
- Cancel-add ratio
- BBO change count
- Price momentum in ticks
- Order flow toxicity

**Model inputs:** target_ticks (T), stop_ticks (S)

### Geometries
(5,5), (10,5), (10,10), (15,5), (15,10), (15,15), (19,7), (19,19), (25,10), (25,25)

### Model
- XGBoost regression, `binary:logistic` objective
- Output: calibrated P(target hit before stop)
- Decision rule: trade when P > S/(T+S) + margin

### Validation
- CPCV (10 groups, k=2, 45 folds) with event-level purge/embargo
- Null hypothesis test: shuffled features → P ≈ S/(T+S)
- Calibration curve
- Feature importance stability (Kendall's tau > 0.5 across folds)
- Temporal holdout (170 train / 81 test)
- Label consistency: 0% flip rate (same simulate_barrier for labels and backtest)

## Infrastructure
- `crates/event-features/` — LOB feature computation
- `crates/event-labels/` — tick-level barrier simulation
- `tools/event-export/` — Parquet export binary
- `tools/event-backtest/` — CPCV + serial PnL backtest

## Implementation Status
- [x] CommittedState exposed from BookBuilder with bbo_changed flag
- [x] event-features crate: 42 LOB features + geometry inputs
- [x] event-labels crate: simulate_barrier with multi-geometry support
- [x] event-export tool: Parquet export pipeline
- [x] event-backtest tool: baseline analysis + CPCV scaffolding
- [x] EC2 batch export — bilateral (292 days, 897M rows, 23 GB)
- [x] Distributed fold sharding (`--fold-range`, `--mode aggregate`)
- [x] Distributed launch script (N× c7a instances)
- [ ] `--holdout-pct` for 80/20 chronological day-level holdout
- [ ] Spot vCPU quota increase (128 → 512)
- [ ] Distributed imbalance CPCV on 8× c7a.16xlarge
- [ ] Validation pipeline (DSR gate, CI, negative fold fraction, Ljung-Box, profit factor)
- [ ] Analysis and documentation

## Distributed Execution

### Architecture
The 45-fold CPCV is too large for a single machine when loading all rows for a geometry (~94M rows for 10:5). Fold sharding distributes folds across N machines:

1. **Shard assignment:** `--fold-range START:END` (exclusive end) assigns a contiguous subset of folds to each machine
2. **Per-machine execution:** Each instance loads data, trains/tests its assigned folds, writes `fold-results-partial.json`
3. **Upload:** Each machine uploads its partial results to S3 (`--s3-output`)
4. **Aggregation:** A local `--mode aggregate` pass reads all partials, sorts by `split_idx`, and computes cross-fold statistics into `imbalance-cpcv-report.json`

### Instance Sizing
| Instance | vCPU | RAM | Status |
|----------|------|-----|--------|
| c7a.8xlarge | 32 | 64 GB | OOMed on 94M rows (10:5 geometry) |
| c7a.16xlarge | 64 | 128 GB | Target for relaunch (fits 94M rows) |

### First Run Outcome (2026-03-04)
- 4× c7a.8xlarge, `--geometry "10:5"`, `--ofi-threshold 2.0`
- OOMed: 94M rows per geometry peaks well above 64 GB
- Root cause: OFI threshold 2.0 is a no-op (see below), so no row reduction occurred

## OFI Distribution Finding

The `--ofi-threshold 2.0` filter was designed to select high-imbalance events. On the 10:5 geometry (bilateral export):

| Statistic | |ofi_fast| value |
|-----------|-----------------|
| Median | 36 |
| p5 | 4.1 |
| p95 | (high) |
| Rows filtered at threshold 2.0 | <3% |
| Threshold for 50% filter | ~91 |

**Conclusion:** OFI filtering is ineffective as a memory reduction strategy for this dataset. Use `--holdout-pct` (day-level 80/20 split) or `--subsample-pct` instead.

## Holdout Design

Planned `--holdout-pct` flag (not yet implemented):
- **Split level:** Day (chronological)
- **Ratio:** 80% train-CPCV / 20% holdout-test
- **Purpose:** Reduce per-fold memory by ~20% AND provide an out-of-sample holdout for final validation
- **Day assignment:** First 234 days → CPCV pool, last 58 days → holdout
- **CPCV runs only on the 234-day pool** (10 groups, k=2, 45 folds)
- **Holdout evaluation:** Best model from CPCV evaluated once on 58 holdout days

## Validation Pipeline

After CPCV completes, the aggregated report must pass these gates:

| Gate | Criterion | Rationale |
|------|-----------|-----------|
| Deflated Sharpe Ratio (DSR) | > 0 (p < 0.05) | Overfitting-adjusted significance |
| Expectancy 95% CI | Lower bound > 0 | Non-zero edge across folds |
| Negative fold fraction | < 30% of 45 folds | Edge is persistent, not concentrated |
| Ljung-Box (lag 10) | p > 0.05 | No serial dependence in trade PnLs |
| Profit factor | > 1.0 (pooled) | More gross profit than gross loss |
| Calibration | Mean absolute error < 0.05 | Model probabilities are well-calibrated |

## Scripts
- `scripts/batch-export.sh` — EC2 batch export orchestration
- `scripts/ec2-launch-cpcv.sh` — Single-instance BBO CPCV (r7a.4xlarge)
- `scripts/ec2-launch-imbalance-cpcv.sh` — Single-instance imbalance CPCV (c7a.32xlarge)
- `scripts/ec2-launch-imbalance-cpcv-distributed.sh` — Distributed imbalance CPCV (N× c7a instances)
