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
**Status:** Active
**Period:** Mar 2026–
**Hypothesis:** A single regression model predicting P(price hits +T before -S | LOB state) from event-level microstructure features, with entry at bid/ask and tick-level barrier simulation, can generate positive serial expectancy.

**Key Innovations:**
- Event-level (committed state) vs bar-level analysis
- 42 LOB features + (T, S) geometry as model inputs = 44 dimensions
- Entry at bid/ask, not mid — spread is implicit in execution
- Multi-geometry training: 10 (T,S) pairs per evaluation point
- Probability regression, not direction classification
- Decision rule: trade when P_model > P_null + margin, where P_null = S/(T+S)

**See:** `research/03-event-lob-probability/README.md`
