use common::bar::Bar;
use common::book::{BookSnapshot, SNAPSHOT_INTERVAL_NS};
use common::time_utils;

use crate::builder_base::{extract_trade, BarAccumulator};
use crate::BarBuilder;

/// Time-based bar builder.
///
/// Emits a bar every `interval_seconds` seconds (e.g., 5 for 5-second bars).
/// Uses snapshot count to determine bar boundaries (interval_ns / SNAPSHOT_INTERVAL_NS).
///
/// Bars are contiguous: the previous bar's close_mid seeds the next bar's high/low
/// accumulator (matching C++ bar_builder_base contiguous initialization), so the
/// carry-over open price is included in the new bar's high/low tracking.
pub struct TimeBarBuilder {
    #[allow(dead_code)]
    interval_ns: u64,
    snaps_per_bar: u64,
    acc: BarAccumulator,
    /// Previous bar's close_mid, used to seed next bar's high/low (C++ contiguous init).
    prev_close_mid: Option<f32>,
}

impl TimeBarBuilder {
    pub fn new(interval_seconds: u64) -> Self {
        let interval_ns = interval_seconds * time_utils::NS_PER_SEC;
        let snaps_per_bar = interval_ns / SNAPSHOT_INTERVAL_NS;
        Self {
            interval_ns,
            snaps_per_bar,
            acc: BarAccumulator::default(),
            prev_close_mid: None,
        }
    }
}

impl BarBuilder for TimeBarBuilder {
    fn on_snapshot(&mut self, snap: &BookSnapshot) -> Option<Bar> {
        let trade = extract_trade(snap);

        if !self.acc.active {
            self.acc.start_bar_at(snap);
            // Contiguous bar initialization: seed high/low from previous bar's close_mid.
            // This ensures the carry-over open price is included in the bar's range,
            // matching C++ bar_builder_base behavior.
            if let Some(prev_close) = self.prev_close_mid {
                self.acc.high_mid = self.acc.high_mid.max(prev_close);
                self.acc.low_mid = self.acc.low_mid.min(prev_close);
            }
        }

        self.acc.update_bar(snap, &trade);

        if self.acc.snapshot_count as u64 >= self.snaps_per_bar {
            if let Some(mut bar) = self.acc.finalize_bar() {
                self.prev_close_mid = Some(bar.close_mid);
                bar.bar_duration_s =
                    self.interval_ns as f32 / time_utils::NS_PER_SEC as f32;
                return Some(bar);
            }
        }

        None
    }

    fn flush(&mut self) -> Option<Bar> {
        self.acc.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::book::TRADE_BUF_LEN;

    fn make_snap(ts: u64, mid: f32) -> BookSnapshot {
        let mut snap = BookSnapshot::default();
        snap.timestamp = ts;
        snap.mid_price = mid;
        snap.spread = 0.25;
        snap
    }

    fn make_snap_with_trade(ts: u64, mid: f32, price: f32, size: f32, agg: f32) -> BookSnapshot {
        let mut snap = make_snap(ts, mid);
        snap.trades[TRADE_BUF_LEN - 1] = [price, size, agg];
        snap
    }

    #[test]
    fn test_5s_bar_emits_after_50_snapshots() {
        let mut builder = TimeBarBuilder::new(5);
        let base_ts = 1_000_000_000u64;

        for i in 0..49 {
            let result = builder.on_snapshot(&make_snap(base_ts + i * SNAPSHOT_INTERVAL_NS, 4500.0));
            assert!(result.is_none(), "should not emit at snapshot {}", i);
        }

        // 50th snapshot should emit
        let result = builder.on_snapshot(&make_snap(base_ts + 49 * SNAPSHOT_INTERVAL_NS, 4501.0));
        assert!(result.is_some());

        let bar = result.unwrap();
        assert_eq!(bar.snapshot_count, 50);
        assert!((bar.open_mid - 4500.0).abs() < 1e-6);
        assert!((bar.close_mid - 4501.0).abs() < 1e-6);
    }

    #[test]
    fn test_contiguous_bars() {
        let mut builder = TimeBarBuilder::new(5);
        let base_ts = 1_000_000_000u64;

        // First bar
        for i in 0..50 {
            builder.on_snapshot(&make_snap(base_ts + i * SNAPSHOT_INTERVAL_NS, 4500.0));
        }

        // Second bar should start immediately
        for i in 50..99 {
            let result = builder.on_snapshot(&make_snap(base_ts + i * SNAPSHOT_INTERVAL_NS, 4502.0));
            assert!(result.is_none());
        }
        let result = builder.on_snapshot(&make_snap(base_ts + 99 * SNAPSHOT_INTERVAL_NS, 4502.0));
        assert!(result.is_some());
    }

    #[test]
    fn test_flush_partial() {
        let mut builder = TimeBarBuilder::new(5);
        builder.on_snapshot(&make_snap(1_000_000_000, 4500.0));
        builder.on_snapshot(&make_snap(1_100_000_000, 4501.0));

        let bar = builder.flush();
        assert!(bar.is_some());
        assert_eq!(bar.unwrap().snapshot_count, 2);
    }

    #[test]
    fn test_vwap_with_trades() {
        let mut builder = TimeBarBuilder::new(5);
        let base_ts = 1_000_000_000u64;

        // Emit 50 snapshots, first with a trade → this emits a full bar
        let snap = make_snap_with_trade(base_ts, 4500.0, 4500.0, 10.0, 1.0);
        let result = builder.on_snapshot(&snap);
        assert!(result.is_none()); // not yet 50 snapshots

        for i in 1..49 {
            builder.on_snapshot(&make_snap(base_ts + i * SNAPSHOT_INTERVAL_NS, 4500.0));
        }

        // 50th snapshot → triggers bar emission
        let bar = builder
            .on_snapshot(&make_snap(base_ts + 49 * SNAPSHOT_INTERVAL_NS, 4500.0))
            .unwrap();
        assert_eq!(bar.volume, 10);
        assert!((bar.vwap - 4500.0).abs() < 1e-6);
        assert!((bar.buy_volume - 10.0).abs() < 1e-6);
    }
}
