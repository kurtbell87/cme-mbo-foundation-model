# TDD Spec: Rithmic Protobuf Codegen

**Phase:** 2 (Parallel with Phases 0, 1)
**Crate:** `crates/rithmic-client/` (existing stub — extend)
**Priority:** HIGH — prerequisite for Phase 3 (WebSocket client).

---

## Context

Rithmic R|API+ uses Protocol Buffers (proto2 syntax) for message serialization over WebSocket connections. There are 156 `.proto` files in `~/Downloads/0.89.0.0/proto/` (SDK version 0.89.0.0). All protos are in package `rti` and use a non-standard field number `154467` for the `template_id` field that identifies message types.

The `rithmic-client` crate already exists as a stub. We need to:
1. Copy proto files into the repo
2. Set up `prost-build` for proto2 compilation
3. Generate Rust types
4. Create a template ID routing/dispatch system

---

## What to Build

### 1. Proto File Organization

Copy all 156 proto files from `~/Downloads/0.89.0.0/proto/` to `proto/rti/` in the workspace root:

```
proto/
└── rti/
    ├── best_bid_offer.proto
    ├── bracket_updates.proto
    ├── depth_by_order.proto
    ├── exchange_order_notification.proto
    ├── forced_logout.proto
    ├── last_trade.proto
    ├── request_heartbeat.proto
    ├── request_login.proto
    ├── request_logout.proto
    ├── request_market_data_update.proto
    ├── request_new_order.proto
    ├── request_rithmic_system_gateway_info.proto
    ├── request_rithmic_system_info.proto
    ├── response_heartbeat.proto
    ├── response_login.proto
    ├── response_logout.proto
    ├── response_rithmic_system_gateway_info.proto
    ├── response_rithmic_system_info.proto
    ├── rithmic_order_notification.proto
    ├── ... (156 files total)
    └── trade_route.proto
```

### 2. Prost Build Configuration

**File:** `crates/rithmic-client/build.rs`

```rust
fn main() {
    let proto_dir = std::path::Path::new("../../proto/rti");
    let proto_files: Vec<_> = std::fs::read_dir(proto_dir)
        .expect("proto directory not found")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "proto"))
        .collect();

    prost_build::Config::new()
        .out_dir("src/generated")
        .compile_protos(&proto_files, &[proto_dir])
        .expect("proto compilation failed");
}
```

**Key detail:** All Rithmic protos use `syntax = "proto2"`. Prost supports proto2 natively — fields are `Option<T>` instead of bare `T`.

**Key detail:** The `template_id` field uses field number `154467` (very large). Prost handles large field numbers correctly.

### 3. Template ID Routing Table

Every Rithmic protobuf message has a `template_id` field (field number 154467) that identifies the message type. The routing table maps template IDs to Rust types for deserialization.

**Critical template IDs for the trading system:**

#### Authentication & Session
| Template ID | Message | Direction |
|------------|---------|-----------|
| 10 | `RequestLogin` | Client → Server |
| 11 | `ResponseLogin` | Server → Client |
| 12 | `RequestLogout` | Client → Server |
| 13 | `ResponseLogout` | Server → Client |
| 14 | `RequestRithmicSystemGatewayInfo` | Client → Server |
| 15 | `ResponseRithmicSystemGatewayInfo` | Server → Client |
| 16 | `RequestRithmicSystemInfo` | Client → Server |
| 17 | `ResponseRithmicSystemInfo` | Server → Client |
| 18 | `RequestHeartbeat` | Client → Server |
| 19 | `ResponseHeartbeat` | Server → Client |
| 75 | `Reject` | Server → Client |
| 77 | `ForcedLogout` | Server → Client |

#### Market Data
| Template ID | Message | Direction |
|------------|---------|-----------|
| 100 | `RequestMarketDataUpdate` | Client → Server |
| 101 | `ResponseMarketDataUpdate` | Server → Client |
| 150 | `LastTrade` | Server → Client (push) |
| 151 | `BestBidOffer` | Server → Client (push) |
| 156 | `DepthByOrder` | Server → Client (push) |
| 157 | `DepthByOrderEndEvent` | Server → Client (push) |

#### Order Management
| Template ID | Message | Direction |
|------------|---------|-----------|
| 312 | `RequestNewOrder` | Client → Server |
| 313 | `ResponseNewOrder` | Server → Client |
| 314 | `RequestModifyOrder` | Client → Server |
| 315 | `ResponseModifyOrder` | Server → Client |
| 316 | `RequestCancelOrder` | Client → Server |
| 317 | `ResponseCancelOrder` | Server → Client |
| 330 | `RequestBracketOrder` | Client → Server |
| 331 | `ResponseBracketOrder` | Server → Client |
| 308 | `RequestExitPosition` | Client → Server |
| 309 | `ResponseExitPosition` | Server → Client |

#### Notifications
| Template ID | Message | Direction |
|------------|---------|-----------|
| 351 | `RithmicOrderNotification` | Server → Client (push) |
| 352 | `ExchangeOrderNotification` | Server → Client (push) |
| 353 | `BracketUpdates` | Server → Client (push) |

#### PnL
| Template ID | Message | Direction |
|------------|---------|-----------|
| 400 | `RequestPnLPositionSnapshot` | Client → Server |
| 401 | `ResponsePnLPositionSnapshot` | Server → Client |
| 402 | `RequestPnLPositionUpdates` | Client → Server |
| 403 | `ResponsePnLPositionUpdates` | Server → Client |
| 450 | `InstrumentPnLPositionUpdate` | Server → Client (push) |
| 451 | `AccountPnLPositionUpdate` | Server → Client (push) |

### 4. Message Dispatch

Create a dispatch mechanism:

```rust
/// Raw message envelope — just enough to extract template_id
pub fn extract_template_id(buf: &[u8]) -> Result<i32> {
    // Decode just the template_id field (field number 154467)
    // without deserializing the full message
}

/// Typed message enum for all messages we care about
pub enum RithmicMessage {
    // Auth
    RequestLogin(RequestLogin),
    ResponseLogin(ResponseLogin),
    RequestHeartbeat(RequestHeartbeat),
    ResponseHeartbeat(ResponseHeartbeat),
    Reject(Reject),
    ForcedLogout(ForcedLogout),

    // Market Data
    BestBidOffer(BestBidOffer),
    LastTrade(LastTrade),
    DepthByOrder(DepthByOrder),
    DepthByOrderEndEvent(DepthByOrderEndEvent),

    // Orders
    ResponseNewOrder(ResponseNewOrder),
    ResponseModifyOrder(ResponseModifyOrder),
    ResponseCancelOrder(ResponseCancelOrder),
    RithmicOrderNotification(RithmicOrderNotification),
    ExchangeOrderNotification(ExchangeOrderNotification),

    // PnL
    InstrumentPnLPositionUpdate(InstrumentPnLPositionUpdate),
    AccountPnLPositionUpdate(AccountPnLPositionUpdate),

    // Catch-all
    Unknown(i32, Vec<u8>),
}

/// Dispatch raw bytes to typed message
pub fn decode_message(buf: &[u8]) -> Result<RithmicMessage> {
    let template_id = extract_template_id(buf)?;
    match template_id {
        10 => Ok(RithmicMessage::RequestLogin(RequestLogin::decode(buf)?)),
        11 => Ok(RithmicMessage::ResponseLogin(ResponseLogin::decode(buf)?)),
        // ... etc
        id => Ok(RithmicMessage::Unknown(id, buf.to_vec())),
    }
}
```

### 5. Message Serialization Helpers

For client-to-server messages, provide builder helpers:

```rust
impl RequestLogin {
    pub fn new(
        user: &str,
        password: &str,
        app_name: &str,
        app_version: &str,
        system_name: &str,
        infra_type: InfraType,
    ) -> Self;
}

pub enum InfraType {
    TickerPlant = 1,
    OrderPlant = 2,
    HistoryPlant = 3,
    PnLPlant = 4,
    RepositoryPlant = 5,
}
```

---

## Proto File Details

**Syntax:** All files use `syntax = "proto2"` and `package rti;`

**Common pattern:** Every message has:
- `optional int32 template_id = 154467;` — message type identifier
- `optional string user_msg = 132760;` — user-defined correlation tag

**Optional fields:** In proto2, all fields are `optional` by default, generating `Option<T>` in Rust via prost.

---

## Exit Criteria

- [ ] All 156 proto files copied to `proto/rti/` in workspace root
- [ ] `crates/rithmic-client/build.rs` compiles all protos via prost-build
- [ ] Generated Rust types exist in `crates/rithmic-client/src/generated/`
- [ ] `extract_template_id()` correctly reads template_id from raw bytes
- [ ] `RithmicMessage` enum covers all critical message types (auth, market data, orders, PnL)
- [ ] `decode_message()` dispatches to correct type based on template_id
- [ ] Round-trip test: construct → encode → decode → verify for at least 5 message types
- [ ] `RequestLogin` builder sets correct fields including `infra_type`
- [ ] All tests pass, `cargo build` succeeds for the workspace

---

## Test Plan

### RED Phase Tests

**T1: Proto compilation** — `cargo build` succeeds, all 156 protos compile without errors.

**T2: Template ID extraction** — Encode a `ResponseLogin` with known `template_id = 11`, verify `extract_template_id()` returns 11.

**T3: Round-trip: ResponseLogin** — Create `ResponseLogin` with fields set, encode to bytes, decode back, verify all fields match.

**T4: Round-trip: BestBidOffer** — Same pattern for market data message.

**T5: Round-trip: RithmicOrderNotification** — Same pattern for order notification.

**T6: Round-trip: LastTrade** — Same pattern for trade data.

**T7: Round-trip: RequestHeartbeat** — Same pattern for heartbeat.

**T8: Message dispatch** — Encode several different message types, pass through `decode_message()`, verify correct `RithmicMessage` variant returned.

**T9: Unknown template ID** — Encode a message with an unrecognized template_id, verify `RithmicMessage::Unknown` is returned (not an error).

**T10: RequestLogin builder** — Construct with known params, verify all fields are set correctly.

### GREEN Phase Implementation

1. Copy proto files: `cp -r ~/Downloads/0.89.0.0/proto/*.proto proto/rti/`
2. Create `proto/rti/` directory in repo
3. Add `prost` and `prost-build` dependencies to `crates/rithmic-client/Cargo.toml`
4. Write `build.rs` for proto compilation
5. Create `src/generated/` module for generated code
6. Implement `extract_template_id()` using low-level protobuf varint parsing
7. Define `RithmicMessage` enum
8. Implement `decode_message()` dispatch table
9. Implement `RequestLogin` and other message builders
10. Write all tests

### Notes on Proto2 + Prost

- Prost generates `Option<T>` for proto2 optional fields
- Large field numbers (154467) work fine — prost uses varint encoding
- Package `rti` maps to `mod rti { ... }` in Rust
- If proto files have imports between each other, prost-build handles this automatically when all files are compiled together
