use crate::types::{WsEnvelope, WsServerMessage};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use types::{Response, UserId};

/// Per-channel broadcast buffer size. Overridable via
/// `VELA_FEED_CHANNEL_SIZE` at process start. Slow subscribers that fall
/// more than this many messages behind receive `RecvError::Lagged` and
/// increment `FEED_LAG_DROPS`.
fn channel_capacity() -> usize {
    std::env::var("VELA_FEED_CHANNEL_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024)
}

/// Cumulative count of publish attempts that landed on a broadcast
/// channel with zero live receivers. Exported via /metrics.
pub static FEED_NO_SUBSCRIBER_DROPS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Increment on every publish where broadcast::Sender::send returned Err
/// (all receivers dropped or channel closed). Latency-neutral.
fn note_drop_if_err<T>(result: Result<usize, broadcast::error::SendError<T>>) {
    if result.is_err() {
        FEED_NO_SUBSCRIBER_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub struct FeedManager {
    public_tx: broadcast::Sender<WsServerMessage>,
    private_txs: HashMap<[u8; 20], broadcast::Sender<WsServerMessage>>,
    private_envelope_txs: HashMap<[u8; 20], broadcast::Sender<WsEnvelope>>,
    private_envelope_seqs: HashMap<[u8; 20], u64>,
    /// Public authenticated channel for per-fill toxicity events.
    pub toxicity_tx: broadcast::Sender<serde_json::Value>,
    /// Institutional drop-copy: fills-only, per-account, delivered to a
    /// connection separate from the trading connection so risk and
    /// back-office systems can consume fills without depending on the
    /// trading session's health. Every fill emitted to the account
    /// channel is mirrored here.
    dropcopy_txs: HashMap<[u8; 20], broadcast::Sender<WsEnvelope>>,
    /// Independent sequence numbers per dropcopy channel so gap
    /// detection works without interference from the account channel.
    dropcopy_seqs: HashMap<[u8; 20], u64>,
    channel_capacity: usize,
}

impl FeedManager {
    pub fn new() -> Self {
        let capacity = channel_capacity();
        let (public_tx, _) = broadcast::channel(capacity);
        let (toxicity_tx, _) = broadcast::channel(capacity);
        FeedManager {
            public_tx,
            private_txs: HashMap::new(),
            private_envelope_txs: HashMap::new(),
            private_envelope_seqs: HashMap::new(),
            toxicity_tx,
            dropcopy_txs: HashMap::new(),
            dropcopy_seqs: HashMap::new(),
            channel_capacity: capacity,
        }
    }

    pub fn subscribe_public(&self) -> broadcast::Receiver<WsServerMessage> {
        self.public_tx.subscribe()
    }

    pub fn subscribe_private(&mut self, user: &UserId) -> broadcast::Receiver<WsServerMessage> {
        let capacity = self.channel_capacity;
        self.private_txs
            .entry(user.0)
            .or_insert_with(|| broadcast::channel(capacity).0)
            .subscribe()
    }

    pub fn subscribe_account_private(&mut self, user: &UserId) -> broadcast::Receiver<WsEnvelope> {
        let capacity = self.channel_capacity;
        self.private_envelope_txs
            .entry(user.0)
            .or_insert_with(|| broadcast::channel(capacity).0)
            .subscribe()
    }

    /// Subscribe to the fills-only drop-copy channel for `user`. Every
    /// fill on this account is mirrored here from the account channel,
    /// with its own sequence numbering, on an independent broadcast so
    /// a slow risk/back-office consumer never back-pressures trading.
    pub fn subscribe_dropcopy(&mut self, user: &UserId) -> broadcast::Receiver<WsEnvelope> {
        let capacity = self.channel_capacity;
        self.dropcopy_txs
            .entry(user.0)
            .or_insert_with(|| broadcast::channel(capacity).0)
            .subscribe()
    }

    fn next_dropcopy_seq(&mut self, user_bytes: [u8; 20]) -> u64 {
        let seq = self.dropcopy_seqs.entry(user_bytes).or_insert(0);
        *seq += 1;
        *seq
    }

    /// Returns a receiver for the toxicity event broadcast channel.
    pub fn subscribe_toxicity(&self) -> broadcast::Receiver<serde_json::Value> {
        self.toxicity_tx.subscribe()
    }

    pub fn publish_public(&self, msg: WsServerMessage) {
        note_drop_if_err(self.public_tx.send(msg));
    }

    pub fn publish_private(&self, user: &UserId, msg: WsServerMessage) {
        if let Some(tx) = self.private_txs.get(&user.0) {
            note_drop_if_err(tx.send(msg));
        }
    }

    /// Broadcast a toxicity event to all authenticated subscribers.
    pub fn publish_toxicity(&self, event: serde_json::Value) {
        note_drop_if_err(self.toxicity_tx.send(event));
    }

    fn next_account_seq(&mut self, user_bytes: [u8; 20]) -> u64 {
        let seq = self.private_envelope_seqs.entry(user_bytes).or_insert(0);
        *seq += 1;
        *seq
    }

    fn send_account_envelope(
        &mut self,
        user_bytes: [u8; 20],
        msg_type: &str,
        data: serde_json::Value,
    ) {
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

    /// Send a fill payload to the drop-copy channel for `user_bytes`.
    /// The envelope's channel string is `dropcopy:{address}` and its
    /// sequence number is drawn from the independent dropcopy_seqs map
    /// so drop-copy consumers can detect gaps without being confused by
    /// the account channel's seq stream.
    fn send_dropcopy_envelope(&mut self, user_bytes: [u8; 20], data: serde_json::Value) {
        let address = format!("0x{}", hex::encode(user_bytes));
        let channel = format!("dropcopy:{}", address);
        let seq = self.next_dropcopy_seq(user_bytes);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let envelope = WsEnvelope {
            msg_type: "fill".to_string(),
            channel,
            seq,
            data,
            timestamp: ts,
        };
        if let Some(tx) = self.dropcopy_txs.get(&user_bytes) {
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
                    self.send_account_envelope(fill.taker.0, "fill", fill_data.clone());

                    // Mirror to the drop-copy channel with independent
                    // sequence numbering so risk / back-office systems
                    // can consume fills on a connection separate from
                    // trading. Each side's drop-copy is delivered
                    // independently; the sender is fire-and-forget so a
                    // slow drop-copy consumer never back-pressures the
                    // trading path.
                    self.send_dropcopy_envelope(fill.maker.0, fill_data.clone());
                    self.send_dropcopy_envelope(fill.taker.0, fill_data);

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
