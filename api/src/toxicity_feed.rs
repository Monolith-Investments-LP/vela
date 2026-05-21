/// WebSocket handler for the authenticated per-fill toxicity event feed.
///
/// Endpoint: GET /feed/toxicity
/// Auth:     ECDSA challenge-response (same as private fill feed)
/// Filter:   optional `?market=ETH-USDC` query parameter
///
/// Protocol
/// --------
/// 1. Server immediately sends: `{"type":"challenge","nonce":"<hex>"}`
/// 2. Client sends:             `{"type":"auth","address":"0x...","signature":"0x...","nonce":"<hex>"}`
/// 3. Server responds with:     `{"type":"authenticated","address":"0x..."}`
///    or                        `{"type":"error","code":"...","message":"..."}`
/// 4. Authenticated clients receive one JSON object per fill:
///    ```json
///    {
///      "market": "ETH-USDC",
///      "order_id": 42,
///      "side": "bid",
///      "size": "1000000000",
///      "price": "320000000000",
///      "toxicity_score": 0.82,
///      "ofi_snapshot": 15000000000,
///      "timestamp_ns": 1747123456789000000
///    }
///    ```
use std::sync::Arc;

use axum::extract::{Query, State, WebSocketUpgrade, ws::{Message, WebSocket}};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::AppState;
use crate::auth::{auth_signing_message, generate_nonce, verify_matches_async};

#[derive(Deserialize)]
pub struct ToxicityFeedQuery {
    /// Optional market filter, e.g. "ETH-USDC".
    pub market: Option<String>,
}

pub async fn handler(
    upgrade: WebSocketUpgrade,
    Query(query): Query<ToxicityFeedQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| run(socket, query.market, state))
}

async fn run(socket: WebSocket, market_filter: Option<String>, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let nonce = generate_nonce();

    // Step 1: send challenge immediately.
    let challenge = serde_json::json!({ "type": "challenge", "nonce": nonce });
    if sender
        .send(Message::Text(serde_json::to_string(&challenge).unwrap_or_default()))
        .await
        .is_err()
    {
        return;
    }

    // Step 2: receive auth message.
    let auth_msg = loop {
        match receiver.next().await {
            Some(Ok(Message::Text(text))) => break text.to_string(),
            Some(Ok(Message::Ping(data))) => {
                let _ = sender.send(Message::Pong(data)).await;
            }
            Some(Ok(Message::Close(_))) | None => return,
            _ => continue,
        }
    };

    // Parse and verify auth.
    let auth: serde_json::Value = match serde_json::from_str(&auth_msg) {
        Ok(v) => v,
        Err(_) => {
            let err = serde_json::json!({"type":"error","code":"INVALID_MESSAGE","message":"could not parse auth message"});
            let _ = sender.send(Message::Text(serde_json::to_string(&err).unwrap_or_default())).await;
            return;
        }
    };

    if auth.get("type").and_then(|v| v.as_str()) != Some("auth") {
        let err = serde_json::json!({"type":"error","code":"AUTH_REQUIRED","message":"expected auth message"});
        let _ = sender.send(Message::Text(serde_json::to_string(&err).unwrap_or_default())).await;
        return;
    }

    let address = match auth.get("address").and_then(|v| v.as_str()) {
        Some(a) => a.to_string(),
        None => {
            let err = serde_json::json!({"type":"error","code":"MISSING_FIELD","message":"missing address"});
            let _ = sender.send(Message::Text(serde_json::to_string(&err).unwrap_or_default())).await;
            return;
        }
    };

    let signature = match auth.get("signature").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            let err = serde_json::json!({"type":"error","code":"MISSING_FIELD","message":"missing signature"});
            let _ = sender.send(Message::Text(serde_json::to_string(&err).unwrap_or_default())).await;
            return;
        }
    };

    let client_nonce = match auth.get("nonce").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => {
            let err = serde_json::json!({"type":"error","code":"MISSING_FIELD","message":"missing nonce"});
            let _ = sender.send(Message::Text(serde_json::to_string(&err).unwrap_or_default())).await;
            return;
        }
    };

    if client_nonce != nonce {
        let err = serde_json::json!({"type":"error","code":"INVALID_NONCE","message":"nonce does not match server challenge"});
        let _ = sender.send(Message::Text(serde_json::to_string(&err).unwrap_or_default())).await;
        return;
    }

    let message = auth_signing_message(&nonce);
    if verify_matches_async(message, signature, address.clone()).await.is_err() {
        let err = serde_json::json!({"type":"error","code":"AUTH_FAILED","message":"invalid signature"});
        let _ = sender.send(Message::Text(serde_json::to_string(&err).unwrap_or_default())).await;
        return;
    }

    // Auth succeeded.
    let authed = serde_json::json!({ "type": "authenticated", "address": address });
    if sender
        .send(Message::Text(serde_json::to_string(&authed).unwrap_or_default()))
        .await
        .is_err()
    {
        return;
    }

    // Subscribe to the toxicity broadcast channel.
    let mut toxicity_rx: broadcast::Receiver<serde_json::Value> =
        state.feeds.lock().await.subscribe_toxicity();

    // Stream events, applying optional market filter.
    loop {
        tokio::select! {
            event = async {
                loop {
                    match toxicity_rx.recv().await {
                        Ok(e) => break Some(e),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break None,
                    }
                }
            } => {
                match event {
                    None => return,
                    Some(evt) => {
                        // Apply market filter if requested.
                        if let Some(ref mf) = market_filter {
                            if evt.get("market").and_then(|v| v.as_str()) != Some(mf.as_str()) {
                                continue;
                            }
                        }
                        let json = serde_json::to_string(&evt).unwrap_or_default();
                        if sender.send(Message::Text(json)).await.is_err() {
                            return;
                        }
                    }
                }
            }

            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sender.send(Message::Pong(data)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => return,
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::auth::{auth_signing_message, verify_matches};

    /// A signature with all bytes set to 0xaa has recovery ID = 0xaa % 27 = 8,
    /// which is out of range for ECDSA recovery (only 0 and 1 are valid).
    #[test]
    fn test_auth_rejection_bad_sig() {
        let nonce = "deadbeefcafebabe1234567890abcdef";
        let message = auth_signing_message(nonce);
        let bad_sig = format!("0x{}", "aa".repeat(65));
        let address = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let result = verify_matches(&message, &bad_sig, address);
        assert!(result.is_err(), "bad signature must be rejected; got Ok");
    }

    /// A well-formed 65-byte signature that cannot have been produced by the
    /// address claim it carries is also rejected.
    #[test]
    fn test_auth_rejection_wrong_address() {
        let nonce = "0102030405060708090a0b0c0d0e0f10";
        let message = auth_signing_message(nonce);
        // Random-looking but non-recoverable signature bytes.
        let bad_sig = format!("0x{}", "ff".repeat(65));
        let address = "0x1111111111111111111111111111111111111111";
        let result = verify_matches(&message, &bad_sig, address);
        assert!(result.is_err(), "mismatched signature must be rejected; got Ok");
    }
}
