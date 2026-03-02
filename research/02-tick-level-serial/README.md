# Thread 02: Tick-Level Serial Re-simulation

## Hypothesis
Re-simulating the bar-level model's signals at tick resolution with serial execution constraint would reveal true performance at execution resolution.

## Method
- **Data:** Same 251-day MES MBO dataset
- **Signals:** Bar-level 2-stage XGBoost predictions from Thread 01
- **Re-simulation:** Walk tick_mid_prices forward from each signal, check barriers at every tick
- **Barrier sweep:** Tested geometries: 19:7 (original), 19:19 (symmetric), time-exit (no barriers)
- **Execution:** Serial only (one position at a time, entry at signal bar)

## Key Results

### 19:7 Geometry (Original)
- Sharpe: -15.2
- Win rate: 26.6% (null: S/(T+S) = 7/26 = 26.9%)
- Expectancy: -$1.65/trade
- All 45 CPCV folds negative, all 81 holdout days negative

### 19:19 Geometry (Symmetric)
- Win rate: ~50% (recovered symmetry)
- Expectancy: still negative (transaction costs)
- Target rate matches null P = 19/38 = 50%

### Time-Exit (No Barriers)
- Directional accuracy: ~50% (coin flip)
- Confirms model has no directional edge

### Label Diagnostic
- 12–31% of labels flip between bar-level and tick-level barrier simulation
- Intra-bar price excursions cross barriers that close_mid misses

## Conclusion
The 2-stage classifier predicts direction no better than random at execution resolution. The LOB-Math null hypothesis is confirmed: P(target) = S/(T+S) for all geometries tested. Root causes:
1. Labels used close_mid, ignoring intra-bar barrier breaches
2. Entry at mid, not bid/ask
3. Classification (direction) instead of regression (probability)
4. Bar-level aggregation discards microstructure

## Result Files
- `results/` — EC2 CPCV runs, barrier sweep diagnostics
