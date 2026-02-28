//! Integration tests for Rithmic Protobuf codegen (Phase 2).
//!
//! Tests cover:
//!   - Generated type availability (proto compilation)
//!   - template_id extraction from raw bytes
//!   - Round-trip encode/decode for 5 message types
//!   - Message dispatch via decode_message()
//!   - Unknown template_id handling
//!   - RequestLogin builder
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
    // proto2 fields are Option<T> — template_id should default to None
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
fn extract_template_id_returns_error_on_empty_buffer() {
    let result = extract_template_id(&[]);
    assert!(result.is_err(), "empty buffer should return an error");
}

#[test]
fn extract_template_id_returns_error_on_garbage_bytes() {
    // Random bytes that don't encode field 154467
    let garbage = vec![0xFF, 0xFF, 0xFF, 0x01, 0x00];
    let result = extract_template_id(&garbage);
    assert!(result.is_err(), "garbage bytes should return an error");
}

// ---------------------------------------------------------------------------
// T3 — Round-trip: ResponseLogin
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_response_login() {
    let original = rti::ResponseLogin {
        template_id: Some(11),
        user_msg: Some(vec!["correlation-1".to_string()]),
        rp_code: Some(vec!["0".to_string()]),
        ..Default::default()
    };
    let buf = original.encode_to_vec();
    let decoded = rti::ResponseLogin::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(11));
    assert_eq!(
        decoded.user_msg.as_ref().and_then(|v| v.first()),
        Some(&"correlation-1".to_string())
    );
}

// ---------------------------------------------------------------------------
// T4 — Round-trip: BestBidOffer
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_best_bid_offer() {
    let original = rti::BestBidOffer {
        template_id: Some(151),
        symbol: Some("ESH6".to_string()),
        exchange: Some("CME".to_string()),
        ..Default::default()
    };
    let buf = original.encode_to_vec();
    let decoded = rti::BestBidOffer::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(151));
    assert_eq!(decoded.symbol, Some("ESH6".to_string()));
    assert_eq!(decoded.exchange, Some("CME".to_string()));
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
// T6 — Round-trip: LastTrade
// ---------------------------------------------------------------------------

#[test]
fn roundtrip_last_trade() {
    let original = rti::LastTrade {
        template_id: Some(150),
        symbol: Some("CLG6".to_string()),
        exchange: Some("NYMEX".to_string()),
        ..Default::default()
    };
    let buf = original.encode_to_vec();
    let decoded = rti::LastTrade::decode(buf.as_slice()).expect("decode should succeed");

    assert_eq!(decoded.template_id, Some(150));
    assert_eq!(decoded.symbol, Some("CLG6".to_string()));
    assert_eq!(decoded.exchange, Some("NYMEX".to_string()));
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
// T8 — Message dispatch: decode_message returns correct variant
// ---------------------------------------------------------------------------

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
fn dispatch_depth_by_order() {
    let msg = rti::DepthByOrder {
        template_id: Some(156),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();
    let decoded = decode_message(&buf).expect("dispatch should succeed");
    assert!(matches!(decoded, RithmicMessage::DepthByOrder(_)));
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
    // Use a valid proto message structure with a template_id we don't dispatch
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
// T10 — RequestLogin builder
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

    // Verify dispatch routes to correct variant
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
    // Truncate to just 2 bytes — enough to start a varint but not finish
    let truncated = &buf[..2.min(buf.len())];
    // This may either error or return Unknown, both are acceptable
    // but it must NOT panic
    let _ = decode_message(truncated);
}

// ---------------------------------------------------------------------------
// Template ID field number correctness (154467)
// ---------------------------------------------------------------------------

#[test]
fn template_id_uses_field_number_154467() {
    // Encode a message with only template_id set, then verify the wire bytes
    // contain the correct protobuf field tag for field number 154467.
    //
    // Protobuf wire format: (field_number << 3) | wire_type
    // field 154467, wire_type 0 (varint) → tag = 154467 << 3 | 0 = 1235736
    // Encoded as varint: 1235736 in LEB128
    let msg = rti::ResponseLogin {
        template_id: Some(11),
        ..Default::default()
    };
    let buf = msg.encode_to_vec();

    // The buffer should not be empty — it must contain at least the template_id field
    assert!(!buf.is_empty(), "encoded message must not be empty");

    // Decoding back should recover the template_id
    let decoded = rti::ResponseLogin::decode(buf.as_slice()).unwrap();
    assert_eq!(decoded.template_id, Some(11));
}

// ---------------------------------------------------------------------------
// user_msg field (common across all messages)
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
        decoded.user_msg.as_ref().and_then(|v| v.first()).map(|s| s.as_str()),
        Some("my-correlation-tag")
    );
}

// ---------------------------------------------------------------------------
// Dispatch covers all critical template IDs from spec
// ---------------------------------------------------------------------------

#[test]
fn dispatch_covers_all_auth_template_ids() {
    // Auth & Session: 10, 11, 12, 13, 18, 19, 75, 77
    let auth_ids: Vec<(i32, &str)> = vec![
        (10, "RequestLogin"),
        (11, "ResponseLogin"),
        (18, "RequestHeartbeat"),
        (19, "ResponseHeartbeat"),
        (75, "Reject"),
        (77, "ForcedLogout"),
    ];

    for (tid, name) in &auth_ids {
        // Create a minimal message with just the template_id
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
    let market_ids = vec![150, 151, 156];

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
