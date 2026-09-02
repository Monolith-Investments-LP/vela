//! FIX 4.4 ↔ `types::PostOrderRequest` compatibility shim.
//!
//! The FIX gateway parses `NewOrderSingle` messages and hands them to
//! this module, which produces a `PostOrderRequest` ready for
//! `state.order_tx`. Kept in a dedicated module so the field mapping is
//! easy to audit (institutional clients treat every mistranslated tag
//! as an incident).
//!
//! Auth model
//! ----------
//! FIX sessions are authenticated at the TCP/session edge (typically
//! mTLS in front of the acceptor). The gateway therefore does NOT
//! attach a wallet signature to the order; the `signature` slot of
//! `PostOrderRequest` is left empty and the FIX-originated flow is
//! trusted at the session boundary. This is the same trust model any
//! serious FIX venue applies.
//!
//! Order-type mapping (v1)
//! -----------------------
//! - OrdType (tag 40) `2` (Limit) is the only supported form.
//! - TimeInForce (tag 59): `0`/`1` (Day/GTC) → `GoodTillCanceled`,
//!   `3` (IOC) → `ImmediateOrCancel`, `4` (FOK) → `FillOrKill`.
//! - Anything else returns `FixAdapterError::UnsupportedOrdType` so
//!   the gateway can send a Reject(reason=3) back to the client.
//!
//! Nonce mapping
//! -------------
//! The engine treats `nonce` as a per-user monotonic. FIX sessions carry
//! their own `MsgSeqNum` (tag 34) which is already monotonic per
//! session and stamped on every message — reusing it means the client
//! doesn't need to manage a second sequence. If MsgSeqNum is missing we
//! fall back to a wall-clock nanosecond timestamp.

use fix::{tag, FixMessage};
use types::{MarketId, OrderSide, OrderType, PostOrderRequest, UserId};

#[derive(Debug, Clone)]
pub enum FixAdapterError {
    MissingTag(u16, &'static str),
    InvalidTag(u16, String),
    UnsupportedOrdType(char),
    InvalidHexAddress(String),
}

impl std::fmt::Display for FixAdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FixAdapterError::MissingTag(t, name) => write!(f, "missing tag {t} ({name})"),
            FixAdapterError::InvalidTag(t, msg) => write!(f, "invalid tag {t}: {msg}"),
            FixAdapterError::UnsupportedOrdType(c) => {
                write!(f, "unsupported OrdType {c:?}; v1 accepts only Limit ('2')")
            }
            FixAdapterError::InvalidHexAddress(s) => write!(f, "invalid hex address: {s:?}"),
        }
    }
}

impl std::error::Error for FixAdapterError {}

pub struct ParsedNewOrder {
    pub request: PostOrderRequest,
    pub cl_ord_id: String,
    pub sender_comp_id: String,
    pub account: String,
}

pub fn parse_new_order_single(msg: &FixMessage) -> Result<ParsedNewOrder, FixAdapterError> {
    let symbol = msg
        .get(tag::SYMBOL)
        .ok_or(FixAdapterError::MissingTag(tag::SYMBOL, "Symbol"))?
        .to_string();

    let side_c = msg
        .get(tag::SIDE)
        .and_then(|s| s.chars().next())
        .ok_or(FixAdapterError::MissingTag(tag::SIDE, "Side"))?;
    let side = match side_c {
        '1' => OrderSide::Bid, // Buy
        '2' => OrderSide::Ask, // Sell
        c => return Err(FixAdapterError::InvalidTag(tag::SIDE, format!("side {c:?}"))),
    };

    let ord_type_c = msg
        .get(tag::ORD_TYPE)
        .and_then(|s| s.chars().next())
        .unwrap_or('2');
    if ord_type_c != '2' {
        return Err(FixAdapterError::UnsupportedOrdType(ord_type_c));
    }
    let tif = msg
        .get(tag::TIME_IN_FORCE)
        .and_then(|s| s.chars().next())
        .unwrap_or('1');
    let order_type = match tif {
        '0' | '1' => OrderType::GoodTillCanceled,
        '3' => OrderType::ImmediateOrCancel,
        '4' => OrderType::FillOrKill,
        c => {
            return Err(FixAdapterError::InvalidTag(
                tag::TIME_IN_FORCE,
                format!("unsupported TIF {c:?}"),
            ))
        }
    };

    let price_dec: f64 = msg
        .get(tag::PRICE)
        .ok_or(FixAdapterError::MissingTag(tag::PRICE, "Price"))?
        .parse()
        .map_err(|_| FixAdapterError::InvalidTag(tag::PRICE, "expected decimal".into()))?;
    let qty_dec: f64 = msg
        .get(tag::ORDER_QTY)
        .ok_or(FixAdapterError::MissingTag(tag::ORDER_QTY, "OrderQty"))?
        .parse()
        .map_err(|_| FixAdapterError::InvalidTag(tag::ORDER_QTY, "expected decimal".into()))?;
    if !price_dec.is_finite() || price_dec <= 0.0 {
        return Err(FixAdapterError::InvalidTag(
            tag::PRICE,
            "price must be positive and finite".into(),
        ));
    }
    if !qty_dec.is_finite() || qty_dec <= 0.0 {
        return Err(FixAdapterError::InvalidTag(
            tag::ORDER_QTY,
            "qty must be positive and finite".into(),
        ));
    }
    let price = (price_dec * 1_000_000.0).round() as u64;
    let quantity = (qty_dec * 1_000_000.0).round() as u64;

    let cl_ord_id = msg.get(tag::CL_ORD_ID).unwrap_or("").to_string();
    let sender_comp_id = msg.get(tag::SENDER_COMP_ID).unwrap_or("").to_string();

    // Account (tag 1) carries the on-chain address. If the client
    // doesn't send it we fall back to the SenderCompID, which is the
    // convention some venues use — but only if it parses as hex.
    let account_raw = msg
        .get(tag::ACCOUNT)
        .map(|s| s.to_string())
        .unwrap_or_else(|| sender_comp_id.clone());
    let user = UserId::from_hex(&account_raw)
        .map_err(|_| FixAdapterError::InvalidHexAddress(account_raw.clone()))?;

    // Prefer MsgSeqNum (tag 34) as the nonce; fall back to wall-clock
    // ns if the message lacked it (shouldn't happen with a compliant
    // session).
    let nonce = msg
        .get(tag::MSG_SEQ_NUM)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(now_ns);

    let request = PostOrderRequest {
        user,
        market: MarketId(symbol),
        side,
        order_type,
        price,
        quantity,
        nonce,
        client_order_id: if cl_ord_id.is_empty() {
            None
        } else {
            Some(cl_ord_id.clone())
        },
        signature: Vec::new(),
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    };
    Ok(ParsedNewOrder {
        request,
        cl_ord_id,
        sender_comp_id,
        account: account_raw,
    })
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

/// FIX side char for an engine-side `OrderSide`.
pub fn side_char(side: OrderSide) -> char {
    match side {
        OrderSide::Bid => '1',
        OrderSide::Ask => '2',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fix::FixMessage;

    fn msg_with(pairs: &[(u16, &str)]) -> FixMessage {
        let mut m = FixMessage::new();
        for &(t, v) in pairs {
            m.set(t, v);
        }
        m
    }

    #[test]
    fn parses_limit_gtc_buy() {
        let msg = msg_with(&[
            (tag::SYMBOL, "ETH-USDC"),
            (tag::SIDE, "1"),
            (tag::ORD_TYPE, "2"),
            (tag::TIME_IN_FORCE, "1"),
            (tag::PRICE, "3200.5"),
            (tag::ORDER_QTY, "0.75"),
            (tag::CL_ORD_ID, "abc-123"),
            (tag::ACCOUNT, "0x0000000000000000000000000000000000000abc"),
            (tag::MSG_SEQ_NUM, "42"),
        ]);
        let parsed = parse_new_order_single(&msg).unwrap();
        assert_eq!(parsed.request.market.0, "ETH-USDC");
        assert_eq!(parsed.request.side, OrderSide::Bid);
        assert_eq!(parsed.request.order_type, OrderType::GoodTillCanceled);
        assert_eq!(parsed.request.price, 3_200_500_000);
        assert_eq!(parsed.request.quantity, 750_000);
        assert_eq!(parsed.request.nonce, 42);
        assert_eq!(parsed.cl_ord_id, "abc-123");
        assert!(parsed.request.signature.is_empty());
    }

    #[test]
    fn maps_tif_to_engine_order_type() {
        for (tif, expected) in [
            ("1", OrderType::GoodTillCanceled),
            ("3", OrderType::ImmediateOrCancel),
            ("4", OrderType::FillOrKill),
        ] {
            let msg = msg_with(&[
                (tag::SYMBOL, "ETH-USDC"),
                (tag::SIDE, "1"),
                (tag::ORD_TYPE, "2"),
                (tag::TIME_IN_FORCE, tif),
                (tag::PRICE, "1"),
                (tag::ORDER_QTY, "1"),
                (tag::ACCOUNT, "0x0000000000000000000000000000000000000abc"),
            ]);
            assert_eq!(parse_new_order_single(&msg).unwrap().request.order_type, expected);
        }
    }

    #[test]
    fn rejects_non_limit_ord_type() {
        let msg = msg_with(&[
            (tag::SYMBOL, "ETH-USDC"),
            (tag::SIDE, "1"),
            (tag::ORD_TYPE, "1"), // Market
            (tag::PRICE, "1"),
            (tag::ORDER_QTY, "1"),
            (tag::ACCOUNT, "0x0000000000000000000000000000000000000abc"),
        ]);
        assert!(matches!(
            parse_new_order_single(&msg),
            Err(FixAdapterError::UnsupportedOrdType('1'))
        ));
    }

    #[test]
    fn rejects_bad_hex_account() {
        let msg = msg_with(&[
            (tag::SYMBOL, "ETH-USDC"),
            (tag::SIDE, "1"),
            (tag::ORD_TYPE, "2"),
            (tag::TIME_IN_FORCE, "1"),
            (tag::PRICE, "1"),
            (tag::ORDER_QTY, "1"),
            (tag::ACCOUNT, "not-hex"),
        ]);
        assert!(matches!(
            parse_new_order_single(&msg),
            Err(FixAdapterError::InvalidHexAddress(_))
        ));
    }

    #[test]
    fn rejects_negative_price() {
        let msg = msg_with(&[
            (tag::SYMBOL, "ETH-USDC"),
            (tag::SIDE, "1"),
            (tag::ORD_TYPE, "2"),
            (tag::TIME_IN_FORCE, "1"),
            (tag::PRICE, "-1"),
            (tag::ORDER_QTY, "1"),
            (tag::ACCOUNT, "0x0000000000000000000000000000000000000abc"),
        ]);
        assert!(matches!(
            parse_new_order_single(&msg),
            Err(FixAdapterError::InvalidTag(_, _))
        ));
    }
}
