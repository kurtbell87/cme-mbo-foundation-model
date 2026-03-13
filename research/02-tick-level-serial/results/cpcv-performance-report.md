# CPCV Backtest Performance Report — Revised

**Strategy:** MBO Directional — Triple-Barrier XGBoost (2-stage)
**Instrument:** ES futures (MBO tick data, 5-second bars)
**Validation Method:** CPCV (45 folds) + Temporal Holdout (train 170, test 81)
**Date:** 2026-03-01
**Run ID:** cpcv-20260301T123259Z-3a0ca2c9

---

## 1. Executive Summary

The previous report showed an annualized Sharpe of 14.21 under the overlapping PnL
model — mechanically correct but not implementable. That model treats every 5-second
bar as an independent trade entry (~3,750/day), but consecutive entries share 719/720
of their forward window.

**This revision adds serial-execution PnL** (one position at a time, enter only after
the previous position closes). Under serial execution:

| Metric | Serial (Base) | Overlapping (Base) |
|--------|--------------|-------------------|
| Mean expectancy | **-$1.65/trade** | +$3.62/trade |
| Win rate | **29.6%** | 51.7% |
| Annualized Sharpe | **-18.26** | +14.21 |
| Trades/day | ~130 | ~3,750 |
| Negative folds | **45/45 (100%)** | 0/45 (0%) |

**The strategy is not viable under serial execution.** All 45 CPCV folds and the
81-day temporal holdout are negative across all cost scenarios including optimistic.
The 29.6% serial win rate is close to the 26.9% expected from a random walk against
asymmetric barriers (19-tick target vs 7-tick stop), indicating the model has
negligible directional edge when positions cannot overlap.

The overlapping model retains value as a **per-bar signal quality metric** — the
model does predict direction slightly better than chance on a per-bar basis. But
the small edge (~2-3 percentage points above random walk) is insufficient to survive
the adverse selection imposed by serial execution with tight stops.

---

## 2. Methodology

### 2.1 Two PnL Models

| | Overlapping (per-bar) | Serial (implementable) |
|---|---|---|
| **Entries** | Every filtered bar with signal (~3,750/day) | One at a time; next entry only after exit (~130/day) |
| **Position overlap** | Consecutive entries share 719/720 of forward window | Zero overlap |
| **Barrier check** | Uses triple-barrier label (pre-computed) | Re-simulates barrier walk from `close_mid` price series |
| **Purpose** | Per-bar signal quality measurement | Realistic execution P&L |
| **Independence** | Trades are ~99.9% correlated | Trades are conditionally independent |

### 2.2 Serial Barrier Re-simulation

For each test day (processed independently):
1. Walk through filtered bars chronologically
2. When model signals and no position is held, enter at `close_mid[bar]`
3. Walk forward through ALL bars (not just filtered) checking:
   - Target: price moves ≥ 19 ticks in trade direction → exit with +19 tick P&L
   - Stop: price moves ≥ 7 ticks against trade direction → exit with -7 tick P&L
   - Horizon: 720 bars reached → exit at realized forward return
4. After exit, advance to next bar and resume scanning

### 2.3 Cross-Validation: CPCV

Same as previous report: 10 groups, k=2 test groups, C(10,2)=45 folds, 500-bar
purge, 4,600-bar embargo. All 251 days.

### 2.4 Temporal Holdout

Train: first 170 days (2022-01-03 to 2022-09-06)
Test: last 81 days (2022-09-07 to 2022-12-30)
Single train/test split. No fold aggregation.

### 2.5 Cost Scenarios

| Scenario | Round-trip cost | Description |
|----------|----------------|-------------|
| Optimistic | $1.24 | Passive fill at mid, minimal fees |
| Base | $2.49 | Standard retail execution |
| Pessimistic | $4.99 | Full spread + elevated fees |

---

## 3. Serial Execution Results (Primary)

### 3.1 CPCV — All 251 Days, 45 Folds

| Metric | Optimistic | Base | Pessimistic |
|--------|-----------|------|-------------|
| Mean expectancy ($/trade) | -0.40 | **-1.65** | -4.15 |
| 95% CI | [-0.46, -0.34] | [-1.71, -1.59] | [-4.21, -4.09] |
| Ann. Sharpe | -4.55 | **-18.26** | -28.11 |
| Win rate | 29.6% | 29.6% | 29.6% |
| Negative folds | 45/45 | 45/45 | 45/45 |
| Avg trades/fold | 6,501 | 6,501 | 6,501 |
| Avg trades/day | ~130 | ~130 | ~130 |

All 45 folds are negative under all cost scenarios. The CI is extremely tight
(±$0.06 at base), confirming this is not a sampling artifact.

### 3.2 Temporal Holdout — Train 170, Test 81

| Metric | Optimistic | Base | Pessimistic |
|--------|-----------|------|-------------|
| Expectancy ($/trade) | -0.36 | **-1.61** | -4.11 |
| Ann. Sharpe | -3.59 | -16.19 | -32.15 |
| Win rate | 29.6% | 29.6% | 29.6% |
| Total trades | 8,844 | 8,844 | 8,844 |
| Trades/day | 109.2 | 109.2 | 109.2 |

The temporal holdout confirms the CPCV finding. Even on a strict out-of-sample
test period, serial execution produces negative expectancy.

### 3.3 Trade Mechanics

| Statistic | Value |
|-----------|-------|
| Average holding period | 35.6 bars (3.0 minutes) |
| Trades/day | ~109–130 |
| Outcome distribution | 70.4% stop, ~29% target, <1% horizon |

The average trade lasts 3 minutes — stops are hit quickly due to the tight
7-tick stop ($1.75 on ES). The holding period is much shorter than the 720-bar
(60-minute) horizon, explaining why trade count (~130/day) far exceeds the
theoretical minimum (~6.4/day if all trades went to horizon).

### 3.4 Why Win Rate ≈ Random Walk

With asymmetric barriers (target=19, stop=7), a symmetric random walk hits the
stop first with probability 19/(19+7) = **73.1%** (gambler's ruin). The observed
stop rate of **70.4%** is only 2.7 percentage points below the random walk
baseline. The model provides a slight directional edge (~2.7pp above random),
but this is far too small to overcome the adverse payoff structure:

```
E[PnL/trade] = 0.296 × (+19 × $1.25) - 0.704 × (7 × $1.25) - cost
             = $7.03 - $6.16 - $2.49
             = -$1.62
```

Even at zero cost, the edge would be +$0.87/trade — marginal and fragile.

---

## 4. Signal Quality Metrics (Overlapping Model)

The overlapping model measures **per-bar predictive accuracy**, not implementable P&L.
It is included for completeness and model development feedback.

### 4.1 CPCV — 45 Folds

| Metric | Optimistic | Base | Pessimistic |
|--------|-----------|------|-------------|
| Mean expectancy ($/trade) | 4.87 | 3.62 | 1.12 |
| 95% CI | [4.68, 5.06] | [3.43, 3.81] | [0.93, 1.31] |
| Ann. Sharpe | 18.96 | 14.21 | 4.48 |
| Deflated Sharpe | 1.000 | 1.000 | 0.972 |
| Win rate | 51.9% | 51.7% | 51.3% |
| Negative folds | 0/45 | 0/45 | 2/45 |
| Avg trades/fold | 187,919 | 187,919 | 187,919 |

### 4.2 Temporal Holdout — Overlapping

| Metric | Optimistic | Base | Pessimistic |
|--------|-----------|------|-------------|
| Expectancy ($/trade) | 4.58 | 3.33 | 0.83 |
| Ann. Sharpe | 19.87 | 14.62 | 3.71 |
| Win rate | 51.6% | 51.3% | 51.0% |

The overlapping model confirms the model predicts direction slightly better than
chance on a per-bar basis, with consistent results across both CPCV and holdout.
The holdout Sharpe (14.62) is comparable to CPCV (14.21), showing no significant
degradation on unseen data.

### 4.3 Reconciling the Two Models

The divergence between overlapping (+$3.62) and serial (-$1.65) stems from
a fundamental measurement difference:

1. **Overlapping model uses pre-computed labels.** For bars where label ≠ 0
   (directional), PnL is ±target/stop based on whether the prediction matches
   the label. For label=0 bars, PnL = fwd_return × direction.

2. **Serial model simulates actual barriers.** Every trade faces the full
   asymmetric barrier (19-tick target vs 7-tick stop), regardless of label.
   With tight stops, most entries are stopped out before reaching target.

3. **Stage 1 barely filters.** The directional filter passes ~97% of bars.
   Many of these are label=0 (hold) bars where the overlapping model charges
   only the small fwd_return cost, but the serial model charges the full
   barrier outcome (usually a stop loss).

4. **Position overlap masked the reality.** The overlapping model's ~3,750
   trades/day share >99% of their forward window. The per-bar signal quality
   is real (51.7% > 50%), but the tiny edge vanishes when forced through
   the asymmetric barrier funnel under serial execution.

---

## 5. Autocorrelation Analysis

### Ljung-Box Test on Serial Daily PnL

| Dataset | Q statistic | p-value | Max lag | Interpretation |
|---------|------------|---------|---------|----------------|
| CPCV (pooled, 45 folds) | 152.66 | 0.0000 | 10 | Significant autocorrelation |
| Temporal holdout (81 days) | 7.49 | 0.679 | 10 | No significant autocorrelation |

The CPCV autocorrelation is expected — each day appears in 9 test folds, creating
mechanical correlation in the pooled daily PnL. The temporal holdout (single split)
shows **no significant autocorrelation** (p=0.68), which is the clean test. This
means the serial daily losses are independently distributed — the strategy doesn't
have trending loss patterns, it simply loses consistently.

---

## 6. Capacity Estimate

Under serial execution, the strategy trades ~110–130 times/day with an average
holding period of 3 minutes. At 1 contract per trade:

- ES RTH volume: ~1.5M contracts/day
- Strategy volume: ~130 contracts/day (0.009% of market)
- Market impact: negligible at 1 contract

Capacity is moot given negative expectancy, but is documented for completeness.
If the model were improved to achieve positive serial expectancy, the short
holding period and modest trade count would allow scaling to 50+ contracts
before impacting the ES order book.

---

## 7. Risk Assessment — Revised

### 7.1 Key Finding

The serial-execution model conclusively demonstrates that the strategy as currently
configured is **not implementable**. The per-bar directional edge (~2.7pp above
random walk) is real but insufficient to overcome the asymmetric barrier structure.

### 7.2 Root Cause Analysis

| Factor | Impact |
|--------|--------|
| **Barrier asymmetry** | 19-tick target vs 7-tick stop creates a 73% base stop rate. The model reduces this to 70.4% — a real but small improvement. |
| **Tight stops** | 7 ticks ($1.75 on ES) is within normal 5-second noise. Most entries are stopped out within 3 minutes before any directional signal can develop. |
| **Stage 1 under-filtering** | The directional filter passes ~97% of bars, failing to screen out low-conviction entries that dominate serial execution outcomes. |
| **Overlap illusion** | The overlapping model's high Sharpe (14.21) was mechanically amplified by ~3,750 correlated entries/day sharing the same forward window. |

### 7.3 Paths Forward

| Approach | Description | Feasibility |
|----------|-------------|------------|
| **Widen stops** | Increase stop to 19+ ticks (symmetric barriers). Reduces random-walk stop probability but may reduce win rate proportionally. | Medium |
| **Strengthen Stage 1** | Only trade bars with p(directional) > 0.80+ threshold. Current ~50% threshold passes nearly everything. | High |
| **Minimum holding period** | Require signal persistence across N consecutive bars before entering. Reduces noise entries. | Medium |
| **Alternative barrier design** | Replace triple-barrier with time-based exit (e.g., always exit after 720 bars). Removes asymmetric barrier issue. | High |
| **Feature engineering** | Add intraday momentum/mean-reversion features that predict 3-minute price paths, not 60-minute outcomes. | Medium |

---

## 8. Summary

| Model | Expectancy (Base) | Sharpe | Win Rate | Viable? |
|-------|------------------|--------|----------|---------|
| Serial (CPCV, 45 folds) | -$1.65/trade | -18.26 | 29.6% | **No** |
| Serial (Holdout, 81 days) | -$1.61/trade | -16.19 | 29.6% | **No** |
| Overlapping (CPCV) | +$3.62/trade | +14.21 | 51.7% | N/A (not implementable) |
| Overlapping (Holdout) | +$3.33/trade | +14.62 | 51.3% | N/A (not implementable) |

The overlapping model measures per-bar signal quality. The serial model measures
what you would actually earn. Both are shown for transparency.

**Recommendation:** Do not deploy this strategy in its current form. The model
has a real but small directional edge that is overwhelmed by the asymmetric
barrier structure. Pursue the paths forward in Section 7.3 — specifically,
strengthening Stage 1 filtering and redesigning the barrier/exit mechanism.

---

## Appendix A: Configuration

| Parameter | Value |
|-----------|-------|
| Groups (N) | 10 |
| Test groups per fold (k) | 2 |
| Total folds | C(10,2) = 45 |
| Purge buffer | 500 bars (41.7 min) |
| Embargo buffer | 4,600 bars (6.4 hrs) |
| Dev days | 251 |
| Holdout split | 170 train / 81 test |
| Forward horizon | 720 bars (60 min) |
| Target | 19 ticks ($4.75 ES) |
| Stop | 7 ticks ($1.75 ES) |
| Tick size | $0.25 |
| Tick value | $1.25 ($12.50 per point) |

## Appendix B: Run Details

| | Value |
|---|---|
| EC2 instance | c7a.32xlarge (128 vCPU, 256 GB) |
| CPCV runtime | 7.1 minutes (45 folds, 8 parallel) |
| Holdout runtime | ~2 minutes (single fold) |
| Run ID | cpcv-20260301T123259Z-3a0ca2c9 |
| JSON outputs | `cpcv-backtest-results.json`, `temporal-holdout-results.json` |

## Appendix C: References

- de Prado, M. L. (2018). *Advances in Financial Machine Learning.* Wiley.
- Bailey, D. H. & Lopez de Prado, M. (2014). "The deflated Sharpe ratio."
  *Journal of Portfolio Management.*
- Sharpe, W. F. (1994). "The Sharpe Ratio." *Journal of Portfolio Management.*
