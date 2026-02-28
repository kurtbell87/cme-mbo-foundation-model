use common::bar::Bar;
use common::book::BookSnapshot;

use crate::builder_base::{extract_trade, BarAccumulator};
use crate::BarBuilder;

/// Tick-based bar builder.
///
/// Emits a bar every `threshold` trade events.
/// Uses `snap.trade_count` for accurate multi-trade snapshots,
/// with fallback to legacy behavior (1 if trade buffer has data).
pub struct TickBarBuilder {
    threshold: u32,
    carry: u32,
    acc: BarAccumulator,
}

impl TickBarBuilder {
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold,
            carry: 0,
            acc: BarAccumulator::default(),
        }
    }
}

impl BarBuilder for TickBarBuilder {
    fn on_snapshot(&mut self, snap: &BookSnapshot) -> Option<Bar> {
        let trade = extract_trade(snap);

        // Effective trade count: use trade_count if set, else fall back to
        // legacy behavior (1 if trade buffer has data, 0 otherwise)
        let tc = if snap.trade_count > 0 {
            snap.trade_count
        } else if trade.has_trade {
            1
        } else {
            0
        };

        if !self.acc.active {
            // Don't start a bar unless there are trades (or carry-over)
            if tc == 0 && self.carry == 0 {
                return None;
            }
            self.acc.start_bar_at(snap);
            self.acc.tick_count = self.carry;
            self.carry = 0;
        }

        self.acc.update_bar(snap, &trade);

        // Undo base class tick_count increment (+1 per trade snapshot)
        // and replace with actual trade count
        if trade.has_trade {
            self.acc.tick_count -= 1;
        }
        self.acc.tick_count += tc;

        if self.acc.tick_count >= self.threshold {
            self.carry = self.acc.tick_count - self.threshold;
            self.acc.tick_count = self.threshold;
            let bar = self.acc.finalize_bar();

            // If carry-over exists, start next bar immediately so flush() works
            if self.carry > 0 {
                self.acc.start_bar_at(snap);
                self.acc.tick_count = self.carry;
                self.carry = 0;
            }

            return bar;
        }

        None
    }

    fn flush(&mut self) -> Option<Bar> {
        if !self.acc.active {
            return None;
        }
        // Only emit partial if there are accumulated trades
        if self.acc.tick_count == 0 {
            self.acc.active = false;
            return None;
        }
        self.acc.finalize_bar()
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

    fn make_snap_with_trade(ts: u64, mid: f32, trade_count: u32) -> BookSnapshot {
        let mut snap = make_snap(ts, mid);
        snap.trades[TRADE_BUF_LEN - 1] = [mid, 1.0, 1.0]; // price, size, aggressor
        snap.trade_count = trade_count;
        snap
    }

    #[test]
    fn test_no_bar_without_trades() {
        let mut builder = TickBarBuilder::new(10);
        let result = builder.on_snapshot(&make_snap(1_000_000_000, 4500.0));
        assert!(result.is_none());
        assert!(!builder.acc.active); // should not have started
    }

    #[test]
    fn test_bar_emits_at_threshold() {
        let mut builder = TickBarBuilder::new(5);

        for i in 0..4 {
            let snap = make_snap_with_trade(1_000_000_000 + i * 100_000_000, 4500.0, 1);
            let result = builder.on_snapshot(&snap);
            assert!(result.is_none());
        }

        let snap = make_snap_with_trade(1_400_000_000, 4501.0, 1);
        let result = builder.on_snapshot(&snap);
        assert!(result.is_some());
        let bar = result.unwrap();
        assert_eq!(bar.tick_count, 5);
    }

    #[test]
    fn test_multi_trade_snapshot() {
        let mut builder = TickBarBuilder::new(5);
        // Snapshot with 5 trades at once
        let snap = make_snap_with_trade(1_000_000_000, 4500.0, 5);
        let result = builder.on_snapshot(&snap);
        assert!(result.is_some());
    }

    #[test]
    fn test_carry_over() {
        let mut builder = TickBarBuilder::new(3);
        // Snapshot with 5 trades (threshold 3 → carry 2)
        let snap = make_snap_with_trade(1_000_000_000, 4500.0, 5);
        let result = builder.on_snapshot(&snap);
        assert!(result.is_some());
        // Should have started a new bar with carry=2
        assert!(builder.acc.active);
        assert_eq!(builder.acc.tick_count, 2);
    }

    #[test]
    fn test_flush_empty_bar() {
        let mut builder = TickBarBuilder::new(10);
        assert!(builder.flush().is_none());
    }
}
