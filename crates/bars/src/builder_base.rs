use common::bar::Bar;
use common::book::{BookSnapshot, BOOK_DEPTH, TRADE_BUF_LEN};
use common::time_utils;

/// Trade info extracted from the most recent trade slot in a snapshot.
#[derive(Debug, Clone, Copy)]
pub struct TradeInfo {
    pub price: f32,
    pub size: f32,
    pub aggressor: f32,
    pub has_trade: bool,
}

/// Extract trade info from the most recent trade slot in a snapshot.
pub fn extract_trade(snap: &BookSnapshot) -> TradeInfo {
    let price = snap.trades[TRADE_BUF_LEN - 1][0];
    let size = snap.trades[TRADE_BUF_LEN - 1][1];
    let agg = snap.trades[TRADE_BUF_LEN - 1][2];
    TradeInfo {
        price,
        size,
        aggressor: agg,
        has_trade: size > 0.0,
    }
}

/// Common accumulation state for all bar builder types.
pub(crate) struct BarAccumulator {
    pub active: bool,

    pub open_ts: u64,
    pub close_ts: u64,
    pub open_mid: f32,
    pub close_mid: f32,
    pub high_mid: f32,
    pub low_mid: f32,
    pub cumulative_volume: u32,
    pub tick_count: u32,
    pub buy_volume: f32,
    pub sell_volume: f32,
    pub vwap_num: f64,
    pub vwap_den: f64,

    pub max_spread: f32,
    pub min_spread: f32,
    pub snapshot_count: u32,

    pub mbo_event_begin: u32,
    pub mbo_event_end: u32,

    pub add_count: u32,
    pub cancel_count: u32,
    pub modify_count: u32,
    pub trade_event_count: u32,

    pub last_snap: BookSnapshot,
}

impl Default for BarAccumulator {
    fn default() -> Self {
        Self {
            active: false,
            open_ts: 0,
            close_ts: 0,
            open_mid: 0.0,
            close_mid: 0.0,
            high_mid: 0.0,
            low_mid: 0.0,
            cumulative_volume: 0,
            tick_count: 0,
            buy_volume: 0.0,
            sell_volume: 0.0,
            vwap_num: 0.0,
            vwap_den: 0.0,
            max_spread: 0.0,
            min_spread: f32::MAX,
            snapshot_count: 0,
            mbo_event_begin: 0,
            mbo_event_end: 0,
            add_count: 0,
            cancel_count: 0,
            modify_count: 0,
            trade_event_count: 0,
            last_snap: BookSnapshot::default(),
        }
    }
}

impl BarAccumulator {
    /// Start a new bar at the given snapshot.
    pub fn start_bar_at(&mut self, snap: &BookSnapshot) {
        self.active = true;
        self.open_ts = snap.timestamp;
        self.open_mid = snap.mid_price;
        self.reset_accumulators(snap.mid_price);
    }

    /// Start a new bar contiguous with the previous bar's close.
    pub fn start_bar_contiguous(&mut self, mid_price: f32) {
        self.active = true;
        self.open_ts = self.close_ts;
        self.open_mid = mid_price;
        self.reset_accumulators(mid_price);
    }

    /// Update the bar with a new snapshot and trade info.
    pub fn update_bar(&mut self, snap: &BookSnapshot, trade: &TradeInfo) {
        self.close_ts = snap.timestamp;
        self.close_mid = snap.mid_price;
        self.high_mid = self.high_mid.max(snap.mid_price);
        self.low_mid = self.low_mid.min(snap.mid_price);

        self.max_spread = self.max_spread.max(snap.spread);
        self.min_spread = self.min_spread.min(snap.spread);
        self.snapshot_count += 1;

        if trade.has_trade {
            let size = trade.size as u32;
            self.cumulative_volume += size;
            self.tick_count += 1;

            if trade.aggressor > 0.0 {
                self.buy_volume += trade.size;
            } else {
                self.sell_volume += trade.size;
            }

            self.vwap_num += trade.price as f64 * trade.size as f64;
            self.vwap_den += trade.size as f64;

            self.trade_event_count += 1;
        }

        self.add_count += 1;
        self.mbo_event_end += 1;
        self.last_snap = snap.clone();
    }

    /// Finalize the current bar and return it.
    pub fn finalize_bar(&mut self) -> Option<Bar> {
        if !self.active {
            return None;
        }

        let mut bar = Bar::default();
        bar.open_ts = self.open_ts;
        bar.close_ts = self.close_ts;
        bar.open_mid = self.open_mid;
        bar.close_mid = self.close_mid;
        bar.high_mid = self.high_mid;
        bar.low_mid = self.low_mid;
        bar.volume = self.cumulative_volume;
        bar.tick_count = self.tick_count;
        bar.buy_volume = self.buy_volume;
        bar.sell_volume = self.sell_volume;

        if self.vwap_den > 0.0 {
            bar.vwap = (self.vwap_num / self.vwap_den) as f32;
        }

        bar.bar_duration_s =
            (bar.close_ts - bar.open_ts) as f32 / time_utils::NS_PER_SEC as f32;
        bar.time_of_day = time_utils::compute_time_of_day(bar.close_ts);

        for i in 0..BOOK_DEPTH {
            bar.bids[i][0] = self.last_snap.bids[i][0];
            bar.bids[i][1] = self.last_snap.bids[i][1];
            bar.asks[i][0] = self.last_snap.asks[i][0];
            bar.asks[i][1] = self.last_snap.asks[i][1];
        }
        bar.spread = self.last_snap.spread;

        bar.max_spread = self.max_spread;
        bar.min_spread = if self.min_spread == f32::MAX {
            0.0
        } else {
            self.min_spread
        };
        bar.snapshot_count = self.snapshot_count;

        bar.mbo_event_begin = self.mbo_event_begin;
        bar.mbo_event_end = self.mbo_event_end;

        bar.add_count = self.add_count;
        bar.cancel_count = self.cancel_count;
        bar.modify_count = self.modify_count;
        bar.trade_event_count = self.trade_event_count;
        bar.cancel_add_ratio =
            self.cancel_count as f32 / (self.add_count as f32 + 1e-8);
        if bar.bar_duration_s > 0.0 {
            let total_msgs = (bar.add_count + bar.cancel_count + bar.modify_count + bar.trade_event_count) as f32;
            bar.message_rate = total_msgs / bar.bar_duration_s;
        }

        self.active = false;
        Some(bar)
    }

    /// Flush: finalize if active.
    pub fn flush(&mut self) -> Option<Bar> {
        if !self.active {
            return None;
        }
        self.finalize_bar()
    }

    fn reset_accumulators(&mut self, mid_price: f32) {
        self.high_mid = mid_price;
        self.low_mid = mid_price;
        self.cumulative_volume = 0;
        self.tick_count = 0;
        self.buy_volume = 0.0;
        self.sell_volume = 0.0;
        self.vwap_num = 0.0;
        self.vwap_den = 0.0;
        self.max_spread = 0.0;
        self.min_spread = f32::MAX;
        self.snapshot_count = 0;
        self.mbo_event_begin = self.mbo_event_end;
        self.add_count = 0;
        self.cancel_count = 0;
        self.modify_count = 0;
        self.trade_event_count = 0;
    }
}
