//! WebSocket connection establishment with TLS for Rithmic.

use crate::error::RithmicError;

use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// Type alias for the connected WebSocket stream.
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Connect to a Rithmic WebSocket endpoint.
///
/// If `cert_pem_path` is provided, loads the CA certificate and uses it
/// for TLS verification. Otherwise, uses the system's default CA store.
pub async fn connect(
    uri: &str,
    cert_pem_path: Option<&str>,
) -> Result<WsStream, RithmicError> {
    let connector = if let Some(cert_path) = cert_pem_path {
        let pem_bytes = tokio::fs::read(cert_path)
            .await
            .map_err(|e| RithmicError::Tls(format!("failed to read cert {cert_path}: {e}")))?;

        let cert = native_tls::Certificate::from_pem(&pem_bytes)
            .map_err(|e| RithmicError::Tls(format!("invalid PEM certificate: {e}")))?;

        let tls = native_tls::TlsConnector::builder()
            .add_root_certificate(cert)
            .build()
            .map_err(|e| RithmicError::Tls(format!("TLS connector build failed: {e}")))?;

        Some(tokio_tungstenite::Connector::NativeTls(tls))
    } else {
        None // use default system TLS
    };

    let (ws_stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
        uri,
        None, // WebSocket config
        false, // disable_nagle
        connector,
    )
    .await
    .map_err(|e| RithmicError::WebSocket(format!("connection failed: {e}")))?;

    Ok(ws_stream)
}

/// Encode a protobuf message with a 4-byte big-endian length prefix
/// and wrap it in a WebSocket binary message.
pub fn encode_ws_message(msg: &impl prost::Message) -> WsMessage {
    let payload = msg.encode_to_vec();
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&payload);
    WsMessage::Binary(buf.into())
}

/// Extract the protobuf payload from a length-prefixed WebSocket binary message.
/// Returns None for non-binary messages (ping/pong/close/text).
pub fn decode_ws_payload(msg: &WsMessage) -> Option<&[u8]> {
    match msg {
        WsMessage::Binary(data) if data.len() >= 4 => {
            let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
            let payload = &data[4..];
            if payload.len() >= len {
                Some(&payload[..len])
            } else {
                Some(payload) // partial, decode will fail gracefully
            }
        }
        _ => None,
    }
}
