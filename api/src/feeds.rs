use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use types::{Response, UserId};
use crate::types::{WsEnvelope, WsServerMessage};

const CHANNEL_CAPACITY: usize = 1024;

pub struct FeedManager {
    public_tx: broadcast::Sender<WsServerMessage>,
    private_txs: HashMap<[u8; 20], broadcast::Sender<WsServerMessage>>,
    private_envelope_txs: HashMap<[u8; 20], broadcast::Sender<WsEnvelope>>,
    private_envelope_seqs: HashMap<[u8; 20], u64>,
    /// Public authenticated channel for per-fill toxicity events.
    pub toxicity_tx: broadcast::Sender<serde_json::Value>,
}

impl FeedManager {
    pub fn new() -> Self {
        let (public_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        let (toxicity_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        FeedManager {
            public_tx,
            private_txs: HashMap::new(),
            private_envelope_txs: HashMap::new(),
            private_envelope_seqs: HashMap::new(),
            toxicity_tx,
        }
    }

    pub fn subscribe_public(&self) -> broadcast::Receiver<WsServerMessage> {
        self.public_tx.subscribe()
    }

    pub fn subscribe_private(&mut self, user: &UserId) -> broadcast::Receiver<WsServerMessage> {
        self.private_txs
            .entry(user.0)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    pub fn subscribe_account_private(&mut self, user: &UserId) -> broadcast::Receiver<WsEnvelope> {
        self.private_envelope_txs
            .entry(user.0)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .subscribe()
    }

    /// Returns a receiver for the toxicity event broadcast channel.
    pub fn subscribe_toxicity(&self) -> broadcast::Receiver<serde_json::Value> {
        self.toxicity_tx.subscribe()
    }

    pub fn publish_public(&self, msg: WsServerMessage) {
        let _ = self.public_tx.send(msg);
    }

    pub fn publish_private(&self, user: &UserId, msg: WsServerMessage) {
        if let Some(tx) = self.private_txs.get(&user.0) {
            let _ = tx.send(msg);
        }
    }

    /// Broadcast a toxicity event to all authenticated subscribers.
    pub fn publish_toxicity(&self, event: serde_json::Value) {
        let _ = self.toxicity_tx.send(event);
    }

    fn next_account_seq(&mut self, user_bytes: [u8; 20]) -> u64 {
        let seq = self.private_envelope_seqs.entry(user_bytes).or_insert(0);
        *seq += 1;
        *seq
    }

    fn send_account_envelope(&mut self, user_bytes: [u8; 20], msg_type: &str, data: serde_json::Value) {
        let address = format!("0x{}", hex::encode(user_bytes));
        let channel = format!("account:{}", address);
        let seq = self.next_account_seq(user_bytes);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let envelope = WsEnvelope {
            msg_type: msg_type.to_string(),
            channel,
            seq,
            data,
            timestamp: ts,
        };
        if let Some(tx) = self.private_envelope_txs.get(&user_bytes) {
            let _ = tx.send(envelope);
        }
    }

    pub fn dispatch_response_batch(&mut self, user: &UserId, responses: &[Response]) {
        for response in responses {
            match response {
                Response::OrderFilled(fill) => {
                    let msg = WsServerMessage::Fill {
                        maker_order_id: fill.maker_order_id,
                        taker_order_id: fill.taker_order_id,
                        price: fill.price.to_string(),
                        quantity: fill.quantity.to_string(),
                        side: format!("{:?}", fill.side).to_lowercase(),
                        maker_fee: fill.maker_fee.to_string(),
                        taker_fee: fill.taker_fee.to_string(),
                        timestamp: fill.timestamp,
                    };
                    if let Some(tx) = self.private_txs.get(&fill.maker.0) {
                        let _ = tx.send(msg.clone());
                    }
                    if let Some(tx) = self.private_txs.get(&fill.taker.0) {
                        let _ = tx.send(msg);
                    }

                    let fill_data = serde_json::json!({
                        "type": "fill",
                        "maker_order_id": fill.maker_order_id,
                        "taker_order_id": fill.taker_order_id,
                        "price": fill.price.to_string(),
                        "quantity": fill.quantity.to_string(),
                        "side": format!("{:?}", fill.side).to_lowercase(),
                        "maker_fee": fill.maker_fee.to_string(),
                        "taker_fee": fill.taker_fee.to_string(),
                        "timestamp": fill.timestamp,
                    });
                    self.send_account_envelope(fill.maker.0, "fill", fill_data.clone());
                    self.send_account_envelope(fill.taker.0, "fill", fill_data);

                    // Emit toxicity event for every fill that was part of a matched
                    // taker order.  Score of 0.0 means the fill was a resting order
                    // that did not contribute to a scored taker order this session
                    // (e.g., restored from snapshot) — we still emit it for completeness.
                    let timestamp_ns = fill.timestamp.saturating_mul(1_000);
                    let toxicity_event = serde_json::json!({
                        "market": fill.market.0,
                        "order_id": fill.taker_order_id,
                        "side": format!("{:?}", fill.side).to_lowercase(),
                        "size": fill.quantity.to_string(),
                        "price": fill.price.to_string(),
                        "toxicity_score": fill.toxicity_score,
                        "ofi_snapshot": fill.ofi_snapshot,
                        "timestamp_ns": timestamp_ns,
                    });
                    let _ = self.toxicity_tx.send(toxicity_event);
                }
                Response::OrderPosted(posted) => {
                    let msg = WsServerMessage::OrderUpdate {
                        order_id: posted.order_id,
                        status: format!("{:?}", posted.status).to_lowercase(),
                        filled_quantity: "0".to_string(),
                    };
                    if let Some(tx) = self.private_txs.get(&user.0) {
                        let _ = tx.send(msg);
                    }
                    let data = serde_json::json!({
                        "type": "order_update",
                        "order_id": posted.order_id,
                        "status": format!("{:?}", posted.status).to_lowercase(),
                        "filled_quantity": "0",
                    });
                    self.send_account_envelope(user.0, "order_update", data);
                }
                Response::OrderCanceled(canceled) => {
                    let msg = WsServerMessage::OrderUpdate {
                        order_id: canceled.order_id,
                        status: "canceled".to_string(),
                        filled_quantity: "0".to_string(),
                    };
                    if let Some(tx) = self.private_txs.get(&user.0) {
                        let _ = tx.send(msg);
                    }
                    let data = serde_json::json!({
                        "type": "order_update",
                        "order_id": canceled.order_id,
                        "status": "canceled",
                        "filled_quantity": "0",
                    });
                    self.send_account_envelope(user.0, "order_update", data);
                }
                Response::BalanceUpdated(update) => {
                    let msg = WsServerMessage::BalanceUpdate {
                        asset: update.asset.as_str().to_string(),
                        available: update.available.to_string(),
                        locked: update.locked.to_string(),
                    };
                    if let Some(tx) = self.private_txs.get(&user.0) {
                        let _ = tx.send(msg);
                    }
                    let data = serde_json::json!({
                        "type": "balance_update",
                        "asset": update.asset.as_str(),
                        "available": update.available.to_string(),
                        "locked": update.locked.to_string(),
                    });
                    self.send_account_envelope(update.user.0, "balance_update", data);
                }
                Response::Error(_) => {}
            }
        }
    }
}

impl Default for FeedManager {
    fn default() -> Self {
        Self::new()
    }
}
