# Research Index — MBO Deep Learning

## Thread 01: Bar-Level CPCV (Python)
**Status:** Completed — negative result at execution resolution
**Period:** Jan–Feb 2026
**Hypothesis:** 2-stage XGBoost (direction filter + prediction) on 5s bar features with triple-barrier labels can generate positive expectancy at execution resolution.

**Key Results:**
- Bar-level Sharpe 2.27, $2.50/trade — internally consistent on bar-level close_mid
- Labels used close_mid only; high_mid/low_mid existed but were never used for barrier checking
- Entry/exit at theoretical mid — full L2 book data available but unused

**Conclusion:** Metric appeared viable but did not account for intra-bar price dynamics or execution costs from bid/ask spread. Results not executable.

**See:** `research/01-bar-level-cpcv/README.md`

---

## Thread 02: Tick-Level Serial Re-simulation (Rust)
**Status:** Completed — confirmed null hypothesis
**Period:** Feb–Mar 2026
**Hypothesis:** Re-simulating the bar-level model's signals at tick resolution with serial execution constraint would reveal true execution-resolution performance.

**Key Results:**
- Serial backtest Sharpe: -15.2, 26.6% win rate
- Matches LOB-Math null hypothesis P(reward) = S/(T+S) = 7/26 = 26.9%
- 12–31% label flip rate between bar-level and tick-level barrier simulation
- Barrier sweep (19:19 symmetric) recovered 50% win rate but still negative expectancy
- Time-exit (no barriers) showed coin-flip directionality

**Conclusion:** The 2-stage classifier has zero predictive edge at execution resolution. Root cause: label and entry/exit resolution mismatch, not model architecture.

**See:** `research/02-tick-level-serial/README.md`

---

## Thread 03: Event-Level LOB Probability Model (Rust)
**Status:** Active — distributed CPCV implemented, awaiting relaunch
**Period:** Mar 2026–
**Hypothesis:** A single regression model predicting P(price hits +T before -S | LOB state) from event-level microstructure features, with entry at bid/ask and tick-level barrier simulation, can generate positive serial expectancy.

**Key Innovations:**
- Event-level (committed state) vs bar-level analysis
- 42 LOB features + (T, S) geometry as model inputs = 44 dimensions
- Entry at bid/ask, not mid — spread is implicit in execution
- Multi-geometry training: 10 (T,S) pairs per evaluation point
- Probability regression, not direction classification
- Decision rule: trade when P_model > P_null + margin, where P_null = S/(T+S)

**Bilateral Export (completed 2026-03-03):**
- Run ID: `bilateral-export-20260303T173028Z-db687d7f`
- 292 days, 897M total rows, 23 GB Parquet on S3
- ~94M rows per geometry (10:5)

**Distributed CPCV Tooling:**
- `--fold-range START:END` for machine-level fold sharding (45 folds across N instances)
- `--mode aggregate` merges partial results into cross-fold report
- Distributed launch script: `scripts/ec2-launch-imbalance-cpcv-distributed.sh`

**OFI Threshold Finding:**
- `|ofi_fast| > 2.0` filters <3% of rows on 10:5 geometry (median = 36, p5 = 4.1)
- OFI filtering is ineffective as a memory reduction strategy; use holdout or subsampling instead

**Next Steps:**
1. Implement `--holdout-pct` for 80/20 chronological day-level split
2. Spot vCPU quota increase (128 → 512)
3. Relaunch distributed imbalance CPCV on 8× c7a.16xlarge (~2-3 hours, ~$22)
4. Run validation pipeline (DSR, expectancy CI, Ljung-Box, profit factor)

**See:** `research/03-event-lob-probability/README.md`
