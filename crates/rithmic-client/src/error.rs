//! Error types for the Rithmic client.

use std::fmt;

/// Errors that can occur during Rithmic client operation.
#[derive(Debug)]
pub enum RithmicError {
    /// WebSocket connection or transport error.
    WebSocket(String),
    /// TLS handshake or certificate error.
    Tls(String),
    /// Failed to decode a protobuf message.
    ProtobufDecode(prost::DecodeError),
    /// Authentication failed (login rejected by server).
    AuthFailed(String),
    /// Heartbeat timeout — no response within 2x heartbeat_interval.
    HeartbeatTimeout,
    /// Server sent a ForcedLogout (77).
    ForcedLogout(String),
    /// Server sent a Reject (75).
    ServerReject(String),
    /// BBO validation found divergence between book and Rithmic BBO.
    BboValidationDivergence {
        book_bid: Option<i64>,
        book_ask: Option<i64>,
        bbo_bid: i64,
        bbo_ask: i64,
    },
    /// Sequence number gap detected in DepthByOrder (160) stream.
    SequenceGap {
        expected: u64,
        received: u64,
    },
    /// Configuration error (missing env var, invalid value, etc.).
    Config(String),
    /// I/O error (file read, cert loading, etc.).
    Io(std::io::Error),
    /// Channel send/receive error.
    Channel(String),
    /// Task join error (spawned task panicked or was cancelled).
    TaskJoin(String),
    /// Book repeatedly diverged and failed to recover — pipeline exiting.
    BookDegraded(String),
}

impl fmt::Display for RithmicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WebSocket(msg) => write!(f, "WebSocket error: {msg}"),
            Self::Tls(msg) => write!(f, "TLS error: {msg}"),
            Self::ProtobufDecode(e) => write!(f, "protobuf decode error: {e}"),
            Self::AuthFailed(msg) => write!(f, "authentication failed: {msg}"),
            Self::HeartbeatTimeout => write!(f, "heartbeat timeout"),
            Self::ForcedLogout(msg) => write!(f, "forced logout: {msg}"),
            Self::ServerReject(msg) => write!(f, "server reject: {msg}"),
            Self::BboValidationDivergence {
                book_bid,
                book_ask,
                bbo_bid,
                bbo_ask,
            } => write!(
                f,
                "BBO validation divergence: book bid={book_bid:?} ask={book_ask:?}, \
                 BBO bid={bbo_bid} ask={bbo_ask}"
            ),
            Self::SequenceGap { expected, received } => {
                write!(f, "sequence gap: expected {expected}, received {received}")
            }
            Self::Config(msg) => write!(f, "config error: {msg}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Channel(msg) => write!(f, "channel error: {msg}"),
            Self::TaskJoin(msg) => write!(f, "task join error: {msg}"),
            Self::BookDegraded(msg) => write!(f, "book degraded: {msg}"),
        }
    }
}

impl std::error::Error for RithmicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ProtobufDecode(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<prost::DecodeError> for RithmicError {
    fn from(e: prost::DecodeError) -> Self {
        Self::ProtobufDecode(e)
    }
}

impl From<std::io::Error> for RithmicError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
