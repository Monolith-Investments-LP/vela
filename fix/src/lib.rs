//! FIX 4.4 message layer.
//!
//! Scope of this crate:
//! - Wire-level message parsing / serialization (FIX 4.4 grammar,
//!   SOH-delimited tag=value pairs, checksum, BodyLength).
//! - Typed builders + parsers for the messages Vela's institutional
//!   flow actually needs on day one:
//!     - Admin: Logon (A), Logout (5), Heartbeat (0), TestRequest (1),
//!       ResendRequest (2), SequenceReset (4), Reject (3).
//!     - App: NewOrderSingle (D), OrderCancelRequest (F),
//!       OrderCancelReplaceRequest (G), ExecutionReport (8),
//!       OrderCancelReject (9).
//! - Sequence-number tracking helpers.
//!
//! Out of scope in *this crate*:
//! - The TCP acceptor / session state machine (that's `fix-gateway`
//!   in api, which wires a tokio TCP server to this codec and routes
//!   parsed messages into the matching engine).
//! - Drop-copy (feeds an ExecReport stream to a separate connection;
//!   composes trivially on top of the codec).
//! - Persistence of the sequence-number log across restart (a
//!   file-backed log — TODO in the gateway).
//!
//! Notes on why not `quickfix-rs`
//! ------------------------------
//! `quickfix-rs` wraps a large C++ library and its build story is
//! painful in cross-compilation contexts (Fly.io, distroless
//! containers). Rolling the subset of FIX 4.4 we actually need is
//! ~1k LoC and eliminates the C++ dependency. If we later need the
//! full FIX 5.0 SP2 message dictionary (repeated groups, complex
//! allocation types, etc), the calculus flips.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

/// FIX field separator (SOH, 0x01).
pub const SOH: u8 = 0x01;

/// Common tag numbers we handle.
pub mod tag {
    pub const BEGIN_STRING: u16 = 8;
    pub const BODY_LENGTH: u16 = 9;
    pub const MSG_TYPE: u16 = 35;
    pub const SENDER_COMP_ID: u16 = 49;
    pub const TARGET_COMP_ID: u16 = 56;
    pub const MSG_SEQ_NUM: u16 = 34;
    pub const SENDING_TIME: u16 = 52;
    pub const CHECKSUM: u16 = 10;

    // Logon
    pub const ENCRYPT_METHOD: u16 = 98;
    pub const HEART_BT_INT: u16 = 108;
    pub const RESET_SEQ_NUM_FLAG: u16 = 141;

    // Order fields
    pub const ACCOUNT: u16 = 1;
    pub const CL_ORD_ID: u16 = 11;
    pub const ORIG_CL_ORD_ID: u16 = 41;
    pub const ORDER_ID: u16 = 37;
    pub const SYMBOL: u16 = 55;
    pub const SIDE: u16 = 54;
    pub const ORDER_QTY: u16 = 38;
    pub const PRICE: u16 = 44;
    pub const ORD_TYPE: u16 = 40;
    pub const TIME_IN_FORCE: u16 = 59;
    pub const TRANSACT_TIME: u16 = 60;

    // ExecReport fields
    pub const EXEC_ID: u16 = 17;
    pub const EXEC_TYPE: u16 = 150;
    pub const ORD_STATUS: u16 = 39;
    pub const LAST_QTY: u16 = 32;
    pub const LAST_PX: u16 = 31;
    pub const LEAVES_QTY: u16 = 151;
    pub const CUM_QTY: u16 = 14;
    pub const AVG_PX: u16 = 6;

    // Session control / errors
    pub const TEST_REQ_ID: u16 = 112;
    pub const GAP_FILL_FLAG: u16 = 123;
    pub const NEW_SEQ_NO: u16 = 36;
    pub const REF_SEQ_NUM: u16 = 45;
    pub const REF_TAG_ID: u16 = 371;
    pub const REF_MSG_TYPE: u16 = 372;
    pub const SESSION_REJECT_REASON: u16 = 373;
    pub const TEXT: u16 = 58;
}

/// FIX 4.4 MsgType single-char codes we handle.
pub mod msg_type {
    pub const HEARTBEAT: &str = "0";
    pub const TEST_REQUEST: &str = "1";
    pub const RESEND_REQUEST: &str = "2";
    pub const REJECT: &str = "3";
    pub const SEQUENCE_RESET: &str = "4";
    pub const LOGOUT: &str = "5";
    pub const EXECUTION_REPORT: &str = "8";
    pub const ORDER_CANCEL_REJECT: &str = "9";
    pub const LOGON: &str = "A";
    pub const NEW_ORDER_SINGLE: &str = "D";
    pub const ORDER_CANCEL_REQUEST: &str = "F";
    pub const ORDER_CANCEL_REPLACE: &str = "G";
}

#[derive(Debug, Error)]
pub enum FixError {
    #[error("malformed message: {0}")]
    Malformed(String),
    #[error("missing required tag {0}")]
    MissingTag(u16),
    #[error("bad checksum: expected {expected:03}, got {got:03}")]
    BadChecksum { expected: u8, got: u8 },
    #[error("bad body-length: header claims {claimed}, actual {actual}")]
    BadBodyLength { claimed: usize, actual: usize },
    #[error("unsupported message type: {0}")]
    UnsupportedMsgType(String),
}

/// Parsed FIX message: an ordered map of tag → raw string value.
///
/// BTreeMap keeps iteration deterministic which matters for
/// re-serialization and for tests. FIX allows repeated tags (in
/// groups), but the messages Vela handles at v1 do not use groups
/// beyond the trivial party-block, so a single-value-per-tag map is
/// enough. Extending to `BTreeMap<u16, Vec<String>>` is a follow-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixMessage {
    pub fields: BTreeMap<u16, String>,
}

impl FixMessage {
    pub fn new() -> Self {
        Self {
            fields: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, tag: u16, value: impl Into<String>) -> &mut Self {
        self.fields.insert(tag, value.into());
        self
    }

    pub fn get(&self, tag: u16) -> Option<&str> {
        self.fields.get(&tag).map(String::as_str)
    }

    pub fn require(&self, tag: u16) -> Result<&str, FixError> {
        self.get(tag).ok_or(FixError::MissingTag(tag))
    }

    pub fn msg_type(&self) -> Option<&str> {
        self.get(tag::MSG_TYPE)
    }
}

impl Default for FixMessage {
    fn default() -> Self {
        Self::new()
    }
}

/// Serialize into the FIX wire format: `8=FIX.4.4|9=<body_length>|<body>|10=<checksum>|`.
/// The `body` is every field except BeginString(8), BodyLength(9),
/// and CheckSum(10), in the tag-ascending order given by the
/// BTreeMap. Tags 8 and 9 are always prepended; tag 10 is always
/// computed and appended.
pub fn serialize(msg: &FixMessage) -> Vec<u8> {
    let mut body = Vec::with_capacity(256);
    for (tag, value) in &msg.fields {
        if *tag == tag::BEGIN_STRING || *tag == tag::BODY_LENGTH || *tag == tag::CHECKSUM {
            continue;
        }
        append_field(&mut body, *tag, value);
    }

    let mut out = Vec::with_capacity(body.len() + 32);
    append_field(&mut out, tag::BEGIN_STRING, "FIX.4.4");
    append_field(&mut out, tag::BODY_LENGTH, &body.len().to_string());
    out.extend_from_slice(&body);

    let checksum = compute_checksum(&out);
    append_field(&mut out, tag::CHECKSUM, &format!("{:03}", checksum));
    out
}

fn append_field(out: &mut Vec<u8>, tag: u16, value: &str) {
    out.extend_from_slice(tag.to_string().as_bytes());
    out.push(b'=');
    out.extend_from_slice(value.as_bytes());
    out.push(SOH);
}

/// FIX 4.4 checksum: sum of every byte in the message excluding the
/// checksum field itself, mod 256, formatted as 3 zero-padded digits.
pub fn compute_checksum(bytes_without_checksum: &[u8]) -> u8 {
    (bytes_without_checksum
        .iter()
        .map(|&b| b as u32)
        .sum::<u32>()
        % 256) as u8
}

/// Parse one complete FIX message from `bytes`. Returns the parsed
/// FixMessage and the number of bytes consumed. Callers should feed
/// TCP bytes to `next_message` in a streaming parser.
pub fn parse(bytes: &[u8]) -> Result<(FixMessage, usize), FixError> {
    // Find the body-length header to know how much to read.
    // Expect the input to start with `8=FIX.4.4\x019=<n>\x01...`.
    let (bs_tag, bs_val, rest_after_bs) = parse_field(bytes)?;
    if bs_tag != tag::BEGIN_STRING {
        return Err(FixError::Malformed(format!(
            "expected tag 8 (BeginString) first, got {bs_tag}"
        )));
    }
    if bs_val != "FIX.4.4" {
        return Err(FixError::UnsupportedMsgType(format!(
            "BeginString {bs_val} — only FIX.4.4 supported in v1"
        )));
    }

    let (bl_tag, bl_val, rest_after_bl) = parse_field(rest_after_bs)?;
    if bl_tag != tag::BODY_LENGTH {
        return Err(FixError::Malformed(format!(
            "expected tag 9 (BodyLength) after 8, got {bl_tag}"
        )));
    }
    let body_len: usize = bl_val
        .parse()
        .map_err(|_| FixError::Malformed(format!("bad BodyLength: {bl_val}")))?;

    if rest_after_bl.len() < body_len {
        return Err(FixError::BadBodyLength {
            claimed: body_len,
            actual: rest_after_bl.len(),
        });
    }
    let (body_bytes, tail) = rest_after_bl.split_at(body_len);

    // Tail must start with the checksum field.
    let (cs_tag, cs_val, tail_rest) = parse_field(tail)?;
    if cs_tag != tag::CHECKSUM {
        return Err(FixError::Malformed(format!(
            "expected tag 10 (CheckSum) after body, got {cs_tag}"
        )));
    }
    let claimed_cs: u8 = cs_val
        .parse()
        .map_err(|_| FixError::Malformed(format!("bad CheckSum: {cs_val}")))?;

    // Compute expected checksum: over BeginString + BodyLength + body.
    let header_end = bytes.len() - rest_after_bl.len();
    let checked = &bytes[..header_end + body_len];
    let expected_cs = compute_checksum(checked);
    if claimed_cs != expected_cs {
        return Err(FixError::BadChecksum {
            expected: expected_cs,
            got: claimed_cs,
        });
    }

    // Parse the body into (tag, value) pairs.
    let mut msg = FixMessage::new();
    msg.set(tag::BEGIN_STRING, bs_val);
    msg.set(tag::BODY_LENGTH, bl_val);
    let mut cursor = body_bytes;
    while !cursor.is_empty() {
        let (t, v, r) = parse_field(cursor)?;
        msg.set(t, v);
        cursor = r;
    }
    msg.set(tag::CHECKSUM, cs_val);

    let consumed = bytes.len() - tail_rest.len();
    Ok((msg, consumed))
}

fn parse_field(bytes: &[u8]) -> Result<(u16, String, &[u8]), FixError> {
    let eq_idx = bytes
        .iter()
        .position(|&b| b == b'=')
        .ok_or_else(|| FixError::Malformed("no '=' in field".into()))?;
    let soh_idx = bytes[eq_idx..]
        .iter()
        .position(|&b| b == SOH)
        .ok_or_else(|| FixError::Malformed("no SOH terminator in field".into()))?
        + eq_idx;
    let tag_str = std::str::from_utf8(&bytes[..eq_idx])
        .map_err(|_| FixError::Malformed("tag not utf-8".into()))?;
    let tag: u16 = tag_str
        .parse()
        .map_err(|_| FixError::Malformed(format!("bad tag: {tag_str}")))?;
    let value_bytes = &bytes[eq_idx + 1..soh_idx];
    let value = std::str::from_utf8(value_bytes)
        .map_err(|_| FixError::Malformed("value not utf-8".into()))?
        .to_string();
    Ok((tag, value, &bytes[soh_idx + 1..]))
}

// ---------- Session state ----------

/// Minimal outbound/inbound sequence number tracker for one session.
///
/// The gateway wraps this with retransmit logic on ResendRequest, and
/// persists state across restart. In-memory state is a
/// starting-point; a file-backed WAL of sent messages is required for
/// full session recovery.
#[derive(Debug, Clone)]
pub struct SessionState {
    pub sender_comp_id: String,
    pub target_comp_id: String,
    pub next_outbound_seq: u32,
    pub next_expected_inbound_seq: u32,
    pub logged_on: bool,
}

impl SessionState {
    pub fn new(sender: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            sender_comp_id: sender.into(),
            target_comp_id: target.into(),
            next_outbound_seq: 1,
            next_expected_inbound_seq: 1,
            logged_on: false,
        }
    }

    /// Stamp a message with standard session fields (SenderCompID,
    /// TargetCompID, MsgSeqNum, SendingTime) and advance the outbound
    /// counter. Overwrites any prior values.
    pub fn stamp_outbound(&mut self, msg: &mut FixMessage, sending_time_utc: &str) {
        msg.set(tag::SENDER_COMP_ID, self.sender_comp_id.clone());
        msg.set(tag::TARGET_COMP_ID, self.target_comp_id.clone());
        msg.set(tag::MSG_SEQ_NUM, self.next_outbound_seq.to_string());
        msg.set(tag::SENDING_TIME, sending_time_utc);
        self.next_outbound_seq += 1;
    }

    /// Check inbound seq against expected. Returns Ok(()) if it
    /// matches (advances the counter), or an error the caller must
    /// convert into a ResendRequest.
    pub fn check_inbound(&mut self, msg: &FixMessage) -> Result<(), FixError> {
        let seq_str = msg.require(tag::MSG_SEQ_NUM)?;
        let seq: u32 = seq_str
            .parse()
            .map_err(|_| FixError::Malformed(format!("bad MsgSeqNum: {seq_str}")))?;
        if seq != self.next_expected_inbound_seq {
            return Err(FixError::Malformed(format!(
                "sequence gap: expected {}, got {}",
                self.next_expected_inbound_seq, seq
            )));
        }
        self.next_expected_inbound_seq += 1;
        Ok(())
    }
}

// ---------- Typed builders ----------

pub struct LogonMsg {
    pub encrypt_method: u8, // 0 = none
    pub heart_bt_int_seconds: u32,
    pub reset_seq_num: bool,
}

pub fn build_logon(m: &LogonMsg) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::LOGON);
    msg.set(tag::ENCRYPT_METHOD, m.encrypt_method.to_string());
    msg.set(tag::HEART_BT_INT, m.heart_bt_int_seconds.to_string());
    if m.reset_seq_num {
        msg.set(tag::RESET_SEQ_NUM_FLAG, "Y");
    }
    msg
}

pub fn build_heartbeat(test_req_id: Option<&str>) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::HEARTBEAT);
    if let Some(id) = test_req_id {
        msg.set(tag::TEST_REQ_ID, id);
    }
    msg
}

pub fn build_test_request(test_req_id: &str) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::TEST_REQUEST);
    msg.set(tag::TEST_REQ_ID, test_req_id);
    msg
}

pub fn build_resend_request(begin_seq: u32, end_seq: u32) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::RESEND_REQUEST);
    msg.set(7, begin_seq.to_string()); // BeginSeqNo
    msg.set(16, end_seq.to_string()); // EndSeqNo (0 = infinity)
    msg
}

pub fn build_logout(text: Option<&str>) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::LOGOUT);
    if let Some(t) = text {
        msg.set(tag::TEXT, t);
    }
    msg
}

pub struct NewOrderSingle<'a> {
    pub cl_ord_id: &'a str,
    pub symbol: &'a str,
    /// '1' = Buy, '2' = Sell.
    pub side: char,
    pub order_qty: &'a str,
    /// '1' = Market, '2' = Limit.
    pub ord_type: char,
    /// Required when ord_type = '2' (Limit). Otherwise ignored.
    pub price: Option<&'a str>,
    /// '0' = Day, '1' = GTC, '3' = IOC, '4' = FOK. Optional; default 0.
    pub time_in_force: Option<char>,
    pub transact_time_utc: &'a str,
}

pub fn build_new_order_single(o: &NewOrderSingle) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::NEW_ORDER_SINGLE);
    msg.set(tag::CL_ORD_ID, o.cl_ord_id);
    msg.set(tag::SYMBOL, o.symbol);
    msg.set(tag::SIDE, o.side.to_string());
    msg.set(tag::ORDER_QTY, o.order_qty);
    msg.set(tag::ORD_TYPE, o.ord_type.to_string());
    if let Some(p) = o.price {
        msg.set(tag::PRICE, p);
    }
    if let Some(tif) = o.time_in_force {
        msg.set(tag::TIME_IN_FORCE, tif.to_string());
    }
    msg.set(tag::TRANSACT_TIME, o.transact_time_utc);
    msg
}

pub struct ExecutionReport<'a> {
    pub order_id: &'a str,
    pub cl_ord_id: &'a str,
    pub exec_id: &'a str,
    /// '0' = New, '4' = Cancelled, 'F' = Trade (partial or full fill).
    pub exec_type: char,
    /// '0' = New, '1' = PartiallyFilled, '2' = Filled, '4' = Cancelled.
    pub ord_status: char,
    pub symbol: &'a str,
    pub side: char,
    pub leaves_qty: &'a str,
    pub cum_qty: &'a str,
    pub avg_px: &'a str,
    pub last_qty: Option<&'a str>,
    pub last_px: Option<&'a str>,
    pub transact_time_utc: &'a str,
    pub text: Option<&'a str>,
}

pub fn build_execution_report(e: &ExecutionReport) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::EXECUTION_REPORT);
    msg.set(tag::ORDER_ID, e.order_id);
    msg.set(tag::CL_ORD_ID, e.cl_ord_id);
    msg.set(tag::EXEC_ID, e.exec_id);
    msg.set(tag::EXEC_TYPE, e.exec_type.to_string());
    msg.set(tag::ORD_STATUS, e.ord_status.to_string());
    msg.set(tag::SYMBOL, e.symbol);
    msg.set(tag::SIDE, e.side.to_string());
    msg.set(tag::LEAVES_QTY, e.leaves_qty);
    msg.set(tag::CUM_QTY, e.cum_qty);
    msg.set(tag::AVG_PX, e.avg_px);
    if let Some(q) = e.last_qty {
        msg.set(tag::LAST_QTY, q);
    }
    if let Some(p) = e.last_px {
        msg.set(tag::LAST_PX, p);
    }
    msg.set(tag::TRANSACT_TIME, e.transact_time_utc);
    if let Some(t) = e.text {
        msg.set(tag::TEXT, t);
    }
    msg
}

pub fn build_reject(
    ref_seq_num: u32,
    ref_tag_id: Option<u16>,
    ref_msg_type: Option<&str>,
    reason: u16,
    text: &str,
) -> FixMessage {
    let mut msg = FixMessage::new();
    msg.set(tag::MSG_TYPE, msg_type::REJECT);
    msg.set(tag::REF_SEQ_NUM, ref_seq_num.to_string());
    if let Some(t) = ref_tag_id {
        msg.set(tag::REF_TAG_ID, t.to_string());
    }
    if let Some(mt) = ref_msg_type {
        msg.set(tag::REF_MSG_TYPE, mt);
    }
    msg.set(tag::SESSION_REJECT_REASON, reason.to_string());
    msg.set(tag::TEXT, text);
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_then_parse_roundtrip_nos() {
        let nos = build_new_order_single(&NewOrderSingle {
            cl_ord_id: "AB123",
            symbol: "BTC-USDC",
            side: '1',
            order_qty: "0.5",
            ord_type: '2',
            price: Some("60000"),
            time_in_force: Some('1'),
            transact_time_utc: "20260831-16:34:12.000",
        });
        let mut msg = nos;
        // Session stamps.
        let mut sess = SessionState::new("VELA", "CLIENT");
        sess.stamp_outbound(&mut msg, "20260831-16:34:12.000");

        let wire = serialize(&msg);
        assert!(wire.starts_with(b"8=FIX.4.4"));
        assert!(wire.ends_with(b"\x01"));
        assert!(wire.contains(&SOH));

        let (parsed, consumed) = parse(&wire).unwrap();
        assert_eq!(consumed, wire.len());
        assert_eq!(parsed.get(tag::MSG_TYPE), Some("D"));
        assert_eq!(parsed.get(tag::CL_ORD_ID), Some("AB123"));
        assert_eq!(parsed.get(tag::SYMBOL), Some("BTC-USDC"));
        assert_eq!(parsed.get(tag::PRICE), Some("60000"));
    }

    #[test]
    fn parse_rejects_bad_checksum() {
        let msg = build_heartbeat(None);
        let mut wire = serialize(&msg);
        // Corrupt the last field's checksum digit.
        let n = wire.len();
        wire[n - 2] ^= 1;
        let err = parse(&wire).unwrap_err();
        assert!(matches!(err, FixError::BadChecksum { .. }));
    }

    #[test]
    fn parse_rejects_wrong_begin_string() {
        // Build a bogus BeginString like FIX.4.2.
        let mut buf = Vec::new();
        append_field(&mut buf, tag::BEGIN_STRING, "FIX.4.2");
        append_field(&mut buf, tag::BODY_LENGTH, "6");
        append_field(&mut buf, tag::MSG_TYPE, "0");
        let cs = compute_checksum(&buf);
        append_field(&mut buf, tag::CHECKSUM, &format!("{:03}", cs));
        let err = parse(&buf).unwrap_err();
        assert!(matches!(err, FixError::UnsupportedMsgType(_)));
    }

    #[test]
    fn session_stamps_bump_seq_num() {
        let mut sess = SessionState::new("VELA", "CLIENT");
        let mut m1 = build_heartbeat(None);
        let mut m2 = build_heartbeat(None);
        sess.stamp_outbound(&mut m1, "20260831-00:00:00");
        sess.stamp_outbound(&mut m2, "20260831-00:00:01");
        assert_eq!(m1.get(tag::MSG_SEQ_NUM), Some("1"));
        assert_eq!(m2.get(tag::MSG_SEQ_NUM), Some("2"));
        assert_eq!(sess.next_outbound_seq, 3);
    }

    #[test]
    fn check_inbound_detects_gap() {
        let mut sess = SessionState::new("VELA", "CLIENT");
        let mut msg = build_heartbeat(None);
        msg.set(tag::MSG_SEQ_NUM, "5");
        let err = sess.check_inbound(&msg).unwrap_err();
        assert!(matches!(err, FixError::Malformed(_)));
    }

    #[test]
    fn check_inbound_accepts_in_order() {
        let mut sess = SessionState::new("VELA", "CLIENT");
        for i in 1..=3 {
            let mut msg = build_heartbeat(None);
            msg.set(tag::MSG_SEQ_NUM, i.to_string());
            sess.check_inbound(&msg).unwrap();
        }
        assert_eq!(sess.next_expected_inbound_seq, 4);
    }

    #[test]
    fn execution_report_carries_expected_fields() {
        let er = build_execution_report(&ExecutionReport {
            order_id: "V-9001",
            cl_ord_id: "AB123",
            exec_id: "E-42",
            exec_type: 'F',
            ord_status: '2',
            symbol: "BTC-USDC",
            side: '1',
            leaves_qty: "0",
            cum_qty: "0.5",
            avg_px: "60000",
            last_qty: Some("0.5"),
            last_px: Some("60000"),
            transact_time_utc: "20260831-16:34:12.000",
            text: None,
        });
        assert_eq!(er.get(tag::MSG_TYPE), Some("8"));
        assert_eq!(er.get(tag::EXEC_TYPE), Some("F"));
        assert_eq!(er.get(tag::ORD_STATUS), Some("2"));
        assert_eq!(er.get(tag::CUM_QTY), Some("0.5"));
    }

    #[test]
    fn logon_message_carries_heartbeat_and_encrypt() {
        let l = build_logon(&LogonMsg {
            encrypt_method: 0,
            heart_bt_int_seconds: 30,
            reset_seq_num: true,
        });
        assert_eq!(l.get(tag::MSG_TYPE), Some("A"));
        assert_eq!(l.get(tag::ENCRYPT_METHOD), Some("0"));
        assert_eq!(l.get(tag::HEART_BT_INT), Some("30"));
        assert_eq!(l.get(tag::RESET_SEQ_NUM_FLAG), Some("Y"));
    }

    #[test]
    fn reject_carries_ref_fields() {
        let r = build_reject(7, Some(40), Some("D"), 5, "value is incorrect");
        assert_eq!(r.get(tag::MSG_TYPE), Some("3"));
        assert_eq!(r.get(tag::REF_SEQ_NUM), Some("7"));
        assert_eq!(r.get(tag::REF_TAG_ID), Some("40"));
        assert_eq!(r.get(tag::REF_MSG_TYPE), Some("D"));
    }
}
