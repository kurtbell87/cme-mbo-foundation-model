use crate::book::BOOK_DEPTH;
use crate::event::DayEventBuffer;

/// Aggregated bar struct — OHLCV + book state + message counts.
#[derive(Debug, Clone)]
pub struct Bar {
    // Temporal fields
    pub open_ts: u64,
    pub close_ts: u64,
    pub time_of_day: f32,
    pub bar_duration_s: f32,

    // OHLCV fields
    pub open_mid: f32,
    pub close_mid: f32,
    pub high_mid: f32,
    pub low_mid: f32,
    pub vwap: f32,
    pub volume: u32,
    pub tick_count: u32,
    pub buy_volume: f32,
    pub sell_volume: f32,

    // Book state at bar close
    pub bids: [[f32; 2]; BOOK_DEPTH],
    pub asks: [[f32; 2]; BOOK_DEPTH],
    pub spread: f32,

    // Spread dynamics
    pub max_spread: f32,
    pub min_spread: f32,
    pub snapshot_count: u32,

    // MBO event references
    pub mbo_event_begin: u32,
    pub mbo_event_end: u32,

    // Message summary
    pub add_count: u32,
    pub cancel_count: u32,
    pub modify_count: u32,
    pub trade_event_count: u32,
    pub cancel_add_ratio: f32,
    pub message_rate: f32,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            open_ts: 0,
            close_ts: 0,
            time_of_day: 0.0,
            bar_duration_s: 0.0,
            open_mid: 0.0,
            close_mid: 0.0,
            high_mid: 0.0,
            low_mid: 0.0,
            vwap: 0.0,
            volume: 0,
            tick_count: 0,
            buy_volume: 0.0,
            sell_volume: 0.0,
            bids: [[0.0; 2]; BOOK_DEPTH],
            asks: [[0.0; 2]; BOOK_DEPTH],
            spread: 0.0,
            max_spread: 0.0,
            min_spread: 0.0,
            snapshot_count: 0,
            mbo_event_begin: 0,
            mbo_event_end: 0,
            add_count: 0,
            cancel_count: 0,
            modify_count: 0,
            trade_event_count: 0,
            cancel_add_ratio: 0.0,
            message_rate: 0.0,
        }
    }
}

/// Adapter for encoder input: 20-row price ladder (10 bids reversed + 10 asks).
#[derive(Debug, Clone)]
pub struct PriceLadderInput {
    pub data: [[f32; 2]; 20],
}

impl PriceLadderInput {
    /// Create from a Bar's book state, with prices relative to `mid_price`.
    pub fn from_bar(bar: &Bar, mid_price: f32) -> Self {
        let mut input = PriceLadderInput {
            data: [[0.0; 2]; 20],
        };
        // Rows 0-9: bids in reverse order (deepest first, best bid at row 9)
        for i in 0..10 {
            let bid_idx = 9 - i; // reverse: row 0 = deepest bid (index 9)
            input.data[i][0] = bar.bids[bid_idx][0] - mid_price;
            input.data[i][1] = bar.bids[bid_idx][1];
        }
        // Rows 10-19: asks in order (best ask at row 10)
        for i in 0..10 {
            input.data[10 + i][0] = bar.asks[i][0] - mid_price;
            input.data[10 + i][1] = bar.asks[i][1];
        }
        input
    }
}

/// Adapter for MBO event sequence within a bar.
#[derive(Debug, Clone, Default)]
pub struct MessageSequenceInput {
    pub events: Vec<Vec<f32>>,
}

impl MessageSequenceInput {
    /// Create from a Bar's MBO event range using a DayEventBuffer.
    pub fn from_bar(bar: &Bar, buf: &DayEventBuffer) -> Self {
        let mut input = MessageSequenceInput::default();
        let span = buf.get_events(bar.mbo_event_begin, bar.mbo_event_end);
        for ev in span {
            input.events.push(vec![
                ev.action as f32,
                ev.price,
                ev.size as f32,
                ev.side as f32,
                ev.ts_event as f32,
            ]);
        }
        // If no events from buffer, produce empty events for the range
        if input.events.is_empty() && bar.mbo_event_end > bar.mbo_event_begin {
            for _ in bar.mbo_event_begin..bar.mbo_event_end {
                input.events.push(vec![0.0, 0.0, 0.0, 0.0, 0.0]);
            }
        }
        input
    }
}
