//! Subscription builders for market data and depth-by-order.

use crate::rti;

/// Build a RequestMarketDataUpdate to subscribe to BBO + LastTrade.
pub fn subscribe_market_data(symbol: &str, exchange: &str) -> rti::RequestMarketDataUpdate {
    rti::RequestMarketDataUpdate::subscribe(symbol, exchange)
}

/// Build a RequestDepthByOrderUpdates to subscribe to MBO stream.
pub fn subscribe_depth_by_order(symbol: &str, exchange: &str) -> rti::RequestDepthByOrderUpdates {
    rti::RequestDepthByOrderUpdates::subscribe(symbol, exchange)
}

/// Build a RequestDepthByOrderSnapshot for re-baseline after gap.
pub fn request_dbo_snapshot(symbol: &str, exchange: &str) -> rti::RequestDepthByOrderSnapshot {
    rti::RequestDepthByOrderSnapshot::new(symbol, exchange)
}
