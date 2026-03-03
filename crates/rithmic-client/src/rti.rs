//! Rithmic protobuf message types.
//!
//! All messages share `template_id` at protobuf field number 154467.
//! Types that only use `Option<T>` fields use `#[derive(prost::Message)]`.
//! Types with `Option<Vec<String>>` or repeated numeric fields implement
//! `prost::Message` manually.

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
        Some(ref vec) => vec
            .iter()
            .map(|s| encoding::string::encoded_len(tag, s))
            .sum(),
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

// -------------------------------------------------------------------------
// Helper: encode/decode Option<Vec<i32>> as repeated int32 field
// -------------------------------------------------------------------------

fn encode_optional_repeated_int32(tag: u32, val: &Option<Vec<i32>>, buf: &mut impl BufMut) {
    if let Some(ref vec) = val {
        for v in vec {
            encoding::int32::encode(tag, v, buf);
        }
    }
}

fn encoded_len_optional_repeated_int32(tag: u32, val: &Option<Vec<i32>>) -> usize {
    match val {
        Some(ref vec) => vec
            .iter()
            .map(|v| encoding::int32::encoded_len(tag, v))
            .sum(),
        None => 0,
    }
}

fn merge_optional_repeated_int32(
    val: &mut Option<Vec<i32>>,
    wire_type: WireType,
    buf: &mut impl Buf,
    ctx: DecodeContext,
) -> Result<(), DecodeError> {
    let mut v = 0i32;
    encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
    val.get_or_insert_with(Vec::new).push(v);
    Ok(())
}

// -------------------------------------------------------------------------
// Helper: encode/decode Option<Vec<f64>> as repeated double field
// -------------------------------------------------------------------------

fn encode_optional_repeated_double(tag: u32, val: &Option<Vec<f64>>, buf: &mut impl BufMut) {
    if let Some(ref vec) = val {
        for v in vec {
            encoding::double::encode(tag, v, buf);
        }
    }
}

fn encoded_len_optional_repeated_double(tag: u32, val: &Option<Vec<f64>>) -> usize {
    match val {
        Some(ref vec) => vec
            .iter()
            .map(|v| encoding::double::encoded_len(tag, v))
            .sum(),
        None => 0,
    }
}

fn merge_optional_repeated_double(
    val: &mut Option<Vec<f64>>,
    wire_type: WireType,
    buf: &mut impl Buf,
    ctx: DecodeContext,
) -> Result<(), DecodeError> {
    let mut v = 0.0f64;
    encoding::double::merge(wire_type, &mut v, buf, ctx)?;
    val.get_or_insert_with(Vec::new).push(v);
    Ok(())
}

// =========================================================================
// Auth & Session Messages
// =========================================================================

// ---- RequestRithmicSystemInfo (16) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestRithmicSystemInfo {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

impl RequestRithmicSystemInfo {
    pub fn new() -> Self {
        Self {
            template_id: Some(16),
        }
    }
}

// ---- ResponseRithmicSystemInfo (17) ----
// Has repeated string fields: system_name, user_msg, rp_code

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseRithmicSystemInfo {
    pub template_id: Option<i32>,
    pub system_name: Option<Vec<String>>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ResponseRithmicSystemInfo {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(v) = self.template_id {
            encoding::int32::encode(TEMPLATE_ID_TAG, &v, buf);
        }
        encode_optional_repeated_string(153628, &self.system_name, buf);
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
            153628 => merge_optional_repeated_string(&mut self.system_name, wire_type, buf, ctx),
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
        len += encoded_len_optional_repeated_string(153628, &self.system_name);
        len += encoded_len_optional_repeated_string(132760, &self.user_msg);
        len += encoded_len_optional_repeated_string(132766, &self.rp_code);
        len
    }

    fn clear(&mut self) {
        self.template_id = None;
        self.system_name = None;
        self.user_msg = None;
        self.rp_code = None;
    }
}

// ---- RequestRithmicSystemGatewayInfo (14) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestRithmicSystemGatewayInfo {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "153628")]
    pub system_name: Option<String>,
}

impl RequestRithmicSystemGatewayInfo {
    pub fn new(system_name: &str) -> Self {
        Self {
            template_id: Some(14),
            system_name: Some(system_name.to_string()),
        }
    }
}

// ---- ResponseRithmicSystemGatewayInfo (15) ----
// Has repeated string fields: gateway_name, gateway_uri

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseRithmicSystemGatewayInfo {
    pub template_id: Option<i32>,
    pub system_name: Option<String>,
    pub gateway_name: Option<Vec<String>>,
    pub gateway_uri: Option<Vec<String>>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ResponseRithmicSystemGatewayInfo {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(v) = self.template_id {
            encoding::int32::encode(TEMPLATE_ID_TAG, &v, buf);
        }
        if let Some(ref s) = self.system_name {
            encoding::string::encode(153628, s, buf);
        }
        encode_optional_repeated_string(153640, &self.gateway_name, buf);
        encode_optional_repeated_string(153641, &self.gateway_uri, buf);
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
            153628 => {
                let mut s = self.system_name.take().unwrap_or_default();
                encoding::string::merge(wire_type, &mut s, buf, ctx)?;
                self.system_name = Some(s);
                Ok(())
            }
            153640 => merge_optional_repeated_string(&mut self.gateway_name, wire_type, buf, ctx),
            153641 => merge_optional_repeated_string(&mut self.gateway_uri, wire_type, buf, ctx),
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
        if let Some(ref s) = self.system_name {
            len += encoding::string::encoded_len(153628, s);
        }
        len += encoded_len_optional_repeated_string(153640, &self.gateway_name);
        len += encoded_len_optional_repeated_string(153641, &self.gateway_uri);
        len += encoded_len_optional_repeated_string(132760, &self.user_msg);
        len += encoded_len_optional_repeated_string(132766, &self.rp_code);
        len
    }

    fn clear(&mut self) {
        self.template_id = None;
        self.system_name = None;
        self.gateway_name = None;
        self.gateway_uri = None;
        self.user_msg = None;
        self.rp_code = None;
    }
}

// ---- RequestLogin (10) — CORRECTED FIELD TAGS ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestLogin {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "153634")]
    pub template_version: Option<String>,
    #[prost(string, optional, tag = "131003")]
    pub user: Option<String>,
    #[prost(string, optional, tag = "130004")]
    pub password: Option<String>,
    #[prost(string, optional, tag = "130002")]
    pub app_name: Option<String>,
    #[prost(string, optional, tag = "131803")]
    pub app_version: Option<String>,
    #[prost(string, optional, tag = "153628")]
    pub system_name: Option<String>,
    #[prost(int32, optional, tag = "153621")]
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
            template_version: Some("3.9".to_string()),
            user: Some(user.to_string()),
            password: Some(password.to_string()),
            app_name: Some(app_name.to_string()),
            app_version: Some(app_version.to_string()),
            system_name: Some(system_name.to_string()),
            infra_type: Some(infra_type.as_i32()),
        }
    }
}

// ---- ResponseLogin (11) — manual (has repeated strings) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseLogin {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
    pub heartbeat_interval: Option<f64>,
    pub unique_user_id: Option<String>,
}

impl prost::Message for ResponseLogin {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(v) = self.template_id {
            encoding::int32::encode(TEMPLATE_ID_TAG, &v, buf);
        }
        encode_optional_repeated_string(132760, &self.user_msg, buf);
        encode_optional_repeated_string(132766, &self.rp_code, buf);
        if let Some(v) = self.heartbeat_interval {
            encoding::double::encode(153633, &v, buf);
        }
        if let Some(ref v) = self.unique_user_id {
            encoding::string::encode(153428, v, buf);
        }
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
            153633 => {
                let mut v = self.heartbeat_interval.unwrap_or_default();
                encoding::double::merge(wire_type, &mut v, buf, ctx)?;
                self.heartbeat_interval = Some(v);
                Ok(())
            }
            153428 => {
                let mut v = self.unique_user_id.take().unwrap_or_default();
                encoding::string::merge(wire_type, &mut v, buf, ctx)?;
                self.unique_user_id = Some(v);
                Ok(())
            }
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
        if let Some(v) = self.heartbeat_interval {
            len += encoding::double::encoded_len(153633, &v);
        }
        if let Some(ref v) = self.unique_user_id {
            len += encoding::string::encoded_len(153428, v);
        }
        len
    }

    fn clear(&mut self) {
        self.template_id = None;
        self.user_msg = None;
        self.rp_code = None;
        self.heartbeat_interval = None;
        self.unique_user_id = None;
    }
}

// ---- RequestLogout (12) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestLogout {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
}

impl RequestLogout {
    pub fn new() -> Self {
        Self {
            template_id: Some(12),
        }
    }
}

// ---- ResponseLogout (13) — manual (has repeated strings) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseLogout {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ResponseLogout {
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

// ---- RequestHeartbeat (18) — manual (has repeated strings) ----

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

// ---- ResponseHeartbeat (19) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct ResponseHeartbeat {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(int32, optional, tag = "150100")]
    pub ssboe: Option<i32>,
    #[prost(int32, optional, tag = "150101")]
    pub usecs: Option<i32>,
}

// ---- Reject (75) — manual (has repeated strings) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct Reject {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for Reject {
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

// ---- ForcedLogout (77) — manual (has repeated strings) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ForcedLogout {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ForcedLogout {
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
// Market Data Subscription Messages
// =========================================================================

// ---- RequestMarketDataUpdate (100) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestMarketDataUpdate {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
    #[prost(int32, optional, tag = "100000")]
    pub request: Option<i32>,
    #[prost(int32, optional, tag = "154211")]
    pub update_bits: Option<i32>,
}

impl RequestMarketDataUpdate {
    /// Subscribe to BBO + LastTrade updates.
    /// update_bits: 1=LAST_TRADE, 2=BBO → 3=both
    pub fn subscribe(symbol: &str, exchange: &str) -> Self {
        Self {
            template_id: Some(100),
            symbol: Some(symbol.to_string()),
            exchange: Some(exchange.to_string()),
            request: Some(1), // SUBSCRIBE
            update_bits: Some(3), // LAST_TRADE | BBO
        }
    }

    pub fn unsubscribe(symbol: &str, exchange: &str) -> Self {
        Self {
            template_id: Some(100),
            symbol: Some(symbol.to_string()),
            exchange: Some(exchange.to_string()),
            request: Some(2), // UNSUBSCRIBE
            update_bits: Some(3),
        }
    }
}

// ---- ResponseMarketDataUpdate (101) — manual (has repeated strings) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseMarketDataUpdate {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ResponseMarketDataUpdate {
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
// Market Data Messages (received from server)
// =========================================================================

// ---- BestBidOffer (151) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct BestBidOffer {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
    #[prost(int32, optional, tag = "149138")]
    pub presence_bits: Option<i32>,
    #[prost(int32, optional, tag = "154571")]
    pub clear_bits: Option<i32>,
    #[prost(double, optional, tag = "100022")]
    pub bid_price: Option<f64>,
    #[prost(int32, optional, tag = "100030")]
    pub bid_size: Option<i32>,
    #[prost(int32, optional, tag = "154403")]
    pub bid_orders: Option<i32>,
    #[prost(int32, optional, tag = "154867")]
    pub bid_implicit_size: Option<i32>,
    #[prost(double, optional, tag = "100025")]
    pub ask_price: Option<f64>,
    #[prost(int32, optional, tag = "100031")]
    pub ask_size: Option<i32>,
    #[prost(int32, optional, tag = "154404")]
    pub ask_orders: Option<i32>,
    #[prost(int32, optional, tag = "154868")]
    pub ask_implicit_size: Option<i32>,
    #[prost(int32, optional, tag = "150100")]
    pub ssboe: Option<i32>,
    #[prost(int32, optional, tag = "150101")]
    pub usecs: Option<i32>,
}

// ---- LastTrade (150) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct LastTrade {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
    #[prost(double, optional, tag = "100006")]
    pub trade_price: Option<f64>,
    #[prost(int32, optional, tag = "100178")]
    pub trade_size: Option<i32>,
    #[prost(int32, optional, tag = "112003")]
    pub aggressor: Option<i32>,
    #[prost(int64, optional, tag = "100032")]
    pub volume: Option<i64>,
    #[prost(int32, optional, tag = "150100")]
    pub ssboe: Option<i32>,
    #[prost(int32, optional, tag = "150101")]
    pub usecs: Option<i32>,
    #[prost(int32, optional, tag = "150400")]
    pub source_ssboe: Option<i32>,
    #[prost(int32, optional, tag = "150401")]
    pub source_usecs: Option<i32>,
    #[prost(int32, optional, tag = "150404")]
    pub source_nsecs: Option<i32>,
}

// ---- OrderBook (156) — L2 book, NOT MBO ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct OrderBook {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
}

// =========================================================================
// Depth-By-Order (MBO) Messages
// =========================================================================

// ---- RequestDepthByOrderSnapshot (115) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestDepthByOrderSnapshot {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
}

impl RequestDepthByOrderSnapshot {
    pub fn new(symbol: &str, exchange: &str) -> Self {
        Self {
            template_id: Some(115),
            symbol: Some(symbol.to_string()),
            exchange: Some(exchange.to_string()),
        }
    }
}

// ---- ResponseDepthByOrderSnapshot (116) — manual (repeated fields) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseDepthByOrderSnapshot {
    pub template_id: Option<i32>,
    pub symbol: Option<String>,
    pub exchange: Option<String>,
    pub update_type: Option<Vec<i32>>,
    pub transaction_type: Option<Vec<i32>>,
    pub depth_price: Option<Vec<f64>>,
    pub depth_size: Option<Vec<i32>>,
    pub exchange_order_id: Option<Vec<String>>,
    pub sequence_number: Option<u64>,
    pub ssboe: Option<i32>,
    pub usecs: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ResponseDepthByOrderSnapshot {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(v) = self.template_id {
            encoding::int32::encode(TEMPLATE_ID_TAG, &v, buf);
        }
        if let Some(ref v) = self.symbol {
            encoding::string::encode(110100, v, buf);
        }
        if let Some(ref v) = self.exchange {
            encoding::string::encode(110101, v, buf);
        }
        encode_optional_repeated_int32(110121, &self.update_type, buf);
        encode_optional_repeated_int32(153612, &self.transaction_type, buf);
        encode_optional_repeated_double(154405, &self.depth_price, buf);
        encode_optional_repeated_int32(154406, &self.depth_size, buf);
        encode_optional_repeated_string(149238, &self.exchange_order_id, buf);
        if let Some(v) = self.sequence_number {
            encoding::uint64::encode(112002, &v, buf);
        }
        if let Some(v) = self.ssboe {
            encoding::int32::encode(150100, &v, buf);
        }
        if let Some(v) = self.usecs {
            encoding::int32::encode(150101, &v, buf);
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
            110100 => {
                let mut v = self.symbol.take().unwrap_or_default();
                encoding::string::merge(wire_type, &mut v, buf, ctx)?;
                self.symbol = Some(v);
                Ok(())
            }
            110101 => {
                let mut v = self.exchange.take().unwrap_or_default();
                encoding::string::merge(wire_type, &mut v, buf, ctx)?;
                self.exchange = Some(v);
                Ok(())
            }
            110121 => merge_optional_repeated_int32(&mut self.update_type, wire_type, buf, ctx),
            153612 => {
                merge_optional_repeated_int32(&mut self.transaction_type, wire_type, buf, ctx)
            }
            154405 => merge_optional_repeated_double(&mut self.depth_price, wire_type, buf, ctx),
            154406 => merge_optional_repeated_int32(&mut self.depth_size, wire_type, buf, ctx),
            149238 => {
                merge_optional_repeated_string(&mut self.exchange_order_id, wire_type, buf, ctx)
            }
            112002 => {
                let mut v = self.sequence_number.unwrap_or_default();
                encoding::uint64::merge(wire_type, &mut v, buf, ctx)?;
                self.sequence_number = Some(v);
                Ok(())
            }
            150100 => {
                let mut v = self.ssboe.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.ssboe = Some(v);
                Ok(())
            }
            150101 => {
                let mut v = self.usecs.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.usecs = Some(v);
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
        if let Some(ref v) = self.symbol {
            len += encoding::string::encoded_len(110100, v);
        }
        if let Some(ref v) = self.exchange {
            len += encoding::string::encoded_len(110101, v);
        }
        len += encoded_len_optional_repeated_int32(110121, &self.update_type);
        len += encoded_len_optional_repeated_int32(153612, &self.transaction_type);
        len += encoded_len_optional_repeated_double(154405, &self.depth_price);
        len += encoded_len_optional_repeated_int32(154406, &self.depth_size);
        len += encoded_len_optional_repeated_string(149238, &self.exchange_order_id);
        if let Some(v) = self.sequence_number {
            len += encoding::uint64::encoded_len(112002, &v);
        }
        if let Some(v) = self.ssboe {
            len += encoding::int32::encoded_len(150100, &v);
        }
        if let Some(v) = self.usecs {
            len += encoding::int32::encoded_len(150101, &v);
        }
        len += encoded_len_optional_repeated_string(132760, &self.user_msg);
        len += encoded_len_optional_repeated_string(132766, &self.rp_code);
        len
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

// ---- RequestDepthByOrderUpdates (117) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestDepthByOrderUpdates {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
    #[prost(int32, optional, tag = "100000")]
    pub request: Option<i32>,
}

impl RequestDepthByOrderUpdates {
    pub fn subscribe(symbol: &str, exchange: &str) -> Self {
        Self {
            template_id: Some(117),
            symbol: Some(symbol.to_string()),
            exchange: Some(exchange.to_string()),
            request: Some(1), // SUBSCRIBE
        }
    }

    pub fn unsubscribe(symbol: &str, exchange: &str) -> Self {
        Self {
            template_id: Some(117),
            symbol: Some(symbol.to_string()),
            exchange: Some(exchange.to_string()),
            request: Some(2), // UNSUBSCRIBE
        }
    }
}

// ---- ResponseDepthByOrderUpdates (118) — manual (has repeated strings) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct ResponseDepthByOrderUpdates {
    pub template_id: Option<i32>,
    pub user_msg: Option<Vec<String>>,
    pub rp_code: Option<Vec<String>>,
}

impl prost::Message for ResponseDepthByOrderUpdates {
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

// ---- DepthByOrder (160) — MBO updates — manual (repeated fields) ----

#[derive(Clone, PartialEq, Debug, Default)]
pub struct DepthByOrder {
    pub template_id: Option<i32>,
    pub symbol: Option<String>,
    pub exchange: Option<String>,
    pub sequence_number: Option<u64>,
    pub update_type: Option<Vec<i32>>,
    pub transaction_type: Option<Vec<i32>>,
    pub depth_price: Option<Vec<f64>>,
    pub depth_size: Option<Vec<i32>>,
    pub exchange_order_id: Option<Vec<String>>,
    pub ssboe: Option<i32>,
    pub usecs: Option<i32>,
    pub source_ssboe: Option<i32>,
    pub source_usecs: Option<i32>,
    pub source_nsecs: Option<i32>,
}

impl prost::Message for DepthByOrder {
    fn encode_raw(&self, buf: &mut impl BufMut) {
        if let Some(v) = self.template_id {
            encoding::int32::encode(TEMPLATE_ID_TAG, &v, buf);
        }
        if let Some(ref v) = self.symbol {
            encoding::string::encode(110100, v, buf);
        }
        if let Some(ref v) = self.exchange {
            encoding::string::encode(110101, v, buf);
        }
        if let Some(v) = self.sequence_number {
            encoding::uint64::encode(112002, &v, buf);
        }
        encode_optional_repeated_int32(110121, &self.update_type, buf);
        encode_optional_repeated_int32(153612, &self.transaction_type, buf);
        encode_optional_repeated_double(154405, &self.depth_price, buf);
        encode_optional_repeated_int32(154406, &self.depth_size, buf);
        encode_optional_repeated_string(149238, &self.exchange_order_id, buf);
        if let Some(v) = self.ssboe {
            encoding::int32::encode(150100, &v, buf);
        }
        if let Some(v) = self.usecs {
            encoding::int32::encode(150101, &v, buf);
        }
        if let Some(v) = self.source_ssboe {
            encoding::int32::encode(150400, &v, buf);
        }
        if let Some(v) = self.source_usecs {
            encoding::int32::encode(150401, &v, buf);
        }
        if let Some(v) = self.source_nsecs {
            encoding::int32::encode(150404, &v, buf);
        }
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
            110100 => {
                let mut v = self.symbol.take().unwrap_or_default();
                encoding::string::merge(wire_type, &mut v, buf, ctx)?;
                self.symbol = Some(v);
                Ok(())
            }
            110101 => {
                let mut v = self.exchange.take().unwrap_or_default();
                encoding::string::merge(wire_type, &mut v, buf, ctx)?;
                self.exchange = Some(v);
                Ok(())
            }
            112002 => {
                let mut v = self.sequence_number.unwrap_or_default();
                encoding::uint64::merge(wire_type, &mut v, buf, ctx)?;
                self.sequence_number = Some(v);
                Ok(())
            }
            110121 => merge_optional_repeated_int32(&mut self.update_type, wire_type, buf, ctx),
            153612 => {
                merge_optional_repeated_int32(&mut self.transaction_type, wire_type, buf, ctx)
            }
            154405 => merge_optional_repeated_double(&mut self.depth_price, wire_type, buf, ctx),
            154406 => merge_optional_repeated_int32(&mut self.depth_size, wire_type, buf, ctx),
            149238 => {
                merge_optional_repeated_string(&mut self.exchange_order_id, wire_type, buf, ctx)
            }
            150100 => {
                let mut v = self.ssboe.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.ssboe = Some(v);
                Ok(())
            }
            150101 => {
                let mut v = self.usecs.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.usecs = Some(v);
                Ok(())
            }
            150400 => {
                let mut v = self.source_ssboe.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.source_ssboe = Some(v);
                Ok(())
            }
            150401 => {
                let mut v = self.source_usecs.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.source_usecs = Some(v);
                Ok(())
            }
            150404 => {
                let mut v = self.source_nsecs.unwrap_or_default();
                encoding::int32::merge(wire_type, &mut v, buf, ctx)?;
                self.source_nsecs = Some(v);
                Ok(())
            }
            _ => encoding::skip_field(wire_type, tag, buf, ctx),
        }
    }

    fn encoded_len(&self) -> usize {
        let mut len = 0;
        if let Some(v) = self.template_id {
            len += encoding::int32::encoded_len(TEMPLATE_ID_TAG, &v);
        }
        if let Some(ref v) = self.symbol {
            len += encoding::string::encoded_len(110100, v);
        }
        if let Some(ref v) = self.exchange {
            len += encoding::string::encoded_len(110101, v);
        }
        if let Some(v) = self.sequence_number {
            len += encoding::uint64::encoded_len(112002, &v);
        }
        len += encoded_len_optional_repeated_int32(110121, &self.update_type);
        len += encoded_len_optional_repeated_int32(153612, &self.transaction_type);
        len += encoded_len_optional_repeated_double(154405, &self.depth_price);
        len += encoded_len_optional_repeated_int32(154406, &self.depth_size);
        len += encoded_len_optional_repeated_string(149238, &self.exchange_order_id);
        if let Some(v) = self.ssboe {
            len += encoding::int32::encoded_len(150100, &v);
        }
        if let Some(v) = self.usecs {
            len += encoding::int32::encoded_len(150101, &v);
        }
        if let Some(v) = self.source_ssboe {
            len += encoding::int32::encoded_len(150400, &v);
        }
        if let Some(v) = self.source_usecs {
            len += encoding::int32::encoded_len(150401, &v);
        }
        if let Some(v) = self.source_nsecs {
            len += encoding::int32::encoded_len(150404, &v);
        }
        len
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

// ---- DepthByOrderEndEvent (161) ----

#[derive(Clone, PartialEq, prost::Message)]
pub struct DepthByOrderEndEvent {
    #[prost(int32, optional, tag = "154467")]
    pub template_id: Option<i32>,
    #[prost(string, optional, tag = "110100")]
    pub symbol: Option<String>,
    #[prost(string, optional, tag = "110101")]
    pub exchange: Option<String>,
    #[prost(uint64, optional, tag = "112002")]
    pub sequence_number: Option<u64>,
    #[prost(int32, optional, tag = "150100")]
    pub ssboe: Option<i32>,
    #[prost(int32, optional, tag = "150101")]
    pub usecs: Option<i32>,
}

// =========================================================================
// Order Notification Messages (kept as stubs — not needed for ticker plant)
// =========================================================================

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

// =========================================================================
// PnL Messages (kept as stubs — not needed for ticker plant)
// =========================================================================

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
