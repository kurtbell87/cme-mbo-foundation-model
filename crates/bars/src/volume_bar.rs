use common::bar::Bar;
use common::book::BookSnapshot;

use crate::builder_base::{extract_trade, BarAccumulator};
use crate::BarBuilder;

/// Volume-based bar builder.
///
/// Emits a bar every `threshold` contracts of cumulative volume.
pub struct VolumeBarBuilder {
    threshold: u32,
    ever_emitted: bool,
    acc: BarAccumulator,
}

impl VolumeBarBuilder {
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold,
            ever_emitted: false,
            acc: BarAccumulator::default(),
        }
    }

    fn start_bar_at(&mut self, snap: &BookSnapshot) {
        if self.ever_emitted {
            self.acc.start_bar_contiguous(snap.mid_price);
        } else {
            self.acc.start_bar_at(snap);
        }
    }
}

impl BarBuilder for VolumeBarBuilder {
    fn on_snapshot(&mut self, snap: &BookSnapshot) -> Option<Bar> {
        let trade = extract_trade(snap);

        if !self.acc.active {
            if !trade.has_trade {
                return None;
            }
            self.start_bar_at(snap);
        }

        self.acc.update_bar(snap, &trade);

        if self.acc.cumulative_volume >= self.threshold {
            let bar = self.acc.finalize_bar();
            self.ever_emitted = true;
            return bar;
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
        snap
    }

    fn make_snap_with_trade(ts: u64, mid: f32, size: f32) -> BookSnapshot {
        let mut snap = make_snap(ts, mid);
        snap.trades[TRADE_BUF_LEN - 1] = [mid, size, 1.0];
        snap
    }

    #[test]
    fn test_no_bar_without_trades() {
        let mut builder = VolumeBarBuilder::new(100);
        let result = builder.on_snapshot(&make_snap(1_000_000_000, 4500.0));
        assert!(result.is_none());
    }

    #[test]
    fn test_bar_emits_at_threshold() {
        let mut builder = VolumeBarBuilder::new(10);

        // 5 contracts
        let result = builder.on_snapshot(&make_snap_with_trade(1_000_000_000, 4500.0, 5.0));
        assert!(result.is_none());

        // 5 more = 10 total
        let result = builder.on_snapshot(&make_snap_with_trade(1_100_000_000, 4500.0, 5.0));
        assert!(result.is_some());
        assert_eq!(result.unwrap().volume, 10);
    }

    #[test]
    fn test_flush_partial() {
        let mut builder = VolumeBarBuilder::new(100);
        builder.on_snapshot(&make_snap_with_trade(1_000_000_000, 4500.0, 5.0));
        let bar = builder.flush();
        assert!(bar.is_some());
        assert_eq!(bar.unwrap().volume, 5);
    }

    #[test]
    fn test_contiguous_after_first() {
        let mut builder = VolumeBarBuilder::new(5);
        // First bar
        let result = builder.on_snapshot(&make_snap_with_trade(1_000_000_000, 4500.0, 5.0));
        assert!(result.is_some());
        assert!(builder.ever_emitted);
    }
}
