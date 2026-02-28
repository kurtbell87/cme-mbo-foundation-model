//! Rithmic protobuf message types.
//!
//! All messages share `template_id` at protobuf field number 154467.
//! Types that only use `Option<T>` fields use `#[derive(prost::Message)]`.
//! Types with `Option<Vec<String>>` fields implement `prost::Message` manually.

use prost::bytes::{Buf, BufMut};
use prost::encoding::{self, DecodeContext, WireType};
use prost::DecodeError;

use crate::InfraType;

// -------------------------------------------------------------------------
// Tag constant shared by all messages
// -------------------------------------------------------------------------
const TEMPLATE_ID_TAG: u32 = 154467;

// -------------------------------------------------------------------------
// Helper: encode/decode Option<Vec<String>> as repeated string field
// -------------------------------------------------------------------------

fn encode_optional_repeated_string(tag: u32, val: &Option<Vec<String>>, buf: &mut impl BufMut) {
    if let Some(ref vec) = val {
        for s in vec {
            encoding::string::encode(tag, s, buf);
        }
    }
}

fn encoded_len_optional_repeated_string(tag: u32, val: &Option<Vec<String>>) -> usize {
    match val {
        Some(ref vec) => vec.iter().map(|s| encoding::string::encoded_len(tag, s)).sum(),
        None => 0,
    }
}

fn merge_optional_repeated_string(
    val: &mut Option<Vec<String>>,
    wire_type: WireType,
    buf: &mut impl Buf,
    ctx: DecodeContext,
) -> Result<(), DecodeError> {
    let mut s = String::new();
    encoding::string::merge(wire_type, &mut s, buf, ctx)?;
    val.get_or_insert_with(Vec::new).push(s);
    Ok(())
}

// =========================================================================
// Simple types — derive prost::Message
// (prost::Message derive provides Debug + Default, so don't derive them)
// =========================================================================

#[derive(Clone, PartialEq, prost::Message)]
pub struct BestBidOffer {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct LastTrade {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DepthByOrder {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RithmicOrderNotification {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ExchangeOrderNotification {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ResponseHeartbeat {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Reject {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ForcedLogout {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InstrumentPnLPositionUpdate {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AccountPnLPositionUpdate {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

// =========================================================================
// RequestLogin — derive works (no Option<Vec<String>> fields used in tests)
// =========================================================================

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestLogin {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "154013")]
    pub user: Option<String>,
    #[prost(string, optional, tag = "154014")]
    pub password: Option<String>,
    #[prost(string, optional, tag = "154000")]
    pub app_name: Option<String>,
    #[prost(string, optional, tag = "154001")]
    pub app_version: Option<String>,
    #[prost(string, optional, tag = "154002")]
    pub system_name: Option<String>,
    #[prost(int32, optional, tag = "154003")]
    pub infra_type: Option<i32>,
}

impl RequestLogin {
    pub fn new(
        user: &str,
        password: &str,
        app_name: &str,
        app_version: &str,
        system_name: &str,
        infra_type: InfraType,
    ) -> Self {
        Self {
            template_id: Some(10),
            user: Some(user.to_string()),
            password: Some(password.to_string()),
            app_name: Some(app_name.to_string()),
            app_version: Some(app_version.to_string()),
            system_name: Some(system_name.to_string()),
            infra_type: Some(infra_type.as_i32()),
        }
    }
}

// =========================================================================
// ResponseLogin — manual prost::Message (has Option<Vec<String>> fields)
// =========================================================================

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseLogin {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ResponseLogin {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(v) = self.template_id {
            encoding::int32::encode(TEMPLATE_ID_TAG, &v, buf);
        }
        encode_optional_repeated_string(132760, &self.user_msg, buf);
        encode_optional_repeated_string(132766, &self.rp_code, buf);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        match tag {
            TEMPLATE_ID_TAG => {
                let mut v = self.template_id.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.template_id = Some(v);
                Ok(())
            }
            132760 => merge_optional_repeated_string(&mut self.user_msg, wire_type, buf, ctx),
            132766 => merge_optional_repeated_string(&mut self.rp_code, wire_type, buf, ctx),
            _ => encoding::skip_field(wire_type, tag, buf, ctx),
        }
    }

    fn encoded_len(&self) -> usize {
        let mut len = 0;
        if let Some(v) = self.template_id {
            len += encoding::int32::encoded_len(TEMPLATE_ID_TAG, &v);
        }
        len += encoded_len_optional_repeated_string(132760, &self.user_msg);
        len += encoded_len_optional_repeated_string(132766, &self.rp_code);
        len
    }

    fn clear(&mut self) {
        self.template_id = None;
        self.user_msg = None;
        self.rp_code = None;
    }
}

// =========================================================================
// RequestHeartbeat — manual prost::Message (has Option<Vec<String>>)
// =========================================================================

#[derive(Clone, PartialEq, Debug, Default)]
pub struct RequestHeartbeat {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
}

impl prost::Message for RequestHeartbeat {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(v) = self.template_id {
            encoding::int32::encode(TEMPLATE_ID_TAG, &v, buf);
        }
        encode_optional_repeated_string(132760, &self.user_msg, buf);
    }

    fn merge_field(
        &mut self,
        tag: u32,
        wire_type: WireType,
        buf: &mut impl Buf,
        ctx: DecodeContext,
    ) -> Result<(), DecodeError> {
        match tag {
            TEMPLATE_ID_TAG => {
                let mut v = self.template_id.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.template_id = Some(v);
                Ok(())
            }
            132760 => merge_optional_repeated_string(&mut self.user_msg, wire_type, buf, ctx),
            _ => encoding::skip_field(wire_type, tag, buf, ctx),
        }
    }

    fn encoded_len(&self) -> usize {
        let mut len = 0;
        if let Some(v) = self.template_id {
            len += encoding::int32::encoded_len(TEMPLATE_ID_TAG, &v);
        }
        len += encoded_len_optional_repeated_string(132760, &self.user_msg);
        len
    }

    fn clear(&mut self) {
        self.template_id = None;
        self.user_msg = None;
    }
}
