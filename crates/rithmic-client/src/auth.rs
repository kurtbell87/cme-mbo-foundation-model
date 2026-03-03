//! Two-phase authentication for Rithmic connections.
//!
//! Phase 1: Connect → RequestRithmicSystemInfo(16) → ResponseRithmicSystemInfo(17)
//!          → extract system_name → server disconnects
//! Phase 2: Reconnect → RequestLogin(10) → ResponseLogin(11)
//!          → extract heartbeat_interval

use futures_util::{SinkExt, StreamExt};

use crate::connection::{connect, decode_ws_payload, encode_ws_message, WsStream};
use crate::error::RithmicError;
use crate::rti;
use crate::{extract_template_id, InfraType};

/// Result of two-phase authentication.
#[derive(Debug)]
pub struct AuthResult {
    /// The authenticated WebSocket stream (from phase 2).
    pub ws_stream: WsStream,
    /// The system name chosen during phase 1.
    pub system_name: String,
    /// Heartbeat interval in seconds (from ResponseLogin).
    pub heartbeat_interval: u64,
}

/// Phase 1: Discover available system names.
///
/// Connects, sends RequestRithmicSystemInfo(16), receives response with
/// available system names, then the server disconnects.
pub async fn discover_system_names(
    uri: &str,
    cert_path: Option<&str>,
) -> Result<Vec<String>, RithmicError> {
    let mut ws = connect(uri, cert_path).await?;

    // Send RequestRithmicSystemInfo
    let req = rti::RequestRithmicSystemInfo::new();
    ws.send(encode_ws_message(&req))
        .await
        .map_err(|e| RithmicError::WebSocket(format!("send system info request: {e}")))?;

    // Read response
    while let Some(msg_result) = ws.next().await {
        let msg = msg_result
            .map_err(|e| RithmicError::WebSocket(format!("read system info response: {e}")))?;

        if let Some(payload) = decode_ws_payload(&msg) {
            let tid = match extract_template_id(payload) {
                Ok(t) => t,
                Err(_) => continue,
            };

            if tid == 17 {
                let resp = <rti::ResponseRithmicSystemInfo as prost::Message>::decode(payload)?;
                return Ok(resp.system_name.unwrap_or_default());
            }
        }
    }

    Err(RithmicError::AuthFailed(
        "server closed before sending system info response".to_string(),
    ))
}

/// Full two-phase authentication.
///
/// 1. Discovers system names, picks the first (or `preferred_system` if provided)
/// 2. Reconnects and logs in with the chosen system name
/// 3. Returns the authenticated WebSocket stream + metadata
pub async fn authenticate(
    uri: &str,
    cert_path: Option<&str>,
    user: &str,
    password: &str,
    app_name: &str,
    app_version: &str,
    preferred_system: Option<&str>,
    infra_type: InfraType,
) -> Result<AuthResult, RithmicError> {
    // Phase 1: discover system names
    let system_names = discover_system_names(uri, cert_path).await?;

    if system_names.is_empty() {
        return Err(RithmicError::AuthFailed(
            "no system names returned".to_string(),
        ));
    }

    eprintln!("[auth] available systems: {:?}", system_names);

    // Pick system name: use preferred if available, otherwise first
    let system_name = if let Some(pref) = preferred_system {
        if system_names.iter().any(|s| s == pref) {
            pref.to_string()
        } else {
            return Err(RithmicError::AuthFailed(format!(
                "preferred system '{}' not in available systems: {:?}",
                pref, system_names
            )));
        }
    } else {
        system_names[0].clone()
    };

    // Phase 2: reconnect and login
    let mut ws = connect(uri, cert_path).await?;

    let login = rti::RequestLogin::new(
        user,
        password,
        app_name,
        app_version,
        &system_name,
        infra_type,
    );

    ws.send(encode_ws_message(&login))
        .await
        .map_err(|e| RithmicError::WebSocket(format!("send login: {e}")))?;

    // Read login response
    while let Some(msg_result) = ws.next().await {
        let msg = msg_result
            .map_err(|e| RithmicError::WebSocket(format!("read login response: {e}")))?;

        if let Some(payload) = decode_ws_payload(&msg) {
            let tid = match extract_template_id(payload) {
                Ok(t) => t,
                Err(_) => continue,
            };

            match tid {
                11 => {
                    let resp = <rti::ResponseLogin as prost::Message>::decode(payload)?;

                    // Check rp_code for success
                    let rp_codes = resp.rp_code.as_deref().unwrap_or(&[]);
                    let is_success = rp_codes.iter().any(|c| c == "0");

                    if !is_success {
                        let user_msgs = resp.user_msg.as_deref().unwrap_or(&[]);
                        return Err(RithmicError::AuthFailed(format!(
                            "login rejected: rp_code={:?}, user_msg={:?}",
                            rp_codes, user_msgs
                        )));
                    }

                    let heartbeat_interval = resp.heartbeat_interval.unwrap_or(60.0) as u64;

                    return Ok(AuthResult {
                        ws_stream: ws,
                        system_name,
                        heartbeat_interval,
                    });
                }
                75 => {
                    let reject = <rti::Reject as prost::Message>::decode(payload)?;
                    return Err(RithmicError::ServerReject(format!(
                        "login rejected: {:?}",
                        reject.user_msg
                    )));
                }
                _ => {
                    // Skip unexpected messages during auth
                }
            }
        }
    }

    Err(RithmicError::AuthFailed(
        "server closed before sending login response".to_string(),
    ))
}
