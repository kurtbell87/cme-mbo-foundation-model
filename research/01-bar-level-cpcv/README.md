# Thread 01: Bar-Level CPCV

## Hypothesis
2-stage XGBoost (direction filter + prediction) on 5-second bar features with triple-barrier labels can generate positive expectancy on MES futures.

## Method
- **Data:** 251 trading days of MES MBO data (2022), ~4630 bars/day
- **Features:** 20 bar-level features (returns, volatility, imbalance, message rates, etc.)
- **Labels:** Triple-barrier (T=19 ticks, S=7 ticks) checked against bar close_mid only
- **Model:** 2-stage XGBoost — Stage 1 filters direction, Stage 2 predicts
- **Validation:** 45-fold CPCV (10 groups, k=2) + 81-day temporal holdout
- **Execution model:** Per-bar (overlapping) and serial (non-overlapping)

## Key Results
- **Per-bar (overlapping):** Sharpe 14.21, $3.62/trade, 51.7% directional accuracy
- **Serial execution:** Sharpe -18.26, -$1.65/trade, 29.6% win rate (negative)
- **Label check:** Barriers only checked against close_mid; high_mid/low_mid data existed but unused
- **Entry/exit:** Theoretical mid price; full L2 book (bids[10], asks[10]) available but unused

## Conclusion
The bar-level model showed internally consistent positive metrics, but these were artifacts of resolution mismatch:
1. Labels ignored intra-bar price dynamics (only checked close_mid)
2. Entry/exit at theoretical mid ignored bid/ask spread
3. Per-bar evaluation allowed impossible overlapping positions

When forced to serial execution, the model has zero edge — matching the null hypothesis from LOB-Math.

## Result Files
- `results/` — Label geometry analysis Parquets from EC2
