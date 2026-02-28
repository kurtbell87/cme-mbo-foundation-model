// Rithmic WebSocket client (Phase 6 — NEW CODE, not migration)
//
// Will provide:
// - Protobuf codegen from 161 Rithmic proto files via prost
// - WebSocket + TLS connections to 5 plant types (Ticker, Order, History, PnL, Repository)
// - Market data subscription → live book building → feature computation → inference
//
// This is entirely new code — no Rithmic integration exists in the C++ codebase.
