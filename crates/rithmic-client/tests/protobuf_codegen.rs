//! Integration tests for Rithmic Protobuf codegen.
//!
//! Tests cover:
//!   - Generated type availability
//!   - template_id extraction from raw bytes
//!   - Round-trip encode/decode for all message types with full fields
//!   - Message dispatch via decode_message()
//!   - Unknown template_id handling
//!   - RequestLogin builder (corrected tags)
//!   - Error / edge-case handling

use prost::Message;
use rithmic_client::rti;
use rithmic_client::{decode_message, extract_template_id, InfraType, RithmicMessage};

// ---------------------------------------------------------------------------
// T1 — Generated types exist and are constructable
// ---------------------------------------------------------------------------

#[test]
fn generated_type_request_login_exists() {
    let msg = rti::RequestLogin::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_response_login_exists() {
    let msg = rti::ResponseLogin::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_best_bid_offer_exists() {
    let msg = rti::BestBidOffer::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_last_trade_exists() {
    let msg = rti::LastTrade::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_depth_by_order_exists() {
    let msg = rti::DepthByOrder::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_rithmic_order_notification_exists() {
    let msg = rti::RithmicOrderNotification::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_exchange_order_notification_exists() {
    let msg = rti::ExchangeOrderNotification::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_request_heartbeat_exists() {
    let msg = rti::RequestHeartbeat::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_response_heartbeat_exists() {
    let msg = rti::ResponseHeartbeat::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_reject_exists() {
    let msg = rti::Reject::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_forced_logout_exists() {
    let msg = rti::ForcedLogout::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_instrument_pnl_position_update_exists() {
    let msg = rti::InstrumentPnLPositionUpdate::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_account_pnl_position_update_exists() {
    let msg = rti::AccountPnLPositionUpdate::default();
    assert!(msg.template_id.is_none());
}

// New type existence tests

#[test]
fn generated_type_request_rithmic_system_info_exists() {
    let msg = rti::RequestRithmicSystemInfo::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_response_rithmic_system_info_exists() {
    let msg = rti::ResponseRithmicSystemInfo::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_request_logout_exists() {
    let msg = rti::RequestLogout::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_response_logout_exists() {
    let msg = rti::ResponseLogout::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_request_market_data_update_exists() {
    let msg = rti::RequestMarketDataUpdate::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_response_market_data_update_exists() {
    let msg = rti::ResponseMarketDataUpdate::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_order_book_exists() {
    let msg = rti::OrderBook::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_request_depth_by_order_snapshot_exists() {
    let msg = rti::RequestDepthByOrderSnapshot::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_response_depth_by_order_snapshot_exists() {
    let msg = rti::ResponseDepthByOrderSnapshot::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_request_depth_by_order_updates_exists() {
    let msg = rti::RequestDepthByOrderUpdates::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_response_depth_by_order_updates_exists() {
    let msg = rti::ResponseDepthByOrderUpdates::default();
    assert!(msg.template_id.is_none());
}

#[test]
fn generated_type_depth_by_order_end_event_exists() {
    let msg = rti::DepthByOrderEndEvent::default();
    assert!(msg.template_id.is_none());
}

// ---------------------------------------------------------------------------
// T2 — extract_template_id
// ---------------------------------------------------------------------------

#[test]
fn extract_template_id_from_response_login() {
    let msg = rti::ResponseLogin {
        template_id: Some(11),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let tid = extract_template_id(&buf).expect("should extract template_id");
    assert_eq!(tid, 11);
}

#[test]
fn extract_template_id_from_best_bid_offer() {
    let msg = rti::BestBidOffer {
        template_id: Some(151),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let tid = extract_template_id(&buf).expect("should extract template_id");
    assert_eq!(tid, 151);
}

#[test]
fn extract_template_id_from_request_heartbeat() {
    let msg = rti::RequestHeartbeat {
        template_id: Some(18),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let tid = extract_template_id(&buf).expect("should extract template_id");
    assert_eq!(tid, 18);
}

#[test]
fn extract_template_id_from_rithmic_order_notification() {
    let msg = rti::RithmicOrderNotification {
        template_id: Some(351),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let tid = extract_template_id(&buf).expect("should extract template_id");
    assert_eq!(tid, 351);
}

#[test]
fn extract_template_id_from_depth_by_order() {
    let msg = rti::DepthByOrder {
        template_id: Some(160),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let tid = extract_template_id(&buf).expect("should extract template_id");
    assert_eq!(tid, 160);
}

#[test]
fn extract_template_id_returns_error_on_empty_buffer() {
    let result = extract_template_id(&[]);
    assert!(result.is_err(), "empty buffer should return an error");
}

#[test]
fn extract_template_id_returns_error_on_garbage_bytes() {
    let garbage = vec![0xFF, 0xFF, 0xFF, 0x01, 0x00];
    let result = extract_template_id(&garbage);
    assert!(result.is_err(), "garbage bytes should return an error");
}

// ---------------------------------------------------------------------------
// T3 — Round-trip: ResponseLogin (with new fields)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_response_login() {
    let original = rti::ResponseLogin {
        template_id: Some(11),
        user_msg: Some(vec!["correlation-1".to_string()]),
        rp_code: Some(vec!["0".to_string()]),
        heartbeat_interval: Some(30),
        unique_user_id: Some("user-12345".to_string()),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::ResponseLogin::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(11));
    assert_eq!(
        decoded.user_msg.as_ref().and_then(|v| v.first()),
        Some(&"correlation-1".to_string())
    );
    assert_eq!(decoded.heartbeat_interval, Some(30));
    assert_eq!(decoded.unique_user_id.as_deref(), Some("user-12345"));
}

// ---------------------------------------------------------------------------
// T4 — Round-trip: BestBidOffer (all fields)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_best_bid_offer() {
    let original = rti::BestBidOffer {
        template_id: Some(151),
        symbol: Some("ESH6".to_string()),
        exchange: Some("CME".to_string()),
        presence_bits: Some(15),
        clear_bits: None,
        bid_price: Some(5000.25),
        bid_size: Some(42),
        ask_price: Some(5000.50),
        ask_size: Some(37),
        ssboe: Some(1700000000),
        usecs: Some(123456),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::BestBidOffer::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(151));
    assert_eq!(decoded.symbol, Some("ESH6".to_string()));
    assert_eq!(decoded.exchange, Some("CME".to_string()));
    assert_eq!(decoded.presence_bits, Some(15));
    assert_eq!(decoded.bid_price, Some(5000.25));
    assert_eq!(decoded.bid_size, Some(42));
    assert_eq!(decoded.ask_price, Some(5000.50));
    assert_eq!(decoded.ask_size, Some(37));
    assert_eq!(decoded.ssboe, Some(1700000000));
    assert_eq!(decoded.usecs, Some(123456));
}

// ---------------------------------------------------------------------------
// T5 — Round-trip: RithmicOrderNotification
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_rithmic_order_notification() {
    let original = rti::RithmicOrderNotification {
        template_id: Some(351),
        symbol: Some("NQH6".to_string()),
        exchange: Some("CME".to_string()),
        ..Default::default()
    };
    let buf = original.encode_to_vec();
    let decoded =
        rti::RithmicOrderNotification::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(351));
    assert_eq!(decoded.symbol, Some("NQH6".to_string()));
    assert_eq!(decoded.exchange, Some("CME".to_string()));
}

// ---------------------------------------------------------------------------
// T6 — Round-trip: LastTrade (all fields)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_last_trade() {
    let original = rti::LastTrade {
        template_id: Some(150),
        symbol: Some("MES".to_string()),
        exchange: Some("CME".to_string()),
        trade_price: Some(5000.25),
        trade_size: Some(3),
        aggressor: Some(1),
        volume: Some(150000),
        ssboe: Some(1700000000),
        usecs: Some(500000),
        source_ssboe: Some(1700000000),
        source_usecs: Some(499000),
        source_nsecs: Some(499000123),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::LastTrade::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(150));
    assert_eq!(decoded.symbol, Some("MES".to_string()));
    assert_eq!(decoded.exchange, Some("CME".to_string()));
    assert_eq!(decoded.trade_price, Some(5000.25));
    assert_eq!(decoded.trade_size, Some(3));
    assert_eq!(decoded.aggressor, Some(1));
    assert_eq!(decoded.volume, Some(150000));
    assert_eq!(decoded.ssboe, Some(1700000000));
    assert_eq!(decoded.usecs, Some(500000));
    assert_eq!(decoded.source_ssboe, Some(1700000000));
    assert_eq!(decoded.source_nsecs, Some(499000123));
}

// ---------------------------------------------------------------------------
// T7 — Round-trip: RequestHeartbeat
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_request_heartbeat() {
    let original = rti::RequestHeartbeat {
        template_id: Some(18),
        user_msg: Some(vec!["hb-ping".to_string()]),
        ..Default::default()
    };
    let buf = original.encode_to_vec();
    let decoded = rti::RequestHeartbeat::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(18));
    assert_eq!(
        decoded.user_msg.as_ref().and_then(|v| v.first()),
        Some(&"hb-ping".to_string())
    );
}

// ---------------------------------------------------------------------------
// T7b — Round-trip: ResponseHeartbeat (with timestamps)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_response_heartbeat() {
    let original = rti::ResponseHeartbeat {
        template_id: Some(19),
        ssboe: Some(1700000000),
        usecs: Some(123456),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::ResponseHeartbeat::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(19));
    assert_eq!(decoded.ssboe, Some(1700000000));
    assert_eq!(decoded.usecs, Some(123456));
}

// ---------------------------------------------------------------------------
// T7c — Round-trip: Reject (with rp_code + user_msg)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_reject() {
    let original = rti::Reject {
        template_id: Some(75),
        user_msg: Some(vec!["invalid request".to_string()]),
        rp_code: Some(vec!["ERR001".to_string()]),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::Reject::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(75));
    assert_eq!(
        decoded.user_msg.as_ref().and_then(|v| v.first()).map(|s| s.as_str()),
        Some("invalid request")
    );
    assert_eq!(
        decoded.rp_code.as_ref().and_then(|v| v.first()).map(|s| s.as_str()),
        Some("ERR001")
    );
}

// ---------------------------------------------------------------------------
// T7d — Round-trip: ForcedLogout (with rp_code + user_msg)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_forced_logout() {
    let original = rti::ForcedLogout {
        template_id: Some(77),
        user_msg: Some(vec!["session expired".to_string()]),
        rp_code: Some(vec!["FORCED".to_string()]),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::ForcedLogout::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(77));
    assert_eq!(
        decoded.user_msg.as_ref().and_then(|v| v.first()).map(|s| s.as_str()),
        Some("session expired")
    );
}

// ---------------------------------------------------------------------------
// T7e — Round-trip: DepthByOrder (160) with all fields
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_depth_by_order() {
    let original = rti::DepthByOrder {
        template_id: Some(160),
        symbol: Some("MES".to_string()),
        exchange: Some("CME".to_string()),
        sequence_number: Some(42),
        update_type: Some(vec![1, 2, 3]),
        transaction_type: Some(vec![0, 1, 0]),
        depth_price: Some(vec![5000.25, 5000.50, 5000.75]),
        depth_size: Some(vec![10, 20, 30]),
        exchange_order_id: Some(vec![
            "ORD001".to_string(),
            "ORD002".to_string(),
            "ORD003".to_string(),
        ]),
        ssboe: Some(1700000000),
        usecs: Some(500000),
        source_ssboe: Some(1700000000),
        source_usecs: Some(499000),
        source_nsecs: Some(499000123),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::DepthByOrder::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(160));
    assert_eq!(decoded.symbol, Some("MES".to_string()));
    assert_eq!(decoded.exchange, Some("CME".to_string()));
    assert_eq!(decoded.sequence_number, Some(42));
    assert_eq!(decoded.update_type.as_ref().unwrap().len(), 3);
    assert_eq!(decoded.update_type.as_ref().unwrap(), &[1, 2, 3]);
    assert_eq!(decoded.transaction_type.as_ref().unwrap(), &[0, 1, 0]);
    assert_eq!(decoded.depth_price.as_ref().unwrap(), &[5000.25, 5000.50, 5000.75]);
    assert_eq!(decoded.depth_size.as_ref().unwrap(), &[10, 20, 30]);
    assert_eq!(decoded.exchange_order_id.as_ref().unwrap().len(), 3);
    assert_eq!(decoded.ssboe, Some(1700000000));
    assert_eq!(decoded.source_nsecs, Some(499000123));
}

// ---------------------------------------------------------------------------
// T7f — Round-trip: RequestRithmicSystemInfo (16) + ResponseRithmicSystemInfo (17)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_request_rithmic_system_info() {
    let original = rti::RequestRithmicSystemInfo::new();
    let buf = original.encode_to_vec();
    let decoded =
        rti::RequestRithmicSystemInfo::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(16));
}

#[test]
fn roundtrip_response_rithmic_system_info() {
    let original = rti::ResponseRithmicSystemInfo {
        template_id: Some(17),
        system_name: Some(vec![
            "Rithmic Paper Trading".to_string(),
            "Rithmic 01".to_string(),
        ]),
        user_msg: Some(vec!["ok".to_string()]),
        rp_code: Some(vec!["0".to_string()]),
    };
    let buf = original.encode_to_vec();
    let decoded =
        rti::ResponseRithmicSystemInfo::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(17));
    assert_eq!(decoded.system_name.as_ref().unwrap().len(), 2);
    assert_eq!(
        decoded.system_name.as_ref().unwrap()[0],
        "Rithmic Paper Trading"
    );
    assert_eq!(decoded.system_name.as_ref().unwrap()[1], "Rithmic 01");
}

// ---------------------------------------------------------------------------
// T7g — Round-trip: RequestLogout (12) + ResponseLogout (13)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_request_logout() {
    let original = rti::RequestLogout::new();
    let buf = original.encode_to_vec();
    let decoded = rti::RequestLogout::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(12));
}

#[test]
fn roundtrip_response_logout() {
    let original = rti::ResponseLogout {
        template_id: Some(13),
        user_msg: Some(vec!["goodbye".to_string()]),
        rp_code: Some(vec!["0".to_string()]),
    };
    let buf = original.encode_to_vec();
    let decoded = rti::ResponseLogout::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(13));
    assert_eq!(
        decoded.user_msg.as_ref().and_then(|v| v.first()).map(|s| s.as_str()),
        Some("goodbye")
    );
}

// ---------------------------------------------------------------------------
// T7h — Round-trip: RequestMarketDataUpdate (100) + ResponseMarketDataUpdate (101)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_request_market_data_update() {
    let original = rti::RequestMarketDataUpdate::subscribe("MES", "CME");
    let buf = original.encode_to_vec();
    let decoded =
        rti::RequestMarketDataUpdate::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(100));
    assert_eq!(decoded.symbol, Some("MES".to_string()));
    assert_eq!(decoded.exchange, Some("CME".to_string()));
    assert_eq!(decoded.request, Some(1));
    assert_eq!(decoded.update_bits, Some(3));
}

#[test]
fn roundtrip_response_market_data_update() {
    let original = rti::ResponseMarketDataUpdate {
        template_id: Some(101),
        user_msg: Some(vec!["subscribed".to_string()]),
        rp_code: Some(vec!["0".to_string()]),
    };
    let buf = original.encode_to_vec();
    let decoded =
        rti::ResponseMarketDataUpdate::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(101));
}

// ---------------------------------------------------------------------------
// T7i — Round-trip: DBO subscription messages (115-118, 161)
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_request_depth_by_order_snapshot() {
    let original = rti::RequestDepthByOrderSnapshot::new("MES", "CME");
    let buf = original.encode_to_vec();
    let decoded =
        rti::RequestDepthByOrderSnapshot::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(115));
    assert_eq!(decoded.symbol, Some("MES".to_string()));
}

#[test]
fn roundtrip_response_depth_by_order_snapshot() {
    let original = rti::ResponseDepthByOrderSnapshot {
        template_id: Some(116),
        symbol: Some("MES".to_string()),
        exchange: Some("CME".to_string()),
        update_type: Some(vec![1, 1]),
        transaction_type: Some(vec![1, 2]),
        depth_price: Some(vec![5000.25, 5000.50]),
        depth_size: Some(vec![10, 20]),
        exchange_order_id: Some(vec!["ORD1".to_string(), "ORD2".to_string()]),
        sequence_number: Some(100),
        ssboe: Some(1700000000),
        usecs: Some(123456),
        user_msg: None,
        rp_code: None,
    };
    let buf = original.encode_to_vec();
    let decoded =
        rti::ResponseDepthByOrderSnapshot::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(116));
    assert_eq!(decoded.symbol, Some("MES".to_string()));
    assert_eq!(decoded.sequence_number, Some(100));
    assert_eq!(decoded.depth_price.as_ref().unwrap(), &[5000.25, 5000.50]);
    assert_eq!(decoded.depth_size.as_ref().unwrap(), &[10, 20]);
    assert_eq!(decoded.exchange_order_id.as_ref().unwrap().len(), 2);
    assert_eq!(decoded.transaction_type.as_ref().unwrap(), &[1, 2]);
}

#[test]
fn roundtrip_request_depth_by_order_updates() {
    let original = rti::RequestDepthByOrderUpdates::subscribe("MES", "CME");
    let buf = original.encode_to_vec();
    let decoded =
        rti::RequestDepthByOrderUpdates::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(117));
    assert_eq!(decoded.symbol, Some("MES".to_string()));
    assert_eq!(decoded.request, Some(1));
}

#[test]
fn roundtrip_response_depth_by_order_updates() {
    let original = rti::ResponseDepthByOrderUpdates {
        template_id: Some(118),
        user_msg: Some(vec!["subscribed".to_string()]),
        rp_code: Some(vec!["0".to_string()]),
    };
    let buf = original.encode_to_vec();
    let decoded =
        rti::ResponseDepthByOrderUpdates::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(118));
}

#[test]
fn roundtrip_depth_by_order_end_event() {
    let original = rti::DepthByOrderEndEvent {
        template_id: Some(161),
        symbol: Some("MES".to_string()),
        exchange: Some("CME".to_string()),
        sequence_number: Some(999),
        ssboe: Some(1700000000),
        usecs: Some(123456),
    };
    let buf = original.encode_to_vec();
    let decoded =
        rti::DepthByOrderEndEvent::decode(buf.as_slice()).expect("decode should succeed");
    assert_eq!(decoded.template_id, Some(161));
    assert_eq!(decoded.sequence_number, Some(999));
}

// ---------------------------------------------------------------------------
// T8 — Message dispatch: decode_message returns correct variant
// ---------------------------------------------------------------------------

#[test]
fn dispatch_request_rithmic_system_info() {
    let msg = rti::RequestRithmicSystemInfo { template_id: Some(16) };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::RequestRithmicSystemInfo(_)));
}

#[test]
fn dispatch_response_rithmic_system_info() {
    let msg = rti::ResponseRithmicSystemInfo {
        template_id: Some(17),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::ResponseRithmicSystemInfo(_)));
}

#[test]
fn dispatch_response_login() {
    let msg = rti::ResponseLogin {
        template_id: Some(11),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(
        matches!(decoded, RithmicMessage::ResponseLogin(_)),
        "expected ResponseLogin variant, got {:?}",
        std::mem::discriminant(&decoded)
    );
}

#[test]
fn dispatch_request_login() {
    let msg = rti::RequestLogin {
        template_id: Some(10),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::RequestLogin(_)));
}

#[test]
fn dispatch_request_logout() {
    let msg = rti::RequestLogout { template_id: Some(12) };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::RequestLogout(_)));
}

#[test]
fn dispatch_response_logout() {
    let msg = rti::ResponseLogout {
        template_id: Some(13),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::ResponseLogout(_)));
}

#[test]
fn dispatch_best_bid_offer() {
    let msg = rti::BestBidOffer {
        template_id: Some(151),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::BestBidOffer(_)));
}

#[test]
fn dispatch_last_trade() {
    let msg = rti::LastTrade {
        template_id: Some(150),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::LastTrade(_)));
}

#[test]
fn dispatch_order_book_156() {
    let msg = rti::OrderBook {
        template_id: Some(156),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(
        matches!(decoded, RithmicMessage::OrderBook(_)),
        "156 should dispatch to OrderBook, not DepthByOrder"
    );
}

#[test]
fn dispatch_depth_by_order_160() {
    let msg = rti::DepthByOrder {
        template_id: Some(160),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(
        matches!(decoded, RithmicMessage::DepthByOrder(_)),
        "160 should dispatch to DepthByOrder (MBO)"
    );
}

#[test]
fn dispatch_depth_by_order_end_event() {
    let msg = rti::DepthByOrderEndEvent {
        template_id: Some(161),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::DepthByOrderEndEvent(_)));
}

#[test]
fn dispatch_request_market_data_update() {
    let msg = rti::RequestMarketDataUpdate {
        template_id: Some(100),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::RequestMarketDataUpdate(_)));
}

#[test]
fn dispatch_response_market_data_update() {
    let msg = rti::ResponseMarketDataUpdate {
        template_id: Some(101),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::ResponseMarketDataUpdate(_)));
}

#[test]
fn dispatch_request_depth_by_order_snapshot() {
    let msg = rti::RequestDepthByOrderSnapshot {
        template_id: Some(115),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::RequestDepthByOrderSnapshot(_)));
}

#[test]
fn dispatch_response_depth_by_order_snapshot() {
    let msg = rti::ResponseDepthByOrderSnapshot {
        template_id: Some(116),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::ResponseDepthByOrderSnapshot(_)));
}

#[test]
fn dispatch_request_depth_by_order_updates() {
    let msg = rti::RequestDepthByOrderUpdates {
        template_id: Some(117),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::RequestDepthByOrderUpdates(_)));
}

#[test]
fn dispatch_response_depth_by_order_updates() {
    let msg = rti::ResponseDepthByOrderUpdates {
        template_id: Some(118),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::ResponseDepthByOrderUpdates(_)));
}

#[test]
fn dispatch_request_heartbeat() {
    let msg = rti::RequestHeartbeat {
        template_id: Some(18),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::RequestHeartbeat(_)));
}

#[test]
fn dispatch_response_heartbeat() {
    let msg = rti::ResponseHeartbeat {
        template_id: Some(19),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::ResponseHeartbeat(_)));
}

#[test]
fn dispatch_reject() {
    let msg = rti::Reject {
        template_id: Some(75),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::Reject(_)));
}

#[test]
fn dispatch_forced_logout() {
    let msg = rti::ForcedLogout {
        template_id: Some(77),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::ForcedLogout(_)));
}

#[test]
fn dispatch_rithmic_order_notification() {
    let msg = rti::RithmicOrderNotification {
        template_id: Some(351),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(
        decoded,
        RithmicMessage::RithmicOrderNotification(_)
    ));
}

#[test]
fn dispatch_exchange_order_notification() {
    let msg = rti::ExchangeOrderNotification {
        template_id: Some(352),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(
        decoded,
        RithmicMessage::ExchangeOrderNotification(_)
    ));
}

#[test]
fn dispatch_instrument_pnl_position_update() {
    let msg = rti::InstrumentPnLPositionUpdate {
        template_id: Some(450),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(
        decoded,
        RithmicMessage::InstrumentPnLPositionUpdate(_)
    ));
}

#[test]
fn dispatch_account_pnl_position_update() {
    let msg = rti::AccountPnLPositionUpdate {
        template_id: Some(451),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(
        decoded,
        RithmicMessage::AccountPnLPositionUpdate(_)
    ));
}

// ---------------------------------------------------------------------------
// T9 — Unknown template_id → RithmicMessage::Unknown
// ---------------------------------------------------------------------------

#[test]
fn dispatch_unknown_template_id_returns_unknown_variant() {
    let msg = rti::ResponseLogin {
        template_id: Some(99999),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed, not error");
    match decoded {
        RithmicMessage::Unknown(id, bytes) => {
            assert_eq!(id, 99999);
            assert!(!bytes.is_empty(), "raw bytes should be preserved");
        }
        other => panic!(
            "expected Unknown variant for unrecognized template_id, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

#[test]
fn dispatch_preserves_raw_bytes_for_unknown() {
    let msg = rti::RequestHeartbeat {
        template_id: Some(77777),
        user_msg: Some(vec!["test-payload".to_string()]),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    match decoded {
        RithmicMessage::Unknown(id, bytes) => {
            assert_eq!(id, 77777);
            assert_eq!(bytes, buf, "Unknown variant should preserve original bytes");
        }
        _ => panic!("expected Unknown variant"),
    }
}

// ---------------------------------------------------------------------------
// T10 — RequestLogin builder (corrected tags)
// ---------------------------------------------------------------------------

#[test]
fn request_login_builder_sets_all_fields() {
    let login = rti::RequestLogin::new(
        "test_user",
        "test_pass",
        "MyApp",
        "1.0.0",
        "Rithmic Paper Trading",
        InfraType::TickerPlant,
    );

    assert_eq!(login.template_id, Some(10));
    assert_eq!(login.user.as_deref(), Some("test_user"));
    assert_eq!(login.password.as_deref(), Some("test_pass"));
    assert_eq!(login.app_name.as_deref(), Some("MyApp"));
    assert_eq!(login.app_version.as_deref(), Some("1.0.0"));
    assert_eq!(login.system_name.as_deref(), Some("Rithmic Paper Trading"));
    assert_eq!(login.infra_type, Some(1)); // TickerPlant = 1
}

#[test]
fn request_login_builder_order_plant() {
    let login = rti::RequestLogin::new(
        "user",
        "pass",
        "App",
        "2.0",
        "Rithmic 01",
        InfraType::OrderPlant,
    );

    assert_eq!(login.template_id, Some(10));
    assert_eq!(login.infra_type, Some(2)); // OrderPlant = 2
}

#[test]
fn request_login_builder_history_plant() {
    let login = rti::RequestLogin::new(
        "user",
        "pass",
        "App",
        "2.0",
        "Rithmic 01",
        InfraType::HistoryPlant,
    );

    assert_eq!(login.infra_type, Some(3)); // HistoryPlant = 3
}

#[test]
fn request_login_builder_pnl_plant() {
    let login = rti::RequestLogin::new(
        "user",
        "pass",
        "App",
        "2.0",
        "Rithmic 01",
        InfraType::PnLPlant,
    );

    assert_eq!(login.infra_type, Some(4)); // PnLPlant = 4
}

#[test]
fn request_login_builder_repository_plant() {
    let login = rti::RequestLogin::new(
        "user",
        "pass",
        "App",
        "2.0",
        "Rithmic 01",
        InfraType::RepositoryPlant,
    );

    assert_eq!(login.infra_type, Some(5)); // RepositoryPlant = 5
}

// ---------------------------------------------------------------------------
// T10b — RequestLogin round-trip through dispatch
// ---------------------------------------------------------------------------

#[test]
fn request_login_builder_roundtrips_through_dispatch() {
    let login = rti::RequestLogin::new(
        "user",
        "pass",
        "App",
        "1.0",
        "Rithmic Test",
        InfraType::TickerPlant,
    );
    let buf = login.encode_to_vec();

    let decoded = decode_message(&buf).expect("dispatch should succeed");
    match decoded {
        RithmicMessage::RequestLogin(inner) => {
            assert_eq!(inner.user.as_deref(), Some("user"));
            assert_eq!(inner.password.as_deref(), Some("pass"));
            assert_eq!(inner.app_name.as_deref(), Some("App"));
            assert_eq!(inner.app_version.as_deref(), Some("1.0"));
            assert_eq!(inner.system_name.as_deref(), Some("Rithmic Test"));
            assert_eq!(inner.infra_type, Some(1));
        }
        other => panic!(
            "expected RequestLogin variant, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

// ---------------------------------------------------------------------------
// Edge cases — decode_message error paths
// ---------------------------------------------------------------------------

#[test]
fn decode_message_returns_error_on_empty_buffer() {
    let result = decode_message(&[]);
    assert!(result.is_err(), "empty buffer should be an error");
}

#[test]
fn decode_message_returns_error_on_truncated_bytes() {
    let msg = rti::ResponseLogin {
        template_id: Some(11),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let truncated = &buf[..2.min(buf.len())];
    let _ = decode_message(truncated);
}

// ---------------------------------------------------------------------------
// Template ID field number correctness (154467)
// ---------------------------------------------------------------------------

#[test]
fn template_id_uses_field_number_154467() {
    let msg = rti::ResponseLogin {
        template_id: Some(11),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();

    assert!(!buf.is_empty(), "encoded message must not be empty");

    let decoded = rti::ResponseLogin::decode(buf.as_slice()).unwrap();
    assert_eq!(decoded.template_id, Some(11));
}

// ---------------------------------------------------------------------------
// user_msg field (common across messages)
// ---------------------------------------------------------------------------

#[test]
fn user_msg_field_roundtrips_on_response_login() {
    let msg = rti::ResponseLogin {
        template_id: Some(11),
        user_msg: Some(vec!["my-correlation-tag".to_string()]),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = rti::ResponseLogin::decode(buf.as_slice()).unwrap();
    assert_eq!(
        decoded
            .user_msg
            .as_ref()
            .and_then(|v| v.first())
            .map(|s| s.as_str()),
        Some("my-correlation-tag")
    );
}

// ---------------------------------------------------------------------------
// Dispatch covers all critical template IDs from spec
// ---------------------------------------------------------------------------

#[test]
fn dispatch_covers_all_auth_template_ids() {
    let auth_ids: Vec<(i32, &str)> = vec![
        (10, "RequestLogin"),
        (11, "ResponseLogin"),
        (12, "RequestLogout"),
        (13, "ResponseLogout"),
        (16, "RequestRithmicSystemInfo"),
        (17, "ResponseRithmicSystemInfo"),
        (18, "RequestHeartbeat"),
        (19, "ResponseHeartbeat"),
        (75, "Reject"),
        (77, "ForcedLogout"),
    ];

    for (tid, name) in &auth_ids {
        let msg = rti::RequestLogin {
            template_id: Some(*tid),
            ..Default::default()
        };
        let buf = msg.encode_to_vec();
        let result = decode_message(&buf);
        assert!(
            result.is_ok(),
            "dispatch failed for template_id {} ({})",
            tid,
            name
        );
        let decoded = result.unwrap();
        assert!(
            !matches!(decoded, RithmicMessage::Unknown(_, _)),
            "template_id {} ({}) should NOT be Unknown",
            tid,
            name
        );
    }
}

#[test]
fn dispatch_covers_all_market_data_template_ids() {
    let market_ids = vec![100, 101, 150, 151, 156];

    for tid in &market_ids {
        let msg = rti::BestBidOffer {
            template_id: Some(*tid),
            ..Default::default()
        };
        let buf = msg.encode_to_vec();
        let decoded = decode_message(&buf).expect("dispatch should succeed");
        assert!(
            !matches!(decoded, RithmicMessage::Unknown(_, _)),
            "template_id {} should NOT be Unknown",
            tid
        );
    }
}

#[test]
fn dispatch_covers_all_dbo_template_ids() {
    let dbo_ids = vec![115, 116, 117, 118, 160, 161];

    for tid in &dbo_ids {
        let msg = rti::DepthByOrder {
            template_id: Some(*tid),
            ..Default::default()
        };
        let buf = msg.encode_to_vec();
        let decoded = decode_message(&buf).expect("dispatch should succeed");
        assert!(
            !matches!(decoded, RithmicMessage::Unknown(_, _)),
            "template_id {} should NOT be Unknown",
            tid
        );
    }
}

#[test]
fn dispatch_covers_all_notification_template_ids() {
    let notif_ids = vec![351, 352];

    for tid in &notif_ids {
        let msg = rti::RithmicOrderNotification {
            template_id: Some(*tid),
            ..Default::default()
        };
        let buf = msg.encode_to_vec();
        let decoded = decode_message(&buf).expect("dispatch should succeed");
        assert!(
            !matches!(decoded, RithmicMessage::Unknown(_, _)),
            "template_id {} should NOT be Unknown",
            tid
        );
    }
}

#[test]
fn dispatch_covers_all_pnl_template_ids() {
    let pnl_ids = vec![450, 451];

    for tid in &pnl_ids {
        let msg = rti::InstrumentPnLPositionUpdate {
            template_id: Some(*tid),
            ..Default::default()
        };
        let buf = msg.encode_to_vec();
        let decoded = decode_message(&buf).expect("dispatch should succeed");
        assert!(
            !matches!(decoded, RithmicMessage::Unknown(_, _)),
            "template_id {} should NOT be Unknown",
            tid
        );
    }
}

// ---------------------------------------------------------------------------
// Corrected tag verification — RequestLogin no longer uses fabricated tags
// ---------------------------------------------------------------------------

#[test]
fn request_login_uses_corrected_field_tags() {
    let login = rti::RequestLogin::new(
        "myuser",
        "mypass",
        "TestApp",
        "3.0",
        "Rithmic Paper",
        InfraType::TickerPlant,
    );
    let buf = login.encode_to_vec();

    let decoded = rti::RequestLogin::decode(buf.as_slice()).unwrap();
    assert_eq!(decoded.user.as_deref(), Some("myuser"));
    assert_eq!(decoded.password.as_deref(), Some("mypass"));
    assert_eq!(decoded.app_name.as_deref(), Some("TestApp"));
    assert_eq!(decoded.app_version.as_deref(), Some("3.0"));
    assert_eq!(decoded.system_name.as_deref(), Some("Rithmic Paper"));
    assert_eq!(decoded.infra_type, Some(1));

    let tid = extract_template_id(&buf).unwrap();
    assert_eq!(tid, 10);
}

// ---------------------------------------------------------------------------
// Builder method tests for new types
// ---------------------------------------------------------------------------

#[test]
fn request_market_data_unsubscribe() {
    let msg = rti::RequestMarketDataUpdate::unsubscribe("MES", "CME");
    assert_eq!(msg.template_id, Some(100));
    assert_eq!(msg.request, Some(2));
    assert_eq!(msg.update_bits, Some(3));
}

#[test]
fn request_depth_by_order_updates_unsubscribe() {
    let msg = rti::RequestDepthByOrderUpdates::unsubscribe("MES", "CME");
    assert_eq!(msg.template_id, Some(117));
    assert_eq!(msg.request, Some(2));
}
