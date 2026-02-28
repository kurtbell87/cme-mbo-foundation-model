pub mod rti;

use prost::Message;

// ---------------------------------------------------------------------------
// InfraType
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfraType {
    TickerPlant,
    OrderPlant,
    HistoryPlant,
    PnLPlant,
    RepositoryPlant,
}

impl InfraType {
    pub fn as_i32(self) -> i32 {
        match self {
            InfraType::TickerPlant => 1,
            InfraType::OrderPlant => 2,
            InfraType::HistoryPlant => 3,
            InfraType::PnLPlant => 4,
            InfraType::RepositoryPlant => 5,
        }
    }
}

// ---------------------------------------------------------------------------
// RithmicMessage
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum RithmicMessage {
    RequestLogin(rti::RequestLogin),
    ResponseLogin(rti::ResponseLogin),
    BestBidOffer(rti::BestBidOffer),
    LastTrade(rti::LastTrade),
    DepthByOrder(rti::DepthByOrder),
    RithmicOrderNotification(rti::RithmicOrderNotification),
    ExchangeOrderNotification(rti::ExchangeOrderNotification),
    RequestHeartbeat(rti::RequestHeartbeat),
    ResponseHeartbeat(rti::ResponseHeartbeat),
    Reject(rti::Reject),
    ForcedLogout(rti::ForcedLogout),
    InstrumentPnLPositionUpdate(rti::InstrumentPnLPositionUpdate),
    AccountPnLPositionUpdate(rti::AccountPnLPositionUpdate),
    Unknown(i32, Vec<u8>),
}

// ---------------------------------------------------------------------------
// extract_template_id
// ---------------------------------------------------------------------------

/// Helper struct to extract just the template_id from any Rithmic message.
#[derive(Clone, PartialEq, prost::Message)]
struct TemplateIdOnly {
    #[prost(int32, optional, tag = "154467")]
    template_id: Option<i32>,
}

/// Extract the template_id (field 154467) from raw protobuf bytes.
pub fn extract_template_id(buf: &[u8]) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    if buf.is_empty() {
        return Err("empty buffer".into());
    }
    let msg = TemplateIdOnly::decode(buf)?;
    msg.template_id.ok_or_else(|| "no template_id field found".into())
}

// ---------------------------------------------------------------------------
// decode_message
// ---------------------------------------------------------------------------

/// Decode a raw protobuf message into the appropriate RithmicMessage variant
/// based on the template_id.
pub fn decode_message(buf: &[u8]) -> Result<RithmicMessage, Box<dyn std::error::Error + Send + Sync>> {
    if buf.is_empty() {
        return Err("empty buffer".into());
    }

    let tid = extract_template_id(buf)?;

    match tid {
        10 => Ok(RithmicMessage::RequestLogin(
            rti::RequestLogin::decode(buf)?,
        )),
        11 => Ok(RithmicMessage::ResponseLogin(
            rti::ResponseLogin::decode(buf)?,
        )),
        18 => Ok(RithmicMessage::RequestHeartbeat(
            rti::RequestHeartbeat::decode(buf)?,
        )),
        19 => Ok(RithmicMessage::ResponseHeartbeat(
            rti::ResponseHeartbeat::decode(buf)?,
        )),
        75 => Ok(RithmicMessage::Reject(rti::Reject::decode(buf)?)),
        77 => Ok(RithmicMessage::ForcedLogout(
            rti::ForcedLogout::decode(buf)?,
        )),
        150 => Ok(RithmicMessage::LastTrade(rti::LastTrade::decode(buf)?)),
        151 => Ok(RithmicMessage::BestBidOffer(
            rti::BestBidOffer::decode(buf)?,
        )),
        156 => Ok(RithmicMessage::DepthByOrder(
            rti::DepthByOrder::decode(buf)?,
        )),
        351 => Ok(RithmicMessage::RithmicOrderNotification(
            rti::RithmicOrderNotification::decode(buf)?,
        )),
        352 => Ok(RithmicMessage::ExchangeOrderNotification(
            rti::ExchangeOrderNotification::decode(buf)?,
        )),
        450 => Ok(RithmicMessage::InstrumentPnLPositionUpdate(
            rti::InstrumentPnLPositionUpdate::decode(buf)?,
        )),
        451 => Ok(RithmicMessage::AccountPnLPositionUpdate(
            rti::AccountPnLPositionUpdate::decode(buf)?,
        )),
        _ => Ok(RithmicMessage::Unknown(tid, buf.to_vec())),
    }
}
