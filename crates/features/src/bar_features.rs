use common::bar::Bar;
use common::book::BOOK_DEPTH;
use std::collections::VecDeque;

const EPS: f32 = 1e-8;
const TWO_PI: f32 = 2.0 * std::f32::consts::PI;
const RTH_OPEN_HOUR: f32 = 9.5;
const RTH_CLOSE_HOUR: f32 = 16.0;
const EWMA_SPAN: usize = 20;
const EWMA_ALPHA: f32 = 2.0 / (EWMA_SPAN as f32 + 1.0);

/// All Track A features + metadata + forward returns (62 features total).
#[derive(Debug, Clone)]
pub struct BarFeatureRow {
    // Bar metadata
    pub timestamp: u64,
    pub bar_type: String,
    pub bar_param: f32,
    pub day: i32,
    pub is_warmup: bool,

    // Category 1: Book Shape (32 features)
    pub book_imbalance_1: f32,
    pub book_imbalance_3: f32,
    pub book_imbalance_5: f32,
    pub book_imbalance_10: f32,
    pub weighted_imbalance: f32,
    pub spread: f32,
    pub bid_depth_profile: [f32; 10],
    pub ask_depth_profile: [f32; 10],
    pub depth_concentration_bid: f32,
    pub depth_concentration_ask: f32,
    pub book_slope_bid: f32,
    pub book_slope_ask: f32,
    pub level_count_bid: i32,
    pub level_count_ask: i32,

    // Category 2: Order Flow (7 features)
    pub net_volume: f32,
    pub volume_imbalance: f32,
    pub trade_count: u32,
    pub avg_trade_size: f32,
    pub large_trade_count: u32,
    pub vwap_distance: f32,
    pub kyle_lambda: f32,

    // Category 3: Price Dynamics (9 features)
    pub return_1: f32,
    pub return_5: f32,
    pub return_20: f32,
    pub volatility_20: f32,
    pub volatility_50: f32,
    pub momentum: f32,
    pub high_low_range_20: f32,
    pub high_low_range_50: f32,
    pub close_position: f32,

    // Category 4: Cross-Scale Dynamics (4 features)
    pub volume_surprise: f32,
    pub duration_surprise: f32,
    pub acceleration: f32,
    pub vol_price_corr: f32,

    // Category 5: Time Context (5 features)
    pub time_sin: f32,
    pub time_cos: f32,
    pub minutes_since_open: f32,
    pub minutes_to_close: f32,
    pub session_volume_frac: f32,

    // Category 6: Message Microstructure (5 features)
    pub cancel_add_ratio: f32,
    pub message_rate: f32,
    pub modify_fraction: f32,
    pub order_flow_toxicity: f32,
    pub cancel_concentration: f32,

    // Forward returns (targets, not features)
    pub fwd_return_1: f32,
    pub fwd_return_5: f32,
    pub fwd_return_20: f32,
    pub fwd_return_100: f32,
}

impl Default for BarFeatureRow {
    fn default() -> Self {
        Self {
            timestamp: 0,
            bar_type: String::new(),
            bar_param: 0.0,
            day: 0,
            is_warmup: false,
            book_imbalance_1: 0.0,
            book_imbalance_3: 0.0,
            book_imbalance_5: 0.0,
            book_imbalance_10: 0.0,
            weighted_imbalance: 0.0,
            spread: 0.0,
            bid_depth_profile: [0.0; 10],
            ask_depth_profile: [0.0; 10],
            depth_concentration_bid: 0.0,
            depth_concentration_ask: 0.0,
            book_slope_bid: 0.0,
            book_slope_ask: 0.0,
            level_count_bid: 0,
            level_count_ask: 0,
            net_volume: 0.0,
            volume_imbalance: 0.0,
            trade_count: 0,
            avg_trade_size: 0.0,
            large_trade_count: 0,
            vwap_distance: 0.0,
            kyle_lambda: 0.0,
            return_1: 0.0,
            return_5: 0.0,
            return_20: 0.0,
            volatility_20: 0.0,
            volatility_50: 0.0,
            momentum: 0.0,
            high_low_range_20: 0.0,
            high_low_range_50: 0.0,
            close_position: 0.0,
            volume_surprise: 0.0,
            duration_surprise: 0.0,
            acceleration: 0.0,
            vol_price_corr: 0.0,
            time_sin: 0.0,
            time_cos: 0.0,
            minutes_since_open: 0.0,
            minutes_to_close: 0.0,
            session_volume_frac: 0.0,
            cancel_add_ratio: 0.0,
            message_rate: 0.0,
            modify_fraction: 0.0,
            order_flow_toxicity: 0.0,
            cancel_concentration: 0.0,
            fwd_return_1: 0.0,
            fwd_return_5: 0.0,
            fwd_return_20: 0.0,
            fwd_return_100: 0.0,
        }
    }
}

impl BarFeatureRow {
    /// Total Track A feature count (excluding metadata and forward returns).
    pub fn feature_count() -> usize {
        62
    }

    /// Get all feature names in canonical order.
    pub fn feature_names() -> Vec<&'static str> {
        vec![
            // Cat 1: Book Shape
            "book_imbalance_1", "book_imbalance_3", "book_imbalance_5", "book_imbalance_10",
            "weighted_imbalance", "spread",
            "bid_depth_profile_0", "bid_depth_profile_1", "bid_depth_profile_2",
            "bid_depth_profile_3", "bid_depth_profile_4", "bid_depth_profile_5",
            "bid_depth_profile_6", "bid_depth_profile_7", "bid_depth_profile_8",
            "bid_depth_profile_9",
            "ask_depth_profile_0", "ask_depth_profile_1", "ask_depth_profile_2",
            "ask_depth_profile_3", "ask_depth_profile_4", "ask_depth_profile_5",
            "ask_depth_profile_6", "ask_depth_profile_7", "ask_depth_profile_8",
            "ask_depth_profile_9",
            "depth_concentration_bid", "depth_concentration_ask",
            "book_slope_bid", "book_slope_ask",
            "level_count_bid", "level_count_ask",
            // Cat 2: Order Flow
            "net_volume", "volume_imbalance", "trade_count", "avg_trade_size",
            "large_trade_count", "vwap_distance", "kyle_lambda",
            // Cat 3: Price Dynamics
            "return_1", "return_5", "return_20",
            "volatility_20", "volatility_50", "momentum",
            "high_low_range_20", "high_low_range_50", "close_position",
            // Cat 4: Cross-Scale Dynamics
            "volume_surprise", "duration_surprise", "acceleration", "vol_price_corr",
            // Cat 5: Time Context
            "time_sin", "time_cos", "minutes_since_open", "minutes_to_close",
            "session_volume_frac",
            // Cat 6: Message Microstructure
            "cancel_add_ratio", "message_rate", "modify_fraction",
            "order_flow_toxicity", "cancel_concentration",
        ]
    }
}

/// Computes Track A features incrementally from a sequence of Bars.
pub struct BarFeatureComputer {
    tick_size: f32,
    bar_count: usize,

    // Lookback buffers
    close_mids: VecDeque<f32>,
    high_mids: VecDeque<f32>,
    low_mids: VecDeque<f32>,
    volumes: VecDeque<f32>,
    returns: VecDeque<f32>,
    net_volumes: VecDeque<f32>,
    abs_returns: VecDeque<f32>,

    // EWMA state
    ewma_volume: f32,
    ewma_duration: f32,
    ewma_initialized: bool,

    // Acceleration
    prev_return_1: f32,

    // Session volume tracking
    cumulative_volume: f32,
    prior_day_totals: Vec<f32>,
}

impl BarFeatureComputer {
    pub fn new() -> Self {
        Self::with_tick_size(0.25)
    }

    pub fn with_tick_size(tick_size: f32) -> Self {
        Self {
            tick_size,
            bar_count: 0,
            close_mids: VecDeque::new(),
            high_mids: VecDeque::new(),
            low_mids: VecDeque::new(),
            volumes: VecDeque::new(),
            returns: VecDeque::new(),
            net_volumes: VecDeque::new(),
            abs_returns: VecDeque::new(),
            ewma_volume: 0.0,
            ewma_duration: 0.0,
            ewma_initialized: false,
            prev_return_1: f32::NAN,
            cumulative_volume: 0.0,
            prior_day_totals: Vec::new(),
        }
    }

    /// Process a single bar and return its feature row.
    pub fn update(&mut self, bar: &Bar) -> BarFeatureRow {
        let mut row = BarFeatureRow::default();
        row.timestamp = bar.close_ts;

        // Category 1: Book Shape
        self.compute_book_shape(bar, &mut row);

        // Category 2: Order Flow
        self.compute_order_flow(bar, &mut row);

        // Category 3: Price Dynamics
        self.close_mids.push_back(bar.close_mid);
        self.high_mids.push_back(bar.high_mid);
        self.low_mids.push_back(bar.low_mid);
        self.compute_price_dynamics(&mut row);

        // Category 4: Cross-Scale Dynamics
        self.compute_cross_scale(bar, &mut row);

        // Category 5: Time Context
        self.compute_time_context(bar, &mut row);

        // Category 6: Message Microstructure
        self.compute_message_microstructure(bar, &mut row);

        // Warmup flag
        row.is_warmup = self.bar_count < 50;

        self.bar_count += 1;
        row
    }

    /// Process all bars and fill forward returns (batch mode).
    pub fn compute_all(&mut self, bars: &[Bar]) -> Vec<BarFeatureRow> {
        self.reset();
        let mut rows: Vec<BarFeatureRow> = bars.iter().map(|bar| self.update(bar)).collect();
        self.fixup_rolling_features(bars, &mut rows);
        self.fill_forward_returns(bars, &mut rows);
        rows
    }

    /// Reset state for session boundary.
    pub fn reset(&mut self) {
        self.bar_count = 0;
        self.close_mids.clear();
        self.high_mids.clear();
        self.low_mids.clear();
        self.volumes.clear();
        self.returns.clear();
        self.net_volumes.clear();
        self.abs_returns.clear();
        self.ewma_volume = 0.0;
        self.ewma_duration = 0.0;
        self.ewma_initialized = false;
        self.prev_return_1 = f32::NAN;
        self.cumulative_volume = 0.0;
    }

    /// Report session-end total volume for session_volume_frac.
    pub fn end_session(&mut self, total_volume: f32) {
        self.prior_day_totals.push(total_volume);
        self.reset();
    }

    // === Category 1: Book Shape ===

    fn compute_book_shape(&self, bar: &Bar, row: &mut BarFeatureRow) {
        row.book_imbalance_1 = Self::book_imbalance(bar, 1);
        row.book_imbalance_3 = Self::book_imbalance(bar, 3);
        row.book_imbalance_5 = Self::book_imbalance(bar, 5);
        row.book_imbalance_10 = Self::book_imbalance(bar, 10);
        row.weighted_imbalance = Self::weighted_imbalance(bar);
        row.spread = bar.spread / self.tick_size;

        for i in 0..BOOK_DEPTH {
            row.bid_depth_profile[i] = bar.bids[i][1];
            row.ask_depth_profile[i] = bar.asks[i][1];
        }

        row.depth_concentration_bid = Self::depth_hhi(&bar.bids);
        row.depth_concentration_ask = Self::depth_hhi(&bar.asks);
        row.book_slope_bid = Self::book_slope(&bar.bids);
        row.book_slope_ask = Self::book_slope(&bar.asks);
        row.level_count_bid = Self::level_count(&bar.bids);
        row.level_count_ask = Self::level_count(&bar.asks);
    }

    fn book_imbalance(bar: &Bar, depth: usize) -> f32 {
        let mut bid_vol = 0.0f32;
        let mut ask_vol = 0.0f32;
        for i in 0..depth.min(BOOK_DEPTH) {
            bid_vol += bar.bids[i][1];
            ask_vol += bar.asks[i][1];
        }
        (bid_vol - ask_vol) / (bid_vol + ask_vol + EPS)
    }

    fn weighted_imbalance(bar: &Bar) -> f32 {
        let mut w_bid = 0.0f32;
        let mut w_ask = 0.0f32;
        for i in 0..BOOK_DEPTH {
            let w = 1.0 / (i + 1) as f32;
            w_bid += bar.bids[i][1] * w;
            w_ask += bar.asks[i][1] * w;
        }
        (w_bid - w_ask) / (w_bid + w_ask + EPS)
    }

    fn depth_hhi(levels: &[[f32; 2]; BOOK_DEPTH]) -> f32 {
        let total: f32 = levels.iter().map(|l| l[1]).sum();
        if total < EPS {
            return 0.0;
        }
        levels
            .iter()
            .map(|l| {
                let frac = l[1] / total;
                frac * frac
            })
            .sum()
    }

    fn book_slope(levels: &[[f32; 2]; BOOK_DEPTH]) -> f32 {
        let mut n = 0;
        let mut sum_x = 0.0f32;
        let mut sum_y = 0.0f32;
        let mut sum_xy = 0.0f32;
        let mut sum_xx = 0.0f32;

        for i in 0..BOOK_DEPTH {
            if levels[i][1] <= 0.0 {
                continue;
            }
            let x = i as f32;
            let y = levels[i][1].ln();
            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_xx += x * x;
            n += 1;
        }
        if n < 2 {
            return 0.0;
        }
        let nf = n as f32;
        let denom = nf * sum_xx - sum_x * sum_x;
        if denom.abs() < EPS {
            return 0.0;
        }
        (nf * sum_xy - sum_x * sum_y) / denom
    }

    fn level_count(levels: &[[f32; 2]; BOOK_DEPTH]) -> i32 {
        levels.iter().filter(|l| l[1] > 0.0).count() as i32
    }

    // === Category 2: Order Flow ===

    fn compute_order_flow(&mut self, bar: &Bar, row: &mut BarFeatureRow) {
        row.net_volume = bar.buy_volume - bar.sell_volume;

        let total_vol = bar.volume as f32;
        row.volume_imbalance = if total_vol > EPS {
            row.net_volume / (total_vol + EPS)
        } else {
            0.0
        };

        row.trade_count = bar.trade_event_count;

        row.avg_trade_size = if bar.trade_event_count > 0 {
            total_vol / bar.trade_event_count as f32
        } else {
            0.0
        };

        self.volumes.push_back(total_vol);
        if self.volumes.len() > 20 {
            self.volumes.pop_front();
        }
        row.large_trade_count = self.compute_large_trade_count(bar);

        row.vwap_distance = (bar.close_mid - bar.vwap) / self.tick_size;

        self.net_volumes.push_back(row.net_volume);
        if self.net_volumes.len() > 20 {
            self.net_volumes.pop_front();
        }
        row.kyle_lambda = self.compute_kyle_lambda();
    }

    fn compute_large_trade_count(&self, bar: &Bar) -> u32 {
        if self.volumes.len() < 20 {
            return 0;
        }
        let mut sorted: Vec<f32> = self.volumes.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = (sorted[9] + sorted[10]) / 2.0;
        let threshold = 2.0 * median;
        if bar.volume as f32 > threshold {
            1
        } else {
            0
        }
    }

    fn compute_kyle_lambda(&self) -> f32 {
        if self.close_mids.len() < 21 || self.net_volumes.len() < 20 {
            return f32::NAN;
        }

        let n = 20;
        let cm_start = self.close_mids.len() - n - 1;

        let mut sum_xy = 0.0f32;
        let mut sum_xx = 0.0f32;
        for i in 0..n {
            let delta_mid =
                (self.close_mids[cm_start + i + 1] - self.close_mids[cm_start + i]) / self.tick_size;
            let nv = self.net_volumes[self.net_volumes.len() - n + i];
            sum_xy += nv * delta_mid;
            sum_xx += nv * nv;
        }
        if sum_xx < EPS {
            0.0
        } else {
            sum_xy / sum_xx
        }
    }

    // === Category 3: Price Dynamics ===

    fn compute_price_dynamics(&mut self, row: &mut BarFeatureRow) {
        let n = self.close_mids.len();

        row.return_1 = if n >= 2 {
            (self.close_mids[n - 1] - self.close_mids[n - 2]) / self.tick_size
        } else {
            f32::NAN
        };

        row.return_5 = if n >= 6 {
            (self.close_mids[n - 1] - self.close_mids[n - 6]) / self.tick_size
        } else {
            f32::NAN
        };

        row.return_20 = if n >= 21 {
            (self.close_mids[n - 1] - self.close_mids[n - 21]) / self.tick_size
        } else {
            f32::NAN
        };

        if n >= 2 {
            let r1 = (self.close_mids[n - 1] - self.close_mids[n - 2]) / self.tick_size;
            self.returns.push_back(r1);
            self.abs_returns.push_back(r1.abs());
        }

        row.volatility_20 = if self.returns.len() >= 20 {
            Self::rolling_std(&self.returns, 20)
        } else {
            f32::NAN
        };

        row.volatility_50 = if self.returns.len() >= 50 {
            Self::rolling_std(&self.returns, 50)
        } else {
            f32::NAN
        };

        row.momentum = if self.returns.len() >= 20 {
            self.returns.iter().rev().take(20).sum()
        } else if !self.returns.is_empty() {
            self.returns.iter().sum()
        } else {
            0.0
        };

        row.high_low_range_20 = if n > 20 {
            self.high_low_range(20)
        } else {
            f32::NAN
        };

        row.high_low_range_50 = if n > 50 {
            self.high_low_range(50)
        } else {
            f32::NAN
        };

        row.close_position = if n > 20 {
            let max_high = self.high_mids.iter().rev().take(20).copied().fold(f32::NEG_INFINITY, f32::max);
            let min_low = self.low_mids.iter().rev().take(20).copied().fold(f32::INFINITY, f32::min);
            let range = max_high - min_low + EPS;
            (*self.close_mids.back().unwrap() - min_low) / range
        } else if n >= 2 {
            let max_high = self.high_mids.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let min_low = self.low_mids.iter().copied().fold(f32::INFINITY, f32::min);
            let range = max_high - min_low + EPS;
            (*self.close_mids.back().unwrap() - min_low) / range
        } else {
            0.5
        };
    }

    fn rolling_std(data: &VecDeque<f32>, window: usize) -> f32 {
        if data.len() < window {
            return f32::NAN;
        }
        let mut sum = 0.0f32;
        let mut sum_sq = 0.0f32;
        for &v in data.iter().rev().take(window) {
            sum += v;
            sum_sq += v * v;
        }
        let n = window as f32;
        let mean = sum / n;
        let var = (sum_sq / n - mean * mean).max(0.0);
        var.sqrt()
    }

    fn high_low_range(&self, window: usize) -> f32 {
        let max_h = self.high_mids.iter().rev().take(window).copied().fold(f32::NEG_INFINITY, f32::max);
        let min_l = self.low_mids.iter().rev().take(window).copied().fold(f32::INFINITY, f32::min);
        (max_h - min_l) / self.tick_size
    }

    // === Category 4: Cross-Scale Dynamics ===

    fn compute_cross_scale(&mut self, bar: &Bar, row: &mut BarFeatureRow) {
        let vol = bar.volume as f32;
        let dur = bar.bar_duration_s;

        if !self.ewma_initialized {
            self.ewma_volume = vol;
            self.ewma_duration = dur;
            self.ewma_initialized = true;
        } else {
            self.ewma_volume = EWMA_ALPHA * vol + (1.0 - EWMA_ALPHA) * self.ewma_volume;
            self.ewma_duration = EWMA_ALPHA * dur + (1.0 - EWMA_ALPHA) * self.ewma_duration;
        }

        row.volume_surprise = if self.ewma_volume > EPS {
            vol / self.ewma_volume
        } else {
            1.0
        };
        row.duration_surprise = if self.ewma_duration > EPS {
            dur / self.ewma_duration
        } else {
            1.0
        };

        let curr_return_1 = row.return_1;
        if !curr_return_1.is_nan() && !self.prev_return_1.is_nan() {
            row.acceleration = curr_return_1 - self.prev_return_1;
        } else {
            row.acceleration = 0.0;
        }
        self.prev_return_1 = curr_return_1;

        row.vol_price_corr = self.compute_vol_price_corr();
    }

    fn compute_vol_price_corr(&self) -> f32 {
        if self.volumes.len() < 20 || self.abs_returns.len() < 20 {
            return f32::NAN;
        }
        let vols: Vec<f32> = self.volumes.iter().rev().take(20).copied().collect();
        let rets: Vec<f32> = self.abs_returns.iter().rev().take(20).copied().collect();
        Self::pearson_corr(&vols, &rets)
    }

    // === Category 5: Time Context ===

    fn compute_time_context(&mut self, bar: &Bar, row: &mut BarFeatureRow) {
        let tod = bar.time_of_day;
        let frac = tod / 24.0;
        row.time_sin = (TWO_PI * frac).sin();
        row.time_cos = (TWO_PI * frac).cos();

        row.minutes_since_open = ((tod - RTH_OPEN_HOUR) * 60.0).max(0.0);
        row.minutes_to_close = ((RTH_CLOSE_HOUR - tod) * 60.0).max(0.0);

        self.cumulative_volume += bar.volume as f32;
        if !self.prior_day_totals.is_empty() {
            let avg: f32 =
                self.prior_day_totals.iter().sum::<f32>() / self.prior_day_totals.len() as f32;
            row.session_volume_frac = if avg > EPS {
                self.cumulative_volume / avg
            } else {
                0.0
            };
        } else {
            row.session_volume_frac = 0.0;
        }
    }

    // === Category 6: Message Microstructure ===

    fn compute_message_microstructure(&self, bar: &Bar, row: &mut BarFeatureRow) {
        row.cancel_add_ratio = bar.cancel_count as f32 / (bar.add_count as f32 + EPS);

        let total_msgs = (bar.add_count + bar.cancel_count + bar.modify_count) as f32;
        row.message_rate = if bar.bar_duration_s > EPS {
            total_msgs / bar.bar_duration_s
        } else {
            0.0
        };

        row.modify_fraction = if total_msgs > EPS {
            bar.modify_count as f32 / (total_msgs + EPS)
        } else {
            0.0
        };

        if bar.trade_event_count > 0 {
            let mid_move = (bar.close_mid - bar.open_mid).abs();
            let max_possible = self.tick_size * bar.trade_event_count as f32;
            row.order_flow_toxicity = (mid_move / (max_possible + EPS)).min(1.0);
        } else {
            row.order_flow_toxicity = 0.0;
        }

        row.cancel_concentration = (bar.cancel_count as f32 / (total_msgs + EPS)).min(1.0);
    }

    // === Helpers ===

    fn pearson_corr(xs: &[f32], ys: &[f32]) -> f32 {
        let n = xs.len();
        let mut sx = 0.0f32;
        let mut sy = 0.0f32;
        let mut sxy = 0.0f32;
        let mut sxx = 0.0f32;
        let mut syy = 0.0f32;
        for i in 0..n {
            sx += xs[i];
            sy += ys[i];
            sxy += xs[i] * ys[i];
            sxx += xs[i] * xs[i];
            syy += ys[i] * ys[i];
        }
        let nf = n as f32;
        let cov = nf * sxy - sx * sy;
        let vx = nf * sxx - sx * sx;
        let vy = nf * syy - sy * sy;
        let denom = (vx * vy).sqrt();
        if denom < EPS {
            return 0.0;
        }
        (cov / denom).clamp(-1.0, 1.0)
    }

    fn std_of_slice(data: &[f32], start: usize, count: usize) -> f32 {
        let mut sum = 0.0f32;
        let mut sum_sq = 0.0f32;
        for j in start..start + count {
            sum += data[j];
            sum_sq += data[j] * data[j];
        }
        let nf = count as f32;
        let mean = sum / nf;
        let var = (sum_sq / nf - mean * mean).max(0.0);
        var.sqrt()
    }

    fn high_low_range_of(bars: &[Bar], end: usize, count: usize, tick_size: f32) -> f32 {
        let mut max_h = bars[end].high_mid;
        let mut min_l = bars[end].low_mid;
        for j in (end + 1 - count)..=end {
            max_h = max_h.max(bars[j].high_mid);
            min_l = min_l.min(bars[j].low_mid);
        }
        (max_h - min_l) / tick_size
    }

    fn fixup_rolling_features(&self, bars: &[Bar], rows: &mut [BarFeatureRow]) {
        let n = bars.len();

        let mut all_returns: Vec<f32> = Vec::with_capacity(n);
        for i in 1..n {
            all_returns.push((bars[i].close_mid - bars[i - 1].close_mid) / self.tick_size);
        }

        for i in 0..n {
            if rows[i].volatility_20.is_nan() && i >= 2 {
                let count = i.min(20);
                rows[i].volatility_20 = Self::std_of_slice(&all_returns, i - count, count);
            }

            if rows[i].volatility_50.is_nan() && i >= 2 {
                let count = i.min(50);
                rows[i].volatility_50 = Self::std_of_slice(&all_returns, i - count, count);
            }

            if rows[i].kyle_lambda.is_nan() && i >= 2 {
                let count = i.min(20);
                let mut sum_xy = 0.0f32;
                let mut sum_xx = 0.0f32;
                for j in (i - count)..i {
                    let delta_mid = all_returns[j];
                    let nv = bars[j + 1].buy_volume - bars[j + 1].sell_volume;
                    sum_xy += nv * delta_mid;
                    sum_xx += nv * nv;
                }
                rows[i].kyle_lambda = if sum_xx > EPS { sum_xy / sum_xx } else { 0.0 };
            }

            if rows[i].vol_price_corr.is_nan() && i >= 2 {
                let count = i.min(20);
                let mut vols = vec![0.0f32; count];
                let mut abs_rets = vec![0.0f32; count];
                for j in 0..count {
                    vols[j] = bars[i - count + j + 1].volume as f32;
                    abs_rets[j] = all_returns[i - count + j].abs();
                }
                rows[i].vol_price_corr = Self::pearson_corr(&vols, &abs_rets);
            }

            if rows[i].high_low_range_20.is_nan() && i >= 1 {
                let count = (i + 1).min(20);
                rows[i].high_low_range_20 = Self::high_low_range_of(bars, i, count, self.tick_size);
            }

            if rows[i].high_low_range_50.is_nan() && i >= 1 {
                let count = (i + 1).min(50);
                rows[i].high_low_range_50 = Self::high_low_range_of(bars, i, count, self.tick_size);
            }
        }
    }

    fn fwd_return(&self, bars: &[Bar], i: usize, horizon: usize) -> f32 {
        if i + horizon < bars.len() {
            (bars[i + horizon].close_mid - bars[i].close_mid) / self.tick_size
        } else {
            f32::NAN
        }
    }

    fn fill_forward_returns(&self, bars: &[Bar], rows: &mut [BarFeatureRow]) {
        for i in 0..bars.len() {
            rows[i].fwd_return_1 = self.fwd_return(bars, i, 1);
            rows[i].fwd_return_5 = self.fwd_return(bars, i, 5);
            rows[i].fwd_return_20 = self.fwd_return(bars, i, 20);
            rows[i].fwd_return_100 = self.fwd_return(bars, i, 100);
        }
    }
}

impl Default for BarFeatureComputer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bar(close_ts: u64, close_mid: f32, volume: u32) -> Bar {
        Bar {
            close_ts,
            close_mid,
            open_mid: close_mid,
            high_mid: close_mid + 0.25,
            low_mid: close_mid - 0.25,
            volume,
            buy_volume: volume as f32 * 0.6,
            sell_volume: volume as f32 * 0.4,
            vwap: close_mid,
            bar_duration_s: 5.0,
            time_of_day: 10.0,
            trade_event_count: volume.min(5),
            add_count: 100,
            cancel_count: 30,
            modify_count: 10,
            spread: 0.25,
            bids: {
                let mut bids = [[0.0f32; 2]; BOOK_DEPTH];
                for i in 0..BOOK_DEPTH {
                    bids[i] = [close_mid - 0.25 * (i + 1) as f32, 10.0];
                }
                bids
            },
            asks: {
                let mut asks = [[0.0f32; 2]; BOOK_DEPTH];
                for i in 0..BOOK_DEPTH {
                    asks[i] = [close_mid + 0.25 * (i + 1) as f32, 10.0];
                }
                asks
            },
            ..Default::default()
        }
    }

    #[test]
    fn test_feature_count() {
        assert_eq!(BarFeatureRow::feature_count(), 62);
        assert_eq!(BarFeatureRow::feature_names().len(), 62);
    }

    #[test]
    fn test_single_bar_basic() {
        let mut computer = BarFeatureComputer::new();
        let bar = make_bar(1_000_000_000, 4500.0, 100);
        let row = computer.update(&bar);

        assert!((row.spread - 1.0).abs() < 1e-6); // 0.25 / 0.25 = 1 tick
        assert!(row.is_warmup); // first bar is warmup
    }

    #[test]
    fn test_book_imbalance_equal() {
        let mut bar = make_bar(1_000_000_000, 4500.0, 100);
        // Equal bid/ask sizes → imbalance ≈ 0
        for i in 0..BOOK_DEPTH {
            bar.bids[i][1] = 10.0;
            bar.asks[i][1] = 10.0;
        }
        let mut computer = BarFeatureComputer::new();
        let row = computer.update(&bar);
        assert!(row.book_imbalance_1.abs() < 0.01);
        assert!(row.book_imbalance_10.abs() < 0.01);
    }

    #[test]
    fn test_warmup_clears_at_50() {
        let mut computer = BarFeatureComputer::new();
        for i in 0..51 {
            let bar = make_bar(i as u64 * 5_000_000_000, 4500.0 + i as f32 * 0.25, 100);
            let row = computer.update(&bar);
            if i < 50 {
                assert!(row.is_warmup, "bar {} should be warmup", i);
            } else {
                assert!(!row.is_warmup, "bar {} should not be warmup", i);
            }
        }
    }

    #[test]
    fn test_compute_all_forward_returns() {
        let bars: Vec<Bar> = (0..10)
            .map(|i| make_bar(i as u64 * 5_000_000_000, 4500.0 + i as f32 * 0.25, 100))
            .collect();

        let mut computer = BarFeatureComputer::new();
        let rows = computer.compute_all(&bars);

        assert_eq!(rows.len(), 10);
        // fwd_return_1 for bar 0: (4500.25 - 4500.0) / 0.25 = 1.0
        assert!((rows[0].fwd_return_1 - 1.0).abs() < 1e-4);
        // fwd_return_5 for bar 0: (4501.25 - 4500.0) / 0.25 = 5.0
        assert!((rows[0].fwd_return_5 - 5.0).abs() < 1e-4);
        // Last bar should have NaN fwd_return_1
        assert!(rows[9].fwd_return_1.is_nan());
    }

    #[test]
    fn test_depth_hhi_uniform() {
        let mut levels = [[0.0f32; 2]; BOOK_DEPTH];
        for i in 0..BOOK_DEPTH {
            levels[i][1] = 10.0; // uniform
        }
        let hhi = BarFeatureComputer::depth_hhi(&levels);
        // Uniform: HHI = 10 * (1/10)^2 = 0.1
        assert!((hhi - 0.1).abs() < 1e-6);
    }

    #[test]
    fn test_depth_hhi_concentrated() {
        let mut levels = [[0.0f32; 2]; BOOK_DEPTH];
        levels[0][1] = 100.0; // all depth at level 0
        let hhi = BarFeatureComputer::depth_hhi(&levels);
        assert!((hhi - 1.0).abs() < 1e-6);
    }
}
