use std::sync::Arc;
use engine;
use axum::{
    extract::{Path, Query, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use futures_util::{SinkExt, StreamExt};
use k256::ecdsa::SigningKey;
use sha3::{Digest, Keccak256};
use tower_http::cors::{AllowOrigin, CorsLayer};
use types::{
    AssetId, CancelOrderRequest, DepositRequest, Fill, MarketId, NonceWindow, OrderSide, OrderStatus,
    OrderType, PostOrderRequest, Request as EngineRequest, Response as EngineResponse, UserId, UserMetadata, WithdrawalRequest,
    PRICE_DECIMALS, QUANTITY_DECIMALS,
};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;
use crate::{
    AppState,
    auth::{cancel_signing_message, eth_message_hash, order_signing_message, verify_matches_async, withdrawal_signing_message},
    types::{
        ApiResponse, BalanceResponse, BatchDetail, BatchSummary, BookLevel, BookResponse,
        CancelOrderBody, DepositBody, MarketResponse, OrderFillRecord, PostOrderBody,
        StateRootData, StoredFill, StoredOrder, WithdrawBody, WsEnvelope, format_amount,
    },
    wal,
    wal::{WalDeposit, WalFillCreated, WalOrderCancel, WalOrderPost, WalOrderProcessed, WalWithdrawalRequest},
    ws::handle_ws,
};

#[derive(serde::Deserialize)]
struct WithdrawalSignatureRequest {
    user: String,
    asset: String,
    amount: String,
    nonce: u64,
}

#[derive(serde::Serialize)]
struct WithdrawalSignatureData {
    signature: String,
    user: String,
    asset: String,
    amount_wei: String,
    nonce: u64,
}

fn parse_eth_amount_wei(s: &str) -> Option<u128> {
    let s = s.trim();
    let (integer_part, frac_part) = match s.find('.') {
        Some(pos) => (&s[..pos], &s[pos + 1..]),
        None => (s, ""),
    };
    let integer_val: u128 = integer_part.parse().ok()?;
    let mut frac_str = frac_part.to_string();
    while frac_str.len() < 18 {
        frac_str.push('0');
    }
    frac_str.truncate(18);
    let frac_val: u128 = frac_str.parse().ok()?;
    integer_val.checked_mul(1_000_000_000_000_000_000u128)?.checked_add(frac_val)
}

fn asset_address_for(asset: &str) -> Option<[u8; 20]> {
    if asset.eq_ignore_ascii_case("ETH") {
        Some([0u8; 20])
    } else {
        None
    }
}

fn sign_withdrawal_op(
    operator_key_hex: String,
    user_bytes: [u8; 20],
    asset_addr: [u8; 20],
    amount_wei: u128,
    nonce: u64,
    chain_id: u64,
    settlement_addr: [u8; 20],
) -> Result<String, String> {
    let key_hex = operator_key_hex.strip_prefix("0x").unwrap_or(&operator_key_hex).to_string();
    let key_bytes = hex::decode(&key_hex).map_err(|_| "invalid operator key".to_string())?;
    let signing_key = SigningKey::from_slice(&key_bytes).map_err(|e| e.to_string())?;

    // Must match VelaSettlement.withdrawHash: keccak256(user || asset || amount ||
    // nonce || chainid || address(this)) inside the "\x19Ethereum Signed Message" envelope.
    let mut packed: Vec<u8> = Vec::with_capacity(20 + 20 + 32 + 32 + 32 + 20);
    packed.extend_from_slice(&user_bytes);
    packed.extend_from_slice(&asset_addr);

    let mut amount_bytes = [0u8; 32];
    amount_bytes[16..].copy_from_slice(&amount_wei.to_be_bytes());
    packed.extend_from_slice(&amount_bytes);

    let mut nonce_bytes = [0u8; 32];
    nonce_bytes[24..].copy_from_slice(&nonce.to_be_bytes());
    packed.extend_from_slice(&nonce_bytes);

    let mut chain_id_bytes = [0u8; 32];
    chain_id_bytes[24..].copy_from_slice(&chain_id.to_be_bytes());
    packed.extend_from_slice(&chain_id_bytes);

    packed.extend_from_slice(&settlement_addr);

    let inner_hash: [u8; 32] = {
        let mut h = Keccak256::new();
        h.update(&packed);
        h.finalize().into()
    };

    let final_hash = eth_message_hash(&inner_hash);

    let (sig, recid) = signing_key
        .sign_prehash_recoverable(&final_hash)
        .map_err(|e| e.to_string())?;

    // OZ ECDSA rejects malleable (high-s) signatures — normalise before returning.
    let sig = sig.normalize_s().unwrap_or(sig);

    let mut eth_sig = Vec::with_capacity(65);
    eth_sig.extend_from_slice(sig.to_bytes().as_ref());
    eth_sig.push(recid.to_byte() + 27);

    Ok(format!("0x{}", hex::encode(&eth_sig)))
}

fn parse_hex_address(s: &str) -> Result<[u8; 20], String> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(trimmed).map_err(|_| format!("invalid hex address: {s}"))?;
    if bytes.len() != 20 {
        return Err(format!("address must be 20 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn settlement_context() -> Result<(u64, [u8; 20]), String> {
    let chain_id: u64 = std::env::var("VELA_CHAIN_ID")
        .unwrap_or_else(|_| "11155111".to_string())
        .parse()
        .map_err(|_| "VELA_CHAIN_ID must be a u64".to_string())?;
    let addr_str = std::env::var("VELA_SETTLEMENT_ADDRESS")
        .map_err(|_| "VELA_SETTLEMENT_ADDRESS env var must be set to the deployed contract address".to_string())?;
    let addr = parse_hex_address(&addr_str)?;
    Ok((chain_id, addr))
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "https://vela.monolithsystematic.com".parse::<HeaderValue>().unwrap(),
            "https://vela-vert.vercel.app".parse::<HeaderValue>().unwrap(),
            "http://localhost:3000".parse::<HeaderValue>().unwrap(),
        ]))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(tower_http::cors::Any);

    Router::new()
        .route("/health", get(health))
        .route("/status", get(status_handler))
        .route("/fees/public", get(fees_public_handler))
        .route("/markets", get(list_markets))
        .route("/markets/:market/book", get(get_book))
        .route("/account/:address/balances", get(get_balances))
        .route("/account/:address/orders", get(get_open_orders))
        .route("/account/:address/orders/by-client-id/:client_id", get(get_order_by_client_id))
        .route("/orders", post(post_order))
        .route("/orders/cancel", post(cancel_order))
        .route("/orders/:order_id", get(get_order_by_id))
        .route("/orders/:order_id/da-proof", get(get_da_proof))
        .route("/trades", get(list_trades))
        .route("/trades/:market_id", get(list_trades_by_market))
        .route("/withdrawals", post(initiate_withdrawal))
        .route("/withdrawal-signature", post(withdrawal_signature_handler))
        .route("/deposit", post(deposit_handler))
        .route("/force-include", post(force_include_handler))
        .route("/ws", get(ws_handler))
        .route("/feed/toxicity", get(crate::toxicity_feed::handler))
        .route("/feed/ohlcv/:market/:timeframe", get(ohlcv_feed_handler))
        .route("/fees", get(list_fees))
        .route("/markets/:market_id/fees", get(get_market_fees))
        .route("/admin/fees", get(admin_fees_handler))
        .route("/admin/state", get(admin_state_handler))
        .route("/admin/reserves", get(admin_reserves_handler))
        .route("/batches", get(list_batches))
        .route("/batches/:batch_id", get(get_batch))
        .route("/state-root", get(get_state_root))
        .route("/ohlcv/:market_id", get(ohlcv_handler))
        .route("/referral/register", post(register_referral))
        .route("/referral/:address", get(get_referral_handler))
        .route("/leaderboard", get(get_leaderboard))
        .route("/anchors", get(get_anchors))
        .route("/incidents", get(get_incidents))
        .route("/admin/incidents", post(create_incident))
        .route("/decisions", get(get_decisions))
        .route("/admin/decisions", post(create_decision))
        .route("/market-makers", get(get_market_makers))
        .route("/market-makers/register", post(register_market_maker))
        .route("/analytics", get(analytics_handler))
        .route("/analytics/:market_id", get(analytics_market_handler))
        .route("/batches/:batch_id/proof", get(batch_proof_handler))
        .route("/batches/:batch_id/attestation", get(batch_attestation_handler))
        .route("/proofs/stats", get(proof_stats_handler))
        .route("/proofs", get(proofs_list_handler))
        .route("/tee/stats", get(tee_stats_handler))
        .route("/attestations", get(attestations_list_handler))
        .route("/wal/stats", get(wal_stats_handler))
        .route("/order/encrypted", post(crate::committee_handler::post_encrypted_order))
        .route("/committee/share", post(crate::committee_handler::post_committee_share))
        .with_state(state)
        .layer(cors)
}

async fn health() -> &'static str {
    "ok"
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn list_markets(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let engine = state.engine.lock().await;
    let market_ids: Vec<_> = engine.markets.values().map(|m| (m.id.clone(), m.base, m.quote)).collect();
    drop(engine);
    let mut markets: Vec<MarketResponse> = Vec::with_capacity(market_ids.len());
    for (market_id, base, quote) in market_ids {
        let (best_bid, best_ask, spread) =
            if let Some(shard_arc) = state.shards.shards.get(&market_id) {
                let shard = shard_arc.lock().await;
                let book = shard.engine.order_books.get(&market_id);
                (
                    book.and_then(|b| b.best_bid()).map(|p| format_amount(p, PRICE_DECIMALS)),
                    book.and_then(|b| b.best_ask()).map(|p| format_amount(p, PRICE_DECIMALS)),
                    book.and_then(|b| b.spread()).map(|s| format_amount(s, PRICE_DECIMALS)),
                )
            } else {
                (None, None, None)
            };
        markets.push(MarketResponse {
            id: market_id.0,
            base: base.as_str().to_string(),
            quote: quote.as_str().to_string(),
            best_bid,
            best_ask,
            spread,
        });
    }
    Json(ApiResponse::ok(markets))
}

async fn get_book(
    Path(market): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let market_id = MarketId(market.clone());
    match state.shards.shards.get(&market_id) {
        Some(shard_arc) => {
            let shard = shard_arc.lock().await;
            match shard.engine.order_books.get(&market_id) {
                Some(book) => {
                    let bids = book.depth_bids(50).iter().map(|(p, q)| BookLevel {
                        price: format_amount(*p, PRICE_DECIMALS),
                        quantity: format_amount(*q, QUANTITY_DECIMALS),
                    }).collect();
                    let asks = book.depth_asks(50).iter().map(|(p, q)| BookLevel {
                        price: format_amount(*p, PRICE_DECIMALS),
                        quantity: format_amount(*q, QUANTITY_DECIMALS),
                    }).collect();
                    (StatusCode::OK, Json(ApiResponse::ok(BookResponse { market, bids, asks }))).into_response()
                }
                None => (StatusCode::NOT_FOUND, Json(ApiResponse::<()>::err("market not found"))).into_response(),
            }
        }
        None => (StatusCode::NOT_FOUND, Json(ApiResponse::<()>::err("market not found"))).into_response(),
    }
}

async fn get_balances(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&address) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response(),
    };
    let us = state.shards.user_state.read().await;
    let balances: Vec<BalanceResponse> = us.balances.iter()
        .filter(|((u, _), _)| u == &user)
        .map(|((_, asset), bal)| BalanceResponse {
            asset: asset.as_str().to_string(),
            available: format_amount(bal.available, 8),
            locked: format_amount(bal.locked, 8),
            total: format_amount(bal.total(), 8),
        })
        .collect();
    (StatusCode::OK, Json(ApiResponse::ok(balances))).into_response()
}

async fn get_open_orders(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&address) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response(),
    };
    let open_order_ids = {
        let us = state.shards.user_state.read().await;
        us.metadata.get(&user).map(|m| m.iter_order_ids().collect::<Vec<_>>()).unwrap_or_default()
    };
    let mut orders: Vec<serde_json::Value> = Vec::new();
    for shard_arc in state.shards.shards.values() {
        let shard = shard_arc.lock().await;
        for book in shard.engine.order_books.values() {
            for &id in &open_order_ids {
                if let Some(o) = book.get_order(id) {
                    orders.push(serde_json::json!({
                        "id": o.id,
                        "market": o.market.0,
                        "side": format!("{:?}", o.side).to_lowercase(),
                        "order_type": format!("{:?}", o.order_type).to_lowercase(),
                        "price": format_amount(o.price, PRICE_DECIMALS),
                        "quantity": format_amount(o.quantity, QUANTITY_DECIMALS),
                        "filled_quantity": format_amount(o.filled_quantity, QUANTITY_DECIMALS),
                        "status": format!("{:?}", o.status).to_lowercase(),
                        "nonce": o.nonce,
                        "client_order_id": o.client_order_id,
                        "timestamp": o.timestamp,
                    }));
                }
            }
        }
    }
    (StatusCode::OK, Json(ApiResponse::ok(orders))).into_response()
}

async fn get_order_by_client_id(
    Path((address, client_id)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&address) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response(),
    };

    let mut found_order = None;
    'outer: for shard_arc in state.shards.shards.values() {
        let shard = shard_arc.lock().await;
        for book in shard.engine.order_books.values() {
            if let Some(oid) = book.find_by_client_order_id(&user, &client_id) {
                found_order = book.get_order(oid).map(|o| serde_json::json!({
                    "id": o.id,
                    "market": o.market.0,
                    "side": format!("{:?}", o.side).to_lowercase(),
                    "order_type": format!("{:?}", o.order_type).to_lowercase(),
                    "price": format_amount(o.price, PRICE_DECIMALS),
                    "quantity": format_amount(o.quantity, QUANTITY_DECIMALS),
                    "filled_quantity": format_amount(o.filled_quantity, QUANTITY_DECIMALS),
                    "status": format!("{:?}", o.status).to_lowercase(),
                    "nonce": o.nonce,
                    "client_order_id": o.client_order_id,
                    "timestamp": o.timestamp,
                }));
                break 'outer;
            }
        }
    }
    match found_order {
        Some(o) => (StatusCode::OK, Json(ApiResponse::ok(o))).into_response(),
        None => (StatusCode::NOT_FOUND, Json(ApiResponse::<()>::err("order not found"))).into_response(),
    }
}

async fn post_order(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PostOrderBody>,
) -> impl IntoResponse {
    if !state.order_limiter.check(&body.address) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(ApiResponse::<()>::err("Rate limit exceeded. Please slow down."))).into_response();
    }

    if !body.address.starts_with("0x") || body.address.len() != 42 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Invalid wallet address format"))).into_response();
    }

    let user = match UserId::from_hex(&body.address) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Invalid wallet address format"))).into_response(),
    };

    if body.price == 0 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Price must be greater than 0"))).into_response();
    }
    if body.quantity == 0 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Quantity must be greater than 0"))).into_response();
    }
    if body.price >= u64::MAX / 2 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Price exceeds maximum allowed value"))).into_response();
    }
    if body.quantity >= u64::MAX / 2 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Quantity exceeds maximum allowed value"))).into_response();
    }

    let side_str = format!("{:?}", body.side).to_lowercase();
    let msg = order_signing_message(&body.market, &side_str, body.price, body.quantity, body.nonce, body.client_order_id.as_deref());
    if verify_matches_async(msg, body.signature.clone(), body.address.clone()).await.is_err() {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("invalid signature"))).into_response();
    }

    let req = PostOrderRequest {
        user: user.clone(),
        market: MarketId(body.market.clone()),
        side: body.side,
        order_type: body.order_type,
        price: body.price,
        quantity: body.quantity,
        nonce: body.nonce,
        client_order_id: body.client_order_id.clone(),
        signature: vec![],
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    {
        let engine = state.engine.lock().await;
        if !engine.markets.contains_key(&req.market) {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Market not found."))).into_response();
        }
    }

    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: EngineRequest::PostOrder(req),
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine unavailable"))).into_response();
    }
    let responses = match resp_rx.await {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine error"))).into_response(),
    };

    state.feeds.lock().await.dispatch_response_batch(&user, &responses);

    if let Some(msg) = first_engine_error(&responses) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err(msg))).into_response();
    }

    record_order_and_fills(&state, &body, &responses, ts).await;

    (StatusCode::OK, Json(ApiResponse::ok(responses))).into_response()
}

async fn record_order_and_fills(
    state: &Arc<AppState>,
    body: &PostOrderBody,
    responses: &[EngineResponse],
    ts: u64,
) {
    let fill_pairs: Vec<(String, Fill)> = responses
        .iter()
        .filter_map(|r| if let EngineResponse::OrderFilled(f) = r { Some(f.clone()) } else { None })
        .map(|f| {
            let id = format!("fill_{}_{}", f.maker_order_id, f.taker_order_id);
            (id, f)
        })
        .collect();

    let posted = responses.iter().find_map(|r| {
        if let EngineResponse::OrderPosted(p) = r { Some(p.clone()) } else { None }
    });

    let Some(posted) = posted else { return };

    state.orders_today.fetch_add(1, Ordering::Relaxed);

    let total_filled: u64 = fill_pairs.iter().map(|(_, f)| f.quantity).sum();

    let self_fills: Vec<OrderFillRecord> = fill_pairs
        .iter()
        .map(|(fill_id, f)| {
            let (counterparty_order_id, counterparty_address) = if f.taker_order_id == posted.order_id {
                (f.maker_order_id, f.maker.to_hex())
            } else {
                (f.taker_order_id, f.taker.to_hex())
            };
            OrderFillRecord {
                fill_id: fill_id.clone(),
                counterparty_order_id,
                counterparty_address,
                price: f.price,
                quantity: f.quantity,
                timestamp: f.timestamp,
            }
        })
        .collect();

    let new_order = StoredOrder {
        id: posted.order_id,
        market_id: body.market.clone(),
        user: body.address.clone(),
        side: side_to_str(body.side).to_string(),
        price: body.price,
        quantity: body.quantity,
        filled_quantity: total_filled,
        status: status_to_str(posted.status).to_string(),
        order_type: order_type_to_str(body.order_type).to_string(),
        time_in_force: order_type_to_tif(body.order_type).to_string(),
        nonce: body.nonce,
        client_order_id: body.client_order_id.clone(),
        signature: body.signature.clone(),
        created_at: ts,
        updated_at: ts,
        fills: self_fills,
        da_hash: None,
    };

    {
        let mut fills_guard = state.fills.lock().await;
        for (fill_id, f) in &fill_pairs {
            fills_guard.push(StoredFill {
                id: fill_id.clone(),
                market_id: body.market.clone(),
                price: f.price,
                quantity: f.quantity,
                maker_order_id: f.maker_order_id,
                taker_order_id: f.taker_order_id,
                maker_address: f.maker.to_hex(),
                taker_address: f.taker.to_hex(),
                timestamp: f.timestamp,
                side: side_to_str(f.side).to_string(),
                synthetic: false,
            });
            // Cap at 100k fills per market, evicting oldest first.
            let market_count = fills_guard.iter().filter(|fill| fill.market_id == body.market).count();
            if market_count > 100_000 {
                if let Some(idx) = fills_guard.iter().position(|fill| fill.market_id == body.market) {
                    fills_guard.remove(idx);
                }
            }
            let notional_micro = (f.price as u128 * f.quantity as u128 / 10_000_000_000u128) as u64;
            let taker_fee = notional_micro * 5 / 10000;
            let maker_rebate = notional_micro / 10000;
            state.fills_today.fetch_add(1, Ordering::Relaxed);
            state.volume_today_usdc.fetch_add(notional_micro, Ordering::Relaxed);
            state.fees_collected_today.fetch_add(taker_fee, Ordering::Relaxed);
            state.total_taker_fees_collected.fetch_add(taker_fee, Ordering::Relaxed);
            state.total_maker_rebates_paid.fetch_add(maker_rebate, Ordering::Relaxed);
        }

        // Broadcast updated current candle for each timeframe after all fills are stored.
        if !fill_pairs.is_empty() {
            let ws_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let last_ts = fill_pairs.iter().map(|(_, f)| f.timestamp).max().unwrap_or(0);
            let ts_s = last_ts / 1_000_000;

            const OHLCV_TIMEFRAMES: &[(&str, u64)] = &[
                ("1m", 60), ("5m", 300), ("15m", 900),
                ("1H", 3600), ("4H", 14400), ("1D", 86400),
            ];
            for &(tf_name, interval_secs) in OHLCV_TIMEFRAMES {
                let bucket = (ts_s / interval_secs) * interval_secs;
                let bucket_start_us = bucket * 1_000_000;
                let bucket_end_us = (bucket + interval_secs) * 1_000_000;

                let bucket_fills: Vec<_> = fills_guard.iter()
                    .filter(|fill| fill.market_id == body.market
                        && fill.timestamp >= bucket_start_us
                        && fill.timestamp < bucket_end_us)
                    .collect();

                if bucket_fills.is_empty() { continue; }

                let open = bucket_fills[0].price as f64 / 1_000_000.0;
                let close = bucket_fills[bucket_fills.len() - 1].price as f64 / 1_000_000.0;
                let high = bucket_fills.iter().map(|f| f.price).max().unwrap() as f64 / 1_000_000.0;
                let low = bucket_fills.iter().map(|f| f.price).min().unwrap() as f64 / 1_000_000.0;
                let volume = bucket_fills.iter().map(|f| f.quantity as f64).sum::<f64>() / 1_000_000.0;

                let channel = format!("ohlcv:{}:{}", body.market, tf_name);
                let seq = {
                    let entry = state.ws_seqs.entry(channel.clone())
                        .or_insert_with(|| std::sync::atomic::AtomicU64::new(0));
                    entry.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
                };
                let envelope = WsEnvelope {
                    msg_type: "ohlcv".to_string(),
                    channel,
                    seq,
                    data: serde_json::json!({
                        "market": body.market,
                        "timeframe": tf_name,
                        "candle": {
                            "time": bucket,
                            "open": open,
                            "high": high,
                            "low": low,
                            "close": close,
                            "volume": volume,
                        }
                    }),
                    timestamp: ws_now,
                };
                let _ = state.ws_tx.send(envelope);
            }
        }
    }

    let ws_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    for (fill_id, f) in &fill_pairs {
        use crate::types::WsEnvelope;
        let channel = format!("trades:{}", body.market);
        let seq = {
            let entry = state.ws_seqs
                .entry(channel.clone())
                .or_insert_with(|| std::sync::atomic::AtomicU64::new(0));
            entry.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
        };
        let envelope = WsEnvelope {
            msg_type: "trade".to_string(),
            channel,
            seq,
            data: serde_json::json!({
                "id": fill_id,
                "market_id": body.market,
                "price": f.price.to_string(),
                "quantity": f.quantity.to_string(),
                "side": side_to_str(f.side),
                "maker_order_id": f.maker_order_id,
                "taker_order_id": f.taker_order_id,
                "maker_address": f.maker.to_hex(),
                "taker_address": f.taker.to_hex(),
                "timestamp": f.timestamp,
            }),
            timestamp: ws_ts,
        };
        let _ = state.ws_tx.send(envelope);
    }

    {
        let mut orders_guard = state.stored_orders.lock().await;
        for (fill_id, f) in &fill_pairs {
            if let Some(maker_order) = orders_guard.get_mut(&f.maker_order_id) {
                maker_order.filled_quantity += f.quantity;
                maker_order.status = if maker_order.filled_quantity >= maker_order.quantity {
                    "filled".to_string()
                } else {
                    "partially_filled".to_string()
                };
                maker_order.updated_at = ts;
                maker_order.fills.push(OrderFillRecord {
                    fill_id: fill_id.clone(),
                    counterparty_order_id: f.taker_order_id,
                    counterparty_address: f.taker.to_hex(),
                    price: f.price,
                    quantity: f.quantity,
                    timestamp: f.timestamp,
                });
            }
        }
        orders_guard.insert(posted.order_id, new_order.clone());
    }

    let wal_post = WalOrderPost {
        order_id: posted.order_id,
        user: body.address.clone(),
        market_id: body.market.clone(),
        side: side_to_str(body.side).to_string(),
        price: body.price,
        quantity: body.quantity,
        order_type: order_type_to_str(body.order_type).to_string(),
        time_in_force: order_type_to_tif(body.order_type).to_string(),
        nonce: body.nonce,
        client_order_id: body.client_order_id.clone(),
    };
    if let Err(e) = state.wal.append(wal::ORDER_POST, &wal_post).await {
        tracing::error!("WAL ORDER_POST failed: {e}");
    }

    let result_str = if total_filled >= body.quantity {
        "filled"
    } else if total_filled > 0 {
        "partial"
    } else if matches!(posted.status, OrderStatus::Open | OrderStatus::PartiallyFilled) {
        "resting"
    } else {
        "rejected"
    };

    let wal_processed = WalOrderProcessed {
        order_id: posted.order_id,
        result: result_str.to_string(),
        fill_ids: fill_pairs.iter().map(|(id, _)| id.clone()).collect(),
        filled_quantity: total_filled,
        rejection_reason: None,
    };
    if let Err(e) = state.wal.append(wal::ORDER_PROCESSED, &wal_processed).await {
        tracing::error!("WAL ORDER_PROCESSED failed: {e}");
    }

    for (fill_id, f) in &fill_pairs {
        let wal_fill = WalFillCreated {
            fill_id: fill_id.clone(),
            market_id: body.market.clone(),
            maker_order_id: f.maker_order_id,
            taker_order_id: f.taker_order_id,
            maker_address: f.maker.to_hex(),
            taker_address: f.taker.to_hex(),
            price: f.price,
            quantity: f.quantity,
            maker_fee: f.maker_fee,
            taker_fee: f.taker_fee as u64,
        };
        if let Err(e) = state.wal.append(wal::FILL_CREATED, &wal_fill).await {
            tracing::error!("WAL FILL_CREATED failed: {e}");
        }
    }
    // One fsync per order batch — covers ORDER_POST, ORDER_PROCESSED, and all FILL_CREATED entries.
    if let Err(e) = state.wal.flush().await {
        tracing::error!("WAL flush failed: {e}");
    }

    let da_order_id = new_order.id;
    let da_bytes = serde_json::to_vec(&new_order).unwrap_or_default();
    let state_da = Arc::clone(state);
    tokio::spawn(async move {
        let seq = state_da.da.next_seq();
        let da = Arc::clone(&state_da.da);
        if let Ok(Ok((hash_hex, _url))) = tokio::task::spawn_blocking(move || da.submit_order(seq, &da_bytes)).await {
            if let Some(o) = state_da.stored_orders.lock().await.get_mut(&da_order_id) {
                o.da_hash = Some(hash_hex);
            }
        }
    });
}

fn side_to_str(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Bid => "bid",
        OrderSide::Ask => "ask",
    }
}

fn order_type_to_str(ot: OrderType) -> &'static str {
    match ot {
        OrderType::GoodTillCanceled => "limit",
        OrderType::PostOnly => "post_only",
        OrderType::ImmediateOrCancel => "limit",
        OrderType::FillOrKill => "limit",
    }
}

fn order_type_to_tif(ot: OrderType) -> &'static str {
    match ot {
        OrderType::GoodTillCanceled => "gtc",
        OrderType::PostOnly => "post_only",
        OrderType::ImmediateOrCancel => "ioc",
        OrderType::FillOrKill => "fok",
    }
}

fn status_to_str(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Open => "open",
        OrderStatus::PartiallyFilled => "partially_filled",
        OrderStatus::Filled => "filled",
        OrderStatus::Canceled => "cancelled",
        OrderStatus::Rejected => "rejected",
    }
}

async fn get_da_proof(
    Path(order_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let orders = state.stored_orders.lock().await;
    match orders.get(&order_id) {
        Some(order) => {
            let da_hash = order.da_hash.clone();
            (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
                "order_id": order_id,
                "da_hash": da_hash,
                "backend": "local",
            })))).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(ApiResponse::<()>::err("order not found"))).into_response(),
    }
}

async fn get_order_by_id(
    Path(order_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let orders = state.stored_orders.lock().await;
    match orders.get(&order_id) {
        Some(order) => (StatusCode::OK, Json(ApiResponse::ok(order.clone()))).into_response(),
        None => (StatusCode::NOT_FOUND, Json(ApiResponse::<()>::err("order not found"))).into_response(),
    }
}

async fn list_trades(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let fills = state.fills.lock().await;
    let mut result = fills.clone();
    result.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    result.truncate(500);
    Json(ApiResponse::ok(result))
}

async fn list_trades_by_market(
    Path(market_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let fills = state.fills.lock().await;
    let mut result: Vec<StoredFill> = fills
        .iter()
        .filter(|f| f.market_id == market_id)
        .cloned()
        .collect();
    result.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    result.truncate(500);
    Json(ApiResponse::ok(result))
}

async fn admin_reserves_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !state.verify_admin_token(provided) {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("unauthorized"))).into_response();
    }

    let us = state.shards.user_state.read().await;

    let mut engine_balances: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for ((_, asset), bal) in &us.balances {
        *engine_balances.entry(asset.as_str().to_string()).or_insert(0) += bal.total();
    }

    let total_users = us.metadata.len();
    let snapshot_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
        "engine_balances": engine_balances,
        "total_users": total_users,
        "snapshot_time": snapshot_time,
    })))).into_response()
}

async fn cancel_order(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CancelOrderBody>,
) -> impl IntoResponse {
    if !state.order_limiter.check(&body.address) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(ApiResponse::<()>::err("Rate limit exceeded. Please slow down."))).into_response();
    }

    let user = match UserId::from_hex(&body.address) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response(),
    };

    let msg = cancel_signing_message(
        body.order_id,
        body.client_order_id.as_deref(),
        body.nonce,
    );
    if verify_matches_async(msg, body.signature.clone(), body.address.clone()).await.is_err() {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("invalid signature"))).into_response();
    }

    let cancel_client_order_id = body.client_order_id.clone();
    let cancel_order_id_hint = body.order_id;

    let req = CancelOrderRequest {
        user: user.clone(),
        order_id: body.order_id,
        client_order_id: body.client_order_id,
        nonce: body.nonce,
        signature: vec![],
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: EngineRequest::CancelOrder(req),
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine unavailable"))).into_response();
    }
    let responses = match resp_rx.await {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine error"))).into_response(),
    };

    state.feeds.lock().await.dispatch_response_batch(&user, &responses);

    if let Some(msg) = first_engine_error(&responses) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err(msg))).into_response();
    }

    let canceled_id = responses.iter().find_map(|r| {
        if let EngineResponse::OrderCanceled(c) = r { Some(c.order_id) } else { None }
    }).or(cancel_order_id_hint);

    if let Some(order_id) = canceled_id {
        let wal_cancel = WalOrderCancel {
            order_id,
            client_order_id: cancel_client_order_id,
            user: user.to_hex(),
            reason: "user".to_string(),
        };
        if let Err(e) = state.wal.append(wal::ORDER_CANCEL, &wal_cancel).await {
            tracing::error!("WAL ORDER_CANCEL failed: {e}");
        }
    }

    (StatusCode::OK, Json(ApiResponse::ok(responses))).into_response()
}

async fn initiate_withdrawal(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WithdrawBody>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&body.address) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response(),
    };

    let msg = withdrawal_signing_message(&body.asset, body.amount, body.nonce);
    if verify_matches_async(msg, body.signature.clone(), body.address.clone()).await.is_err() {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("invalid signature"))).into_response();
    }

    let wal_asset = body.asset.clone();
    let wal_amount = body.amount;
    let wal_nonce = body.nonce;
    let wal_user_hex = body.address.clone();

    let req = WithdrawalRequest {
        user: user.clone(),
        asset: AssetId::from_str(&body.asset),
        amount: body.amount,
        nonce: body.nonce,
        signature: vec![],
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: EngineRequest::Withdrawal(req),
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine unavailable"))).into_response();
    }
    let responses = match resp_rx.await {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine error"))).into_response(),
    };

    state.feeds.lock().await.dispatch_response_batch(&user, &responses);

    if let Some(msg) = first_engine_error(&responses) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err(msg))).into_response();
    }

    let wal_wr = WalWithdrawalRequest {
        user: wal_user_hex,
        asset: wal_asset,
        amount: wal_amount,
        nonce: wal_nonce,
    };
    if let Err(e) = state.wal.append(wal::WITHDRAWAL_REQUEST, &wal_wr).await {
        tracing::error!("WAL WITHDRAWAL_REQUEST failed: {e}");
    }

    (StatusCode::OK, Json(ApiResponse::ok(responses))).into_response()
}

async fn deposit_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DepositBody>,
) -> impl IntoResponse {
    if !state.deposit_limiter.check(&body.user) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(ApiResponse::<()>::err("Rate limit exceeded. Please slow down."))).into_response();
    }

    if !body.user.starts_with("0x") || body.user.len() != 42 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Invalid wallet address format"))).into_response();
    }

    let user = match UserId::from_hex(&body.user) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Invalid wallet address format"))).into_response(),
    };

    if !KNOWN_ASSETS.iter().any(|&a| a.eq_ignore_ascii_case(&body.asset)) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Invalid asset. Supported: ETH, USDC, BTC, SOL, AVAX, MATIC, LINK, UNI, ARB, OP, AAVE, DOGE"))).into_response();
    }

    let amount = match parse_decimal_amount(&body.amount) {
        Some(a) => a,
        None => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid amount"))).into_response(),
    };

    if amount == 0 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Amount must be greater than 0"))).into_response();
    }

    if amount > 1_000_000_000_000u64 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Amount exceeds maximum deposit limit of 1,000,000"))).into_response();
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    let mut hasher = Keccak256::new();
    hasher.update(ts.to_le_bytes());
    hasher.update(body.user.as_bytes());
    hasher.update(body.asset.as_bytes());
    let hash_result = hasher.finalize();
    let mut l1_tx_hash = [0u8; 32];
    l1_tx_hash.copy_from_slice(&hash_result);

    let wal_dep_user = body.user.clone();
    let wal_dep_asset = body.asset.clone();
    let wal_tx_hash_hex = hex::encode(l1_tx_hash);

    let req = DepositRequest {
        user: user.clone(),
        asset: AssetId::from_str(&body.asset),
        amount,
        l1_tx_hash,
    };

    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: EngineRequest::Deposit(req),
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine unavailable"))).into_response();
    }
    let responses = match resp_rx.await {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine error"))).into_response(),
    };

    state.feeds.lock().await.dispatch_response_batch(&user, &responses);

    if let Some(msg) = first_engine_error(&responses) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err(msg))).into_response();
    }

    let wal_dep = WalDeposit {
        user: wal_dep_user,
        asset: wal_dep_asset,
        amount,
        tx_hash: Some(wal_tx_hash_hex),
    };
    if let Err(e) = state.wal.append(wal::DEPOSIT, &wal_dep).await {
        tracing::error!("WAL DEPOSIT failed: {e}");
    }

    let us = state.shards.user_state.read().await;
    let balances: Vec<BalanceResponse> = us.balances.iter()
        .filter(|((u, _), _)| u == &user)
        .map(|((_, asset), bal)| BalanceResponse {
            asset: asset.as_str().to_string(),
            available: format_amount(bal.available, 8),
            locked: format_amount(bal.locked, 8),
            total: format_amount(bal.total(), 8),
        })
        .collect();

    (StatusCode::OK, Json(ApiResponse::ok(balances))).into_response()
}

async fn withdrawal_signature_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<WithdrawalSignatureRequest>,
) -> impl IntoResponse {
    if !state.deposit_limiter.check(&body.user) {
        return (StatusCode::TOO_MANY_REQUESTS, Json(ApiResponse::<()>::err("Rate limit exceeded. Please slow down."))).into_response();
    }

    let operator_key = match std::env::var("OPERATOR_PRIVATE_KEY") {
        Ok(k) => k,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("Operator key not configured"))).into_response(),
    };

    let user_id = match UserId::from_hex(&body.user) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response(),
    };

    let asset_addr = match asset_address_for(&body.asset) {
        Some(a) => a,
        None => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("unsupported asset"))).into_response(),
    };

    let amount_wei = match parse_eth_amount_wei(&body.amount) {
        Some(a) => a,
        None => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid amount"))).into_response(),
    };

    let user_bytes = user_id.0;
    let user_hex = format!("0x{}", hex::encode(user_bytes));
    let asset_hex = format!("0x{}", hex::encode(asset_addr));
    let amount_wei_str = amount_wei.to_string();
    let nonce = body.nonce;

    let (chain_id, settlement_addr) = match settlement_context() {
        Ok(ctx) => ctx,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err(e))).into_response(),
    };

    let signature = match tokio::task::spawn_blocking(move || {
        sign_withdrawal_op(operator_key, user_bytes, asset_addr, amount_wei, nonce, chain_id, settlement_addr)
    })
    .await
    {
        Ok(Ok(sig)) => sig,
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err(e))).into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("signing failed"))).into_response(),
    };

    (StatusCode::OK, Json(ApiResponse::ok(WithdrawalSignatureData {
        signature,
        user: user_hex,
        asset: asset_hex,
        amount_wei: amount_wei_str,
        nonce,
    }))).into_response()
}


// ---------------------------------------------------------------------------
// VEL-P2-10: Forced-inclusion endpoint
// ---------------------------------------------------------------------------
//
// Users who believe the sequencer is censoring their transactions can submit
// them through this endpoint with an L1 proof.  In production the endpoint
// will verify a Merkle proof against the VelaSettlement.sol contract's delayed
// inbox root.  For the beta this is gated behind the admin token, since full
// L1 proof verification requires the on-chain integration (mainnet-only).
//
// Flow:
//  1. User submits transaction to L1 VelaSettlement.delayedInbox().
//  2. After timeout (1 hour on mainnet), user calls this endpoint with the
//     L1 tx hash and optional Merkle proof.
//  3. Engine processes the request immediately, bypassing signature checks
//     (the L1 submission is the proof of user intent).
//  4. Response mirrors the normal engine response format.

#[derive(serde::Deserialize)]
struct ForceIncludeBody {
    /// Hex-encoded L1 transaction hash (0x-prefixed, 32 bytes).
    l1_tx_hash: String,
    /// Type of the forced transaction.
    #[serde(flatten)]
    request: ForceIncludeRequest,
}

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ForceIncludeRequest {
    /// Force-credit a deposit that the sequencer refused to process.
    Deposit {
        user: String,
        asset: String,
        amount: u64,
    },
    /// Force-include a withdrawal request.
    Withdrawal {
        user: String,
        asset: String,
        amount: u64,
        nonce: u64,
    },
}

async fn force_include_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<ForceIncludeBody>,
) -> impl IntoResponse {
    // Gate behind admin token in beta — production will verify an L1 Merkle proof.
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "forced inclusion requires x-admin-token in beta;                  mainnet will verify L1 Merkle proof against VelaSettlement.delayedInbox()",
            )),
        )
            .into_response();
    }

    // Decode and validate the L1 tx hash — provides replay protection.
    let hash_str = body.l1_tx_hash.strip_prefix("0x").unwrap_or(&body.l1_tx_hash);
    let hash_bytes = match hex::decode(hash_str) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("l1_tx_hash must be a 0x-prefixed 32-byte hex string")),
            )
                .into_response();
        }
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    let (request, req_user) = match body.request {
        ForceIncludeRequest::Deposit { ref user, ref asset, amount } => {
            let uid = match UserId::from_hex(user) {
                Ok(u) => u,
                Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid user address"))).into_response(),
            };
            let req = EngineRequest::Deposit(DepositRequest {
                user: uid.clone(),
                asset: AssetId::from_str(asset),
                amount,
                l1_tx_hash: hash_bytes,
            });
            (req, uid)
        }
        ForceIncludeRequest::Withdrawal { ref user, ref asset, amount, nonce } => {
            let uid = match UserId::from_hex(user) {
                Ok(u) => u,
                Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid user address"))).into_response(),
            };
            let req = EngineRequest::Withdrawal(WithdrawalRequest {
                user: uid.clone(),
                asset: AssetId::from_str(asset),
                amount,
                nonce,
                signature: vec![], // bypassed — L1 tx hash is the proof of intent
            });
            (req, uid)
        }
    };

    // Process through the sharded dispatcher.
    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request,
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine unavailable"))).into_response();
    }
    let responses = match resp_rx.await {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(ApiResponse::<()>::err("engine error"))).into_response(),
    };

    state
        .feeds
        .lock()
        .await
        .dispatch_response_batch(&req_user, &responses);

    if let Some(msg) = first_engine_error(&responses) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err(msg))).into_response();
    }

    (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
        "l1_tx_hash": body.l1_tx_hash,
        "responses": responses,
        "note": "forced inclusion processed; committer will include in next batch",
    })))).into_response()
}

async fn admin_state_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !state.verify_admin_token(provided) {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("unauthorized"))).into_response();
    }

    let (market_ids_bases_quotes, total_users, total_deposits, total_open_orders) = {
        let engine = state.engine.lock().await;
        let mids: Vec<_> = engine.markets.values().map(|m| (m.id.clone(), m.base, m.quote)).collect();
        let us = state.shards.user_state.read().await;
        let tu = us.metadata.len();
        let td: Vec<serde_json::Value> = us.balances.iter().map(|((user, asset), bal)| {
            serde_json::json!({
                "user": format!("0x{}", hex::encode(user.0)),
                "asset": asset.as_str(),
                "amount": format_amount(bal.total(), 8),
            })
        }).collect();
        let too: usize = us.metadata.values().map(|m| m.order_id_count()).sum();
        (mids, tu, td, too)
    };

    let mut markets: Vec<serde_json::Value> = Vec::new();
    for (market_id, base, quote) in market_ids_bases_quotes {
        let (best_bid, best_ask) = if let Some(shard_arc) = state.shards.shards.get(&market_id) {
            let shard = shard_arc.lock().await;
            let book = shard.engine.order_books.get(&market_id);
            (
                book.and_then(|b| b.best_bid()).map(|p| format_amount(p, PRICE_DECIMALS)),
                book.and_then(|b| b.best_ask()).map(|p| format_amount(p, PRICE_DECIMALS)),
            )
        } else {
            (None, None)
        };
        markets.push(serde_json::json!({
            "id": market_id.0,
            "base": base.as_str(),
            "quote": quote.as_str(),
            "best_bid": best_bid,
            "best_ask": best_ask,
        }));
    }

    let snapshot_path = {
        let dir = std::env::var("SNAPSHOT_DIR").unwrap_or_else(|_| "/data".to_string());
        format!("{dir}/engine_snapshot.json")
    };
    let snapshot_exists = std::path::Path::new(&snapshot_path).exists();

    let uptime_secs = state.start_time.elapsed().as_secs();

    (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
        "markets": markets,
        "total_users": total_users,
        "total_deposits": total_deposits,
        "total_open_orders": total_open_orders,
        "snapshot_exists": snapshot_exists,
        "uptime_secs": uptime_secs,
    })))).into_response()
}

fn batch_state_root(fill_ids: &[String]) -> String {
    let mut hasher = Keccak256::new();
    for id in fill_ids {
        hasher.update(id.as_bytes());
    }
    format!("0x{}", hex::encode(hasher.finalize()))
}

fn stored_fill_to_proof_fill(fill: &StoredFill) -> zkvm::ProofFill {
    zkvm::ProofFill {
        fill_id: fill.id.clone(),
        market_id: fill.market_id.clone(),
        price: fill.price,
        quantity: fill.quantity,
        maker_address: fill.maker_address.clone(),
        taker_address: fill.taker_address.clone(),
        timestamp: fill.timestamp,
    }
}

fn spawn_proof(
    state: Arc<AppState>,
    batch_id: u64,
    state_root_before: String,
    state_root_after: String,
    fills: Vec<zkvm::ProofFill>,
    orders_processed: u64,
    timestamp: u64,
) {
    let pending = zkvm::BatchProof {
        batch_id,
        status: zkvm::ProofStatus::Pending,
        proof_bytes: None,
        public_inputs: None,
        prover: "placeholder".to_string(),
        generated_at: None,
        proving_time_ms: None,
        proof_size_bytes: None,
    };
    let proofs = Arc::clone(&state.proofs);
    let prover = Arc::clone(&state.prover);
    tokio::spawn(async move {
        proofs.lock().await.insert(batch_id, pending);
        let request = zkvm::ProofRequest {
            batch_id,
            state_root_before,
            state_root_after,
            fills,
            orders_processed,
            timestamp,
        };
        let result = prover.prove_batch(request).await;
        proofs.lock().await.insert(batch_id, result.proof);
    });
}

fn spawn_attestation(
    state: Arc<AppState>,
    batch_id: u64,
    state_root: String,
    fill_count: u64,
    orders_processed: u64,
    timestamp: u64,
) {
    let attestations = Arc::clone(&state.attestations);
    let attester = Arc::clone(&state.attester);
    let operator_address = std::env::var("OPERATOR_ADDRESS")
        .unwrap_or_else(|_| "0x0000000000000000000000000000000000000000".to_string());
    tokio::spawn(async move {
        let binary_hash = attester.binary_hash();
        let request = tee::AttestationRequest {
            batch_id,
            state_root,
            binary_hash,
            fill_count,
            orders_processed,
            timestamp,
            operator_address,
        };
        let result = attester.attest_batch(request).await;
        let mut store = attestations.lock().await;
        store.insert(batch_id, result.record);
        if store.len() > 1000 {
            if let Some(&min_key) = store.keys().min() {
                store.remove(&min_key);
            }
        }
    });
}

async fn list_batches(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    const WINDOW_US: u64 = 30_000_000;
    let fills = state.fills.lock().await;
    let mut windows: BTreeMap<u64, Vec<StoredFill>> = BTreeMap::new();
    for fill in fills.iter() {
        windows.entry(fill.timestamp / WINDOW_US).or_default().push(fill.clone());
    }
    drop(fills);

    let window_vec: Vec<(u64, Vec<StoredFill>)> = windows.into_iter().collect();
    let mut prev_root = format!("0x{}", "0".repeat(64));

    let batches: Vec<BatchSummary> = window_vec.iter().enumerate().map(|(idx, (window_key, batch_fills))| {
        let fill_ids: Vec<String> = batch_fills.iter().map(|f| f.id.clone()).collect();
        let mut order_ids: HashSet<u64> = HashSet::new();
        let mut markets: HashSet<String> = HashSet::new();
        for fill in batch_fills {
            order_ids.insert(fill.maker_order_id);
            order_ids.insert(fill.taker_order_id);
            markets.insert(fill.market_id.clone());
        }
        let mut markets_vec: Vec<String> = markets.into_iter().collect();
        markets_vec.sort();
        let state_root = batch_state_root(&fill_ids);
        BatchSummary {
            batch_id: (idx + 1) as u64,
            timestamp: window_key * WINDOW_US / 1000,
            fill_count: batch_fills.len(),
            order_count: order_ids.len(),
            markets: markets_vec,
            state_root,
            operator_signature: format!("0x{}", "0".repeat(130)),
            fills: fill_ids,
        }
    }).collect();

    for (idx, (window_key, batch_fills)) in window_vec.iter().enumerate() {
        let batch_id = (idx + 1) as u64;
        let fill_ids: Vec<String> = batch_fills.iter().map(|f| f.id.clone()).collect();
        let state_root_after = batch_state_root(&fill_ids);
        let state_root_before = prev_root.clone();
        prev_root = state_root_after.clone();

        let has_proof = state.proofs.lock().await.contains_key(&batch_id);
        if !has_proof {
            let proof_fills: Vec<zkvm::ProofFill> = batch_fills.iter().map(stored_fill_to_proof_fill).collect();
            let orders_processed = batch_fills.len() as u64 * (idx as u64 + 1);
            let timestamp = window_key * WINDOW_US / 1000;
            spawn_proof(Arc::clone(&state), batch_id, state_root_before, state_root_after.clone(), proof_fills, orders_processed, timestamp);
        }
        let has_attestation = state.attestations.lock().await.contains_key(&batch_id);
        if !has_attestation {
            let fill_count = batch_fills.len() as u64;
            let orders_processed = fill_count * (idx as u64 + 1);
            let timestamp = window_key * WINDOW_US / 1000;
            spawn_attestation(Arc::clone(&state), batch_id, state_root_after, fill_count, orders_processed, timestamp);
        }
    }

    Json(ApiResponse::ok(batches))
}

async fn get_batch(
    Path(batch_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    const WINDOW_US: u64 = 30_000_000;
    let fills = state.fills.lock().await;
    let mut windows: BTreeMap<u64, Vec<StoredFill>> = BTreeMap::new();
    for fill in fills.iter() {
        windows.entry(fill.timestamp / WINDOW_US).or_default().push(fill.clone());
    }
    drop(fills);

    let target_idx = batch_id.saturating_sub(1) as usize;
    let window_vec: Vec<(u64, Vec<StoredFill>)> = windows.into_iter().collect();
    let entry = window_vec.into_iter().enumerate().nth(target_idx);
    match entry {
        None => (StatusCode::NOT_FOUND, Json(ApiResponse::<()>::err("batch not found"))).into_response(),
        Some((idx, (window_key, batch_fills))) => {
            let fill_ids: Vec<String> = batch_fills.iter().map(|f| f.id.clone()).collect();
            let mut order_ids: HashSet<u64> = HashSet::new();
            let mut markets: HashSet<String> = HashSet::new();
            for fill in &batch_fills {
                order_ids.insert(fill.maker_order_id);
                order_ids.insert(fill.taker_order_id);
                markets.insert(fill.market_id.clone());
            }
            let mut markets_vec: Vec<String> = markets.into_iter().collect();
            markets_vec.sort();
            let state_root = batch_state_root(&fill_ids);

            let has_proof = state.proofs.lock().await.contains_key(&batch_id);
            if !has_proof {
                let state_root_after = state_root.clone();
                let state_root_before = if idx == 0 {
                    format!("0x{}", "0".repeat(64))
                } else {
                    let fills_guard = state.fills.lock().await;
                    let mut prev_windows: BTreeMap<u64, Vec<StoredFill>> = BTreeMap::new();
                    for f in fills_guard.iter() {
                        prev_windows.entry(f.timestamp / WINDOW_US).or_default().push(f.clone());
                    }
                    drop(fills_guard);
                    let prev_fill_ids: Vec<String> = prev_windows
                        .into_iter()
                        .nth(idx - 1)
                        .map(|(_, fs)| fs.iter().map(|f| f.id.clone()).collect())
                        .unwrap_or_default();
                    batch_state_root(&prev_fill_ids)
                };
                let proof_fills: Vec<zkvm::ProofFill> = batch_fills.iter().map(stored_fill_to_proof_fill).collect();
                let orders_processed = batch_fills.len() as u64;
                let timestamp = window_key * WINDOW_US / 1000;
                spawn_proof(Arc::clone(&state), batch_id, state_root_before, state_root_after, proof_fills, orders_processed, timestamp);
            }

            let has_attestation = state.attestations.lock().await.contains_key(&batch_id);
            if !has_attestation {
                let fill_count = batch_fills.len() as u64;
                let orders_processed = fill_count;
                let timestamp = window_key * WINDOW_US / 1000;
                spawn_attestation(Arc::clone(&state), batch_id, state_root.clone(), fill_count, orders_processed, timestamp);
            }

            let detail = BatchDetail {
                batch_id,
                timestamp: window_key * WINDOW_US / 1000,
                fill_count: batch_fills.len(),
                order_count: order_ids.len(),
                markets: markets_vec,
                state_root,
                operator_signature: format!("0x{}", "0".repeat(130)),
                fills: batch_fills,
            };
            (StatusCode::OK, Json(ApiResponse::ok(detail))).into_response()
        }
    }
}

async fn get_state_root(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let (order_count, user_count) = {
        let us = state.shards.user_state.read().await;
        let oc: usize = us.metadata.values().map(|m| m.order_id_count()).sum();
        let uc = us.metadata.len();
        (oc, uc)
    };

    let fills = state.fills.lock().await;
    let fill_ids: Vec<String> = fills.iter().map(|f| f.id.clone()).collect();
    drop(fills);

    let orders = state.stored_orders.lock().await;
    let order_ids: Vec<String> = orders.keys().map(|k| k.to_string()).collect();
    drop(orders);

    let mut hasher = Keccak256::new();
    for id in &fill_ids {
        hasher.update(id.as_bytes());
    }
    for id in &order_ids {
        hasher.update(id.as_bytes());
    }
    let state_root = format!("0x{}", hex::encode(hasher.finalize()));
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let last_anchor_tx = state.last_anchor_tx.lock().await.clone();
    let last_anchor_time_raw = state.last_anchor_time.load(Ordering::Relaxed);
    let last_anchor_time = if last_anchor_time_raw == 0 { None } else { Some(last_anchor_time_raw) };
    let anchor_count = state.anchor_count.load(Ordering::Relaxed);

    Json(ApiResponse::ok(StateRootData {
        state_root,
        timestamp,
        order_count,
        user_count,
        block_number: None,
        last_anchor_tx,
        last_anchor_time,
        anchor_count,
    }))
}

async fn get_anchors(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let anchors = state.anchors.lock().await;
    let mut result = anchors.clone();
    drop(anchors);

    result.reverse();

    let total = state.anchor_count.load(Ordering::Relaxed);

    let anchors_out: Vec<serde_json::Value> = result
        .iter()
        .map(|a| {
            serde_json::json!({
                "anchor_id": a.anchor_id,
                "state_root": a.state_root,
                "tx_hash": a.tx_hash,
                "timestamp": a.timestamp,
                "orders_processed": a.orders_processed,
                "block_number": a.block_number,
                "etherscan_url": format!("https://sepolia.etherscan.io/tx/{}", a.tx_hash),
            })
        })
        .collect();

    Json(ApiResponse::ok(serde_json::json!({
        "anchors": anchors_out,
        "total": total,
    })))
}

#[derive(serde::Deserialize)]
struct OhlcvQuery {
    timeframe: Option<String>,
    limit: Option<usize>,
}

#[derive(serde::Serialize, Clone)]
struct OhlcvCandle {
    time: u64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

fn timeframe_interval_secs(timeframe: &str) -> u64 {
    match timeframe {
        "1m" => 60,
        "5m" => 300,
        "15m" => 900,
        "1H" => 3600,
        "4H" => 14400,
        "1D" => 86400,
        _ => 3600,
    }
}

fn seed_price_for_market(market_id: &str) -> f64 {
    match market_id.split('-').next().unwrap_or("") {
        "BTC" => 65_000.0,
        "ETH" => 3_500.0,
        "SOL" => 150.0,
        "AVAX" => 35.0,
        "MATIC" => 1.0,
        "LINK" => 15.0,
        "UNI" => 10.0,
        "ARB" => 1.2,
        "OP" => 2.0,
        "AAVE" => 90.0,
        "DOGE" => 0.15,
        _ => 100.0,
    }
}

fn generate_simulated_candles(market_id: &str, interval_secs: u64, limit: usize) -> Vec<OhlcvCandle> {
    let base_price = seed_price_for_market(market_id);
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let latest_bucket = (now_s / interval_secs) * interval_secs;
    let count = limit.max(2);
    let mut candles = Vec::with_capacity(count);
    let mut price = base_price;
    for i in 0..count {
        let time = latest_bucket.saturating_sub(((count - 1 - i) as u64) * interval_secs);
        let noise = ((i as f64 * 0.7 + 13.0).sin() * 0.003 + 0.0005) * price;
        let open = price;
        let close = price + noise;
        let high = open.max(close) + noise.abs() * 0.3;
        let low = open.min(close) - noise.abs() * 0.3;
        let volume = base_price * 0.1 * (1.0 + (i as f64 * 0.3).sin().abs());
        candles.push(OhlcvCandle { time, open, high, low, close, volume });
        price = close;
    }
    candles
}

async fn ohlcv_handler(
    Path(market_id): Path<String>,
    Query(query): Query<OhlcvQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let timeframe = query.timeframe.as_deref().unwrap_or("1H");
    let limit = query.limit.unwrap_or(100).min(500);
    let interval_secs = timeframe_interval_secs(timeframe);

    let fills = state.fills.lock().await;
    let mut market_fills: Vec<&StoredFill> = fills
        .iter()
        .filter(|f| f.market_id == market_id)
        .collect();
    market_fills.sort_by_key(|f| f.timestamp);

    let mut buckets: BTreeMap<u64, Vec<&StoredFill>> = BTreeMap::new();
    for fill in &market_fills {
        let ts_s = fill.timestamp / 1_000_000;
        let bucket = (ts_s / interval_secs) * interval_secs;
        buckets.entry(bucket).or_default().push(fill);
    }

    let mut candles: Vec<OhlcvCandle> = buckets
        .into_iter()
        .map(|(bucket_time, bucket_fills)| {
            let open = bucket_fills.first().unwrap().price as f64 / 1_000_000.0;
            let close = bucket_fills.last().unwrap().price as f64 / 1_000_000.0;
            let high = bucket_fills.iter().map(|f| f.price).max().unwrap() as f64 / 1_000_000.0;
            let low = bucket_fills.iter().map(|f| f.price).min().unwrap() as f64 / 1_000_000.0;
            let volume = bucket_fills.iter().map(|f| f.quantity as f64).sum::<f64>() / 1_000_000.0;
            OhlcvCandle { time: bucket_time, open, high, low, close, volume }
        })
        .collect();

    // Sort ascending (most recent last), keep only the most recent `limit` candles.
    candles.sort_by_key(|c| c.time);
    if candles.len() > limit {
        candles.drain(..candles.len() - limit);
    }

    let has_live_prices = market_fills.iter().any(|f| f.synthetic);
    let has_real_data = market_fills.iter().any(|f| !f.synthetic) && candles.len() >= 2;

    if !has_live_prices && !has_real_data {
        candles = generate_simulated_candles(&market_id, interval_secs, limit);
    }

    let count = candles.len();
    Json(ApiResponse::ok(serde_json::json!({
        "market_id": market_id,
        "timeframe": timeframe,
        "candles": candles,
        "count": count,
        "has_real_data": has_real_data,
        "has_live_prices": has_live_prices,
    })))
}

async fn ohlcv_feed_handler(
    Path((market, timeframe)): Path<(String, String)>,
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| ohlcv_feed_ws(socket, market, timeframe, state))
}

async fn ohlcv_feed_ws(socket: WebSocket, market: String, timeframe: String, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let channel = format!("ohlcv:{}:{}", market, timeframe);
    let interval_secs = timeframe_interval_secs(&timeframe);

    // Send current candle snapshot on connect.
    {
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let bucket = (now_s / interval_secs) * interval_secs;
        let bucket_start_us = bucket * 1_000_000;
        let bucket_end_us = (bucket + interval_secs) * 1_000_000;

        let fills = state.fills.lock().await;
        let mut bucket_fills: Vec<_> = fills.iter()
            .filter(|f| f.market_id == market
                && f.timestamp >= bucket_start_us
                && f.timestamp < bucket_end_us)
            .collect();
        bucket_fills.sort_by_key(|f| f.timestamp);

        if !bucket_fills.is_empty() {
            let open = bucket_fills[0].price as f64 / 1_000_000.0;
            let close = bucket_fills[bucket_fills.len() - 1].price as f64 / 1_000_000.0;
            let high = bucket_fills.iter().map(|f| f.price).max().unwrap() as f64 / 1_000_000.0;
            let low = bucket_fills.iter().map(|f| f.price).min().unwrap() as f64 / 1_000_000.0;
            let volume = bucket_fills.iter().map(|f| f.quantity as f64).sum::<f64>() / 1_000_000.0;
            let snap = serde_json::json!({
                "type": "ohlcv",
                "channel": channel,
                "data": {
                    "market": market,
                    "timeframe": timeframe,
                    "candle": { "time": bucket, "open": open, "high": high, "low": low, "close": close, "volume": volume }
                }
            });
            if sender.send(Message::Text(serde_json::to_string(&snap).unwrap_or_default())).await.is_err() {
                return;
            }
        }
    }

    let mut ws_rx = state.ws_tx.subscribe();

    loop {
        tokio::select! {
            msg = async {
                loop {
                    match ws_rx.recv().await {
                        Ok(m) => break Some(m),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break None,
                    }
                }
            } => {
                match msg {
                    None => return,
                    Some(envelope) if envelope.channel == channel => {
                        let json = serde_json::to_string(&envelope).unwrap_or_default();
                        if sender.send(Message::Text(json)).await.is_err() { return; }
                    }
                    Some(_) => {}
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

fn parse_decimal_amount(s: &str) -> Option<u64> {
    let s = s.trim();
    let (integer_part, frac_part) = match s.find('.') {
        Some(pos) => (&s[..pos], &s[pos + 1..]),
        None => (s, ""),
    };
    let integer_val: u64 = integer_part.parse().ok()?;
    let mut frac_str = frac_part.to_string();
    while frac_str.len() < 6 {
        frac_str.push('0');
    }
    let frac_val: u64 = frac_str[..6].parse().ok()?;
    integer_val.checked_mul(1_000_000)?.checked_add(frac_val)
}

const KNOWN_ASSETS: &[&str] = &["ETH", "USDC", "BTC", "SOL", "AVAX", "MATIC", "LINK", "UNI", "ARB", "OP", "AAVE", "DOGE"];

fn engine_error_to_message(err: &str) -> String {
    if err.contains("insufficient") || err.contains("balance") {
        "Insufficient balance. Please deposit funds before trading.".to_string()
    } else if err.contains("nonce") {
        "Duplicate order. Please try again.".to_string()
    } else if err.contains("signature") || err.contains("verify") {
        "Invalid signature. Please reconnect your wallet and try again.".to_string()
    } else if err.contains("market") || err.contains("not found") {
        "Market not found.".to_string()
    } else if err.contains("credit") {
        "Credit limit exceeded. Reduce your open orders or deposit more funds.".to_string()
    } else if err.contains("post_only") || err.contains("would match") {
        "Post-only order would have matched immediately. Order rejected.".to_string()
    } else {
        "Order rejected. Please check your parameters and try again.".to_string()
    }
}

fn first_engine_error(responses: &[EngineResponse]) -> Option<String> {
    responses.iter().find_map(|r| {
        if let EngineResponse::Error(e) = r {
            Some(engine_error_to_message(&e.message))
        } else {
            None
        }
    })
}

async fn get_market_fees(
    Path(market_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let engine = state.engine.lock().await;
    let mid = MarketId(market_id.clone());
    match engine.markets.get(&mid) {
        Some(m) => {
            let maker_fee_pct = m.maker_fee_bps as f64 / 100.0;
            let taker_fee_pct = m.taker_fee_bps as f64 / 100.0;
            (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
                "market": market_id,
                "maker_fee_bps": m.maker_fee_bps,
                "taker_fee_bps": m.taker_fee_bps,
                "maker_fee_pct": maker_fee_pct,
                "taker_fee_pct": taker_fee_pct,
            })))).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(ApiResponse::<()>::err("market not found"))).into_response(),
    }
}

async fn list_fees(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let engine = state.engine.lock().await;
    let fees: Vec<serde_json::Value> = engine.markets.values().map(|m| {
        serde_json::json!({
            "market": m.id.0,
            "maker_fee_bps": m.maker_fee_bps,
            "taker_fee_bps": m.taker_fee_bps,
            "maker_fee_pct": m.maker_fee_bps as f64 / 100.0,
            "taker_fee_pct": m.taker_fee_bps as f64 / 100.0,
        })
    }).collect();
    Json(ApiResponse::ok(fees))
}

#[derive(serde::Deserialize)]
struct ReferralRegisterBody {
    user: String,
    #[serde(rename = "ref")]
    referrer: String,
    signature: String,
    nonce: u64,
}

fn default_user_metadata(user: &UserId) -> UserMetadata {
    UserMetadata {
        user: user.clone(),
        nonce_window: NonceWindow::new(),
        open_order_ids: [0u64; 64],
        credit_ratio: 1.0,
        total_quoted_notional: 0,
        actual_collateral: 0,
        ref_by: None,
        ref_earnings: 0,
        referred_users: vec![],
    }
}

async fn register_referral(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ReferralRegisterBody>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&body.user) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid user address"))).into_response(),
    };
    let ref_user = match UserId::from_hex(&body.referrer) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid ref address"))).into_response(),
    };
    if user == ref_user {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("cannot refer yourself"))).into_response();
    }
    let msg = format!("vela:referral:{}:{}:{}", body.user.to_lowercase(), body.referrer.to_lowercase(), body.nonce).into_bytes();
    if verify_matches_async(msg, body.signature.clone(), body.user.clone()).await.is_err() {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("invalid signature"))).into_response();
    }
    let mut us = state.shards.user_state.write().await;
    let ref_exists = us.metadata.contains_key(&ref_user)
        || us.balances.keys().any(|(u, _)| u == &ref_user);
    if !ref_exists {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("referrer not found"))).into_response();
    }
    {
        let existing = us.metadata.get(&user);
        if existing.map(|m| m.ref_by.is_some()).unwrap_or(false) {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("referrer already set"))).into_response();
        }
    }
    let mut user_meta = us.metadata.get(&user).cloned().unwrap_or_else(|| default_user_metadata(&user));
    user_meta.ref_by = Some(body.referrer.to_lowercase());
    us.metadata.insert(user.clone(), user_meta);
    let mut ref_meta = us.metadata.get(&ref_user).cloned().unwrap_or_else(|| default_user_metadata(&ref_user));
    let user_hex = body.user.to_lowercase();
    if !ref_meta.referred_users.contains(&user_hex) {
        ref_meta.referred_users.push(user_hex);
    }
    us.metadata.insert(ref_user, ref_meta);
    (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({"registered": true})))).into_response()
}

async fn get_referral_handler(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&address) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response(),
    };
    let us = state.shards.user_state.read().await;
    let meta = us.metadata.get(&user).cloned().unwrap_or_else(|| default_user_metadata(&user));
    let earnings_usdc = format!("{:.6}", meta.ref_earnings as f64 / 1_000_000.0);
    (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
        "address": address.to_lowercase(),
        "referrer": meta.ref_by,
        "referred_count": meta.referred_users.len(),
        "total_earnings_usdc": earnings_usdc,
        "referred_users": meta.referred_users,
    })))).into_response()
}

async fn status_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let uptime_secs = state.start_time.elapsed().as_secs();

    let active_markets = {
        let engine = state.engine.lock().await;
        engine.markets.len()
    };

    let (fill_ids, order_ids) = {
        let fills = state.fills.lock().await;
        let fids: Vec<String> = fills.iter().map(|f| f.id.clone()).collect();
        drop(fills);
        let orders = state.stored_orders.lock().await;
        let oids: Vec<String> = orders.keys().map(|k| k.to_string()).collect();
        (fids, oids)
    };

    let mut hasher = Keccak256::new();
    for id in &fill_ids { hasher.update(id.as_bytes()); }
    for id in &order_ids { hasher.update(id.as_bytes()); }
    let last_state_root = format!("0x{}", hex::encode(hasher.finalize()));

    let last_snapshot_ts = state.last_snapshot_ts.load(Ordering::Relaxed);

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let snapshot_stale = if last_snapshot_ts == 0 {
        uptime_secs > 300
    } else {
        now_ms.saturating_sub(last_snapshot_ts) > 300_000
    };

    let status = if uptime_secs < 30 {
        "starting"
    } else if snapshot_stale {
        "degraded"
    } else {
        "operational"
    };

    let orders_today = state.orders_today.load(Ordering::Relaxed);
    let fills_today = state.fills_today.load(Ordering::Relaxed);
    let volume_raw = state.volume_today_usdc.load(Ordering::Relaxed);
    let volume_str = format!("{:.2}", volume_raw as f64 / 1_000_000.0);
    let ws_clients = state.ws_client_count.load(Ordering::Relaxed);
    let restart_reason = state.last_restart_reason.lock().unwrap().clone();

    Json(ApiResponse::ok(serde_json::json!({
        "status": status,
        "engine_uptime_seconds": uptime_secs,
        "engine_version": state.engine_version,
        "last_snapshot_timestamp": last_snapshot_ts,
        "last_state_root": last_state_root,
        "orders_processed_today": orders_today,
        "fills_today": fills_today,
        "volume_today_usdc": volume_str,
        "active_markets": active_markets,
        "connected_ws_clients": ws_clients,
        "last_restart_reason": restart_reason,
    })))
}

async fn fees_public_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let taker_raw = state.total_taker_fees_collected.load(Ordering::Relaxed);
    let rebates_raw = state.total_maker_rebates_paid.load(Ordering::Relaxed);
    let net_raw = taker_raw.saturating_sub(rebates_raw);
    let today_raw = state.fees_collected_today.load(Ordering::Relaxed);

    let fmt = |v: u64| format!("{:.6}", v as f64 / 1_000_000.0);

    Json(ApiResponse::ok(serde_json::json!({
        "total_taker_fees_collected_usdc": fmt(taker_raw),
        "total_maker_rebates_paid_usdc": fmt(rebates_raw),
        "net_exchange_revenue_usdc": fmt(net_raw),
        "fees_collected_today_usdc": fmt(today_raw),
        "maker_fee_bps": -1,
        "taker_fee_bps": 5,
        "since": "2026-04-01T00:00:00Z",
    })))
}

async fn get_leaderboard(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let fills = state.fills.lock().await;
    let mut volume_map: std::collections::HashMap<String, (f64, u64, u64)> = std::collections::HashMap::new();
    for fill in fills.iter() {
        let notional = fill.price as f64 * fill.quantity as f64 / 1_000_000_000_000.0;
        let taker = fill.taker_address.to_lowercase();
        let maker = fill.maker_address.to_lowercase();
        let e = volume_map.entry(taker).or_insert((0.0, 0, 0));
        e.0 += notional;
        e.1 += 1;
        let e2 = volume_map.entry(maker).or_insert((0.0, 0, 0));
        e2.0 += notional;
        e2.2 += 1;
    }
    drop(fills);
    let mut traders: Vec<serde_json::Value> = volume_map.into_iter().map(|(addr, (vol, taker_count, maker_count))| {
        serde_json::json!({
            "address": addr,
            "volume_usdc": format!("{:.2}", vol),
            "fill_count": taker_count + maker_count,
            "maker_count": maker_count,
            "taker_count": taker_count,
        })
    }).collect();
    traders.sort_by(|a, b| {
        let va: f64 = a["volume_usdc"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
        let vb: f64 = b["volume_usdc"].as_str().unwrap_or("0").parse().unwrap_or(0.0);
        vb.partial_cmp(&va).unwrap_or(std::cmp::Ordering::Equal)
    });
    traders.truncate(20);
    let us = state.shards.user_state.read().await;
    let mut referrers: Vec<serde_json::Value> = us.metadata.iter()
        .filter(|(_, m)| !m.referred_users.is_empty() || m.ref_earnings > 0)
        .map(|(user, m)| serde_json::json!({
            "address": user.to_hex(),
            "referred_count": m.referred_users.len(),
            "earnings_usdc": format!("{:.6}", m.ref_earnings as f64 / 1_000_000.0),
        }))
        .collect();
    referrers.sort_by(|a, b| {
        let ra = a["referred_count"].as_u64().unwrap_or(0);
        let rb = b["referred_count"].as_u64().unwrap_or(0);
        rb.cmp(&ra)
    });
    referrers.truncate(10);
    (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
        "top_traders": traders,
        "top_referrers": referrers,
        "period": "all_time",
    })))).into_response()
}

// ---------------------------------------------------------------------------
// Transparency endpoints: incidents, decisions, market makers
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CreateIncidentBody {
    incident_type: String,
    description: String,
    impact: String,
    resolved_at: Option<u64>,
}

#[derive(serde::Deserialize)]
struct CreateDecisionBody {
    decision_type: String,
    title: String,
    description: String,
    rationale: String,
    effective_date: u64,
    operator_signature: String,
}

#[derive(serde::Deserialize)]
struct RegisterMMBody {
    address: String,
    display_name: Option<String>,
    signature: String,
    nonce: u64,
}

#[derive(serde::Serialize)]
struct MMEntry {
    address: String,
    display_name: Option<String>,
    registered_at: u64,
    is_internal: bool,
}

const INTERNAL_MM_REGISTERED_AT: u64 = 1775001600000;

async fn get_incidents(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let incidents = state.incidents.lock().await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let thirty_days_ms: u64 = 30 * 24 * 3600 * 1000;
    let threshold = now_ms.saturating_sub(thirty_days_ms);
    let all_clear = !incidents.iter().any(|i| i.started_at >= threshold);
    let total = incidents.len();
    let data = incidents.clone();
    Json(ApiResponse::ok(serde_json::json!({
        "incidents": data,
        "total": total,
        "all_clear": all_clear,
    })))
}

async fn create_incident(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateIncidentBody>,
) -> impl IntoResponse {
    let provided = headers.get("x-admin-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("unauthorized"))).into_response();
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut incidents = state.incidents.lock().await;
    let next_id = incidents.iter().map(|i| i.id).max().unwrap_or(0) + 1;
    let incident = crate::types::Incident {
        id: next_id,
        incident_type: body.incident_type,
        started_at: now_ms,
        resolved_at: body.resolved_at,
        description: body.description,
        impact: body.impact,
    };
    incidents.push(incident.clone());
    (StatusCode::OK, Json(ApiResponse::ok(incident))).into_response()
}

async fn get_decisions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let decisions = state.decisions.lock().await;
    let total = decisions.len();
    let pending_count = decisions.iter().filter(|d| d.status == "PENDING").count();
    let data = decisions.clone();
    Json(ApiResponse::ok(serde_json::json!({
        "decisions": data,
        "total": total,
        "pending_count": pending_count,
    })))
}

async fn create_decision(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateDecisionBody>,
) -> impl IntoResponse {
    let provided = headers.get("x-admin-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("unauthorized"))).into_response();
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut decisions = state.decisions.lock().await;
    let next_id = decisions.iter().map(|d| d.id).max().unwrap_or(0) + 1;
    let decision = crate::types::Decision {
        id: next_id,
        decision_type: body.decision_type,
        title: body.title,
        description: body.description,
        rationale: body.rationale,
        effective_date: body.effective_date,
        announced_at: now_ms,
        status: "PENDING".to_string(),
        operator_signature: body.operator_signature,
    };
    decisions.push(decision.clone());
    (StatusCode::OK, Json(ApiResponse::ok(decision))).into_response()
}

async fn get_market_makers(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let operator_address = std::env::var("OPERATOR_WALLET_ADDRESS")
        .unwrap_or_else(|_| "0x63c1C089e08EF6949f6Ee8dB1F3c2dC7f3e9B64EC0".to_string());

    let mut entries: Vec<MMEntry> = vec![MMEntry {
        address: operator_address,
        display_name: Some("Monolith Systematic LLC (Internal MM Bot)".to_string()),
        registered_at: INTERNAL_MM_REGISTERED_AT,
        is_internal: true,
    }];

    let registered = state.registered_mms.lock().await;
    for mm in registered.iter() {
        entries.push(MMEntry {
            address: mm.address.clone(),
            display_name: mm.display_name.clone(),
            registered_at: mm.registered_at,
            is_internal: false,
        });
    }

    Json(ApiResponse::ok(serde_json::json!({ "market_makers": entries })))
}

async fn register_market_maker(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterMMBody>,
) -> impl IntoResponse {
    if !body.address.starts_with("0x") || body.address.len() != 42 {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("invalid address"))).into_response();
    }

    if let Some(ref name) = body.display_name {
        if name.len() > 64 {
            return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("display_name exceeds 64 characters"))).into_response();
        }
    }

    let msg = format!("vela:mm-register:{}:{}", body.address.to_lowercase(), body.nonce).into_bytes();
    if verify_matches_async(msg, body.signature.clone(), body.address.clone()).await.is_err() {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("invalid signature"))).into_response();
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut registered = state.registered_mms.lock().await;
    let addr_lower = body.address.to_lowercase();
    if registered.iter().any(|mm| mm.address.to_lowercase() == addr_lower) {
        return (StatusCode::CONFLICT, Json(ApiResponse::<()>::err("address already registered"))).into_response();
    }

    let mm = crate::types::RegisteredMM {
        address: body.address,
        display_name: body.display_name,
        registered_at: now_ms,
        signature: body.signature,
    };
    registered.push(mm.clone());
    (StatusCode::OK, Json(ApiResponse::ok(mm))).into_response()
}

// ---------------------------------------------------------------------------
// VEL-T2-05: Market analytics endpoint
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct AnalyticsQuery {
    market_id: Option<String>,
    timeframe: Option<String>,
}

#[derive(serde::Deserialize)]
struct AnalyticsTimeframeQuery {
    timeframe: Option<String>,
}

#[derive(serde::Serialize)]
struct MarketAnalytics {
    market_id: String,
    current_spread_bps: Option<f64>,
    current_bid: Option<u64>,
    current_ask: Option<u64>,
    current_mid: Option<u64>,
    slippage_1k_usdc: Option<f64>,
    slippage_10k_usdc: Option<f64>,
    slippage_100k_usdc: Option<f64>,
    total_volume_usdc: String,
    fill_count: usize,
    avg_fill_size_usdc: String,
    largest_fill_usdc: String,
    depth_1pct_bid_usdc: String,
    depth_1pct_ask_usdc: String,
    depth_1pct_total_usdc: String,
}

#[derive(serde::Serialize)]
struct AnalyticsData {
    timeframe: String,
    markets: Vec<MarketAnalytics>,
    generated_at: u64,
}

fn fmt_usdc_2dp(v: f64) -> String {
    format!("{:.2}", v)
}

fn compute_slippage_bps(asks: &[(u64, u64)], mid: u64, budget_usdc: u64) -> Option<f64> {
    if mid == 0 || asks.is_empty() {
        return None;
    }
    let mut remaining: u128 = budget_usdc as u128 * 10_000_000_000_000_000u128;
    let mut total_qty: u128 = 0;
    let mut total_cost: u128 = 0;
    for &(price_fp, qty_fp) in asks {
        if remaining == 0 || price_fp == 0 {
            break;
        }
        let max_qty = remaining / price_fp as u128;
        let fill_qty = max_qty.min(qty_fp as u128);
        if fill_qty == 0 {
            continue;
        }
        let cost = fill_qty * price_fp as u128;
        total_qty += fill_qty;
        total_cost += cost;
        remaining = remaining.saturating_sub(cost);
    }
    if total_qty == 0 {
        return None;
    }
    let avg_price = total_cost as f64 / total_qty as f64;
    Some(((avg_price - mid as f64) / mid as f64 * 10_000.0).max(0.0))
}

fn compute_depth_usdc_1pct(levels: &[(u64, u64)], mid: u64, is_bid: bool) -> f64 {
    if mid == 0 {
        return 0.0;
    }
    let bound = if is_bid { mid * 99 / 100 } else { mid * 101 / 100 };
    let mut total_qty: u128 = 0;
    for &(price, qty) in levels {
        let in_range = if is_bid { price >= bound } else { price <= bound };
        if !in_range {
            break;
        }
        total_qty += qty as u128;
    }
    total_qty as f64 * mid as f64 / 1e16
}

async fn build_analytics_data(
    filter_market: Option<&str>,
    timeframe_str: &str,
    state: &Arc<AppState>,
) -> ApiResponse<AnalyticsData> {
    let window_us: u64 = match timeframe_str {
        "1H" => 3_600_000_000,
        "7D" => 604_800_000_000,
        _ => 86_400_000_000,
    };

    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let window_start = now_us.saturating_sub(window_us);

    // Collect market IDs from static engine config, then sample order books from shards.
    let mut market_ids: Vec<String> = {
        let engine = state.engine.lock().await;
        if let Some(mid) = filter_market {
            if engine.markets.contains_key(&MarketId(mid.to_string())) {
                vec![mid.to_string()]
            } else {
                vec![]
            }
        } else {
            let mut ids: Vec<String> = engine.markets.keys().map(|k| k.0.clone()).collect();
            ids.sort();
            ids
        }
    };
    market_ids.retain(|id| state.shards.shards.contains_key(&MarketId(id.clone())));

    // Pre-collect book snapshots per market (one shard lock at a time).
    struct BookSnapshot {
        best_bid: Option<u64>,
        best_ask: Option<u64>,
        asks_depth: Vec<(u64, u64)>,
        bids_depth: Vec<(u64, u64)>,
    }
    let mut book_snaps: std::collections::HashMap<String, BookSnapshot> = std::collections::HashMap::new();
    for market_id in &market_ids {
        if let Some(shard_arc) = state.shards.shards.get(&MarketId(market_id.clone())) {
            let shard = shard_arc.lock().await;
            if let Some(book) = shard.engine.order_books.get(&MarketId(market_id.clone())) {
                book_snaps.insert(market_id.clone(), BookSnapshot {
                    best_bid: book.best_bid(),
                    best_ask: book.best_ask(),
                    asks_depth: book.depth_asks(500),
                    bids_depth: book.depth_bids(500),
                });
            }
        }
    }

    let fills = state.fills.lock().await;

    let mut markets = Vec::new();

    for market_id in &market_ids {
        let snap = book_snaps.get(market_id.as_str());

        let (best_bid, best_ask) = snap.map(|s| (s.best_bid, s.best_ask)).unwrap_or((None, None));

        let current_mid = match (best_bid, best_ask) {
            (Some(bid), Some(ask)) => Some(bid + (ask - bid) / 2),
            _ => None,
        };

        let current_spread_bps = match (best_bid, best_ask, current_mid) {
            (Some(bid), Some(ask), Some(mid)) if mid > 0 => {
                Some((ask - bid) as f64 / mid as f64 * 10_000.0)
            }
            _ => None,
        };

        let (slippage_1k, slippage_10k, slippage_100k, depth_bid, depth_ask) =
            match (snap, current_mid) {
                (Some(s), Some(mid)) => {
                    (
                        compute_slippage_bps(&s.asks_depth, mid, 1_000),
                        compute_slippage_bps(&s.asks_depth, mid, 10_000),
                        compute_slippage_bps(&s.asks_depth, mid, 100_000),
                        compute_depth_usdc_1pct(&s.bids_depth, mid, true),
                        compute_depth_usdc_1pct(&s.asks_depth, mid, false),
                    )
                }
                _ => (None, None, None, 0.0, 0.0),
            };

        let market_fills: Vec<&StoredFill> = fills
            .iter()
            .filter(|f| f.market_id == *market_id && f.timestamp >= window_start)
            .collect();

        let fill_count = market_fills.len();
        let notionals: Vec<f64> = market_fills
            .iter()
            .map(|f| f.price as f64 * f.quantity as f64 / 1e16)
            .collect();
        let total_volume: f64 = notionals.iter().sum();
        let largest_fill: f64 = notionals.iter().cloned().fold(0.0f64, f64::max);
        let avg_fill = if fill_count > 0 { total_volume / fill_count as f64 } else { 0.0 };

        markets.push(MarketAnalytics {
            market_id: market_id.clone(),
            current_spread_bps,
            current_bid: best_bid,
            current_ask: best_ask,
            current_mid,
            slippage_1k_usdc: slippage_1k,
            slippage_10k_usdc: slippage_10k,
            slippage_100k_usdc: slippage_100k,
            total_volume_usdc: fmt_usdc_2dp(total_volume),
            fill_count,
            avg_fill_size_usdc: fmt_usdc_2dp(avg_fill),
            largest_fill_usdc: fmt_usdc_2dp(largest_fill),
            depth_1pct_bid_usdc: fmt_usdc_2dp(depth_bid),
            depth_1pct_ask_usdc: fmt_usdc_2dp(depth_ask),
            depth_1pct_total_usdc: fmt_usdc_2dp(depth_bid + depth_ask),
        });
    }

    let generated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    ApiResponse::ok(AnalyticsData {
        timeframe: timeframe_str.to_string(),
        markets,
        generated_at,
    })
}

async fn analytics_handler(
    Query(query): Query<AnalyticsQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let tf = query.timeframe.as_deref().unwrap_or("24H");
    Json(build_analytics_data(query.market_id.as_deref(), tf, &state).await)
}

async fn analytics_market_handler(
    Path(market_id): Path<String>,
    Query(query): Query<AnalyticsTimeframeQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let tf = query.timeframe.as_deref().unwrap_or("24H");
    Json(build_analytics_data(Some(&market_id), tf, &state).await)
}

async fn admin_fees_handler(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !state.verify_admin_token(provided) {
        return (StatusCode::UNAUTHORIZED, Json(ApiResponse::<()>::err("unauthorized"))).into_response();
    }

    let us = state.shards.user_state.read().await;
    let fee_balances: std::collections::HashMap<String, u64> = us.fee_balances.clone();
    let total_usdc = fee_balances.get("USDC").copied().unwrap_or(0);
    let total_fees_collected_usdc = format_amount(total_usdc, PRICE_DECIMALS);

    (StatusCode::OK, Json(ApiResponse::ok(serde_json::json!({
        "fee_balances": fee_balances,
        "total_fees_collected_usdc": total_fees_collected_usdc,
    })))).into_response()
}

#[derive(serde::Deserialize, Default)]
struct ProofsListQuery {
    limit: Option<usize>,
    cursor: Option<u64>,
}

async fn batch_proof_handler(
    Path(batch_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let proof = state.proofs.lock().await.get(&batch_id).cloned();
    match proof {
        Some(p) => {
            let status_str = match p.status {
                zkvm::ProofStatus::Proven => "proven",
                zkvm::ProofStatus::Pending => "pending",
                zkvm::ProofStatus::Skipped => "skipped",
                zkvm::ProofStatus::Failed => "failed",
            };
            let data = serde_json::json!({
                "batch_id": p.batch_id,
                "status": status_str,
                "prover": p.prover,
                "public_inputs": p.public_inputs,
                "proof_bytes": p.proof_bytes.as_ref().map(|b| format!("0x{}", hex::encode(b))),
                "generated_at": p.generated_at,
                "proving_time_ms": p.proving_time_ms,
                "proof_size_bytes": p.proof_size_bytes,
                "verification_note": "Optimistic mode: proof generated only if challenged. Full ZK proving coming post-Stanford AFT Lab.",
            });
            Json(ApiResponse::ok(data)).into_response()
        }
        None => {
            let data = serde_json::json!({
                "batch_id": batch_id,
                "status": "pending",
                "prover": "placeholder",
                "public_inputs": null,
                "verification_note": "Proof pending. Check back shortly.",
            });
            Json(ApiResponse::ok(data)).into_response()
        }
    }
}

async fn proof_stats_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let proofs = state.proofs.lock().await;
    let total_batches = proofs.len() as u64;
    let mut proven: u64 = 0;
    let mut skipped: u64 = 0;
    let mut pending: u64 = 0;
    let mut failed: u64 = 0;
    for p in proofs.values() {
        match p.status {
            zkvm::ProofStatus::Proven => proven += 1,
            zkvm::ProofStatus::Skipped => skipped += 1,
            zkvm::ProofStatus::Pending => pending += 1,
            zkvm::ProofStatus::Failed => failed += 1,
        }
    }
    drop(proofs);

    Json(ApiResponse::ok(serde_json::json!({
        "total_batches": total_batches,
        "proven": proven,
        "skipped": skipped,
        "pending": pending,
        "failed": failed,
        "prover_mode": "optimistic",
        "prover_version": "placeholder-0.1.0",
        "sp1_integration_status": "coming_soon",
        "note": "Full ZK proof generation ships post-Stanford AFT Lab (June 2026).",
    }))).into_response()
}

async fn proofs_list_handler(
    Query(params): Query<ProofsListQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(200);
    let cursor = params.cursor;

    let proofs_map = state.proofs.lock().await;
    let mut all: Vec<&zkvm::BatchProof> = proofs_map.values().collect();
    all.sort_by(|a, b| b.batch_id.cmp(&a.batch_id));

    let filtered: Vec<&zkvm::BatchProof> = all
        .into_iter()
        .filter(|p| cursor.is_none_or(|c| p.batch_id < c))
        .take(limit)
        .collect();

    let items: Vec<serde_json::Value> = filtered.iter().map(|p| {
        let status_str = match p.status {
            zkvm::ProofStatus::Proven => "proven",
            zkvm::ProofStatus::Pending => "pending",
            zkvm::ProofStatus::Skipped => "skipped",
            zkvm::ProofStatus::Failed => "failed",
        };
        serde_json::json!({
            "batch_id": p.batch_id,
            "status": status_str,
            "prover": p.prover,
            "generated_at": p.generated_at,
            "proving_time_ms": p.proving_time_ms,
            "proof_size_bytes": p.proof_size_bytes,
            "public_inputs": p.public_inputs,
        })
    }).collect();

    let next_cursor = filtered.last().map(|p| p.batch_id);
    drop(proofs_map);

    Json(ApiResponse::ok(serde_json::json!({
        "proofs": items,
        "next_cursor": next_cursor,
    }))).into_response()
}

async fn batch_attestation_handler(
    Path(batch_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let record = state.attestations.lock().await.get(&batch_id).cloned();
    match record {
        Some(r) => {
            let status_str = match r.status {
                tee::AttestationStatus::Attested => "attested",
                tee::AttestationStatus::Pending => "pending",
                tee::AttestationStatus::Simulated => "simulated",
                tee::AttestationStatus::Failed => "failed",
            };
            let platform_str = match r.platform {
                tee::TeePlatform::AmdSevSnp => "amd_sev_snp",
                tee::TeePlatform::IntelTdx => "intel_tdx",
                tee::TeePlatform::AwsNitro => "aws_nitro",
                tee::TeePlatform::Placeholder => "placeholder",
            };
            let data = serde_json::json!({
                "batch_id": r.batch_id,
                "status": status_str,
                "platform": platform_str,
                "binary_hash": format!("sha256:{}", r.binary_hash),
                "state_root": r.state_root,
                "fill_count": r.fill_count,
                "operator_address": r.operator_address,
                "generated_at": r.generated_at,
                "attestation_time_ms": r.attestation_time_ms,
                "attester_version": r.attester_version,
                "verification_note": r.verification_note,
                "attestation_report": r.attestation_report.as_ref().map(|b| format!("0x{}", hex::encode(b))),
                "vcek_cert": r.vcek_cert,
                "measurement": r.measurement,
                "etherscan_anchor_tx": r.etherscan_anchor_tx,
            });
            Json(ApiResponse::ok(data)).into_response()
        }
        None => {
            let data = serde_json::json!({
                "batch_id": batch_id,
                "status": "pending",
                "verification_note": "Attestation pending for this batch.",
            });
            Json(ApiResponse::ok(data)).into_response()
        }
    }
}

async fn tee_stats_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let attestations = state.attestations.lock().await;
    let total_batches = attestations.len() as u64;
    let mut attested: u64 = 0;
    let mut simulated: u64 = 0;
    let mut pending: u64 = 0;
    let mut failed: u64 = 0;
    for r in attestations.values() {
        match r.status {
            tee::AttestationStatus::Attested => attested += 1,
            tee::AttestationStatus::Simulated => simulated += 1,
            tee::AttestationStatus::Pending => pending += 1,
            tee::AttestationStatus::Failed => failed += 1,
        }
    }
    let binary_hash = format!("sha256:{}", state.attester.binary_hash());
    drop(attestations);

    Json(ApiResponse::ok(serde_json::json!({
        "platform": "placeholder",
        "platform_status": "development",
        "binary_hash": binary_hash,
        "total_batches": total_batches,
        "attested": attested,
        "simulated": simulated,
        "pending": pending,
        "failed": failed,
        "attestation_roadmap": {
            "current": "Placeholder — structural correctness only",
            "phase_2": "AMD SEV-SNP confidential VM (June 2026)",
            "phase_3": "NVIDIA H100 GPU attestation for ZK acceleration",
            "reference": "https://oasis.net/blog/verifiable-ai-with-tees",
        },
    }))).into_response()
}

#[derive(serde::Deserialize)]
struct AttestationsListQuery {
    limit: Option<usize>,
    cursor: Option<u64>,
}

async fn attestations_list_handler(
    Query(params): Query<AttestationsListQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(200);
    let cursor = params.cursor;

    let store = state.attestations.lock().await;
    let mut all: Vec<&tee::AttestationRecord> = store.values().collect();
    all.sort_by(|a, b| b.batch_id.cmp(&a.batch_id));

    let filtered: Vec<&tee::AttestationRecord> = all
        .into_iter()
        .filter(|r| cursor.is_none_or(|c| r.batch_id < c))
        .take(limit)
        .collect();

    let items: Vec<serde_json::Value> = filtered.iter().map(|r| {
        let status_str = match r.status {
            tee::AttestationStatus::Attested => "attested",
            tee::AttestationStatus::Simulated => "simulated",
            tee::AttestationStatus::Pending => "pending",
            tee::AttestationStatus::Failed => "failed",
        };
        let platform_str = match r.platform {
            tee::TeePlatform::AmdSevSnp => "amd_sev_snp",
            tee::TeePlatform::IntelTdx => "intel_tdx",
            tee::TeePlatform::AwsNitro => "aws_nitro",
            tee::TeePlatform::Placeholder => "placeholder",
        };
        serde_json::json!({
            "batch_id": r.batch_id,
            "status": status_str,
            "platform": platform_str,
            "binary_hash": format!("sha256:{}", r.binary_hash),
            "generated_at": r.generated_at,
            "fill_count": r.fill_count,
        })
    }).collect();

    let next_cursor = filtered.last().map(|r| r.batch_id);
    drop(store);

    Json(ApiResponse::ok(serde_json::json!({
        "attestations": items,
        "next_cursor": next_cursor,
    }))).into_response()
}

async fn wal_stats_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let stats = state.wal.stats().await;
    Json(ApiResponse::ok(serde_json::json!({
        "current_sequence": stats.current_sequence,
        "current_segment": stats.current_segment,
        "segment_size_bytes": stats.segment_size_bytes,
        "last_checkpoint_sequence": stats.last_checkpoint_sequence,
        "last_checkpoint_time": stats.last_checkpoint_time,
        "entries_since_checkpoint": stats.entries_since_checkpoint,
        "last_engine_start_reason": stats.last_engine_start_reason,
        "wal_enabled": true,
    })))
}

#[cfg(test)]
mod tee_tests {
    use super::*;
    use axum_test::TestServer;

    fn make_test_app() -> TestServer {
        use engine::MatchingEngine;
        use types::FeeConfig;
        let engine = MatchingEngine::new(FeeConfig::default(), 5.0);
        let wal_dir = std::env::temp_dir().join(format!(
            "vela_tee_test_wal_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let wal = std::sync::Arc::new(crate::wal::Wal::new(&wal_dir).unwrap());
        let state = crate::AppState::new(engine, wal);
        let router = build_router(state);
        TestServer::new(router).unwrap()
    }

    #[tokio::test]
    async fn test_batch_attestation_pending_when_no_batch() {
        let server = make_test_app();
        let res = server.get("/batches/9999/attestation").await;
        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["status"], "pending");
        assert_eq!(body["data"]["batch_id"], 9999);
    }

    #[tokio::test]
    async fn test_tee_stats_structure() {
        let server = make_test_app();
        let res = server.get("/tee/stats").await;
        res.assert_status_ok();
        let body: serde_json::Value = res.json();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"]["platform"], "placeholder");
        assert!(body["data"]["binary_hash"].as_str().unwrap().starts_with("sha256:"));
        assert_eq!(body["data"]["total_batches"], 0);
    }
}
