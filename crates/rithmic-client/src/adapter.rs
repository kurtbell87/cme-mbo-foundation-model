//! Adapter layer: converts Rithmic protobuf messages into pipeline types.
//!
//! All prices are converted to i64 fixed-point at 1e-9 scale at this boundary.
//! No f64 prices leak past the adapter.

use rustc_hash::FxHashMap;

use crate::rti;

// =========================================================================
// Types
// =========================================================================

/// Instrument configuration for the adapter.
#[derive(Debug, Clone)]
pub struct InstrumentConfig {
    pub symbol: String,
    pub exchange: String,
    pub tick_size: f64,
    pub instrument_id: u32,
}

/// An order event suitable for feeding into BookBuilder.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderEvent {
    /// Event timestamp in nanoseconds (exchange time preferred).
    pub ts_event: u64,
    /// Gateway timestamp in nanoseconds (ssboe + usecs).
    /// Always extracted when available, even when ts_event uses exchange time.
    pub gateway_ts_ns: u64,
    /// Local wall-clock at WebSocket receive (nanos since UNIX epoch).
    pub receive_wall_ns: u64,
    /// Numeric order ID (mapped from exchange_order_id string).
    pub order_id: u64,
    /// Instrument identifier.
    pub instrument_id: u32,
    /// Action: 'A' (Add), 'C' (Cancel), 'M' (Modify), 'T' (Trade).
    pub action: char,
    /// Side: 'B' (Bid/Buy), 'A' (Ask/Sell).
    pub side: char,
    /// Price in i64 fixed-point (1e-9 scale).
    pub price: i64,
    /// Size.
    pub size: u32,
    /// Flags. 0x80 = last event in batch.
    pub flags: u8,
}

/// BBO update with i64 fixed-point prices.
#[derive(Debug, Clone, PartialEq)]
pub struct BboUpdate {
    /// Timestamp in nanoseconds (gateway time — BBO has no exchange timestamp).
    pub ts_ns: u64,
    /// Local wall-clock at WebSocket receive (nanos since UNIX epoch).
    pub receive_wall_ns: u64,
    /// Instrument identifier (matches SymbolConfig.instrument_id).
    pub instrument_id: u32,
    pub bid_price: i64,
    pub bid_size: i32,
    /// Implied/synthetic bid quantity from calendar spreads (0 = outright only).
    pub bid_implicit_size: i32,
    pub ask_price: i64,
    pub ask_size: i32,
    /// Implied/synthetic ask quantity from calendar spreads (0 = outright only).
    pub ask_implicit_size: i32,
}

/// Maps exchange_order_id strings to monotonically assigned u64 IDs.
///
/// No removal on DELETE — a subsequent LastTrade (150) may reference
/// the same exchange_order_id after a fill removes the order.
/// At /MES message rates (~10-50k orders/day) this is bounded.
#[derive(Debug)]
pub struct OrderIdMap {
    map: FxHashMap<String, u64>,
    next_id: u64,
}

impl Default for OrderIdMap {
    fn default() -> Self {
        Self::new()
    }
}

impl OrderIdMap {
    pub fn new() -> Self {
        Self {
            map: FxHashMap::default(),
            next_id: 1, // start at 1, 0 reserved for trades without order_id
        }
    }

    /// Get or assign a numeric ID for the given exchange order ID.
    pub fn get_or_assign(&mut self, exchange_order_id: &str) -> u64 {
        if let Some(&id) = self.map.get(exchange_order_id) {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.map.insert(exchange_order_id.to_string(), id);
        id
    }

    /// Look up an existing ID without assigning.
    pub fn get(&self, exchange_order_id: &str) -> Option<u64> {
        self.map.get(exchange_order_id).copied()
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// =========================================================================
// Price conversion
// =========================================================================

/// Convert an f64 price to i64 fixed-point at 1e-9 scale.
/// Uses `.round()` before `as i64` to avoid truncation bugs.
#[inline]
pub fn price_to_fixed(price: f64) -> i64 {
    (price * 1e9).round() as i64
}

// =========================================================================
// Timestamp conversion — two explicitly separate functions
// =========================================================================

/// Convert exchange timestamp fields to nanoseconds.
/// `source_ssboe` is seconds, `source_nsecs` is already nanoseconds.
#[inline]
pub fn exchange_ts_ns(source_ssboe: i32, source_nsecs: i32) -> u64 {
    source_ssboe as u64 * 1_000_000_000 + source_nsecs as u64
}

/// Convert gateway timestamp fields to nanoseconds.
/// `ssboe` is seconds, `usecs` is microseconds (multiply by 1000).
#[inline]
pub fn gateway_ts_ns(ssboe: i32, usecs: i32) -> u64 {
    ssboe as u64 * 1_000_000_000 + usecs as u64 * 1_000
}

// =========================================================================
// Conversion: DepthByOrder (160) → Vec<OrderEvent>
// =========================================================================

/// Rithmic update_type values for DepthByOrder.
const UPDATE_TYPE_NEW: i32 = 1;
const UPDATE_TYPE_CHANGE: i32 = 2;
const UPDATE_TYPE_DELETE: i32 = 3;

/// Rithmic transaction_type values.
const TRANSACTION_TYPE_BUY: i32 = 1;
const TRANSACTION_TYPE_SELL: i32 = 2;

/// Convert a DepthByOrder (160) message into OrderEvents.
pub fn depth_by_order_to_events(
    msg: &rti::DepthByOrder,
    instrument_id: u32,
    order_id_map: &mut OrderIdMap,
    receive_wall_ns: u64,
) -> Vec<OrderEvent> {
    let update_types = msg.update_type.as_deref().unwrap_or(&[]);
    let transaction_types = msg.transaction_type.as_deref().unwrap_or(&[]);
    let prices = msg.depth_price.as_deref().unwrap_or(&[]);
    let sizes = msg.depth_size.as_deref().unwrap_or(&[]);
    let order_ids = msg.exchange_order_id.as_deref().unwrap_or(&[]);

    let count = update_types.len();
    if count == 0 {
        return Vec::new();
    }

    // Compute timestamp: prefer exchange time, fall back to gateway time
    let ts_event = if let (Some(ss), Some(ns)) = (msg.source_ssboe, msg.source_nsecs) {
        exchange_ts_ns(ss, ns)
    } else if let (Some(ss), Some(us)) = (msg.ssboe, msg.usecs) {
        gateway_ts_ns(ss, us)
    } else {
        0
    };

    // Always extract gateway timestamp (even when exchange ts is available)
    let gw_ts = if let (Some(ss), Some(us)) = (msg.ssboe, msg.usecs) {
        gateway_ts_ns(ss, us)
    } else {
        0
    };

    let mut events = Vec::with_capacity(count);

    for i in 0..count {
        let ut = update_types.get(i).copied().unwrap_or(0);
        let tt = transaction_types.get(i).copied().unwrap_or(0);
        let price_f64 = prices.get(i).copied().unwrap_or(0.0);
        let size = sizes.get(i).copied().unwrap_or(0) as u32;
        let oid_str = order_ids
            .get(i)
            .map(|s| s.as_str())
            .unwrap_or("");

        let action = match ut {
            UPDATE_TYPE_NEW => 'A',
            UPDATE_TYPE_CHANGE => 'M',
            UPDATE_TYPE_DELETE => 'C',
            _ => 'A', // default to Add for unknown
        };

        let side = match tt {
            TRANSACTION_TYPE_BUY => 'B',
            TRANSACTION_TYPE_SELL => 'A',
            _ => 'B', // default
        };

        let order_id = order_id_map.get_or_assign(oid_str);
        let price = price_to_fixed(price_f64);

        // Last event in batch gets F_LAST flag
        let flags = if i == count - 1 { 0x80 } else { 0 };

        events.push(OrderEvent {
            ts_event,
            gateway_ts_ns: gw_ts,
            receive_wall_ns,
            order_id,
            instrument_id,
            action,
            side,
            price,
            size,
            flags,
        });
    }

    events
}

// =========================================================================
// Conversion: ResponseDepthByOrderSnapshot (116) → Vec<OrderEvent>
// =========================================================================

/// Convert a ResponseDepthByOrderSnapshot (116) message into OrderEvents.
///
/// Unlike incremental DBO (160) where all fields are parallel arrays,
/// snapshot messages describe ONE price level per message:
///   - `transaction_type` (depth_side) — single value: the side for this level
///   - `depth_price` — single value: the price for this level
///   - `depth_size` / `exchange_order_id` — repeated: one entry per order at this level
///
/// All snapshot entries are action='A' (Add) — they represent active orders.
pub fn snapshot_response_to_events(
    msg: &rti::ResponseDepthByOrderSnapshot,
    instrument_id: u32,
    order_id_map: &mut OrderIdMap,
    receive_wall_ns: u64,
) -> Vec<OrderEvent> {
    let sizes = msg.depth_size.as_deref().unwrap_or(&[]);
    let order_ids = msg.exchange_order_id.as_deref().unwrap_or(&[]);

    // Count = number of orders at this price level
    let count = order_ids.len().max(sizes.len());
    if count == 0 {
        return Vec::new();
    }

    // Single price for the entire message (one level per message)
    let price_f64 = msg.depth_price.as_deref().and_then(|v| v.first().copied()).unwrap_or(0.0);
    let price = price_to_fixed(price_f64);

    // Single side for the entire message
    let tt = msg.transaction_type.as_deref().and_then(|v| v.first().copied()).unwrap_or(0);
    let side = match tt {
        TRANSACTION_TYPE_BUY => 'B',
        TRANSACTION_TYPE_SELL => 'A',
        _ => 'B', // default
    };

    // Compute timestamp from gateway fields (snapshots typically don't have source_*)
    let ts_event = if let (Some(ss), Some(us)) = (msg.ssboe, msg.usecs) {
        gateway_ts_ns(ss, us)
    } else {
        0
    };

    let mut events = Vec::with_capacity(count);

    for i in 0..count {
        let size = sizes.get(i).copied().unwrap_or(0) as u32;
        let oid_str = order_ids.get(i).map(|s| s.as_str()).unwrap_or("");
        let order_id = order_id_map.get_or_assign(oid_str);

        // Last event in batch gets F_LAST flag
        let flags = if i == count - 1 { 0x80 } else { 0 };

        events.push(OrderEvent {
            ts_event,
            gateway_ts_ns: ts_event, // snapshot uses gateway time for ts_event already
            receive_wall_ns,
            order_id,
            instrument_id,
            action: 'A', // snapshot entries are always Add
            side,
            price,
            size,
            flags,
        });
    }

    events
}

// =========================================================================
// Conversion: LastTrade (150) → OrderEvent
// =========================================================================

/// Convert a LastTrade (150) message into a single Trade OrderEvent.
pub fn last_trade_to_event(
    msg: &rti::LastTrade,
    instrument_id: u32,
    receive_wall_ns: u64,
) -> Option<OrderEvent> {
    let price_f64 = msg.trade_price?;
    let size = msg.trade_size? as u32;

    // Compute timestamp: prefer exchange time, fall back to gateway time
    let ts_event = if let (Some(ss), Some(ns)) = (msg.source_ssboe, msg.source_nsecs) {
        exchange_ts_ns(ss, ns)
    } else if let (Some(ss), Some(us)) = (msg.ssboe, msg.usecs) {
        gateway_ts_ns(ss, us)
    } else {
        0
    };

    // Always extract gateway timestamp
    let gw_ts = if let (Some(ss), Some(us)) = (msg.ssboe, msg.usecs) {
        gateway_ts_ns(ss, us)
    } else {
        0
    };

    // Map aggressor: 1=BUY→'B', 2=SELL→'A'
    let side = match msg.aggressor {
        Some(1) => 'B',
        Some(2) => 'A',
        _ => 'B', // default
    };

    Some(OrderEvent {
        ts_event,
        gateway_ts_ns: gw_ts,
        receive_wall_ns,
        order_id: 0, // trades don't have a persistent order_id
        instrument_id,
        action: 'T',
        side,
        price: price_to_fixed(price_f64),
        size,
        flags: 0x80, // trades are always their own batch
    })
}

// =========================================================================
// Conversion: BestBidOffer (151) → BboUpdate
// =========================================================================

/// Convert a BestBidOffer (151) message into a BboUpdate.
pub fn best_bid_offer_to_update(msg: &rti::BestBidOffer, instrument_id: u32, receive_wall_ns: u64) -> Option<BboUpdate> {
    let bid_price = msg.bid_price?;
    let ask_price = msg.ask_price?;

    let ts_ns = if let (Some(ss), Some(us)) = (msg.ssboe, msg.usecs) {
        gateway_ts_ns(ss, us)
    } else {
        0
    };

    Some(BboUpdate {
        ts_ns,
        receive_wall_ns,
        instrument_id,
        bid_price: price_to_fixed(bid_price),
        bid_size: msg.bid_size.unwrap_or(0),
        bid_implicit_size: msg.bid_implicit_size.unwrap_or(0),
        ask_price: price_to_fixed(ask_price),
        ask_size: msg.ask_size.unwrap_or(0),
        ask_implicit_size: msg.ask_implicit_size.unwrap_or(0),
    })
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_to_fixed_exact_quarter_tick() {
        // 5000.25 * 1e9 = 5000250000000
        assert_eq!(price_to_fixed(5000.25), 5_000_250_000_000);
    }

    #[test]
    fn price_to_fixed_round_avoids_truncation() {
        // Simulate potential floating-point imprecision
        let price = 5000.25;
        let fixed = price_to_fixed(price);
        // Without .round(), (5000.25 * 1e9) could be 5000249999999.99997
        // which truncates to 5000249999999. With .round(), it's 5000250000000.
        assert_eq!(fixed, 5_000_250_000_000);
    }

    #[test]
    fn price_to_fixed_zero() {
        assert_eq!(price_to_fixed(0.0), 0);
    }

    #[test]
    fn price_to_fixed_negative() {
        assert_eq!(price_to_fixed(-1.5), -1_500_000_000);
    }

    #[test]
    fn exchange_ts_ns_computation() {
        let ts = exchange_ts_ns(1700000000, 123456789);
        assert_eq!(ts, 1_700_000_000_123_456_789);
    }

    #[test]
    fn gateway_ts_ns_computation() {
        // usecs = 500000 → 500000 * 1000 = 500_000_000 ns
        let ts = gateway_ts_ns(1700000000, 500000);
        assert_eq!(ts, 1_700_000_000_500_000_000);
    }

    #[test]
    fn timestamp_functions_differ_in_scaling() {
        // Same numeric inputs produce different results due to nsecs vs usecs scaling
        let ex = exchange_ts_ns(1, 1000);
        let gw = gateway_ts_ns(1, 1000);
        assert_eq!(ex, 1_000_001_000); // 1s + 1000ns
        assert_eq!(gw, 1_001_000_000); // 1s + 1000us = 1s + 1ms
        assert_ne!(ex, gw);
    }

    #[test]
    fn order_id_map_assigns_monotonic_ids() {
        let mut map = OrderIdMap::new();
        let id1 = map.get_or_assign("ORD001");
        let id2 = map.get_or_assign("ORD002");
        let id3 = map.get_or_assign("ORD003");
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }

    #[test]
    fn order_id_map_returns_same_id_for_same_key() {
        let mut map = OrderIdMap::new();
        let id1 = map.get_or_assign("ORD001");
        let id2 = map.get_or_assign("ORD001");
        assert_eq!(id1, id2);
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn order_id_map_delete_then_trade_resolves() {
        // Simulate: order added, then deleted, then trade references same order_id
        let mut map = OrderIdMap::new();
        let id_add = map.get_or_assign("ORD001");
        // After DELETE, map still has the entry (no removal on delete)
        let id_trade = map.get_or_assign("ORD001");
        assert_eq!(id_add, id_trade);
    }

    #[test]
    fn order_id_map_get_without_assign() {
        let mut map = OrderIdMap::new();
        assert_eq!(map.get("ORD001"), None);
        map.get_or_assign("ORD001");
        assert_eq!(map.get("ORD001"), Some(1));
    }

    #[test]
    fn depth_by_order_to_events_basic() {
        let msg = rti::DepthByOrder {
            template_id: Some(160),
            symbol: Some("MES".to_string()),
            exchange: Some("CME".to_string()),
            sequence_number: Some(1),
            update_type: Some(vec![1, 1, 3]), // NEW, NEW, DELETE
            transaction_type: Some(vec![1, 2, 1]), // BUY, SELL, BUY
            depth_price: Some(vec![5000.25, 5000.50, 5000.00]),
            depth_size: Some(vec![10, 20, 5]),
            exchange_order_id: Some(vec![
                "ORD1".to_string(),
                "ORD2".to_string(),
                "ORD3".to_string(),
            ]),
            ssboe: Some(1700000000),
            usecs: Some(500000),
            source_ssboe: Some(1700000000),
            source_usecs: None,
            source_nsecs: Some(499000123),
        };

        let mut oid_map = OrderIdMap::new();
        let events = depth_by_order_to_events(&msg, 1, &mut oid_map, 9999);

        assert_eq!(events.len(), 3);

        // First event: NEW BUY
        assert_eq!(events[0].action, 'A');
        assert_eq!(events[0].side, 'B');
        assert_eq!(events[0].price, 5_000_250_000_000);
        assert_eq!(events[0].size, 10);
        assert_eq!(events[0].flags, 0); // not last
        assert_eq!(events[0].receive_wall_ns, 9999);
        // gateway_ts_ns always extracted
        assert_eq!(events[0].gateway_ts_ns, 1_700_000_000_500_000_000);

        // Second event: NEW SELL
        assert_eq!(events[1].action, 'A');
        assert_eq!(events[1].side, 'A');
        assert_eq!(events[1].price, 5_000_500_000_000);
        assert_eq!(events[1].size, 20);
        assert_eq!(events[1].flags, 0);

        // Third event: DELETE BUY (last in batch)
        assert_eq!(events[2].action, 'C');
        assert_eq!(events[2].side, 'B');
        assert_eq!(events[2].flags, 0x80); // F_LAST
    }

    #[test]
    fn depth_by_order_to_events_uses_exchange_timestamp() {
        let msg = rti::DepthByOrder {
            template_id: Some(160),
            update_type: Some(vec![1]),
            transaction_type: Some(vec![1]),
            depth_price: Some(vec![5000.25]),
            depth_size: Some(vec![10]),
            exchange_order_id: Some(vec!["ORD1".to_string()]),
            source_ssboe: Some(1700000000),
            source_nsecs: Some(123456789),
            ssboe: Some(1700000001), // gateway is later
            usecs: Some(0),
            ..Default::default()
        };

        let mut oid_map = OrderIdMap::new();
        let events = depth_by_order_to_events(&msg, 1, &mut oid_map, 0);
        // Should use exchange timestamp (source_ssboe + source_nsecs)
        assert_eq!(events[0].ts_event, 1_700_000_000_123_456_789);
    }

    #[test]
    fn depth_by_order_to_events_falls_back_to_gateway() {
        let msg = rti::DepthByOrder {
            template_id: Some(160),
            update_type: Some(vec![1]),
            transaction_type: Some(vec![1]),
            depth_price: Some(vec![5000.25]),
            depth_size: Some(vec![10]),
            exchange_order_id: Some(vec!["ORD1".to_string()]),
            source_ssboe: None,
            source_nsecs: None,
            ssboe: Some(1700000000),
            usecs: Some(500000),
            ..Default::default()
        };

        let mut oid_map = OrderIdMap::new();
        let events = depth_by_order_to_events(&msg, 1, &mut oid_map, 0);
        // Should fall back to gateway timestamp
        assert_eq!(events[0].ts_event, 1_700_000_000_500_000_000);
    }

    #[test]
    fn depth_by_order_to_events_empty_msg() {
        let msg = rti::DepthByOrder {
            template_id: Some(160),
            ..Default::default()
        };
        let mut oid_map = OrderIdMap::new();
        let events = depth_by_order_to_events(&msg, 1, &mut oid_map, 0);
        assert!(events.is_empty());
    }

    #[test]
    fn depth_by_order_single_event_gets_f_last() {
        let msg = rti::DepthByOrder {
            template_id: Some(160),
            update_type: Some(vec![2]), // CHANGE
            transaction_type: Some(vec![2]), // SELL
            depth_price: Some(vec![5000.50]),
            depth_size: Some(vec![15]),
            exchange_order_id: Some(vec!["ORD1".to_string()]),
            ssboe: Some(1700000000),
            usecs: Some(0),
            ..Default::default()
        };

        let mut oid_map = OrderIdMap::new();
        let events = depth_by_order_to_events(&msg, 1, &mut oid_map, 0);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, 'M');
        assert_eq!(events[0].side, 'A');
        assert_eq!(events[0].flags, 0x80); // single = last
    }

    #[test]
    fn last_trade_to_event_basic() {
        let msg = rti::LastTrade {
            template_id: Some(150),
            symbol: Some("MES".to_string()),
            exchange: Some("CME".to_string()),
            trade_price: Some(5000.25),
            trade_size: Some(3),
            aggressor: Some(1), // BUY
            volume: Some(150000),
            ssboe: Some(1700000000),
            usecs: Some(500000),
            source_ssboe: Some(1700000000),
            source_usecs: None,
            source_nsecs: Some(499000123),
        };

        let event = last_trade_to_event(&msg, 1, 7777).unwrap();
        assert_eq!(event.action, 'T');
        assert_eq!(event.side, 'B');
        assert_eq!(event.price, 5_000_250_000_000);
        assert_eq!(event.size, 3);
        assert_eq!(event.flags, 0x80); // trades always F_LAST
        assert_eq!(event.order_id, 0); // trades don't have order_id
        assert_eq!(event.ts_event, 1_700_000_000_499_000_123);
        assert_eq!(event.receive_wall_ns, 7777);
        assert_eq!(event.gateway_ts_ns, 1_700_000_000_500_000_000);
    }

    #[test]
    fn last_trade_to_event_sell_aggressor() {
        let msg = rti::LastTrade {
            template_id: Some(150),
            trade_price: Some(5000.00),
            trade_size: Some(1),
            aggressor: Some(2), // SELL
            ssboe: Some(1700000000),
            usecs: Some(0),
            ..Default::default()
        };

        let event = last_trade_to_event(&msg, 1, 0).unwrap();
        assert_eq!(event.side, 'A');
    }

    #[test]
    fn last_trade_to_event_none_when_missing_price() {
        let msg = rti::LastTrade {
            template_id: Some(150),
            trade_price: None,
            trade_size: Some(1),
            ..Default::default()
        };
        assert!(last_trade_to_event(&msg, 1, 0).is_none());
    }

    #[test]
    fn best_bid_offer_to_update_basic() {
        let msg = rti::BestBidOffer {
            template_id: Some(151),
            symbol: Some("MES".to_string()),
            exchange: Some("CME".to_string()),
            bid_price: Some(5000.25),
            bid_size: Some(42),
            ask_price: Some(5000.50),
            ask_size: Some(37),
            ssboe: Some(1700000000),
            usecs: Some(123456),
            ..Default::default()
        };

        let update = best_bid_offer_to_update(&msg, 1, 5555).unwrap();
        assert_eq!(update.bid_price, 5_000_250_000_000);
        assert_eq!(update.bid_size, 42);
        assert_eq!(update.ask_price, 5_000_500_000_000);
        assert_eq!(update.ask_size, 37);
        assert_eq!(update.ts_ns, 1_700_000_000_123_456_000);
        assert_eq!(update.receive_wall_ns, 5555);
    }

    #[test]
    fn best_bid_offer_to_update_none_when_missing_prices() {
        let msg = rti::BestBidOffer {
            template_id: Some(151),
            bid_price: None,
            ask_price: Some(5000.50),
            ..Default::default()
        };
        assert!(best_bid_offer_to_update(&msg, 1, 0).is_none());
    }

    #[test]
    fn price_consistency_between_dbo_and_bbo() {
        // The same price value converted through DBO and BBO paths
        // must produce the exact same i64 fixed-point value
        let price = 5000.25;

        // Via DBO path
        let dbo_fixed = price_to_fixed(price);

        // Via BBO path
        let bbo_msg = rti::BestBidOffer {
            template_id: Some(151),
            bid_price: Some(price),
            ask_price: Some(price),
            ssboe: Some(0),
            usecs: Some(0),
            ..Default::default()
        };
        let bbo_update = best_bid_offer_to_update(&bbo_msg, 1, 0).unwrap();

        assert_eq!(dbo_fixed, bbo_update.bid_price);
        assert_eq!(dbo_fixed, bbo_update.ask_price);
    }
}
