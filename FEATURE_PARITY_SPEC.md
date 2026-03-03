# Feature Parity Specification

**Audit Date:** 2026-02-27
**Auditor:** External (Claude Opus 4.6)
**Scope:** Complete feature parity spec for replicating the MBO-DL research pipeline in a live Rithmic-based execution system.
**Source Repo Snapshot:** MBO-DL-02152026

---

## Overview

| Property | Value |
|----------|-------|
| **Model architecture** | Two-stage XGBoost (Stage 1: directional filter, Stage 2: long/short) |
| **Features per stage** | 20 non-spatial features (identical for both stages) |
| **Bar definition** | 5-second time bars from 100ms book snapshots |
| **Session boundaries** | RTH only: 09:30:00 ET – 16:00:00 ET |
| **Warmup period** | First 50 bars of each session discarded |
| **Tick size** | 0.25 (MES /ES micro) |
| **Label geometry** | TP=19 ticks, SL=7 ticks, volume_horizon=50000, max_time_horizon=3600s |
| **Normalization** | Z-score (mean/std from training data), NaN→0.0 |
| **Parquet schema** | 152 columns (6 metadata + 62 Track A + 40 book + 33 msg + 4 fwd returns + 1 event count + 3 TB labels + 3 bidirectional) |

---

## 1. Bar Construction

### 1.1 Snapshot Generation

**Code reference:** `tools/bar_feature_export.cpp:40-246` (StreamingBookBuilder), `src/book_builder.hpp` (BookBuilder)

Raw MBO events from Databento `.dbn.zst` files are processed into **100ms book snapshots**:

- **Snapshot interval:** 100,000,000 nanoseconds (100ms) — constant `SNAPSHOT_INTERVAL` at `bar_feature_export.cpp:100`
- **Alignment:** Snapshots are aligned to absolute clock time, starting at RTH open (09:30:00.000 ET). The first snapshot timestamp = `rth_open`, subsequent = `rth_open + N * 100ms`.
- **Snapshot content:** 10-level bid/ask book (price, size), 50-trade rolling buffer (price, size, aggressor_side), mid_price, spread, time_of_day, trade_count.

**Book reconstruction logic (StreamingBookBuilder):**

| MBO Action | Code | Effect |
|------------|------|--------|
| `'A'` (Add) | `apply_add()` | Insert order into `orders_` map, add size to price level |
| `'C'` (Cancel) | `apply_cancel()` | Remove order from `orders_`, subtract size from level |
| `'M'` (Modify) | `apply_modify()` | Remove old order from level, insert at new price/size |
| `'T'` (Trade) | `apply_trade()` | Append to 50-trade rolling buffer with aggressor side (+1.0 buy, -1.0 sell); increment `pending_trade_count_` |
| `'F'` (Fill) | `apply_fill()` | Reduce order size or remove if `remaining_size == 0` |
| `'R'` (Clear) | `apply_clear()` | Clear all orders and levels |

**Snapshot emission:** On every `F_LAST` flag (bit 0x80 in Databento `flags` field), the builder advances time to emit any pending 100ms snapshots up to `ts_event`. Each snapshot captures the book state as of the most recent committed event.

**Mid price:** `(best_bid + best_ask) / 2.0`. If one side is empty but both sides have been seen previously, carries forward the last valid mid/spread.

**Price encoding:** Databento MBO prices are fixed-point int64 with 9 decimal places. Conversion: `float(price) / 1e9`.

**RTH filtering:** Only events with `ts_event >= rth_open && ts_event < rth_close` are recorded as MBO events. Book reconstruction processes all events (including pre-market) to build accurate state, but snapshots are only emitted within the RTH window.

### 1.2 Time Bar Construction

**Code reference:** `src/bars/time_bar_builder.hpp`, `src/bars/bar_builder_base.hpp`

| Property | Value | Code Reference |
|----------|-------|----------------|
| **Bar duration** | 5 seconds | CLI `--bar-param 5` |
| **Snapshots per bar** | 50 (5s / 100ms) | `time_bar_builder.hpp:9` |
| **Alignment** | Clock-aligned. Bars are contiguous: bar N+1 opens at bar N's close timestamp | `time_bar_builder.hpp:23` |
| **Trigger** | Emits when `snapshot_count_ >= snaps_per_bar_` (50 snapshots) | `time_bar_builder.hpp:20` |

**Per-bar aggregation rules:**

| Bar Field | Aggregation | Code Reference |
|-----------|-------------|----------------|
| `open_ts` | Timestamp of first snapshot | `bar_builder_base.hpp:67-68` |
| `close_ts` | Timestamp of last snapshot | `bar_builder_base.hpp:81` |
| `open_mid` | Mid price at first snapshot | `bar_builder_base.hpp:69` |
| `close_mid` | Mid price at last snapshot | `bar_builder_base.hpp:82` |
| `high_mid` | Max mid price across all snapshots | `bar_builder_base.hpp:83` |
| `low_mid` | Min mid price across all snapshots | `bar_builder_base.hpp:84` |
| `volume` | Sum of trade sizes across all snapshots (from most recent trade slot) | `bar_builder_base.hpp:91` |
| `tick_count` | Count of snapshots with a trade | `bar_builder_base.hpp:93` |
| `buy_volume` | Sum of trade sizes where aggressor > 0 | `bar_builder_base.hpp:95-96` |
| `sell_volume` | Sum of trade sizes where aggressor < 0 | `bar_builder_base.hpp:97-98` |
| `vwap` | Σ(trade_price × trade_size) / Σ(trade_size) | `bar_builder_base.hpp:101-102, 127-129` |
| `bids[10][2]` | Book state at **last snapshot** (price, size) | `bar_builder_base.hpp:135-141` |
| `asks[10][2]` | Book state at **last snapshot** (price, size) | `bar_builder_base.hpp:135-141` |
| `spread` | Spread at last snapshot | `bar_builder_base.hpp:141` |
| `bar_duration_s` | `(close_ts - open_ts) / 1e9` | `bar_builder_base.hpp:131-132` |
| `time_of_day` | `compute_time_of_day(close_ts)` — fractional hours since midnight ET | `bar_builder_base.hpp:133` |

**CRITICAL — Trade extraction from snapshots:** The `extract_trade()` function (`bar_builder_base.hpp:59-64`) reads only the **last slot** of the 50-trade rolling buffer (`trades[49]`). This means each snapshot contributes **at most one trade** to bar aggregation. If multiple trades occur within a single 100ms snapshot, only the last one's price/size/side is captured by the bar builder. The `trade_count` field on the snapshot (set by StreamingBookBuilder from `pending_trade_count_`) captures the **true** count of `action='T'` events since the previous snapshot.

**CRITICAL — MBO event assignment to bars:** After bar construction, the export tool reassigns MBO event indices and recounts message types (`bar_feature_export.cpp:808-837`). Events are assigned to bars by timestamp: `mbo_events[cursor].ts_event <= bar.close_ts`. Message counts (`add_count`, `cancel_count`, `modify_count`, `trade_event_count`) are recomputed from the actual MBO events, overriding the bar builder's internal counts.

### 1.3 Session Boundaries

**Code reference:** `bar_feature_export.cpp:763-765`, `src/time_utils.hpp`

| Property | Value |
|----------|-------|
| **RTH open** | 09:30:00.000 ET (midnight_ns + 9.5 hours) |
| **RTH close** | 16:00:00.000 ET (midnight_ns + 16 hours) |
| **RTH duration** | 6.5 hours = 23,400 seconds |
| **Bars per session** | ~4,680 (23,400s / 5s) |
| **Pre/post-market** | Excluded entirely. No snapshots or bars outside RTH. |
| **Overnight** | No carryover between sessions. `BarFeatureComputer::reset()` is called per-day in batch mode. |

**Midnight reference:** `time_utils::REF_MIDNIGHT_ET_NS` = 2022-01-03 00:00:00 ET in UTC nanoseconds = `1641186000 * 1e9`. All dates are computed as offsets from this reference.

### 1.4 Incomplete/Partial Bars

- **Last bar of session:** If the session ends with fewer than 50 snapshots accumulated, `flush()` emits the partial bar. This partial bar IS included in the feature export.
- **First bar of session:** Always a full 50-snapshot bar (the builder doesn't emit until threshold is met).

---

## 2. Feature Catalog

The model uses **20 non-spatial features** selected from the 62 Track A features computed by `BarFeatureComputer`. These 20 features are defined in `NON_SPATIAL_FEATURES` (identically across all experiment scripts).

**Feature ordering (as passed to XGBoost):**

```python
NON_SPATIAL_FEATURES = [
    "weighted_imbalance",    # [0]
    "spread",                # [1]
    "net_volume",            # [2]
    "volume_imbalance",      # [3]
    "trade_count",           # [4]
    "avg_trade_size",        # [5]
    "vwap_distance",         # [6]
    "return_1",              # [7]
    "return_5",              # [8]
    "return_20",             # [9]
    "volatility_20",         # [10]
    "volatility_50",         # [11]
    "high_low_range_50",     # [12]
    "close_position",        # [13]
    "cancel_add_ratio",      # [14]
    "message_rate",          # [15]
    "modify_fraction",       # [16]
    "time_sin",              # [17]
    "time_cos",              # [18]
    "minutes_since_open",    # [19]
]
```

**Code references for feature selection:**
- `.kit/results/cpcv-corrected-costs/run_experiment.py:55-62`
- `.kit/results/e2e-cnn-classification/run_experiment.py:70-77`
- `scripts/hybrid_model/run_corrected_experiment.py:51-58`

All experiments assert `features.shape[1] == 20`.

---

### 2.1 Category 1: Book Shape (4 features used of 32 available)

#### weighted_imbalance
- **Stages:** 1, 2
- **Definition:** Inverse-distance-weighted book imbalance across 10 levels
- **Formula:** `(Σ(bid_size[i] / (i+1)) - Σ(ask_size[i] / (i+1))) / (Σ(bid_size[i] / (i+1)) + Σ(ask_size[i] / (i+1)) + ε)` for i ∈ [0,9]
- **Raw inputs:** `bar.bids[i][1]`, `bar.asks[i][1]` — order book sizes at each of 10 levels, from **bar-close snapshot**
- **Window:** Single bar (instantaneous book state at bar close)
- **Range:** [-1, +1]
- **NaN handling:** Never NaN (always computable from book state)
- **Code reference:** `src/features/bar_features.hpp:384-393` (`weighted_imbalance()`), called at line 351
- **Live replication notes:** Requires 10-level book depth. Rithmic BBO gives only level 0; full depth (if available) needed. If only BBO available, this feature degrades to `book_imbalance_1`.

#### spread
- **Stages:** 1, 2
- **Definition:** Bid-ask spread in ticks at bar close
- **Formula:** `bar.spread / tick_size` where `tick_size = 0.25`
- **Raw inputs:** `bar.spread` = `best_ask_price - best_bid_price` at bar-close snapshot
- **Window:** Single bar (instantaneous)
- **NaN handling:** Never NaN
- **Code reference:** `src/features/bar_features.hpp:354`
- **Live replication notes:** Available from Rithmic BBO. Straightforward.

### 2.2 Category 2: Order Flow (5 features used of 7 available)

#### net_volume
- **Stages:** 1, 2
- **Definition:** Buy volume minus sell volume within the bar
- **Formula:** `bar.buy_volume - bar.sell_volume`
- **Raw inputs:** `buy_volume` = Σ(trade_size where aggressor_side > 0), `sell_volume` = Σ(trade_size where aggressor_side < 0), aggregated across bar's snapshots
- **Window:** Single bar
- **NaN handling:** Never NaN (0 if no trades)
- **Code reference:** `src/features/bar_features.hpp:441`
- **Live replication notes:** Requires trade aggressor classification. Rithmic provides aggressor side on trades.

#### volume_imbalance
- **Stages:** 1, 2
- **Definition:** Net volume as a fraction of total volume
- **Formula:** `net_volume / (total_volume + ε)` where `ε = 1e-8`
- **Raw inputs:** Same as `net_volume` plus `bar.volume`
- **Window:** Single bar
- **Range:** [-1, +1]
- **NaN handling:** 0 if `total_volume < ε`
- **Code reference:** `src/features/bar_features.hpp:443-444`
- **Live replication notes:** Straightforward given net_volume.

#### trade_count
- **Stages:** 1, 2
- **Definition:** Number of trade events (MBO action='T') within the bar
- **Formula:** `bar.trade_event_count` (recomputed from MBO events at `bar_feature_export.cpp:822-829`)
- **Raw inputs:** Count of `action='T'` MBO events with `ts_event <= bar.close_ts` and `ts_event > prev_bar.close_ts`
- **Window:** Single bar
- **NaN handling:** Never NaN (0 if no trades)
- **Code reference:** `bar_feature_export.cpp:822-829` (recount), `src/features/bar_features.hpp:446`
- **Live replication notes:** Count trade messages from Rithmic within the bar window.

#### avg_trade_size
- **Stages:** 1, 2
- **Definition:** Average trade size per trade event
- **Formula:** `bar.volume / bar.trade_event_count` (0 if no trades)
- **Raw inputs:** `bar.volume`, `bar.trade_event_count`
- **Window:** Single bar
- **NaN handling:** 0 if no trades
- **Code reference:** `src/features/bar_features.hpp:448-449`
- **Live replication notes:** Straightforward.

#### vwap_distance
- **Stages:** 1, 2
- **Definition:** Distance from close mid to VWAP, in ticks
- **Formula:** `(bar.close_mid - bar.vwap) / tick_size`
- **Raw inputs:** `bar.close_mid`, `bar.vwap` = Σ(trade_price × trade_size) / Σ(trade_size)
- **Window:** Single bar
- **NaN handling:** 0 if `vwap_den_ == 0` (VWAP defaults to 0, so distance = `close_mid / 0.25`)
- **Code reference:** `src/features/bar_features.hpp:458`
- **Live replication notes:** Track cumulative price×volume and volume within the bar. **Edge case:** If VWAP denominator is zero (no trades), `bar.vwap = 0` which produces a large nonsensical `vwap_distance`. This is an implicit assumption in the research code.

### 2.3 Category 3: Price Dynamics (7 features used of 9 available)

#### return_1
- **Stages:** 1, 2
- **Definition:** 1-bar return in ticks (price change from previous bar)
- **Formula:** `(close_mid[t] - close_mid[t-1]) / tick_size`
- **Raw inputs:** `close_mid` of current and previous bar
- **Window:** Lookback 1 bar
- **NaN handling:** NaN for bar index 0. In batch mode, fixup fills from available data. After z-scoring, NaN→0.0.
- **Code reference:** `src/features/bar_features.hpp:506-510`
- **Live replication notes:** Trivial — store previous bar's close_mid.

#### return_5
- **Stages:** 1, 2
- **Definition:** 5-bar return in ticks
- **Formula:** `(close_mid[t] - close_mid[t-5]) / tick_size`
- **Raw inputs:** `close_mid` of current and 5-bars-ago
- **Window:** Lookback 5 bars
- **NaN handling:** NaN for bars 0-4. Fixup fills from available data. After z-scoring, NaN→0.0.
- **Code reference:** `src/features/bar_features.hpp:513-517`
- **Live replication notes:** Maintain a 5-bar circular buffer of close_mid values.

#### return_20
- **Stages:** 1, 2
- **Definition:** 20-bar return in ticks
- **Formula:** `(close_mid[t] - close_mid[t-20]) / tick_size`
- **Raw inputs:** `close_mid` of current and 20-bars-ago
- **Window:** Lookback 20 bars
- **NaN handling:** NaN for bars 0-19. Fixup fills from available data. After z-scoring, NaN→0.0.
- **Code reference:** `src/features/bar_features.hpp:520-524`
- **Live replication notes:** Maintain a 20-bar circular buffer.

#### volatility_20
- **Stages:** 1, 2
- **Definition:** Rolling standard deviation of 1-bar returns over 20 bars (population std, not sample)
- **Formula:** `std_pop(return_1[t-19..t])` where `std_pop = sqrt(mean(x²) - mean(x)²)`
- **Raw inputs:** Last 20 values of `return_1`
- **Window:** Rolling 20 bars
- **NaN handling:** NaN for bars 0-1 (need ≥2 returns). **Batch fixup:** If `2 ≤ i < 20`, uses available returns (`bar_features.hpp:786-789`). After z-scoring, NaN→0.0.
- **Code reference:** `src/features/bar_features.hpp:534-538`, `rolling_std()` at lines 592-604
- **CRITICAL — Population std, not sample std:** The `rolling_std()` function divides by `n` (not `n-1`). `var = sum_sq/n - mean²`.
- **Live replication notes:** Use population standard deviation. Maintain a 20-element deque of 1-bar returns.

#### volatility_50
- **Stages:** 1, 2
- **Definition:** Rolling population standard deviation of 1-bar returns over 50 bars
- **Formula:** `std_pop(return_1[t-49..t])`
- **Raw inputs:** Last 50 values of `return_1`
- **Window:** Rolling 50 bars
- **NaN handling:** Same as `volatility_20` but needs 50 returns. Batch fixup uses available returns.
- **Code reference:** `src/features/bar_features.hpp:541-545`
- **Live replication notes:** Same as volatility_20 but with a 50-element buffer.

#### high_low_range_50
- **Stages:** 1, 2
- **Definition:** Price range (max high - min low) over last 50 bars, in ticks
- **Formula:** `(max(high_mid[t-49..t]) - min(low_mid[t-49..t])) / tick_size`
- **Raw inputs:** `high_mid` and `low_mid` of last 50 bars
- **Window:** Rolling 50 bars
- **NaN handling:** NaN for bars 0-49 (need > 50 close_mids, per `bar_features.hpp:569-573`). **Batch fixup:** Uses available bars if `i >= 1` (`bar_features.hpp:828-831`).
- **Code reference:** `src/features/bar_features.hpp:569-573`, `high_low_range()` at 606-610
- **Live replication notes:** Maintain 50-bar deques for high_mid and low_mid.

#### close_position
- **Stages:** 1, 2
- **Definition:** Position of current close within the 20-bar high-low range [0, 1]
- **Formula:** `(close_mid[t] - min(low_mid[t-19..t])) / (max(high_mid[t-19..t]) - min(low_mid[t-19..t]) + ε)`
- **Raw inputs:** Current `close_mid`, last 20 `high_mid` and `low_mid`
- **Window:** Rolling 20 bars
- **NaN handling:** For bars 0: returns 0.5. For bars 1+: uses all available bars. Never NaN.
- **Code reference:** `src/features/bar_features.hpp:577-589`
- **Live replication notes:** Maintain 20-bar deques for high_mid and low_mid.

### 2.4 Category 4: Cross-Scale Dynamics (0 features used of 4 available)

None of the 4 cross-scale features (`volume_surprise`, `duration_surprise`, `acceleration`, `vol_price_corr`) are in the 20-feature set used by XGBoost. They are computed but not selected.

### 2.5 Category 5: Time Context (3 features used of 5 available)

#### time_sin
- **Stages:** 1, 2
- **Definition:** Sine component of time-of-day encoding
- **Formula:** `sin(2π × time_of_day / 24.0)` where `time_of_day` = fractional hours since midnight ET
- **Raw inputs:** `bar.time_of_day` (from `bar.close_ts`)
- **Window:** Single bar (instantaneous)
- **NaN handling:** Never NaN
- **Code reference:** `src/features/bar_features.hpp:660-661`
- **Live replication notes:** Trivial. Compute from current wall clock time.

#### time_cos
- **Stages:** 1, 2
- **Definition:** Cosine component of time-of-day encoding
- **Formula:** `cos(2π × time_of_day / 24.0)`
- **Raw inputs:** Same as `time_sin`
- **Window:** Single bar (instantaneous)
- **NaN handling:** Never NaN
- **Code reference:** `src/features/bar_features.hpp:662`
- **Live replication notes:** Trivial.

#### minutes_since_open
- **Stages:** 1, 2
- **Definition:** Minutes elapsed since RTH open (09:30 ET)
- **Formula:** `(time_of_day - 9.5) × 60`, clamped to `≥ 0`
- **Raw inputs:** `bar.time_of_day`
- **Window:** Single bar (instantaneous)
- **Range:** [0, 390] during RTH
- **NaN handling:** Never NaN
- **Code reference:** `src/features/bar_features.hpp:664-665`
- **Live replication notes:** Trivial. Compute from wall clock.

### 2.6 Category 6: Message Microstructure (3 features used of 5 available)

#### cancel_add_ratio
- **Stages:** 1, 2
- **Definition:** Ratio of cancel messages to add messages within the bar
- **Formula:** `cancel_count / (add_count + ε)` where `ε = 1e-8`
- **Raw inputs:** `bar.cancel_count`, `bar.add_count` — recomputed from MBO events at `bar_feature_export.cpp:822-829`
- **Window:** Single bar
- **NaN handling:** Near-zero if no adds (due to epsilon denominator)
- **Code reference:** `src/features/bar_features.hpp:689-690`
- **IMPORTANT:** In the export pipeline, message counts are recomputed from the actual MBO events assigned to each bar (not from the bar builder's internal counts). See `bar_feature_export.cpp:817-837`.
- **Live replication notes:** Count cancel and add messages from Rithmic MBO feed within bar window. **Requires MBO-level data, not just BBO+trades.**

#### message_rate
- **Stages:** 1, 2
- **Definition:** Total messages per second within the bar
- **Formula:** `(add_count + cancel_count + modify_count) / bar_duration_s` (0 if duration ≤ ε)
- **Raw inputs:** `bar.add_count`, `bar.cancel_count`, `bar.modify_count`, `bar.bar_duration_s`
- **Window:** Single bar
- **NaN handling:** 0 if bar_duration_s ≤ ε
- **Code reference:** `src/features/bar_features.hpp:692-693`
- **NOTE on bar.message_rate vs row.message_rate:** The export tool at `bar_feature_export.cpp:833-836` sets `bar.message_rate` including trade events in the total. However, `BarFeatureComputer::compute_message_microstructure()` at `bar_features.hpp:688-693` independently computes `row.message_rate` from `bar.add_count + bar.cancel_count + bar.modify_count` — **excluding trades**. The Parquet output uses the `BarFeatureRow.message_rate` (from `BarFeatureComputer`), NOT `Bar.message_rate`. **The model was trained on the version that EXCLUDES trades.** The live system must match this: `message_rate = (add_count + cancel_count + modify_count) / bar_duration_s`.
- **Live replication notes:** Count all MBO messages within bar window, divide by 5 seconds. **Requires MBO-level data.**

#### modify_fraction
- **Stages:** 1, 2
- **Definition:** Fraction of total messages that are modify events
- **Formula:** `modify_count / (add_count + cancel_count + modify_count + ε)`
- **Raw inputs:** `bar.add_count`, `bar.cancel_count`, `bar.modify_count`
- **Window:** Single bar
- **NaN handling:** 0 if total_msgs ≤ ε
- **Code reference:** `src/features/bar_features.hpp:695-697`
- **Live replication notes:** **Requires MBO-level data.**

---

## 3. Features NOT Used (42 of 62 Track A features excluded)

The following Track A features are computed by `BarFeatureComputer` and exported to Parquet, but are **NOT** in the 20-feature set used by XGBoost:

**Book Shape (28 excluded):** `book_imbalance_1`, `book_imbalance_3`, `book_imbalance_5`, `book_imbalance_10`, `bid_depth_profile_0..9`, `ask_depth_profile_0..9`, `depth_concentration_bid`, `depth_concentration_ask`, `book_slope_bid`, `book_slope_ask`, `level_count_bid`, `level_count_ask`

**Order Flow (2 excluded):** `large_trade_count`, `kyle_lambda`

**Price Dynamics (2 excluded):** `momentum`, `high_low_range_20`

**Cross-Scale (4 excluded):** `volume_surprise`, `duration_surprise`, `acceleration`, `vol_price_corr`

**Time Context (2 excluded):** `minutes_to_close`, `session_volume_frac`

**Message Microstructure (2 excluded):** `order_flow_toxicity`, `cancel_concentration`

---

## 4. Book Snapshot Columns (40 columns — NOT used by XGBoost)

The Parquet export includes 40 book snapshot columns (`book_snap_0` through `book_snap_39`), but these are used only by the CNN encoder, NOT by the GBT-only pipeline.

**Layout:** 20 rows × 2 columns, flattened row-major.
- Rows 0-9: Bids in **reverse** order (deepest first at row 0, best bid at row 9)
- Rows 10-19: Asks in order (best ask at row 10, deepest at row 19)
- Column 0: `price - close_mid` (price offset from mid)
- Column 1: `size` (raw size, not log-transformed)

**Code reference:** `src/features/raw_representations.hpp:16-49` (BookSnapshotExport)

---

## 5. Message Summary Columns (33 columns — NOT used by XGBoost)

The Parquet export includes 33 message summary columns (`msg_summary_0` through `msg_summary_32`). These are NOT in the 20-feature set.

**Layout:** `bar_feature_export.cpp:316-374`
- Columns 0-29: 10 time-decile bins × 3 action types (add/cancel/modify counts)
- Column 30: cancel/add ratio first half of bar
- Column 31: cancel/add ratio second half of bar
- Column 32: max instantaneous message rate across deciles

---

## 6. Normalization (Pre-Model)

**Code reference:** `.kit/results/cpcv-corrected-costs/run_experiment.py:387-392`

```python
f_mean = np.nanmean(ft_train, axis=0)
f_std = np.nanstd(ft_train, axis=0)
f_std[f_std < 1e-10] = 1.0
ft_train_z = np.nan_to_num((ft_train - f_mean) / f_std, nan=0.0)
ft_val_z = np.nan_to_num((ft_val - f_mean) / f_std, nan=0.0)
ft_test_z = np.nan_to_num((ft_test - f_mean) / f_std, nan=0.0)
```

| Step | Detail |
|------|--------|
| **Statistics source** | Training fold only (no leakage) |
| **Method** | Z-score: `(x - mean) / std` per feature column |
| **Std floor** | `std < 1e-10` → `std = 1.0` (prevents division by zero) |
| **NaN handling** | After z-scoring, `np.nan_to_num(..., nan=0.0)` replaces any remaining NaN with 0.0 |
| **Data type** | `float64` (features cast to float64 before normalization) |
| **Scope** | Per CPCV fold. Each of 45 folds has its own mean/std. |

**For live inference:** Must use the mean/std from the training data of the deployed model (not recomputed per-bar). These statistics must be saved alongside the model checkpoint.

---

## 7. Label Construction

### 7.1 Triple Barrier Labels (Bidirectional)

**Code reference:** `src/backtest/triple_barrier.hpp`

**Configuration (CPCV experiment):**

| Parameter | Value | Code Reference |
|-----------|-------|----------------|
| `target_ticks` | 19 | CLI `--target 19` |
| `stop_ticks` | 7 | CLI `--stop 7` |
| `volume_horizon` | 50,000 contracts | CLI `--volume-horizon 50000` |
| `max_time_horizon_s` | 3,600 seconds (1 hour) | CLI `--max-time-horizon 3600` |
| `min_return_ticks` | 2 | Hardcoded at `bar_feature_export.cpp:739` |
| `tick_size` | 0.25 | Hardcoded at `bar_feature_export.cpp:741` |
| `bidirectional` | true (default) | `triple_barrier.hpp:17` |

**Bidirectional label logic (`compute_bidirectional_tb_label`):**

Two independent races run simultaneously from bar `idx`:
- **Long race:** Does price hit `+target_dist` before hitting `-stop_dist`?
- **Short race:** Does price hit `-target_dist` before hitting `+stop_dist`?

Where:
- `target_dist = target_ticks × tick_size = 19 × 0.25 = 4.75`
- `stop_dist = stop_ticks × tick_size = 7 × 0.25 = 1.75`

Scanning is forward from `idx+1`:
1. **Time cap:** If `elapsed_s >= max_time_horizon_s` → stop scanning.
2. **Target hit:** If `diff >= target_dist` → long triggered. If `-diff >= target_dist` → short triggered.
3. **Stop hit:** If `-diff >= stop_dist` → long stopped (resolved, not triggered). If `diff >= stop_dist` → short stopped.
4. **Volume expiry:** If `cum_volume >= volume_horizon` → stop scanning.
5. **V-reversal override:** If one race triggered and the other was stopped, check if the stopped race's target is also reached without continuation past target or re-stop.

**Label encoding:**

| Condition | Label | exit_type |
|-----------|-------|-----------|
| Only long triggered | +1 | "long_target" |
| Only short triggered | -1 | "short_target" |
| Both triggered (or V-reversal) | 0 | "both" |
| Neither triggered | 0 | "neither" |

### 7.2 Two-Stage Model Labels

**Code reference:** `.kit/results/cpcv-corrected-costs/run_experiment.py:398-455`

| Stage | Target | Classes | Label Formula |
|-------|--------|---------|---------------|
| Stage 1 | Is the bar directional? | Binary (0/1) | `is_directional = (tb_label != 0)` |
| Stage 2 | If directional, which direction? | Binary (0/1) | `is_long = (tb_label == 1)`, trained only on bars where `tb_label != 0` |
| Combined | Final prediction | {-1, 0, +1} | If `P(directional) > 0.50`: use Stage 2 direction. Else: predict 0. |

---

## 8. Pipeline Order of Operations

### 8.1 Research Pipeline (Batch — Data → Model)

```
Raw MBO .dbn.zst files (312 daily files, Databento format)
  │
  ├─[1] StreamingBookBuilder.process_event()
  │     File: tools/bar_feature_export.cpp:40-246
  │     • Reconstructs order book from MBO events
  │     • Emits 100ms BookSnapshots aligned to clock time
  │     • Filters to RTH (09:30-16:00 ET)
  │     • Counts trade events per snapshot
  │
  ├─[2] BarFactory::create("time", 5) → TimeBarBuilder
  │     File: src/bars/time_bar_builder.hpp
  │     • Consumes snapshots, emits bars every 50 snapshots (5 seconds)
  │     • Aggregates OHLCV, book state, trade stats
  │
  ├─[3] MBO event assignment to bars
  │     File: tools/bar_feature_export.cpp:808-837
  │     • Assigns MBO events to bars by timestamp
  │     • Recounts add/cancel/modify/trade from actual events
  │
  ├─[4] BarFeatureComputer.compute_all(bars)
  │     File: src/features/bar_features.hpp:271-283
  │     • Computes 62 Track A features + 4 forward returns
  │     • Fixup: fills NaN rolling features using available data
  │     • Fills forward returns (1, 5, 20, 100 bars ahead)
  │
  ├─[5] BookSnapshotExport.flatten(bar)
  │     File: src/features/raw_representations.hpp:39-48
  │     • Flattens 10-level bid/ask book into 40-element vector
  │
  ├─[6] compute_message_summary(bar, mbo_events)
  │     File: tools/bar_feature_export.cpp:316-374
  │     • Bins MBO events into 10 time deciles × 3 action types → 33 elements
  │
  ├─[7] compute_bidirectional_tb_label(bars, idx, cfg)
  │     File: src/backtest/triple_barrier.hpp:136-243
  │     • Runs long and short race independently
  │     • Produces label ∈ {-1, 0, +1}
  │
  ├─[8] Write Parquet (zstd compressed)
  │     File: tools/bar_feature_export.cpp:379-537
  │     • 152 columns: 6 metadata + 62 features + 40 book + 33 msg + 4 fwd + 1 event + 3 TB + 3 bidir
  │     • Warmup bars (first 50) are SKIPPED in export
  │     • Bars without valid fwd_return_1 are SKIPPED
  │
  ├─[9] Load Parquet in Python (Polars)
  │     File: .kit/results/cpcv-corrected-costs/run_experiment.py:108-184
  │     • Filter is_warmup == 0
  │     • Select 20 NON_SPATIAL_FEATURES columns
  │     • Extract tb_label as target
  │
  ├─[10] CPCV Split (N=10, k=2, 45 splits)
  │      File: .kit/results/cpcv-corrected-costs/run_experiment.py:81-87
  │      • 201 development days + 50 holdout days
  │      • Purge: 500 bars, Embargo: 4,600 bars (~1 trading day)
  │
  ├─[11] Z-score normalize (per fold, train stats only)
  │      File: .kit/results/cpcv-corrected-costs/run_experiment.py:387-392
  │      • mean, std from training fold
  │      • NaN → 0.0 after z-scoring
  │
  ├─[12] Stage 1: XGBoost binary classifier (directional vs hold)
  │      File: .kit/results/cpcv-corrected-costs/run_experiment.py:398-420
  │      • Target: is_directional = (tb_label != 0)
  │      • Params: max_depth=6, lr=0.0134, n_est=2000, early_stop=50
  │
  ├─[13] Stage 2: XGBoost binary classifier (long vs short)
  │      File: .kit/results/cpcv-corrected-costs/run_experiment.py:422-449
  │      • Target: is_long = (tb_label == 1), filtered to directional bars only
  │      • Same params as Stage 1
  │
  └─[14] Combine: P(directional) > 0.50 → Stage 2 direction, else → 0
         File: .kit/results/cpcv-corrected-costs/run_experiment.py:451-455
```

### 8.2 Live Pipeline (Real-Time — Tick → Signal)

```
Rithmic tick data (streaming MBO/BBO + trades)
  │
  ├─[1] Book maintenance (equivalent to StreamingBookBuilder)
  │     • Process each MBO event: add/cancel/modify/trade/fill
  │     • Maintain bid_levels_, ask_levels_, orders_ maps
  │     • Maintain rolling 50-trade buffer
  │     • Emit BookSnapshot every 100ms (timer-aligned)
  │
  ├─[2] Time bar construction (equivalent to TimeBarBuilder)
  │     • Accumulate 50 snapshots → emit bar
  │     • Track OHLCV, book state at close, trade stats
  │
  ├─[3] MBO event counting
  │     • Count add/cancel/modify/trade events within bar window
  │     • Assign to bar by timestamp
  │
  ├─[4] Feature computation (equivalent to BarFeatureComputer.update())
  │     • Compute 20 features from bar + lookback state
  │     • Maintain deques: close_mids(50+), high_mids(20+), low_mids(20+),
  │       volumes(20), returns(50), net_volumes(20), abs_returns(20)
  │     • Maintain EWMA state (not used by model but computed)
  │
  ├─[5] Warmup check
  │     • Discard first 50 bars of each session
  │     • Reset BarFeatureComputer state at session boundaries
  │
  ├─[6] Z-score normalize using saved training statistics
  │     • Apply stored mean/std from model training
  │     • NaN → 0.0
  │
  ├─[7] Stage 1 inference: P(directional)
  │     • If P(directional) ≤ 0.50 → no trade
  │
  └─[8] Stage 2 inference: P(long)
        • If P(long) > 0.50 → long signal
        • If P(long) ≤ 0.50 → short signal
```

---

## 9. MBO Data Requirements

### 9.1 Minimum Data Needed from Live Feed

| Data Type | Required? | Used By | Notes |
|-----------|-----------|---------|-------|
| Best bid/ask price | **YES** | spread, mid_price, all price-derived features | Core |
| Best bid/ask size | **YES** | weighted_imbalance (if only BBO available, degrades to `imbalance_1`) | Minimum for book features |
| 10-level bid depth (price + size) | **PREFERRED** | weighted_imbalance (uses all 10 levels) | Improves weighted_imbalance quality |
| Trade price | **YES** | VWAP, all return features via mid_price | Core |
| Trade size | **YES** | volume, avg_trade_size, VWAP, net_volume | Core |
| Trade aggressor side | **YES** | buy_volume, sell_volume, net_volume, volume_imbalance | Core |
| Order Add messages | **YES** | cancel_add_ratio, message_rate, modify_fraction | MBO-level |
| Order Cancel messages | **YES** | cancel_add_ratio, message_rate | MBO-level |
| Order Modify messages | **YES** | modify_fraction, message_rate | MBO-level |
| Trade event count | **YES** | trade_count | Can count from trade messages |

### 9.2 Features Requiring Full Book Reconstruction

The **weighted_imbalance** feature is the only model input that uses depth beyond BBO. It uses all 10 levels with inverse-distance weighting. However, the best bid/ask (level 0) dominates due to the `1/(i+1)` weighting:
- Level 0: weight 1.0
- Level 1: weight 0.5
- Level 9: weight 0.1

If Rithmic only provides BBO, `weighted_imbalance` degrades to `book_imbalance_1` (BBO-only imbalance). This may or may not meaningfully affect model performance — quantitative assessment needed.

### 9.3 Features Computable from BBO + Trades Only

17 of 20 features are fully computable from BBO + trades:

| Feature | BBO + Trades? |
|---------|---------------|
| spread | YES (BBO) |
| net_volume | YES (trades) |
| volume_imbalance | YES (trades) |
| trade_count | YES (trades) |
| avg_trade_size | YES (trades) |
| vwap_distance | YES (trades + BBO) |
| return_1 | YES (mid from BBO) |
| return_5 | YES (mid from BBO) |
| return_20 | YES (mid from BBO) |
| volatility_20 | YES (derived from returns) |
| volatility_50 | YES (derived from returns) |
| high_low_range_50 | YES (derived from mid) |
| close_position | YES (derived from mid) |
| time_sin | YES (wall clock) |
| time_cos | YES (wall clock) |
| minutes_since_open | YES (wall clock) |
| weighted_imbalance | **PARTIAL** (needs 10 levels for full accuracy) |

### 9.4 Features Requiring MBO-Level Data

3 features require MBO-level message data (individual order add/cancel/modify):

| Feature | Requirement |
|---------|-------------|
| cancel_add_ratio | Count of cancel vs add messages |
| message_rate | Count of all MBO messages / time |
| modify_fraction | Count of modify messages / total messages |

**Gap analysis:** If Rithmic provides only BBO + trades (no individual order messages), these 3 features cannot be computed. Options:
1. **Use Rithmic MBO feed if available** — Rithmic R|API+ can provide order-level data.
2. **Approximate from BBO changes** — Count BBO price/size changes as proxy for add/cancel/modify. This is imprecise.
3. **Retrain model without these 3 features** — Test ablation impact.

### 9.5 Research Data vs Rithmic Feed

| Aspect | Research (Databento MBO) | Live (Rithmic R|API+) |
|--------|--------------------------|----------------------|
| **Data granularity** | Individual order events (Add, Cancel, Modify, Trade, Fill, Clear) | Depends on subscription level |
| **Order ID tracking** | Full order lifecycle via `order_id` | Available via Market Data API |
| **Book depth** | 10 levels constructed from MBO events | Configurable depth via `SubscribeDepthByOrder` |
| **Trade aggressor** | `side` field on Trade action | Available via `TradeInfo::eAggressorSide` |
| **Timestamp precision** | Nanoseconds (from exchange) | Microsecond (source timestamp in Rithmic) |
| **Event sequencing** | `F_LAST` flag marks atomic transaction boundaries | Rithmic provides ordered callbacks |
| **Snapshot rate** | 100ms synthetic snapshots from event stream | Configurable via timer |

---

## 10. XGBoost Model Configuration

### 10.1 Tuned Hyperparameters (CPCV Experiment)

**Code reference:** `.kit/results/cpcv-corrected-costs/run_experiment.py:37-53`

```python
TUNED_XGB_PARAMS_BINARY = {
    "max_depth": 6,
    "learning_rate": 0.0134,
    "min_child_weight": 20,
    "subsample": 0.561,
    "colsample_bytree": 0.748,
    "reg_alpha": 0.0014,
    "reg_lambda": 6.586,
    "n_estimators": 2000,
    "early_stopping_rounds": 50,
    "objective": "binary:logistic",
    "eval_metric": "logloss",
    "tree_method": "hist",
    "random_state": 42,
    "verbosity": 0,
    "n_jobs": -1,
}
```

### 10.2 Default Hyperparameters (earlier experiments)

**Code reference:** `scripts/hybrid_model/train_xgboost.py:26-41`

```python
{
    "objective": "multi:softprob",
    "num_class": 3,
    "max_depth": 6,
    "learning_rate": 0.05,
    "n_estimators": 500,
    "subsample": 0.8,
    "colsample_bytree": 0.8,
    "min_child_weight": 10,
    "reg_alpha": 0.1,
    "reg_lambda": 1.0,
}
```

**NOTE:** The CPCV experiment uses the **tuned** parameters with a **two-stage binary** architecture, not the default 3-class softmax. The deployed model should use the tuned params.

### 10.3 Cross-Validation (CPCV)

| Parameter | Value |
|-----------|-------|
| N (groups) | 10 |
| k (test groups per split) | 2 |
| Total splits | C(10,2) = 45 |
| Purge window | 500 bars |
| Embargo window | 4,600 bars (~1 trading day = 4,680 bars) |
| Development days | 201 (first 201 trading days of 2022) |
| Holdout days | 50 (days 202-251) |
| Internal validation | Last 20% of training days per fold (for early stopping) |

---

## 11. Critical Edge Cases

### 11.1 Session Start (Incomplete Lookbacks)

**Warmup period:** First 50 bars of each session are discarded (`WARMUP_BARS = 50` at `bar_feature_export.cpp:254`).

During warmup, many features produce NaN:
- `return_1`: NaN at bar 0 (need 2 close_mids)
- `return_5`: NaN at bars 0-4
- `return_20`: NaN at bars 0-19
- `volatility_20`: NaN at bars 0-1 (need ≥2 returns)
- `volatility_50`: NaN at bars 0-1
- `high_low_range_50`: NaN at bars 0-49
- `close_position`: 0.5 at bar 0, then uses available data

**Batch fixup:** `BarFeatureComputer::fixup_rolling_features()` (lines 775-833) retroactively fills NaN values using whatever data is available (e.g., `volatility_20` at bar 10 uses only 10 returns). This means features in the first 20-50 bars have **shorter effective lookback windows** than their names suggest.

**Live implication:** At session start (09:30 ET), discard the first 50 bars (first ~4.2 minutes). During warmup bars 2-50, features like `volatility_20` will be computed with partial data (matching the batch fixup behavior). This is acceptable as long as the behavior is identical to the research pipeline.

**State reset:** `BarFeatureComputer::reset()` (line 286) clears all deques and EWMA state at session boundaries. In live: call `reset()` at 09:30 ET each day.

### 11.2 Market Gaps / Halts

The research pipeline processes only RTH data (09:30-16:00 ET) with no handling for mid-session halts. If the exchange halts trading:
- The 100ms snapshot timer continues emitting snapshots with stale book state
- Time bars will contain zero-volume periods
- Features will reflect the stale state

**Live consideration:** Must handle halts explicitly — either pause bar construction or mark bars during halts.

### 11.3 Missing Data / Connectivity Issues

The research pipeline assumes complete, ordered data with no gaps. If the live Rithmic feed drops packets:
- Book state may be incorrect until the next clear event
- Trade counts may be inaccurate
- Feature values will silently drift

**Recommendation:** Implement sequence number tracking on the Rithmic feed. On gap detection, request book rebuild.

### 11.4 Timestamp Precision

The research pipeline uses nanosecond timestamps from Databento (exchange-sourced). Rithmic provides microsecond precision. For 5-second bars and 100ms snapshots, microsecond precision is more than adequate. No precision-related parity issues expected.

### 11.5 Contract Rollover

The research pipeline handles quarterly MES contract rollovers via a hardcoded table (`bar_feature_export.cpp:256-268`). The live system must:
1. Track the front-month contract
2. Switch instrument IDs at rollover
3. Reset book state at rollover
4. Consider the close_mid discontinuity at rollover (affects return features)

### 11.6 VWAP Edge Case

When `bar.volume == 0` (no trades in bar), `bar.vwap = 0.0` (uninitialized). The `vwap_distance` feature becomes `close_mid / tick_size`, which is a very large value (~17,000 for MES at 4,300). This is an implicit assumption in the research code.

**Live replication:** Replicate this behavior exactly. When no trades occur in a bar, VWAP = 0.0 → vwap_distance = close_mid / 0.25. After z-scoring, this extreme value maps to a large positive z-score (likely > 10σ), but `nan_to_num` does not clip it.

---

## 12. Live Replication Risks

### 12.1 Highest Risk Features

| Feature | Risk Level | Reason |
|---------|------------|--------|
| `cancel_add_ratio` | **HIGH** | Requires MBO-level data. If Rithmic doesn't provide individual order messages, cannot compute. |
| `message_rate` | **HIGH** | Same as above. |
| `modify_fraction` | **HIGH** | Same as above. |
| `weighted_imbalance` | **MEDIUM** | Requires 10-level depth. BBO-only degrades accuracy. |
| `volatility_20/50` | **MEDIUM** | Population std (not sample). Easy to get wrong. |
| `vwap_distance` | **MEDIUM** | Zero-volume bar edge case produces extreme values. |
| `trade_count` | **LOW-MEDIUM** | Must count actual trade events, not trade buffer snapshots. |

### 12.2 Implicit Assumptions in Research Code

1. **Population standard deviation:** `rolling_std()` uses `var = sum_sq/n - mean²` (population), not `sum_sq/(n-1) - ...` (sample). Using sample std would produce systematically different volatility values.

2. **Epsilon value:** `ε = 1e-8` is used consistently across all ratio computations. Using a different epsilon (e.g., 1e-6 or 1e-10) would change feature values for edge cases.

3. **Bar-close book state:** Book shape features use the book at the **last snapshot** of the bar, not the time-weighted average or the snapshot at bar open.

4. **VWAP from single trade per snapshot:** The bar builder's VWAP accumulates only the **last trade per 100ms snapshot** (from `extract_trade()`). If multiple trades occur in one snapshot, only the last one's price/size contributes to VWAP. This is a lossy approximation. However, the export tool recounts trade events from MBO data independently.

5. **Batch fixup changes early-bar feature values:** The `fixup_rolling_features()` function in batch mode produces different values for bars 2-50 than the streaming `update()` function would. In streaming mode, `volatility_20` at bar 10 would be NaN (need 20 returns). In batch mode, it's computed from 10 available returns. **The live system must implement the same fixup behavior** — use available data when lookback is incomplete.

6. **Session volume fraction excluded:** `session_volume_frac` is NOT in the 20-feature set, so the Day 1 vs Day 2+ behavior (0.0 on Day 1) doesn't matter.

7. **MBO event recount overrides bar builder counts:** The export tool (`bar_feature_export.cpp:817-837`) recounts `add_count`, `cancel_count`, `modify_count`, `trade_event_count` from the actual MBO event stream, overriding whatever the bar builder computed internally. The live system must count from actual MBO events, not from bar builder internals.

### 12.3 Recommendations for Validation

1. **Replay validation:** Process one day of historical Databento MBO data through both the research C++ pipeline and the live Rithmic pipeline. Compare all 20 feature values bar-by-bar. Maximum acceptable deviation: < 1e-5 (float32 precision).

2. **Feature distribution monitoring:** In production, log feature distributions (mean, std, min, max, % NaN) per session. Alert if any feature's distribution diverges > 2σ from historical.

3. **Canary features:** `time_sin`, `time_cos`, `minutes_since_open` are trivially verifiable (depend only on wall clock). Use these as canary features to validate bar timing alignment.

4. **Lookback buffer audit:** Verify that all deques (`close_mids_`, `volumes_`, `returns_`, etc.) have the correct maximum size and discard policy (all are unbounded deques in the research code — they grow for the entire session and are only reset at session boundaries).

5. **Zero-volume bar test:** Verify `vwap_distance` behavior when no trades occur in a bar. This is the most likely source of extreme feature values.

6. **Normalization checkpoint:** Save the per-fold (mean, std) vectors from CPCV training. At inference time, load the production model's training fold statistics. Verify that the live z-scored features fall within ±10σ of the training distribution.

---

## Appendix A: Complete Parquet Schema (152 columns)

```
 # | Column Name          | Type    | Source
---+----------------------+---------+------------------
 0 | timestamp            | INT64   | bar.close_ts
 1 | bar_type             | STRING  | CLI arg
 2 | bar_param            | STRING  | CLI arg
 3 | day                  | INT64   | date integer (YYYYMMDD)
 4 | is_warmup            | BOOL    | true for first 50 bars
 5 | bar_index            | INT64   | 0-based index within day

 6-67  | [62 Track A features]     | FLOAT64 | BarFeatureRow
       | book_imbalance_1          |         |
       | book_imbalance_3          |         |
       | book_imbalance_5          |         |
       | book_imbalance_10         |         |
       | weighted_imbalance        |         |
       | spread                    |         |
       | bid_depth_profile_0..9    |         |
       | ask_depth_profile_0..9    |         |
       | depth_concentration_bid   |         |
       | depth_concentration_ask   |         |
       | book_slope_bid            |         |
       | book_slope_ask            |         |
       | level_count_bid           |         |
       | level_count_ask           |         |
       | net_volume                |         |
       | volume_imbalance          |         |
       | trade_count               |         |
       | avg_trade_size            |         |
       | large_trade_count         |         |
       | vwap_distance             |         |
       | kyle_lambda               |         |
       | return_1                  |         |
       | return_5                  |         |
       | return_20                 |         |
       | volatility_20             |         |
       | volatility_50             |         |
       | momentum                  |         |
       | high_low_range_20         |         |
       | high_low_range_50         |         |
       | close_position            |         |
       | volume_surprise           |         |
       | duration_surprise         |         |
       | acceleration              |         |
       | vol_price_corr            |         |
       | time_sin                  |         |
       | time_cos                  |         |
       | minutes_since_open        |         |
       | minutes_to_close          |         |
       | session_volume_frac       |         |
       | cancel_add_ratio          |         |
       | message_rate              |         |
       | modify_fraction           |         |
       | order_flow_toxicity       |         |
       | cancel_concentration      |         |

68-107 | book_snap_0..39           | FLOAT64 | BookSnapshotExport
108-140| msg_summary_0..32         | FLOAT64 | MessageSummary

141    | fwd_return_1              | FLOAT64 | forward 1-bar return
142    | fwd_return_5              | FLOAT64 | forward 5-bar return
143    | fwd_return_20             | FLOAT64 | forward 20-bar return
144    | fwd_return_100            | FLOAT64 | forward 100-bar return
145    | mbo_event_count           | FLOAT64 | count of MBO events in bar

146    | tb_label                  | FLOAT64 | bidirectional label {-1, 0, +1}
147    | tb_exit_type              | STRING  | "long_target", "short_target", "both", "neither"
148    | tb_bars_held              | FLOAT64 | bars until resolution

149    | tb_both_triggered         | FLOAT64 | 1.0 if both races triggered
150    | tb_long_triggered         | FLOAT64 | 1.0 if long race triggered
151    | tb_short_triggered        | FLOAT64 | 1.0 if short race triggered
```

---

## Appendix B: Feature Computation Quick Reference

All formulas use: `tick_size = 0.25`, `ε = 1e-8`, `EWMA_ALPHA = 2/(20+1) ≈ 0.0952`

| # | Feature | Formula | Lookback |
|---|---------|---------|----------|
| 0 | weighted_imbalance | `(Σ bid[i]·w[i] - Σ ask[i]·w[i]) / (Σ bid[i]·w[i] + Σ ask[i]·w[i] + ε)`, w[i]=1/(i+1) | 0 bars |
| 1 | spread | `bar.spread / 0.25` | 0 bars |
| 2 | net_volume | `buy_volume - sell_volume` | 0 bars |
| 3 | volume_imbalance | `net_volume / (volume + ε)` | 0 bars |
| 4 | trade_count | Count of action='T' MBO events in bar | 0 bars |
| 5 | avg_trade_size | `volume / trade_event_count` (0 if no trades) | 0 bars |
| 6 | vwap_distance | `(close_mid - vwap) / 0.25` | 0 bars |
| 7 | return_1 | `(close_mid[t] - close_mid[t-1]) / 0.25` | 1 bar |
| 8 | return_5 | `(close_mid[t] - close_mid[t-5]) / 0.25` | 5 bars |
| 9 | return_20 | `(close_mid[t] - close_mid[t-20]) / 0.25` | 20 bars |
| 10 | volatility_20 | `sqrt(mean(r²) - mean(r)²)` over last 20 1-bar returns | 20 bars |
| 11 | volatility_50 | `sqrt(mean(r²) - mean(r)²)` over last 50 1-bar returns | 50 bars |
| 12 | high_low_range_50 | `(max(high_mid[-50:]) - min(low_mid[-50:])) / 0.25` | 50 bars |
| 13 | close_position | `(close_mid - min(low_mid[-20:])) / (max(high_mid[-20:]) - min(low_mid[-20:]) + ε)` | 20 bars |
| 14 | cancel_add_ratio | `cancel_count / (add_count + ε)` | 0 bars |
| 15 | message_rate | `(add + cancel + modify) / bar_duration_s` | 0 bars |
| 16 | modify_fraction | `modify / (add + cancel + modify + ε)` | 0 bars |
| 17 | time_sin | `sin(2π × time_of_day / 24)` | 0 bars |
| 18 | time_cos | `cos(2π × time_of_day / 24)` | 0 bars |
| 19 | minutes_since_open | `max(0, (time_of_day - 9.5) × 60)` | 0 bars |

---

## Appendix C: State That Must Be Maintained Across Bars (Live System)

| State Variable | Type | Max Size | Reset At |
|----------------|------|----------|----------|
| `close_mids_` | deque<float> | Unbounded (grows all session, ~4680 per day) | Session start |
| `high_mids_` | deque<float> | Unbounded | Session start |
| `low_mids_` | deque<float> | Unbounded | Session start |
| `volumes_` | deque<float> | 20 (pop_front at 21) | Session start |
| `returns_` | deque<float> | Unbounded | Session start |
| `net_volumes_` | deque<float> | 20 (pop_front at 21) | Session start |
| `abs_returns_` | deque<float> | Unbounded | Session start |
| `ewma_volume_` | float | 1 | Session start |
| `ewma_duration_` | float | 1 | Session start |
| `ewma_initialized_` | bool | 1 | Session start |
| `prev_return_1_` | float | 1 | Session start (to NaN) |
| `cumulative_volume_` | float | 1 | Session start |
| `prior_day_totals_` | vector<float> | Days processed | Never (accumulates across days) |
| `bar_count_` | int | 1 | Session start |

**NOTE:** `prior_day_totals_` and `session_volume_frac` are NOT used by the 20-feature model, but are maintained by the feature computer. The live system can skip these.

**Effective state for the 20-feature model:**
- Last 50 `close_mid` values (for return_20 and volatility_50)
- Last 50 `high_mid` values (for high_low_range_50)
- Last 50 `low_mid` values (for high_low_range_50 and close_position)
- Last 50 1-bar returns (for volatility_50)
- Current bar's MBO event counts (add, cancel, modify, trade)
- Current bar's trade aggregation (buy_volume, sell_volume, VWAP accumulators)
- Current bar's book state (10-level bid/ask at last snapshot)
- `bar_count_` for warmup check (≥ 50 to be live)
