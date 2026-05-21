use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use types::{MarketId, PRICE_DECIMALS, QUANTITY_DECIMALS};
use crate::AppState;
use crate::auth::{auth_signing_message, generate_nonce, verify_matches_async, ws_auth_signing_message};
use crate::types::{WsClientMessage, WsEnvelope, WsServerMessage, format_amount};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_ws_seq(state: &AppState, channel: &str) -> u64 {
    let entry = state.ws_seqs
        .entry(channel.to_string())
        .or_insert_with(|| std::sync::atomic::AtomicU64::new(0));
    entry.fetch_add(1, Ordering::Relaxed) + 1
}

pub async fn run_background_task(state: Arc<AppState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut markets_counter: u64 = 0;

    loop {
        tick.tick().await;
        markets_counter += 1;
        let ts = now_ms();

        type BookEntry = (String, Vec<[String; 2]>, Vec<[String; 2]>);
        let book_data: Vec<BookEntry> = {
            let engine = state.engine.lock().await;
            engine.markets.keys()
                .filter_map(|market_id| {
                    let book = engine.order_books.get(market_id)?;
                    let bids = book.depth_bids(50).iter()
                        .map(|(p, q)| [format_amount(*p, PRICE_DECIMALS), format_amount(*q, QUANTITY_DECIMALS)])
                        .collect();
                    let asks = book.depth_asks(50).iter()
                        .map(|(p, q)| [format_amount(*p, PRICE_DECIMALS), format_amount(*q, QUANTITY_DECIMALS)])
                        .collect();
                    Some((market_id.0.clone(), bids, asks))
                })
                .collect()
        };

        for (market_id, bids, asks) in book_data {
            let channel = format!("orderbook:{}", market_id);
            let seq = next_ws_seq(&state, &channel);
            let envelope = WsEnvelope {
                msg_type: "snapshot".to_string(),
                channel,
                seq,
                data: serde_json::json!({ "bids": bids, "asks": asks }),
                timestamp: ts,
            };
            let _ = state.ws_tx.send(envelope);
        }

        if markets_counter.is_multiple_of(5) {
            let summaries: Vec<serde_json::Value> = {
                let engine = state.engine.lock().await;
                engine.markets.values().map(|m| {
                    let book = engine.order_books.get(&m.id);
                    serde_json::json!({
                        "id": m.id.0,
                        "base": m.base.as_str(),
                        "quote": m.quote.as_str(),
                        "best_bid": book.and_then(|b| b.best_bid()).map(|p| format_amount(p, PRICE_DECIMALS)),
                        "best_ask": book.and_then(|b| b.best_ask()).map(|p| format_amount(p, PRICE_DECIMALS)),
                        "spread": book.and_then(|b| b.spread()).map(|s| format_amount(s, PRICE_DECIMALS)),
                    })
                }).collect()
            };
            let channel = "markets".to_string();
            let seq = next_ws_seq(&state, &channel);
            let envelope = WsEnvelope {
                msg_type: "markets".to_string(),
                channel,
                seq,
                data: serde_json::json!({ "markets": summaries }),
                timestamp: ts,
            };
            let _ = state.ws_tx.send(envelope);
        }
    }
}

pub async fn handle_ws(socket: WebSocket, state: Arc<AppState>) {
    state.ws_client_count.fetch_add(1, Ordering::Relaxed);
    handle_ws_inner(socket, state.clone()).await;
    state.ws_client_count.fetch_sub(1, Ordering::Relaxed);
}

async fn handle_ws_inner(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut authenticated_user: Option<types::UserId> = None;
    let mut public_rx = state.feeds.lock().await.subscribe_public();
    let mut private_rx: Option<broadcast::Receiver<WsServerMessage>> = None;
    let mut account_rx: Option<broadcast::Receiver<WsEnvelope>> = None;
    let mut pending_nonce: Option<String> = None;
    let mut subscribed_channels: HashSet<String> = HashSet::new();
    let mut ws_rx = state.ws_tx.subscribe();

    loop {
        tokio::select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<WsClientMessage>(&text) {
                            Ok(client_msg) => {
                                let responses = handle_client_message(
                                    client_msg,
                                    &state,
                                    &mut authenticated_user,
                                    &mut private_rx,
                                    &mut account_rx,
                                    &mut pending_nonce,
                                    &mut subscribed_channels,
                                ).await;
                                for json in responses {
                                    if sender.send(Message::Text(json)).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(_) => {
                                let err = WsServerMessage::Error {
                                    code: "INVALID_MESSAGE".to_string(),
                                    message: "could not parse message".to_string(),
                                };
                                let json = serde_json::to_string(&err).unwrap_or_default();
                                let _ = sender.send(Message::Text(json)).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = sender.send(Message::Pong(data)).await;
                    }
                    _ => {}
                }
            }

            msg = async {
                loop {
                    match public_rx.recv().await {
                        Ok(m) => break Some(m),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break None,
                    }
                }
            } => {
                match msg {
                    Some(m) => {
                        let json = serde_json::to_string(&m).unwrap_or_default();
                        if sender.send(Message::Text(json)).await.is_err() { return; }
                    }
                    None => return,
                }
            }

            msg = async {
                match private_rx.as_mut() {
                    None => std::future::pending::<Option<WsServerMessage>>().await,
                    Some(rx) => loop {
                        match rx.recv().await {
                            Ok(m) => break Some(m),
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break None,
                        }
                    },
                }
            } => {
                match msg {
                    Some(m) => {
                        let json = serde_json::to_string(&m).unwrap_or_default();
                        if sender.send(Message::Text(json)).await.is_err() { return; }
                    }
                    None => return,
                }
            }

            msg = async {
                loop {
                    match ws_rx.recv().await {
                        Ok(m) => break Some(m),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break None,
                    }
                }
            } => {
                match msg {
                    Some(envelope) => {
                        if subscribed_channels.contains(&envelope.channel) {
                            let json = serde_json::to_string(&envelope).unwrap_or_default();
                            if sender.send(Message::Text(json)).await.is_err() { return; }
                        }
                    }
                    None => return,
                }
            }

            msg = async {
                match account_rx.as_mut() {
                    None => std::future::pending::<Option<WsEnvelope>>().await,
                    Some(rx) => loop {
                        match rx.recv().await {
                            Ok(m) => break Some(m),
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => break None,
                        }
                    },
                }
            } => {
                match msg {
                    Some(envelope) => {
                        let json = serde_json::to_string(&envelope).unwrap_or_default();
                        if sender.send(Message::Text(json)).await.is_err() { return; }
                    }
                    None => return,
                }
            }
        }
    }
}

async fn handle_client_message(
    msg: WsClientMessage,
    state: &Arc<AppState>,
    authenticated_user: &mut Option<types::UserId>,
    private_rx: &mut Option<broadcast::Receiver<WsServerMessage>>,
    account_rx: &mut Option<broadcast::Receiver<WsEnvelope>>,
    pending_nonce: &mut Option<String>,
    subscribed_channels: &mut HashSet<String>,
) -> Vec<String> {
    match msg {
        WsClientMessage::Ping => {
            vec![serde_json::to_string(&WsServerMessage::Pong).unwrap_or_default()]
        }

        WsClientMessage::RequestChallenge => {
            let nonce = generate_nonce();
            *pending_nonce = Some(nonce.clone());
            vec![serde_json::to_string(&WsServerMessage::Challenge { nonce }).unwrap_or_default()]
        }

        WsClientMessage::Auth { address, signature, nonce, timestamp } => {
            if let Some(ts) = timestamp {
                handle_timestamp_auth(
                    address, signature, ts,
                    state, account_rx, subscribed_channels,
                ).await
            } else {
                let nonce_str = nonce.unwrap_or_default();
                handle_challenge_auth(
                    address, signature, nonce_str,
                    state, authenticated_user, private_rx, pending_nonce,
                ).await
            }
        }

        WsClientMessage::Subscribe { channels } => {
            let ts = now_ms();
            let mut responses: Vec<String> = vec![];

            {
                let engine = state.engine.lock().await;
                let fills = state.fills.lock().await;

                for channel in &channels {
                    subscribed_channels.insert(channel.clone());

                    if let Some(market_str) = channel.strip_prefix("book.") {
                        let market = MarketId(market_str.to_string());
                        if let Some(book) = engine.order_books.get(&market) {
                            let bids = book.depth_bids(20).iter()
                                .map(|(p, q)| [format_amount(*p, PRICE_DECIMALS), format_amount(*q, QUANTITY_DECIMALS)])
                                .collect();
                            let asks = book.depth_asks(20).iter()
                                .map(|(p, q)| [format_amount(*p, PRICE_DECIMALS), format_amount(*q, QUANTITY_DECIMALS)])
                                .collect();
                            let snap = WsServerMessage::BookSnapshot {
                                market: market_str.to_string(),
                                bids,
                                asks,
                            };
                            responses.push(serde_json::to_string(&snap).unwrap_or_default());
                        }
                    } else if let Some(market_str) = channel.strip_prefix("orderbook:") {
                        let market = MarketId(market_str.to_string());
                        if let Some(book) = engine.order_books.get(&market) {
                            let bids: Vec<[String; 2]> = book.depth_bids(50).iter()
                                .map(|(p, q)| [format_amount(*p, PRICE_DECIMALS), format_amount(*q, QUANTITY_DECIMALS)])
                                .collect();
                            let asks: Vec<[String; 2]> = book.depth_asks(50).iter()
                                .map(|(p, q)| [format_amount(*p, PRICE_DECIMALS), format_amount(*q, QUANTITY_DECIMALS)])
                                .collect();
                            let seq = next_ws_seq(state, channel);
                            let envelope = WsEnvelope {
                                msg_type: "snapshot".to_string(),
                                channel: channel.clone(),
                                seq,
                                data: serde_json::json!({ "bids": bids, "asks": asks }),
                                timestamp: ts,
                            };
                            responses.push(serde_json::to_string(&envelope).unwrap_or_default());
                        }
                    } else if let Some(market_str) = channel.strip_prefix("trades:") {
                        let recent: Vec<_> = fills.iter()
                            .filter(|f| f.market_id == market_str)
                            .rev()
                            .take(20)
                            .collect();
                        for fill in recent {
                            let seq = next_ws_seq(state, channel);
                            let envelope = WsEnvelope {
                                msg_type: "trade".to_string(),
                                channel: channel.clone(),
                                seq,
                                data: serde_json::json!({
                                    "id": fill.id,
                                    "market_id": fill.market_id,
                                    "price": fill.price.to_string(),
                                    "quantity": fill.quantity.to_string(),
                                    "side": fill.side,
                                    "maker_order_id": fill.maker_order_id,
                                    "taker_order_id": fill.taker_order_id,
                                    "maker_address": fill.maker_address,
                                    "taker_address": fill.taker_address,
                                    "timestamp": fill.timestamp,
                                }),
                                timestamp: ts,
                            };
                            responses.push(serde_json::to_string(&envelope).unwrap_or_default());
                        }
                    } else if channel == "markets" {
                        let summaries: Vec<serde_json::Value> = engine.markets.values().map(|m| {
                            let book = engine.order_books.get(&m.id);
                            serde_json::json!({
                                "id": m.id.0,
                                "base": m.base.as_str(),
                                "quote": m.quote.as_str(),
                                "best_bid": book.and_then(|b| b.best_bid()).map(|p| format_amount(p, PRICE_DECIMALS)),
                                "best_ask": book.and_then(|b| b.best_ask()).map(|p| format_amount(p, PRICE_DECIMALS)),
                                "spread": book.and_then(|b| b.spread()).map(|s| format_amount(s, PRICE_DECIMALS)),
                            })
                        }).collect();
                        let seq = next_ws_seq(state, channel);
                        let envelope = WsEnvelope {
                            msg_type: "markets".to_string(),
                            channel: channel.clone(),
                            seq,
                            data: serde_json::json!({ "markets": summaries }),
                            timestamp: ts,
                        };
                        responses.push(serde_json::to_string(&envelope).unwrap_or_default());
                    }
                }
            }

            let subscribed = WsServerMessage::Subscribed { channels };
            responses.push(serde_json::to_string(&subscribed).unwrap_or_default());
            responses
        }

        WsClientMessage::Unsubscribe { channels } => {
            for channel in &channels {
                subscribed_channels.remove(channel);
            }
            let msg = WsServerMessage::Subscribed { channels: vec![] };
            vec![serde_json::to_string(&msg).unwrap_or_default()]
        }
    }
}

async fn handle_challenge_auth(
    address: String,
    signature: String,
    nonce: String,
    state: &Arc<AppState>,
    authenticated_user: &mut Option<types::UserId>,
    private_rx: &mut Option<broadcast::Receiver<WsServerMessage>>,
    pending_nonce: &mut Option<String>,
) -> Vec<String> {
    match pending_nonce.take() {
        None => {
            let err = WsServerMessage::Error {
                code: "NO_CHALLENGE".to_string(),
                message: "request a challenge before authenticating".to_string(),
            };
            vec![serde_json::to_string(&err).unwrap_or_default()]
        }
        Some(expected) if expected != nonce => {
            let err = WsServerMessage::Error {
                code: "INVALID_NONCE".to_string(),
                message: "nonce does not match server challenge".to_string(),
            };
            vec![serde_json::to_string(&err).unwrap_or_default()]
        }
        Some(_) => {
            let message = auth_signing_message(&nonce);
            match verify_matches_async(message, signature, address.clone()).await {
                Ok(user) => {
                    let rx = state.feeds.lock().await.subscribe_private(&user);
                    *private_rx = Some(rx);
                    *authenticated_user = Some(user);
                    let resp = WsServerMessage::Authenticated { address };
                    vec![serde_json::to_string(&resp).unwrap_or_default()]
                }
                Err(_) => {
                    let err = WsServerMessage::Error {
                        code: "AUTH_FAILED".to_string(),
                        message: "invalid signature".to_string(),
                    };
                    vec![serde_json::to_string(&err).unwrap_or_default()]
                }
            }
        }
    }
}

async fn handle_timestamp_auth(
    address: String,
    signature: String,
    timestamp: u64,
    state: &Arc<AppState>,
    account_rx: &mut Option<broadcast::Receiver<WsEnvelope>>,
    subscribed_channels: &mut HashSet<String>,
) -> Vec<String> {
    let now = now_ms();
    let diff = now.abs_diff(timestamp);
    if diff > 60_000 {
        let err = WsServerMessage::Error {
            code: "AUTH_EXPIRED".to_string(),
            message: "timestamp expired; must be within 60 seconds of server time".to_string(),
        };
        return vec![serde_json::to_string(&err).unwrap_or_default()];
    }

    let message = ws_auth_signing_message(&address, timestamp);
    match verify_matches_async(message, signature, address.clone()).await {
        Err(_) => {
            let err = WsServerMessage::Error {
                code: "AUTH_FAILED".to_string(),
                message: "Auth failed".to_string(),
            };
            vec![serde_json::to_string(&err).unwrap_or_default()]
        }
        Ok(user) => {
            let channel = format!("account:{}", address);
            subscribed_channels.insert(channel.clone());

            let rx = state.feeds.lock().await.subscribe_account_private(&user);
            *account_rx = Some(rx);

            let ts = now_ms();

            let (balances, orders) = {
                let engine = state.engine.lock().await;
                let bals: Vec<serde_json::Value> = engine.balances.iter()
                    .filter(|((u, _), _)| u == &user)
                    .map(|((_, asset), bal)| serde_json::json!({
                        "asset": asset.as_str(),
                        "available": crate::types::format_amount(bal.available, 8),
                        "locked": crate::types::format_amount(bal.locked, 8),
                        "total": crate::types::format_amount(bal.total(), 8),
                    }))
                    .collect();

                let meta = engine.metadata.get(&user);
                let open_ids = meta.map(|m| m.iter_order_ids().collect::<Vec<_>>()).unwrap_or_default();
                let ords: Vec<serde_json::Value> = engine.order_books.values()
                    .flat_map(|book| {
                        open_ids.iter().filter_map(|&id| {
                            book.get_order(id).map(|o| serde_json::json!({
                                "id": o.id,
                                "market": o.market.0,
                                "side": format!("{:?}", o.side).to_lowercase(),
                                "price": crate::types::format_amount(o.price, PRICE_DECIMALS),
                                "quantity": crate::types::format_amount(o.quantity, QUANTITY_DECIMALS),
                                "filled_quantity": crate::types::format_amount(o.filled_quantity, QUANTITY_DECIMALS),
                                "status": format!("{:?}", o.status).to_lowercase(),
                                "timestamp": o.timestamp,
                            }))
                        })
                    })
                    .collect();
                (bals, ords)
            };

            let seq = next_ws_seq(state, &channel);
            let snapshot = WsEnvelope {
                msg_type: "account_snapshot".to_string(),
                channel,
                seq,
                data: serde_json::json!({
                    "type": "account_snapshot",
                    "balances": balances,
                    "orders": orders,
                }),
                timestamp: ts,
            };

            let authed = WsServerMessage::Authenticated { address };
            vec![
                serde_json::to_string(&authed).unwrap_or_default(),
                serde_json::to_string(&snapshot).unwrap_or_default(),
            ]
        }
    }
}
