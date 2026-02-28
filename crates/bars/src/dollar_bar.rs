use common::bar::Bar;
use common::book::BookSnapshot;

use crate::builder_base::{extract_trade, BarAccumulator};
use crate::BarBuilder;

/// Dollar-value bar builder.
///
/// Emits a bar every `threshold` dollars of cumulative notional volume
/// (price × size × multiplier).
pub struct DollarBarBuilder {
    threshold: f64,
    multiplier: f32,
    cumulative_dollar_volume: f64,
    acc: BarAccumulator,
}

impl DollarBarBuilder {
    pub fn new(threshold: f64, multiplier: f32) -> Self {
        Self {
            threshold,
            multiplier,
            cumulative_dollar_volume: 0.0,
            acc: BarAccumulator::default(),
        }
    }
}

impl BarBuilder for DollarBarBuilder {
    fn on_snapshot(&mut self, snap: &BookSnapshot) -> Option<Bar> {
        let trade = extract_trade(snap);

        if !self.acc.active {
            if !trade.has_trade {
                return None;
            }
            self.acc.start_bar_at(snap);
        }

        self.acc.update_bar(snap, &trade);

        if trade.has_trade {
            self.cumulative_dollar_volume +=
                trade.price as f64 * trade.size as f64 * self.multiplier as f64;
        }

        if self.cumulative_dollar_volume >= self.threshold {
            self.cumulative_dollar_volume = 0.0;
            return self.acc.finalize_bar();
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

    fn make_snap_with_trade(ts: u64, mid: f32, price: f32, size: f32) -> BookSnapshot {
        let mut snap = BookSnapshot::default();
        snap.timestamp = ts;
        snap.mid_price = mid;
        snap.trades[TRADE_BUF_LEN - 1] = [price, size, 1.0];
        snap
    }

    #[test]
    fn test_dollar_bar_threshold() {
        // threshold = $100,000, multiplier = 5.0
        // price=4500.0, size=5 → notional = 4500 * 5 * 5 = $112,500 → exceeds
        let mut builder = DollarBarBuilder::new(100_000.0, 5.0);
        let snap = make_snap_with_trade(1_000_000_000, 4500.0, 4500.0, 5.0);
        let result = builder.on_snapshot(&snap);
        assert!(result.is_some());
    }

    #[test]
    fn test_dollar_bar_accumulation() {
        // threshold = $100,000, multiplier = 5.0
        // Each trade: 4500 * 1 * 5 = $22,500
        // Need 5 trades to reach $112,500 > $100,000
        let mut builder = DollarBarBuilder::new(100_000.0, 5.0);
        for i in 0..4 {
            let snap = make_snap_with_trade(1_000_000_000 + i * 100_000_000, 4500.0, 4500.0, 1.0);
            assert!(builder.on_snapshot(&snap).is_none());
        }
        let snap = make_snap_with_trade(1_400_000_000, 4500.0, 4500.0, 1.0);
        assert!(builder.on_snapshot(&snap).is_some());
    }
}
