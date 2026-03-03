/// Constants and types for order book snapshots.

/// Number of price levels per side (bid/ask).
pub const BOOK_DEPTH: usize = 10;

/// Trade buffer length (circular, left-padded with zeros).
pub const TRADE_BUF_LEN: usize = 50;

/// Snapshot interval in nanoseconds (100ms).
pub const SNAPSHOT_INTERVAL_NS: u64 = 100_000_000;

/// A single 100ms order book snapshot.
///
/// Generated every 100ms during RTH (09:30-16:00 ET).
/// Book levels are stored as `[price, size]` pairs.
/// Bids descending by price, asks ascending by price.
/// Trade buffer is left-padded with zeros (most recent at end).
#[derive(Debug, Clone)]
pub struct BookSnapshot {
    /// Timestamp in nanoseconds.
    pub timestamp: u64,
    /// Bid levels: `[BOOK_DEPTH][2]` = `(price, size)`, descending by price.
    pub bids: [[f32; 2]; BOOK_DEPTH],
    /// Ask levels: `[BOOK_DEPTH][2]` = `(price, size)`, ascending by price.
    pub asks: [[f32; 2]; BOOK_DEPTH],
    /// Trade buffer: `[TRADE_BUF_LEN][3]` = `(price, size, aggressor_side)`.
    /// Left-padded with zeros. `aggressor_side`: +1.0 = buyer, -1.0 = seller.
    pub trades: [[f32; 3]; TRADE_BUF_LEN],
    /// Mid price = (best_bid + best_ask) / 2.
    pub mid_price: f32,
    /// Spread = best_ask - best_bid.
    pub spread: f32,
    /// Fractional hours since midnight ET.
    pub time_of_day: f32,
    /// Number of action='T' events since previous snapshot.
    pub trade_count: u32,
    /// Number of action='A' events since previous snapshot.
    pub add_count: u32,
    /// Number of action='C' events since previous snapshot.
    pub cancel_count: u32,
    /// Number of action='M' events since previous snapshot.
    pub modify_count: u32,
}

impl Default for BookSnapshot {
    fn default() -> Self {
        Self {
            timestamp: 0,
            bids: [[0.0; 2]; BOOK_DEPTH],
            asks: [[0.0; 2]; BOOK_DEPTH],
            trades: [[0.0; 3]; TRADE_BUF_LEN],
            mid_price: 0.0,
            spread: 0.0,
            time_of_day: 0.0,
            trade_count: 0,
            add_count: 0,
            cancel_count: 0,
            modify_count: 0,
        }
    }
}
