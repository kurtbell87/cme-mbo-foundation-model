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
- [ ] Local 5-day export validation
- [ ] EC2 batch export (all 251 days)
- [ ] Full CPCV training on EC2
- [ ] Analysis and documentation

## Scripts
- `scripts/batch-export.sh` — EC2 batch export orchestration
