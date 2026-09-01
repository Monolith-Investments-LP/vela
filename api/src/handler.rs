use crate::{
    auth::{
        cancel_signing_message, eth_message_hash, order_signing_message, verify_matches_async,
        withdrawal_signing_message,
    },
    types::{
        format_amount, ApiResponse, BalanceResponse, BatchDetail, BatchSummary, BookLevel,
        BookResponse, CancelOrderBody, DepositBody, MarketResponse, OrderFillRecord, PostOrderBody,
        StateRootData, StoredFill, StoredOrder, WithdrawBody, WsEnvelope,
    },
    wal,
    wal::{
        WalDeposit, WalFillCreated, WalOrderCancel, WalOrderPost, WalOrderProcessed,
        WalWithdrawalRequest,
    },
    ws::handle_ws,
    AppState,
};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use engine;
use futures_util::{SinkExt, StreamExt};
use k256::ecdsa::SigningKey;
use sha3::{Digest, Keccak256};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use types::{
    AssetId, CancelOrderRequest, DepositRequest, Fill, MarketId, NonceWindow, OrderSide,
    OrderStatus, OrderType, PostOrderRequest, Request as EngineRequest, Response as EngineResponse,
    UserId, UserMetadata, WithdrawalRequest, PRICE_DECIMALS, QUANTITY_DECIMALS,
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
    integer_val
        .checked_mul(1_000_000_000_000_000_000u128)?
        .checked_add(frac_val)
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
    let key_hex = operator_key_hex
        .strip_prefix("0x")
        .unwrap_or(&operator_key_hex)
        .to_string();
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

/// Maximum time we wait for the sharded matching engine to return a
/// dispatched request's response before giving up on the client.
/// Overridable via `VELA_DISPATCH_TIMEOUT_MS` at boot.
fn dispatch_timeout() -> std::time::Duration {
    let ms: u64 = std::env::var("VELA_DISPATCH_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    std::time::Duration::from_millis(ms)
}

/// IEX-style speed bump: an artificial delay applied only to marketable
/// (crossing) orders. Resting orders and cancels are not delayed. The
/// intent is to let maker quote-cancels win the race against an
/// incoming taker, which reduces stale-quote sniping without hurting
/// resting-order latency.
///
/// Default 0 (disabled). Overridable via `VELA_SPEED_BUMP_US` at boot.
/// Tokio's async `sleep` is scheduler-dependent and inaccurate below
/// ~1 ms in practice; treat this as a soft delay, not a hardware timer.
fn speed_bump_duration() -> std::time::Duration {
    let us: u64 = std::env::var("VELA_SPEED_BUMP_US")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    std::time::Duration::from_micros(us)
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
    let addr_str = std::env::var("VELA_SETTLEMENT_ADDRESS").map_err(|_| {
        "VELA_SETTLEMENT_ADDRESS env var must be set to the deployed contract address".to_string()
    })?;
    let addr = parse_hex_address(&addr_str)?;
    Ok((chain_id, addr))
}

pub fn build_router(state: Arc<AppState>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "https://vela.monolithsystematic.com"
                .parse::<HeaderValue>()
                .unwrap(),
            "https://vela-vert.vercel.app"
                .parse::<HeaderValue>()
                .unwrap(),
            "http://localhost:3000".parse::<HeaderValue>().unwrap(),
        ]))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(tower_http::cors::Any);

    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler))
        .route("/openapi.json", get(crate::openapi::openapi_handler))
        .route("/docs", get(crate::openapi::docs_handler))
        .route("/mcp", post(crate::mcp::handle_rpc))
        .route(
            "/agent-stream/schema.json",
            get(crate::agent_schema::schema_handler),
        )
        .route("/agents/:master/tier", get(agent_tier_handler))
        .route("/admin/agent-tier/clear", post(admin_clear_agent_tier))
        .route("/status", get(status_handler))
        .route("/fees/public", get(fees_public_handler))
        .route("/fees/schedule", get(fees_schedule_handler))
        .route("/fees/tier/:address", get(fees_tier_handler))
        .route("/markets", get(list_markets))
        .route("/markets/:market/book", get(get_book))
        .route("/account/:address/balances", get(get_balances))
        .route("/account/:address/orders", get(get_open_orders))
        .route(
            "/account/:address/orders/by-client-id/:client_id",
            get(get_order_by_client_id),
        )
        .route("/orders", post(post_order))
        .route(
            "/orders/from-intent",
            post(crate::verifiable_intent::from_intent_handler),
        )
        .route("/orders/cancel", post(cancel_order))
        .route("/orders/:order_id", get(get_order_by_id))
        .route("/orders/:order_id/da-proof", get(get_da_proof))
        .route("/trades", get(list_trades))
        .route("/trades/:market_id", get(list_trades_by_market))
        .route("/withdrawals", post(initiate_withdrawal))
        .route("/withdrawal-signature", post(withdrawal_signature_handler))
        .route("/deposit", post(deposit_handler))
        .route("/deposit/bridge", post(bridge_deposit_handler))
        .route("/deposit/bridges", get(bridge_registry_handler))
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
        .route("/points/:address", get(get_points_handler))
        .route("/portfolio/:address", get(get_portfolio_handler))
        .route("/portfolio/:address/csv", get(get_portfolio_csv_handler))
        .route(
            "/admin/export/trades/yesterday",
            post(admin_export_trades_yesterday),
        )
        .route("/admin/export/l2/now", post(admin_export_l2_now))
        .route("/agents/register", post(agents_register))
        .route("/agents/revoke", post(agents_revoke))
        .route("/agents/:master", get(agents_list))
        .route(
            "/agents/reasoning/attest",
            post(crate::reasoning_attest::attest_handler),
        )
        .route(
            "/reputation/attest/:address",
            post(crate::reputation::attest_handler),
        )
        .route("/reputation/:address", get(crate::reputation::get_handler))
        .route("/credit/open", post(crate::credit::open_handler))
        .route("/credit/close", post(crate::credit::close_handler))
        .route("/credit/:address", get(crate::credit::get_handler))
        .route(
            "/strategies/publish",
            post(crate::strategies::publish_handler),
        )
        .route("/strategies", get(crate::strategies::list_handler))
        .route(
            "/strategies/:strategy_id",
            get(crate::strategies::get_handler),
        )
        .route(
            "/strategies/:strategy_id/subscribe",
            post(crate::strategies::subscribe_handler),
        )
        .route(
            "/strategies/:strategy_id/unsub",
            post(crate::strategies::unsubscribe_handler),
        )
        .route(
            "/strategies/:strategy_id/subscriptions",
            get(crate::strategies::list_subscriptions_handler),
        )
        .route(
            "/strategies/backtest/attest",
            post(crate::backtest_attest::attest_handler),
        )
        .route(
            "/borrow-lend/markets",
            get(crate::borrow_lend::markets_handler),
        )
        .route(
            "/borrow-lend/account/:address",
            get(crate::borrow_lend::account_handler),
        )
        .route(
            "/borrow-lend/supply",
            post(crate::borrow_lend::supply_handler),
        )
        .route(
            "/borrow-lend/withdraw",
            post(crate::borrow_lend::withdraw_handler),
        )
        .route(
            "/borrow-lend/borrow",
            post(crate::borrow_lend::borrow_handler),
        )
        .route(
            "/borrow-lend/repay",
            post(crate::borrow_lend::repay_handler),
        )
        .route(
            "/borrow-lend/liquidate",
            post(crate::borrow_lend::liquidate_handler),
        )
        .route("/orders/algo/twap", post(post_twap_algo))
        .route("/orders/algo/cancel", post(cancel_algo))
        .route("/orders/algo/:parent_id", get(get_algo_status))
        .route("/listings/propose", post(propose_listing))
        .route("/listings", get(list_listings))
        .route("/listings/:listing_id", get(get_listing))
        .route("/admin/listings/reject", post(admin_reject_listing))
        .route("/vaults/create", post(create_vault))
        .route("/vaults", get(list_vaults))
        .route("/vaults/:vault_id", get(get_vault))
        .route("/vaults/:vault_id/deposit", post(vault_deposit))
        .route("/vaults/:vault_id/withdraw", post(vault_withdraw))
        .route("/vaults/:vault_id/positions/:lp", get(get_lp_position))
        .route("/subaccounts/create", post(create_subaccount))
        .route("/subaccounts/transfer", post(transfer_subaccount))
        .route("/subaccounts/:master", get(list_subaccounts))
        .route("/rfq/request", post(rfq_request))
        .route("/rfq/quote", post(rfq_quote))
        .route("/rfq/accept", post(rfq_accept))
        .route("/rfq/requests", get(list_rfq_requests))
        .route("/rfq/quotes/:rfq_id", get(list_rfq_quotes))
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
        .route(
            "/batches/:batch_id/attestation",
            get(batch_attestation_handler),
        )
        .route("/proofs/stats", get(proof_stats_handler))
        .route("/proofs", get(proofs_list_handler))
        .route("/tee/stats", get(tee_stats_handler))
        .route("/attestations", get(attestations_list_handler))
        .route("/wal/stats", get(wal_stats_handler))
        .route(
            "/order/encrypted",
            post(crate::committee_handler::post_encrypted_order),
        )
        .route(
            "/committee/share",
            post(crate::committee_handler::post_committee_share),
        )
        .with_state(state)
        .layer(cors)
}

async fn health() -> &'static str {
    "ok"
}

/// Prometheus text-format exposition of engine + api counters.
///
/// Deliberately hand-rolled: the alternative is pulling in `prometheus`
/// (heavy) or `metrics` + `metrics-exporter-prometheus` (two crates for
/// a page of metrics). This function costs ~15 lines and has no runtime
/// state.
async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    use std::fmt::Write;
    use std::sync::atomic::Ordering;

    let batch_size_hist = state.batch_metrics.batch_size_histogram_snapshot();
    let (batch_count, batch_size_sum) = {
        let n = batch_size_hist.len() as u64;
        let s: u64 = batch_size_hist.iter().sum();
        (n, s)
    };

    let mut out = String::with_capacity(1024);

    let _ = writeln!(
        out,
        "# HELP vela_orders_today_total Orders accepted since UTC midnight."
    );
    let _ = writeln!(out, "# TYPE vela_orders_today_total counter");
    let _ = writeln!(
        out,
        "vela_orders_today_total {}",
        state.orders_today.load(Ordering::Relaxed)
    );

    let _ = writeln!(
        out,
        "# HELP vela_fills_today_total Fills produced since UTC midnight."
    );
    let _ = writeln!(out, "# TYPE vela_fills_today_total counter");
    let _ = writeln!(
        out,
        "vela_fills_today_total {}",
        state.fills_today.load(Ordering::Relaxed)
    );

    let _ = writeln!(
        out,
        "# HELP vela_volume_today_usdc_micro Volume since UTC midnight (USDC, ×1e6)."
    );
    let _ = writeln!(out, "# TYPE vela_volume_today_usdc_micro counter");
    let _ = writeln!(
        out,
        "vela_volume_today_usdc_micro {}",
        state.volume_today_usdc.load(Ordering::Relaxed)
    );

    let _ = writeln!(
        out,
        "# HELP vela_ws_clients Current WebSocket client count."
    );
    let _ = writeln!(out, "# TYPE vela_ws_clients gauge");
    let _ = writeln!(
        out,
        "vela_ws_clients {}",
        state.ws_client_count.load(Ordering::Relaxed)
    );

    let _ = writeln!(
        out,
        "# HELP vela_last_snapshot_timestamp_ms Wall-clock ms since UNIX epoch of last snapshot."
    );
    let _ = writeln!(out, "# TYPE vela_last_snapshot_timestamp_ms gauge");
    let _ = writeln!(
        out,
        "vela_last_snapshot_timestamp_ms {}",
        state.last_snapshot_ts.load(Ordering::Relaxed)
    );

    let _ = writeln!(
        out,
        "# HELP vela_batch_dispatch_latency_ns Latency of most recent batch dispatch (ns)."
    );
    let _ = writeln!(out, "# TYPE vela_batch_dispatch_latency_ns gauge");
    let _ = writeln!(
        out,
        "vela_batch_dispatch_latency_ns {}",
        state.batch_metrics.batch_dispatch_latency_ns()
    );

    let _ = writeln!(
        out,
        "# HELP vela_orders_per_second Rolling-1s orders-per-second estimate."
    );
    let _ = writeln!(out, "# TYPE vela_orders_per_second gauge");
    let _ = writeln!(
        out,
        "vela_orders_per_second {}",
        state.batch_metrics.orders_per_second()
    );

    let _ = writeln!(
        out,
        "# HELP vela_batch_count_total Batches dispatched (since last histogram drain)."
    );
    let _ = writeln!(out, "# TYPE vela_batch_count_total counter");
    let _ = writeln!(out, "vela_batch_count_total {}", batch_count);

    let _ = writeln!(out, "# HELP vela_batch_size_sum_total Sum of orders across recorded batches (since last histogram drain).");
    let _ = writeln!(out, "# TYPE vela_batch_size_sum_total counter");
    let _ = writeln!(out, "vela_batch_size_sum_total {}", batch_size_sum);

    let _ = writeln!(out, "# HELP vela_order_channel_send_failures_total Order-channel sends that failed (dispatcher gone).");
    let _ = writeln!(out, "# TYPE vela_order_channel_send_failures_total counter");
    let _ = writeln!(
        out,
        "vela_order_channel_send_failures_total {}",
        crate::ORDER_CHANNEL_SEND_FAILURES.load(Ordering::Relaxed)
    );

    let _ = writeln!(
        out,
        "# HELP vela_feed_no_subscriber_drops_total Broadcast publishes with zero live receivers."
    );
    let _ = writeln!(out, "# TYPE vela_feed_no_subscriber_drops_total counter");
    let _ = writeln!(
        out,
        "vela_feed_no_subscriber_drops_total {}",
        crate::feeds::FEED_NO_SUBSCRIBER_DROPS.load(Ordering::Relaxed)
    );

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        out,
    )
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn list_markets(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let engine = state.engine.lock().await;
    let market_ids: Vec<_> = engine
        .markets
        .values()
        .map(|m| (m.id.clone(), m.base, m.quote))
        .collect();
    drop(engine);
    let mut markets: Vec<MarketResponse> = Vec::with_capacity(market_ids.len());
    for (market_id, base, quote) in market_ids {
        let (best_bid, best_ask, spread) =
            if let Some(shard_arc) = state.shards.shards.get(&market_id) {
                let shard = shard_arc.lock().await;
                let book = shard.engine.order_books.get(&market_id);
                (
                    book.and_then(|b| b.best_bid())
                        .map(|p| format_amount(p, PRICE_DECIMALS)),
                    book.and_then(|b| b.best_ask())
                        .map(|p| format_amount(p, PRICE_DECIMALS)),
                    book.and_then(|b| b.spread())
                        .map(|s| format_amount(s, PRICE_DECIMALS)),
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
                    let bids = book
                        .depth_bids(50)
                        .iter()
                        .map(|(p, q)| BookLevel {
                            price: format_amount(*p, PRICE_DECIMALS),
                            quantity: format_amount(*q, QUANTITY_DECIMALS),
                        })
                        .collect();
                    let asks = book
                        .depth_asks(50)
                        .iter()
                        .map(|(p, q)| BookLevel {
                            price: format_amount(*p, PRICE_DECIMALS),
                            quantity: format_amount(*q, QUANTITY_DECIMALS),
                        })
                        .collect();
                    (
                        StatusCode::OK,
                        Json(ApiResponse::ok(BookResponse { market, bids, asks })),
                    )
                        .into_response()
                }
                None => (
                    StatusCode::NOT_FOUND,
                    Json(ApiResponse::<()>::err("market not found")),
                )
                    .into_response(),
            }
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("market not found")),
        )
            .into_response(),
    }
}

async fn get_balances(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&address) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };
    let us = state.shards.user_state.read().await;
    let balances: Vec<BalanceResponse> = us
        .balances
        .iter()
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
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };
    let open_order_ids = {
        let us = state.shards.user_state.read().await;
        us.metadata
            .get(&user)
            .map(|m| m.iter_order_ids().collect::<Vec<_>>())
            .unwrap_or_default()
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
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };

    let mut found_order = None;
    'outer: for shard_arc in state.shards.shards.values() {
        let shard = shard_arc.lock().await;
        for book in shard.engine.order_books.values() {
            if let Some(oid) = book.find_by_client_order_id(&user, &client_id) {
                found_order = book.get_order(oid).map(|o| {
                    serde_json::json!({
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
                    })
                });
                break 'outer;
            }
        }
    }
    match found_order {
        Some(o) => (StatusCode::OK, Json(ApiResponse::ok(o))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("order not found")),
        )
            .into_response(),
    }
}

async fn post_order(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PostOrderBody>,
) -> impl IntoResponse {
    if !state.order_limiter.check(&body.address) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::err(
                "Rate limit exceeded. Please slow down.",
            )),
        )
            .into_response();
    }

    if !body.address.starts_with("0x") || body.address.len() != 42 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("Invalid wallet address format")),
        )
            .into_response();
    }

    let user = match UserId::from_hex(&body.address) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("Invalid wallet address format")),
            )
                .into_response()
        }
    };

    if body.price == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("Price must be greater than 0")),
        )
            .into_response();
    }
    if body.quantity == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("Quantity must be greater than 0")),
        )
            .into_response();
    }
    if body.price >= u64::MAX / 2 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "Price exceeds maximum allowed value",
            )),
        )
            .into_response();
    }
    if body.quantity >= u64::MAX / 2 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "Quantity exceeds maximum allowed value",
            )),
        )
            .into_response();
    }

    let side_str = format!("{:?}", body.side).to_lowercase();
    let msg = order_signing_message(
        &body.market,
        &side_str,
        body.price,
        body.quantity,
        body.nonce,
        body.client_order_id.as_deref(),
    );
    // Notional check for the agent cap: use bid notional for bids
    // (price × qty) and quantity for asks (base being sold). This mirrors
    // what the engine's credit system computes as the trade's notional.
    let order_notional_micro = match body.side {
        types::OrderSide::Bid => body
            .price
            .checked_mul(body.quantity)
            .map(|n| n / 1_000_000)
            .unwrap_or(u64::MAX),
        types::OrderSide::Ask => body.quantity,
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    if crate::agents::verify_master_or_agent_scoped_async(
        msg,
        body.signature.clone(),
        body.address.clone(),
        order_notional_micro,
        now_ms,
        Arc::clone(&state.agents),
        MarketId(body.market.clone()),
        body.side,
        body.order_type,
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "invalid signature or capability scope violated",
            )),
        )
            .into_response();
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
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    // Combine market-existence check with the would-cross probe for the
    // speed bump so we only take the engine lock once here.
    let is_marketable = {
        let engine = state.engine.lock().await;
        if !engine.markets.contains_key(&req.market) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("Market not found.")),
            )
                .into_response();
        }
        // Post-Only orders are rejected on cross by the matcher, so
        // don't waste a delay bumping them just to reject after.
        if req.order_type == types::OrderType::PostOnly {
            false
        } else {
            engine
                .order_books
                .get(&req.market)
                .map(|b| b.would_match(req.side, req.price))
                .unwrap_or(false)
        }
    };

    // Agent-flow toxicity tier gate.
    // Red tier blocks new orders until the operator manually clears.
    // Amber adds an extra deterministic bump on top of the IEX-style
    // speed bump. Green flows unrestricted.
    let addr_lower = body.address.to_ascii_lowercase();
    if crate::agent_tox::should_block(&state, &addr_lower).await {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::<()>::err(
                "toxicity tier: RED — orders blocked pending operator review. Contact support.",
            )),
        )
            .into_response();
    }
    let tier_extra_bump = crate::agent_tox::extra_bump_for(&state, &addr_lower).await;

    // IEX-style speed bump: delay only crossing orders, not resting ones.
    // Amber-tier addresses eat the base bump plus an extra amber delay.
    let bump = speed_bump_duration() + tier_extra_bump;
    if is_marketable && !bump.is_zero() {
        tokio::time::sleep(bump).await;
    }

    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: EngineRequest::PostOrder(req),
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        crate::ORDER_CHANNEL_SEND_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err("engine unavailable")),
        )
            .into_response();
    }
    let responses = match tokio::time::timeout(dispatch_timeout(), resp_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("engine error")),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ApiResponse::<()>::err("engine dispatch timed out")),
            )
                .into_response()
        }
    };

    state
        .feeds
        .lock()
        .await
        .dispatch_response_batch(&user, &responses);

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
        .filter_map(|r| {
            if let EngineResponse::OrderFilled(f) = r {
                Some(f.clone())
            } else {
                None
            }
        })
        .map(|f| {
            let id = format!("fill_{}_{}", f.maker_order_id, f.taker_order_id);
            (id, f)
        })
        .collect();

    let posted = responses.iter().find_map(|r| {
        if let EngineResponse::OrderPosted(p) = r {
            Some(p.clone())
        } else {
            None
        }
    });

    let Some(posted) = posted else { return };

    state.orders_today.fetch_add(1, Ordering::Relaxed);

    let total_filled: u64 = fill_pairs.iter().map(|(_, f)| f.quantity).sum();

    let self_fills: Vec<OrderFillRecord> = fill_pairs
        .iter()
        .map(|(fill_id, f)| {
            let (counterparty_order_id, counterparty_address) =
                if f.taker_order_id == posted.order_id {
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
        reasoning_trace_hash: body.reasoning_trace_hash.clone(),
        agent_id: body.agent_id.clone(),
    };

    // Compliance-audit trail: emit a structured tracing event for
    // agent-tagged orders so operator log aggregators can index and
    // preserve the linkage independently of Vela's own storage.
    if let Some(hash) = &body.reasoning_trace_hash {
        tracing::info!(
            target: "reasoning_trace",
            order_id = posted.order_id,
            user = %body.address.to_lowercase(),
            agent_id = ?body.agent_id,
            reasoning_trace_hash = %hash,
            market = %body.market,
            "agent-tagged order recorded"
        );
    }

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
                toxicity_score: f.toxicity_score,
            });
            // Cap at 100k fills per market, evicting oldest first.
            let market_count = fills_guard
                .iter()
                .filter(|fill| fill.market_id == body.market)
                .count();
            if market_count > 100_000 {
                if let Some(idx) = fills_guard
                    .iter()
                    .position(|fill| fill.market_id == body.market)
                {
                    fills_guard.remove(idx);
                }
            }
            let notional_micro = (f.price as u128 * f.quantity as u128 / 10_000_000_000u128) as u64;
            let taker_fee = notional_micro * 5 / 10000;
            let maker_rebate = notional_micro / 10000;
            state.fills_today.fetch_add(1, Ordering::Relaxed);
            state
                .volume_today_usdc
                .fetch_add(notional_micro, Ordering::Relaxed);
            state
                .fees_collected_today
                .fetch_add(taker_fee, Ordering::Relaxed);
            state
                .total_taker_fees_collected
                .fetch_add(taker_fee, Ordering::Relaxed);
            state
                .total_maker_rebates_paid
                .fetch_add(maker_rebate, Ordering::Relaxed);
        }

        // Broadcast updated current candle for each timeframe after all fills are stored.
        if !fill_pairs.is_empty() {
            let ws_now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let last_ts = fill_pairs
                .iter()
                .map(|(_, f)| f.timestamp)
                .max()
                .unwrap_or(0);
            let ts_s = last_ts / 1_000_000;

            const OHLCV_TIMEFRAMES: &[(&str, u64)] = &[
                ("1m", 60),
                ("5m", 300),
                ("15m", 900),
                ("1H", 3600),
                ("4H", 14400),
                ("1D", 86400),
            ];
            for &(tf_name, interval_secs) in OHLCV_TIMEFRAMES {
                let bucket = (ts_s / interval_secs) * interval_secs;
                let bucket_start_us = bucket * 1_000_000;
                let bucket_end_us = (bucket + interval_secs) * 1_000_000;

                let bucket_fills: Vec<_> = fills_guard
                    .iter()
                    .filter(|fill| {
                        fill.market_id == body.market
                            && fill.timestamp >= bucket_start_us
                            && fill.timestamp < bucket_end_us
                    })
                    .collect();

                if bucket_fills.is_empty() {
                    continue;
                }

                let open = bucket_fills[0].price as f64 / 1_000_000.0;
                let close = bucket_fills[bucket_fills.len() - 1].price as f64 / 1_000_000.0;
                let high = bucket_fills.iter().map(|f| f.price).max().unwrap() as f64 / 1_000_000.0;
                let low = bucket_fills.iter().map(|f| f.price).min().unwrap() as f64 / 1_000_000.0;
                let volume =
                    bucket_fills.iter().map(|f| f.quantity as f64).sum::<f64>() / 1_000_000.0;

                let channel = format!("ohlcv:{}:{}", body.market, tf_name);
                let seq = {
                    let entry = state
                        .ws_seqs
                        .entry(channel.clone())
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
            let entry = state
                .ws_seqs
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
    } else if matches!(
        posted.status,
        OrderStatus::Open | OrderStatus::PartiallyFilled
    ) {
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
        if let Ok(Ok((hash_hex, _url))) =
            tokio::task::spawn_blocking(move || da.submit_order(seq, &da_bytes)).await
        {
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
            (
                StatusCode::OK,
                Json(ApiResponse::ok(serde_json::json!({
                    "order_id": order_id,
                    "da_hash": da_hash,
                    "backend": "local",
                }))),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("order not found")),
        )
            .into_response(),
    }
}

async fn get_order_by_id(
    Path(order_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let orders = state.stored_orders.lock().await;
    match orders.get(&order_id) {
        Some(order) => (StatusCode::OK, Json(ApiResponse::ok(order.clone()))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("order not found")),
        )
            .into_response(),
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
    }

    let us = state.shards.user_state.read().await;

    let mut engine_balances: std::collections::HashMap<String, u64> =
        std::collections::HashMap::new();
    for ((_, asset), bal) in &us.balances {
        *engine_balances
            .entry(asset.as_str().to_string())
            .or_insert(0) += bal.total();
    }

    let total_users = us.metadata.len();
    let snapshot_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "engine_balances": engine_balances,
            "total_users": total_users,
            "snapshot_time": snapshot_time,
        }))),
    )
        .into_response()
}

async fn cancel_order(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CancelOrderBody>,
) -> impl IntoResponse {
    if !state.order_limiter.check(&body.address) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::err(
                "Rate limit exceeded. Please slow down.",
            )),
        )
            .into_response();
    }

    let user = match UserId::from_hex(&body.address) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };

    let msg = cancel_signing_message(body.order_id, body.client_order_id.as_deref(), body.nonce);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    // Cancels bypass the agent notional cap (pass 0).
    if crate::agents::verify_master_or_agent_async(
        msg,
        body.signature.clone(),
        body.address.clone(),
        0,
        now_ms,
        Arc::clone(&state.agents),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("invalid signature")),
        )
            .into_response();
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
        crate::ORDER_CHANNEL_SEND_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err("engine unavailable")),
        )
            .into_response();
    }
    let responses = match tokio::time::timeout(dispatch_timeout(), resp_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("engine error")),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ApiResponse::<()>::err("engine dispatch timed out")),
            )
                .into_response()
        }
    };

    state
        .feeds
        .lock()
        .await
        .dispatch_response_batch(&user, &responses);

    if let Some(msg) = first_engine_error(&responses) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err(msg))).into_response();
    }

    let canceled_id = responses
        .iter()
        .find_map(|r| {
            if let EngineResponse::OrderCanceled(c) = r {
                Some(c.order_id)
            } else {
                None
            }
        })
        .or(cancel_order_id_hint);

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
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };

    let msg = withdrawal_signing_message(&body.asset, body.amount, body.nonce);
    if verify_matches_async(msg, body.signature.clone(), body.address.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("invalid signature")),
        )
            .into_response();
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
        crate::ORDER_CHANNEL_SEND_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err("engine unavailable")),
        )
            .into_response();
    }
    let responses = match tokio::time::timeout(dispatch_timeout(), resp_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("engine error")),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ApiResponse::<()>::err("engine dispatch timed out")),
            )
                .into_response()
        }
    };

    state
        .feeds
        .lock()
        .await
        .dispatch_response_batch(&user, &responses);

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

// ---------------------------------------------------------------------------
// Cross-chain bridge deposits.
//
// Vela's direct deposit path (`/deposit`) covers Ethereum L1 (Sepolia in
// beta, mainnet in prod). For other chains, users bridge in through an
// approved routing partner (LiFi, Across, Relay). The bridge partner
// verifies the source-chain deposit off-chain, exchanges the underlying
// asset for USDC, transfers into the Vela settlement contract, and posts
// a signed receipt to Vela via `/deposit/bridge`. Vela credits the user's
// exchange balance after verifying the receipt against the on-chain
// bridge allowlist.
//
// Trust model: each authorized bridge is a whitelisted secp256k1 public
// key configured at boot via `VELA_BRIDGE_ALLOWLIST` (JSON array of
// `{"bridge_id": ..., "pubkey_hex": ...}`). Compromising a bridge's key
// lets that bridge mint credits without a real deposit, so bridges should
// operate their own multisig/HSM. Bridges also carry brand-level risk
// disclosure per route.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct BridgeDepositBody {
    /// Vela user address the credit lands on.
    user: String,
    /// Asset that ultimately lands (usually "USDC" after bridge swap).
    asset: String,
    /// Amount in the asset's smallest unit (e.g., USDC micro-USD).
    amount: String,
    /// Which bridge submitted this receipt.
    bridge_id: String,
    /// Where the deposit originated (see `types::SourceChain`).
    source_chain: String,
    /// The bridge's transaction hash / receipt id on the source chain.
    /// Doubles as the replay nonce; used as `DepositRequest.l1_tx_hash`.
    source_tx_hash: String,
    /// Signature by the bridge over the canonical deposit message.
    /// Message layout:
    ///   `vela:bridge-deposit:{bridge_id}:{user}:{asset}:{amount}:{source_chain}:{source_tx_hash}`
    signature: String,
}

fn bridge_deposit_signing_message(
    bridge_id: &str,
    user: &str,
    asset: &str,
    amount_str: &str,
    source_chain: &str,
    source_tx_hash: &str,
) -> Vec<u8> {
    format!(
        "vela:bridge-deposit:{}:{}:{}:{}:{}:{}",
        bridge_id, user, asset, amount_str, source_chain, source_tx_hash
    )
    .into_bytes()
}

/// Parse `VELA_BRIDGE_ALLOWLIST` at request time. Format is a JSON array
/// of `{"bridge_id": "lifi", "pubkey_hex": "0x04..."}`. Absent env var =
/// no bridges allowed (all requests to `/deposit/bridge` return 503).
fn parse_bridge_allowlist() -> std::collections::HashMap<String, String> {
    let raw = match std::env::var("VELA_BRIDGE_ALLOWLIST") {
        Ok(s) if !s.is_empty() => s,
        _ => return std::collections::HashMap::new(),
    };
    let parsed: Vec<serde_json::Value> = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return std::collections::HashMap::new(),
    };
    parsed
        .into_iter()
        .filter_map(|v| {
            let id = v.get("bridge_id")?.as_str()?.to_string();
            let pk = v.get("pubkey_hex")?.as_str()?.to_string();
            Some((id, pk))
        })
        .collect()
}

async fn bridge_registry_handler() -> impl IntoResponse {
    let allowlist = parse_bridge_allowlist();
    let bridges: Vec<serde_json::Value> = allowlist
        .into_iter()
        .map(|(id, pk)| {
            serde_json::json!({
                "bridge_id": id,
                "pubkey_hex": pk,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "bridges": bridges,
            "note": "Bridges are whitelisted at boot via VELA_BRIDGE_ALLOWLIST. Deposits attested by a listed bridge are credited to the user's exchange balance.",
        }))),
    )
        .into_response()
}

async fn bridge_deposit_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BridgeDepositBody>,
) -> impl IntoResponse {
    if !state.deposit_limiter.check(&body.user) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::err(
                "Rate limit exceeded. Please slow down.",
            )),
        )
            .into_response();
    }

    let allowlist = parse_bridge_allowlist();
    let bridge_addr_hex = match allowlist.get(&body.bridge_id) {
        Some(pk) => pk.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiResponse::<()>::err(format!(
                    "bridge_id '{}' not in allowlist",
                    body.bridge_id
                ))),
            )
                .into_response()
        }
    };

    // Verify signature. Bridge public keys are Ethereum-style addresses;
    // verify_matches recovers signer and checks it matches the allowlist.
    let msg = bridge_deposit_signing_message(
        &body.bridge_id,
        &body.user,
        &body.asset,
        &body.amount,
        &body.source_chain,
        &body.source_tx_hash,
    );
    if crate::auth::verify_matches_async(msg, body.signature.clone(), bridge_addr_hex)
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "bridge signature did not verify against allowlist entry",
            )),
        )
            .into_response();
    }

    let user_id = match UserId::from_hex(&body.user) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid user address")),
            )
                .into_response()
        }
    };
    let amount: u64 = match body.amount.parse() {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid amount")),
            )
                .into_response()
        }
    };
    if amount == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("amount must be > 0")),
        )
            .into_response();
    }

    // Parse source_chain (lowercase string) → enum. Unknown chains
    // default to Ethereum; unknown source-chain strings should probably
    // reject rather than silently reclassify, so guard explicitly.
    let source_chain = match body.source_chain.to_ascii_lowercase().as_str() {
        "ethereum" => types::SourceChain::Ethereum,
        "arbitrum" => types::SourceChain::Arbitrum,
        "base" => types::SourceChain::Base,
        "optimism" => types::SourceChain::Optimism,
        "polygon" => types::SourceChain::Polygon,
        "solana" => types::SourceChain::Solana,
        "tron" => types::SourceChain::Tron,
        "bitcoin" => types::SourceChain::Bitcoin,
        "bnb" => types::SourceChain::Bnb,
        "avalanche" => types::SourceChain::Avalanche,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!(
                    "unknown source_chain '{}'",
                    body.source_chain
                ))),
            )
                .into_response()
        }
    };

    // Reuse the l1_tx_hash slot for the source-chain tx hash. Serves the
    // same replay-nonce purpose regardless of origin chain. We hash the
    // hex string with keccak so the byte layout is uniform.
    let mut l1_tx_hash = [0u8; 32];
    {
        use sha3::{Digest, Keccak256};
        let mut h = Keccak256::new();
        h.update(format!("{}:{}", body.source_chain, body.source_tx_hash).as_bytes());
        l1_tx_hash.copy_from_slice(&h.finalize());
    }

    let req = DepositRequest {
        user: user_id,
        asset: AssetId::from_str(&body.asset),
        amount,
        l1_tx_hash,
        source_chain,
        bridge_id: Some(body.bridge_id.clone()),
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: EngineRequest::Deposit(req),
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        crate::ORDER_CHANNEL_SEND_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err("engine unavailable")),
        )
            .into_response();
    }
    let responses = match tokio::time::timeout(dispatch_timeout(), resp_rx).await {
        Ok(Ok(r)) => r,
        _ => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ApiResponse::<()>::err("engine dispatch timed out")),
            )
                .into_response();
        }
    };

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "credited": true,
            "user": body.user.to_lowercase(),
            "asset": body.asset,
            "amount": body.amount,
            "source_chain": body.source_chain,
            "source_tx_hash": body.source_tx_hash,
            "bridge_id": body.bridge_id,
            "engine_responses": responses.len(),
        }))),
    )
        .into_response()
}

async fn deposit_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DepositBody>,
) -> impl IntoResponse {
    if !state.deposit_limiter.check(&body.user) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::err(
                "Rate limit exceeded. Please slow down.",
            )),
        )
            .into_response();
    }

    if !body.user.starts_with("0x") || body.user.len() != 42 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("Invalid wallet address format")),
        )
            .into_response();
    }

    let user = match UserId::from_hex(&body.user) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("Invalid wallet address format")),
            )
                .into_response()
        }
    };

    if !KNOWN_ASSETS
        .iter()
        .any(|&a| a.eq_ignore_ascii_case(&body.asset))
    {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err("Invalid asset. Supported: ETH, USDC, BTC, SOL, AVAX, MATIC, LINK, UNI, ARB, OP, AAVE, DOGE"))).into_response();
    }

    let amount = match parse_decimal_amount(&body.amount) {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid amount")),
            )
                .into_response()
        }
    };

    if amount == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("Amount must be greater than 0")),
        )
            .into_response();
    }

    if amount > 1_000_000_000_000u64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "Amount exceeds maximum deposit limit of 1,000,000",
            )),
        )
            .into_response();
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
        source_chain: Default::default(),
        bridge_id: None,
    };

    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: EngineRequest::Deposit(req),
        ts,
        responder,
        decryption_proof: None,
    };
    if state.order_tx.send(channel_item).await.is_err() {
        crate::ORDER_CHANNEL_SEND_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err("engine unavailable")),
        )
            .into_response();
    }
    let responses = match tokio::time::timeout(dispatch_timeout(), resp_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("engine error")),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ApiResponse::<()>::err("engine dispatch timed out")),
            )
                .into_response()
        }
    };

    state
        .feeds
        .lock()
        .await
        .dispatch_response_batch(&user, &responses);

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
    let balances: Vec<BalanceResponse> = us
        .balances
        .iter()
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
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::err(
                "Rate limit exceeded. Please slow down.",
            )),
        )
            .into_response();
    }

    let operator_key = match std::env::var("OPERATOR_PRIVATE_KEY") {
        Ok(k) => k,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("Operator key not configured")),
            )
                .into_response()
        }
    };

    let user_id = match UserId::from_hex(&body.user) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };

    let asset_addr = match asset_address_for(&body.asset) {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("unsupported asset")),
            )
                .into_response()
        }
    };

    let amount_wei = match parse_eth_amount_wei(&body.amount) {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid amount")),
            )
                .into_response()
        }
    };

    let user_bytes = user_id.0;
    let user_hex = format!("0x{}", hex::encode(user_bytes));
    let asset_hex = format!("0x{}", hex::encode(asset_addr));
    let amount_wei_str = amount_wei.to_string();
    let nonce = body.nonce;

    let (chain_id, settlement_addr) = match settlement_context() {
        Ok(ctx) => ctx,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err(e)),
            )
                .into_response()
        }
    };

    let signature = match tokio::task::spawn_blocking(move || {
        sign_withdrawal_op(
            operator_key,
            user_bytes,
            asset_addr,
            amount_wei,
            nonce,
            chain_id,
            settlement_addr,
        )
    })
    .await
    {
        Ok(Ok(sig)) => sig,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err(e)),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("signing failed")),
            )
                .into_response()
        }
    };

    (
        StatusCode::OK,
        Json(ApiResponse::ok(WithdrawalSignatureData {
            signature,
            user: user_hex,
            asset: asset_hex,
            amount_wei: amount_wei_str,
            nonce,
        })),
    )
        .into_response()
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
    let hash_str = body
        .l1_tx_hash
        .strip_prefix("0x")
        .unwrap_or(&body.l1_tx_hash);
    let hash_bytes = match hex::decode(hash_str) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(
                    "l1_tx_hash must be a 0x-prefixed 32-byte hex string",
                )),
            )
                .into_response();
        }
    };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    let (request, req_user) = match body.request {
        ForceIncludeRequest::Deposit {
            ref user,
            ref asset,
            amount,
        } => {
            let uid = match UserId::from_hex(user) {
                Ok(u) => u,
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ApiResponse::<()>::err("invalid user address")),
                    )
                        .into_response()
                }
            };
            let req = EngineRequest::Deposit(DepositRequest {
                user: uid.clone(),
                asset: AssetId::from_str(asset),
                amount,
                l1_tx_hash: hash_bytes,
                source_chain: Default::default(),
                bridge_id: None,
            });
            (req, uid)
        }
        ForceIncludeRequest::Withdrawal {
            ref user,
            ref asset,
            amount,
            nonce,
        } => {
            let uid = match UserId::from_hex(user) {
                Ok(u) => u,
                Err(_) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(ApiResponse::<()>::err("invalid user address")),
                    )
                        .into_response()
                }
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
        crate::ORDER_CHANNEL_SEND_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err("engine unavailable")),
        )
            .into_response();
    }
    let responses = match tokio::time::timeout(dispatch_timeout(), resp_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("engine error")),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::GATEWAY_TIMEOUT,
                Json(ApiResponse::<()>::err("engine dispatch timed out")),
            )
                .into_response()
        }
    };

    state
        .feeds
        .lock()
        .await
        .dispatch_response_batch(&req_user, &responses);

    if let Some(msg) = first_engine_error(&responses) {
        return (StatusCode::BAD_REQUEST, Json(ApiResponse::<()>::err(msg))).into_response();
    }

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "l1_tx_hash": body.l1_tx_hash,
            "responses": responses,
            "note": "forced inclusion processed; committer will include in next batch",
        }))),
    )
        .into_response()
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
    }

    let (market_ids_bases_quotes, total_users, total_deposits, total_open_orders) = {
        let engine = state.engine.lock().await;
        let mids: Vec<_> = engine
            .markets
            .values()
            .map(|m| (m.id.clone(), m.base, m.quote))
            .collect();
        let us = state.shards.user_state.read().await;
        let tu = us.metadata.len();
        let td: Vec<serde_json::Value> = us
            .balances
            .iter()
            .map(|((user, asset), bal)| {
                serde_json::json!({
                    "user": format!("0x{}", hex::encode(user.0)),
                    "asset": asset.as_str(),
                    "amount": format_amount(bal.total(), 8),
                })
            })
            .collect();
        let too: usize = us.metadata.values().map(|m| m.order_id_count()).sum();
        (mids, tu, td, too)
    };

    let mut markets: Vec<serde_json::Value> = Vec::new();
    for (market_id, base, quote) in market_ids_bases_quotes {
        let (best_bid, best_ask) = if let Some(shard_arc) = state.shards.shards.get(&market_id) {
            let shard = shard_arc.lock().await;
            let book = shard.engine.order_books.get(&market_id);
            (
                book.and_then(|b| b.best_bid())
                    .map(|p| format_amount(p, PRICE_DECIMALS)),
                book.and_then(|b| b.best_ask())
                    .map(|p| format_amount(p, PRICE_DECIMALS)),
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

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "markets": markets,
            "total_users": total_users,
            "total_deposits": total_deposits,
            "total_open_orders": total_open_orders,
            "snapshot_exists": snapshot_exists,
            "uptime_secs": uptime_secs,
        }))),
    )
        .into_response()
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
        windows
            .entry(fill.timestamp / WINDOW_US)
            .or_default()
            .push(fill.clone());
    }
    drop(fills);

    let window_vec: Vec<(u64, Vec<StoredFill>)> = windows.into_iter().collect();
    let mut prev_root = format!("0x{}", "0".repeat(64));

    let batches: Vec<BatchSummary> = window_vec
        .iter()
        .enumerate()
        .map(|(idx, (window_key, batch_fills))| {
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
        })
        .collect();

    for (idx, (window_key, batch_fills)) in window_vec.iter().enumerate() {
        let batch_id = (idx + 1) as u64;
        let fill_ids: Vec<String> = batch_fills.iter().map(|f| f.id.clone()).collect();
        let state_root_after = batch_state_root(&fill_ids);
        let state_root_before = prev_root.clone();
        prev_root = state_root_after.clone();

        let has_proof = state.proofs.lock().await.contains_key(&batch_id);
        if !has_proof {
            let proof_fills: Vec<zkvm::ProofFill> =
                batch_fills.iter().map(stored_fill_to_proof_fill).collect();
            let orders_processed = batch_fills.len() as u64 * (idx as u64 + 1);
            let timestamp = window_key * WINDOW_US / 1000;
            spawn_proof(
                Arc::clone(&state),
                batch_id,
                state_root_before,
                state_root_after.clone(),
                proof_fills,
                orders_processed,
                timestamp,
            );
        }
        let has_attestation = state.attestations.lock().await.contains_key(&batch_id);
        if !has_attestation {
            let fill_count = batch_fills.len() as u64;
            let orders_processed = fill_count * (idx as u64 + 1);
            let timestamp = window_key * WINDOW_US / 1000;
            spawn_attestation(
                Arc::clone(&state),
                batch_id,
                state_root_after,
                fill_count,
                orders_processed,
                timestamp,
            );
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
        windows
            .entry(fill.timestamp / WINDOW_US)
            .or_default()
            .push(fill.clone());
    }
    drop(fills);

    let target_idx = batch_id.saturating_sub(1) as usize;
    let window_vec: Vec<(u64, Vec<StoredFill>)> = windows.into_iter().collect();
    let entry = window_vec.into_iter().enumerate().nth(target_idx);
    match entry {
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("batch not found")),
        )
            .into_response(),
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
                        prev_windows
                            .entry(f.timestamp / WINDOW_US)
                            .or_default()
                            .push(f.clone());
                    }
                    drop(fills_guard);
                    let prev_fill_ids: Vec<String> = prev_windows
                        .into_iter()
                        .nth(idx - 1)
                        .map(|(_, fs)| fs.iter().map(|f| f.id.clone()).collect())
                        .unwrap_or_default();
                    batch_state_root(&prev_fill_ids)
                };
                let proof_fills: Vec<zkvm::ProofFill> =
                    batch_fills.iter().map(stored_fill_to_proof_fill).collect();
                let orders_processed = batch_fills.len() as u64;
                let timestamp = window_key * WINDOW_US / 1000;
                spawn_proof(
                    Arc::clone(&state),
                    batch_id,
                    state_root_before,
                    state_root_after,
                    proof_fills,
                    orders_processed,
                    timestamp,
                );
            }

            let has_attestation = state.attestations.lock().await.contains_key(&batch_id);
            if !has_attestation {
                let fill_count = batch_fills.len() as u64;
                let orders_processed = fill_count;
                let timestamp = window_key * WINDOW_US / 1000;
                spawn_attestation(
                    Arc::clone(&state),
                    batch_id,
                    state_root.clone(),
                    fill_count,
                    orders_processed,
                    timestamp,
                );
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
    let last_anchor_time = if last_anchor_time_raw == 0 {
        None
    } else {
        Some(last_anchor_time_raw)
    };
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

fn generate_simulated_candles(
    market_id: &str,
    interval_secs: u64,
    limit: usize,
) -> Vec<OhlcvCandle> {
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
        candles.push(OhlcvCandle {
            time,
            open,
            high,
            low,
            close,
            volume,
        });
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
    let mut market_fills: Vec<&StoredFill> =
        fills.iter().filter(|f| f.market_id == market_id).collect();
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
            OhlcvCandle {
                time: bucket_time,
                open,
                high,
                low,
                close,
                volume,
            }
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
        let mut bucket_fills: Vec<_> = fills
            .iter()
            .filter(|f| {
                f.market_id == market
                    && f.timestamp >= bucket_start_us
                    && f.timestamp < bucket_end_us
            })
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
            if sender
                .send(Message::Text(
                    serde_json::to_string(&snap).unwrap_or_default(),
                ))
                .await
                .is_err()
            {
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

const KNOWN_ASSETS: &[&str] = &[
    "ETH", "USDC", "BTC", "SOL", "AVAX", "MATIC", "LINK", "UNI", "ARB", "OP", "AAVE", "DOGE",
];

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
            (
                StatusCode::OK,
                Json(ApiResponse::ok(serde_json::json!({
                    "market": market_id,
                    "maker_fee_bps": m.maker_fee_bps,
                    "taker_fee_bps": m.taker_fee_bps,
                    "maker_fee_pct": maker_fee_pct,
                    "taker_fee_pct": taker_fee_pct,
                }))),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("market not found")),
        )
            .into_response(),
    }
}

async fn list_fees(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let engine = state.engine.lock().await;
    let fees: Vec<serde_json::Value> = engine
        .markets
        .values()
        .map(|m| {
            serde_json::json!({
                "market": m.id.0,
                "maker_fee_bps": m.maker_fee_bps,
                "taker_fee_bps": m.taker_fee_bps,
                "maker_fee_pct": m.maker_fee_bps as f64 / 100.0,
                "taker_fee_pct": m.taker_fee_bps as f64 / 100.0,
            })
        })
        .collect();
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
        fee_tier: 0,
    }
}

async fn register_referral(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ReferralRegisterBody>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&body.user) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid user address")),
            )
                .into_response()
        }
    };
    let ref_user = match UserId::from_hex(&body.referrer) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid ref address")),
            )
                .into_response()
        }
    };
    if user == ref_user {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("cannot refer yourself")),
        )
            .into_response();
    }
    let msg = format!(
        "vela:referral:{}:{}:{}",
        body.user.to_lowercase(),
        body.referrer.to_lowercase(),
        body.nonce
    )
    .into_bytes();
    if verify_matches_async(msg, body.signature.clone(), body.user.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("invalid signature")),
        )
            .into_response();
    }
    let mut us = state.shards.user_state.write().await;
    let ref_exists =
        us.metadata.contains_key(&ref_user) || us.balances.keys().any(|(u, _)| u == &ref_user);
    if !ref_exists {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("referrer not found")),
        )
            .into_response();
    }
    {
        let existing = us.metadata.get(&user);
        if existing.map(|m| m.ref_by.is_some()).unwrap_or(false) {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("referrer already set")),
            )
                .into_response();
        }
    }
    let mut user_meta = us
        .metadata
        .get(&user)
        .cloned()
        .unwrap_or_else(|| default_user_metadata(&user));
    user_meta.ref_by = Some(body.referrer.to_lowercase());
    us.metadata.insert(user.clone(), user_meta);
    let mut ref_meta = us
        .metadata
        .get(&ref_user)
        .cloned()
        .unwrap_or_else(|| default_user_metadata(&ref_user));
    let user_hex = body.user.to_lowercase();
    if !ref_meta.referred_users.contains(&user_hex) {
        ref_meta.referred_users.push(user_hex);
    }
    us.metadata.insert(ref_user, ref_meta);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({"registered": true}))),
    )
        .into_response()
}

async fn get_referral_handler(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&address) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };
    let us = state.shards.user_state.read().await;
    let meta = us
        .metadata
        .get(&user)
        .cloned()
        .unwrap_or_else(|| default_user_metadata(&user));
    let earnings_usdc = format!("{:.6}", meta.ref_earnings as f64 / 1_000_000.0);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "address": address.to_lowercase(),
            "referrer": meta.ref_by,
            "referred_count": meta.referred_users.len(),
            "total_earnings_usdc": earnings_usdc,
            "referred_users": meta.referred_users,
        }))),
    )
        .into_response()
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
    for id in &fill_ids {
        hasher.update(id.as_bytes());
    }
    for id in &order_ids {
        hasher.update(id.as_bytes());
    }
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

// ---------------------------------------------------------------------------
// Volume-tiered maker rebate program.
//
// User's 30-day USDC volume selects a tier; tier maps to maker+taker bps
// via `types::fee_tiers`. The tier is cached on `UserMetadata.fee_tier` so
// the matching engine reads it in O(1) at fill time. The recompute task
// below runs hourly and rewrites tiers from the last 30d of state.fills.
// ---------------------------------------------------------------------------

/// Sum a user's 30d fills as USDC micro (fixed-point 1e6). Counts both
/// maker and taker sides.
async fn user_30d_volume_micro(state: &AppState, address_lower: &str, now_ms: u64) -> u64 {
    let cutoff = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);
    let fills = state.fills.lock().await;
    fills
        .iter()
        .filter(|f| f.timestamp >= cutoff)
        .filter(|f| {
            f.maker_address.to_ascii_lowercase() == address_lower
                || f.taker_address.to_ascii_lowercase() == address_lower
        })
        .map(|f| (f.price as u128 * f.quantity as u128 / 1_000_000u128) as u64)
        .sum()
}

async fn fees_schedule_handler() -> impl IntoResponse {
    let tiers: Vec<serde_json::Value> = (0..types::fee_tiers::TIER_COUNT)
        .map(|i| {
            let threshold = types::fee_tiers::THRESHOLDS_MICRO[i];
            serde_json::json!({
                "tier": i,
                "min_30d_volume_usdc": format!("{:.2}", threshold as f64 / 1_000_000.0),
                "maker_bps": types::fee_tiers::MAKER_BPS[i],
                "taker_bps": types::fee_tiers::TAKER_BPS[i],
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "window_days": 30,
            "tiers": tiers,
            "notes": "Maker bps negative = rebate. Tier applies per-user; maker and taker of the same fill can be in different tiers.",
        }))),
    )
        .into_response()
}

async fn fees_tier_handler(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let addr = address.to_ascii_lowercase();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let volume_micro = user_30d_volume_micro(&state, &addr, now_ms).await;
    let tier = types::fee_tiers::tier_for_volume(volume_micro);
    let (maker_bps, taker_bps) = types::fee_tiers::fees_for_tier(tier);

    // Cached tier from user metadata (what the matcher is actually using).
    let cached_tier = if let Ok(uid) = UserId::from_hex(&addr) {
        let us = state.shards.user_state.read().await;
        us.metadata.get(&uid).map(|m| m.fee_tier).unwrap_or(0)
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("invalid address")),
        )
            .into_response();
    };

    let next_threshold = if (tier as usize) + 1 < types::fee_tiers::TIER_COUNT {
        Some(types::fee_tiers::THRESHOLDS_MICRO[tier as usize + 1])
    } else {
        None
    };

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "address": addr,
            "computed_tier": tier,
            "cached_tier": cached_tier,
            "volume_30d_usdc": format!("{:.2}", volume_micro as f64 / 1_000_000.0),
            "maker_bps": maker_bps,
            "taker_bps": taker_bps,
            "next_tier_threshold_usdc": next_threshold.map(|v| format!("{:.2}", v as f64 / 1_000_000.0)),
            "note": "computed_tier is what your volume qualifies for now; cached_tier is what the engine applied at last recompute (hourly).",
        }))),
    )
        .into_response()
}

/// Long-running task: every hour, walk all users with recorded fills
/// and update their `UserMetadata.fee_tier` to match their 30d volume.
/// Runs against `state.shards.user_state` so both the main engine and
/// the shard engines see the same tier through the phase-3 fold.
pub async fn run_fee_tier_task(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
    ticker.tick().await; // Skip immediate first tick.

    loop {
        ticker.tick().await;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        // Collect unique addresses that appeared in any recent fill.
        let addresses: std::collections::HashSet<String> = {
            let fills = state.fills.lock().await;
            fills
                .iter()
                .flat_map(|f| {
                    [
                        f.maker_address.to_ascii_lowercase(),
                        f.taker_address.to_ascii_lowercase(),
                    ]
                })
                .collect()
        };

        let mut updates = 0usize;
        for addr in addresses {
            let volume_micro = user_30d_volume_micro(&state, &addr, now_ms).await;
            let new_tier = types::fee_tiers::tier_for_volume(volume_micro);
            let uid = match UserId::from_hex(&addr) {
                Ok(u) => u,
                Err(_) => continue,
            };
            let mut us = state.shards.user_state.write().await;
            if let Some(meta) = us.metadata.get_mut(&uid) {
                if meta.fee_tier != new_tier {
                    meta.fee_tier = new_tier;
                    updates += 1;
                }
            }
        }
        tracing::info!("fee-tier recompute: updated {} users", updates);
    }
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

// ---------------------------------------------------------------------------
// Points system — volume-weighted, toxicity-gated.
//
// Formula:
//   maker_points   = notional_usdc × MAKER_MULTIPLIER
//   taker_points   = notional_usdc × (1.0 − toxicity_score)
//   referral_bonus = ref_earnings_usdc × REFERRAL_MULTIPLIER
//
// Rationale:
// - Makers get a bonus because liquidity provision is the scarce resource
//   we're trying to attract.
// - Takers get penalized proportional to how toxic their flow was. A wash
//   trader whose fills all score near 1.0 earns near-zero taker points.
// - Synthetic (MM-bot) fills have toxicity_score = 0.0 and count normally
//   for volume, but wash-loop trades between the same wallet score high
//   on the OFI accumulator so the toxicity gate catches them naturally.
// ---------------------------------------------------------------------------

const POINTS_MAKER_MULTIPLIER: f64 = 1.5;
const POINTS_REFERRAL_MULTIPLIER: f64 = 0.5;
/// 30-day rolling window for leaderboard eligibility.
const POINTS_WINDOW_MS: u64 = 30 * 24 * 60 * 60 * 1_000;

/// Convert a fill's `price * quantity` (each in 1e6 fixed-point) to a
/// USDC notional as `f64`.
fn fill_notional_usdc(price: u64, quantity: u64) -> f64 {
    (price as f64 * quantity as f64) / 1_000_000_000_000.0
}

/// Compute (maker_points, taker_points) for a single fill.
fn fill_points(fill: &StoredFill) -> (f64, f64) {
    let notional = fill_notional_usdc(fill.price, fill.quantity);
    let maker = notional * POINTS_MAKER_MULTIPLIER;
    let clean = (1.0 - fill.toxicity_score).clamp(0.0, 1.0);
    let taker = notional * clean;
    (maker, taker)
}

#[derive(Default, Clone, Copy)]
struct PointsBreakdown {
    maker_points: f64,
    taker_points: f64,
    volume_usdc: f64,
    maker_count: u64,
    taker_count: u64,
    toxic_taker_count: u64,
}

impl PointsBreakdown {
    fn total(&self) -> f64 {
        self.maker_points + self.taker_points
    }
}

/// Aggregate points per address across all fills in `state.fills`,
/// filtered to the rolling 30-day window.
async fn points_by_address(state: &AppState) -> std::collections::HashMap<String, PointsBreakdown> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff = now_ms.saturating_sub(POINTS_WINDOW_MS);

    let fills = state.fills.lock().await;
    let mut out: std::collections::HashMap<String, PointsBreakdown> =
        std::collections::HashMap::new();
    for fill in fills.iter() {
        if fill.timestamp < cutoff {
            continue;
        }
        let (maker_pts, taker_pts) = fill_points(fill);
        let notional = fill_notional_usdc(fill.price, fill.quantity);

        let maker_entry = out.entry(fill.maker_address.to_lowercase()).or_default();
        maker_entry.maker_points += maker_pts;
        maker_entry.volume_usdc += notional;
        maker_entry.maker_count += 1;

        let taker_entry = out.entry(fill.taker_address.to_lowercase()).or_default();
        taker_entry.taker_points += taker_pts;
        taker_entry.volume_usdc += notional;
        taker_entry.taker_count += 1;
        if fill.toxicity_score > 0.5 {
            taker_entry.toxic_taker_count += 1;
        }
    }
    out
}

async fn get_points_handler(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let addr = address.to_lowercase();
    let by_addr = points_by_address(&state).await;
    let breakdown = by_addr.get(&addr).copied().unwrap_or_default();

    // Referral bonus from persistent metadata.
    let referral_bonus = {
        let us = state.shards.user_state.read().await;
        let user_id = match UserId::from_hex(&addr) {
            Ok(u) => u,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(ApiResponse::<()>::err("invalid address")),
                )
                    .into_response()
            }
        };
        us.metadata
            .get(&user_id)
            .map(|m| (m.ref_earnings as f64 / 1_000_000.0) * POINTS_REFERRAL_MULTIPLIER)
            .unwrap_or(0.0)
    };

    let total = breakdown.total() + referral_bonus;

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "address": addr,
            "window_days": 30,
            "total_points": format!("{:.2}", total),
            "maker_points": format!("{:.2}", breakdown.maker_points),
            "taker_points": format!("{:.2}", breakdown.taker_points),
            "referral_points": format!("{:.2}", referral_bonus),
            "volume_usdc": format!("{:.2}", breakdown.volume_usdc),
            "maker_fill_count": breakdown.maker_count,
            "taker_fill_count": breakdown.taker_count,
            "toxic_taker_fill_count": breakdown.toxic_taker_count,
            "formula": {
                "maker_multiplier": POINTS_MAKER_MULTIPLIER,
                "referral_multiplier": POINTS_REFERRAL_MULTIPLIER,
                "taker_penalty": "notional * (1 - toxicity_score); fills with score > 0.5 are counted as toxic",
            },
        }))),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// RFQ / block-trade venue.
//
// Ships the primitive so it's ready when MM depth is sufficient.
// Deliberately off-book so a $2M taker doesn't leak to the trade tape
// via CLOB slippage. Requester signs a request → whitelisted MMs post
// signed quotes → requester accepts → Vela settles atomically.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct RfqRequestBody {
    requester: String,
    market: String,
    side: types::OrderSide,
    quantity: u64,
    /// Wall-clock ms after which the request stops accepting quotes.
    expires_at_ms: u64,
    nonce: u64,
    /// Signature over `vela:rfq:request:{market}:{side}:{quantity}:{expires_at_ms}:{nonce}`.
    signature: String,
}

fn rfq_request_signing_message(
    market: &str,
    side: types::OrderSide,
    quantity: u64,
    expires_at_ms: u64,
    nonce: u64,
) -> Vec<u8> {
    format!(
        "vela:rfq:request:{}:{:?}:{}:{}:{}",
        market, side, quantity, expires_at_ms, nonce
    )
    .into_bytes()
}

async fn rfq_request(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RfqRequestBody>,
) -> impl IntoResponse {
    let requester = match UserId::from_hex(&body.requester) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid requester address")),
            )
                .into_response()
        }
    };
    let _ = requester;
    let msg = rfq_request_signing_message(
        &body.market,
        body.side,
        body.quantity,
        body.expires_at_ms,
        body.nonce,
    );
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.requester.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "requester signature did not match payload",
            )),
        )
            .into_response();
    }
    if body.quantity == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("quantity must be > 0")),
        )
            .into_response();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if body.expires_at_ms <= now_ms {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "expires_at_ms must be in the future",
            )),
        )
            .into_response();
    }

    let rfq_id = crate::rfq::next_rfq_id();
    let request = crate::rfq::RfqRequest {
        rfq_id,
        requester: body.requester.to_lowercase(),
        market: body.market,
        side: body.side,
        quantity: body.quantity,
        expires_at_ms: body.expires_at_ms,
        created_at_ms: now_ms,
        status: crate::rfq::RfqStatus::Open,
        filled_by_quote_id: None,
    };
    state.rfq.requests.insert(rfq_id, request.clone());

    (StatusCode::OK, Json(ApiResponse::ok(request))).into_response()
}

#[derive(serde::Deserialize)]
struct RfqQuoteBody {
    rfq_id: u64,
    maker: String,
    price: u64,
    quantity: u64,
    expires_at_ms: u64,
    nonce: u64,
    /// Signature over `vela:rfq:quote:{rfq_id}:{price}:{quantity}:{expires_at_ms}:{nonce}`.
    signature: String,
}

fn rfq_quote_signing_message(
    rfq_id: u64,
    price: u64,
    quantity: u64,
    expires_at_ms: u64,
    nonce: u64,
) -> Vec<u8> {
    format!(
        "vela:rfq:quote:{}:{}:{}:{}:{}",
        rfq_id, price, quantity, expires_at_ms, nonce
    )
    .into_bytes()
}

async fn rfq_quote(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RfqQuoteBody>,
) -> impl IntoResponse {
    // Two admit paths, in order:
    //   1. Human-desk allowlist (VELA_RFQ_MAKERS) — always accepted.
    //   2. Agent-maker path (Tier 3.6): non-expired reputation
    //      attestation on file with score_bps ≥ VELA_RFQ_MAKER_MIN_SCORE_BPS.
    // Neither → 403. Tag the quote with the provenance so requesters
    // can see how the quote earned its slot.
    let maker_lower = body.maker.to_lowercase();
    let allow = crate::rfq::maker_allowlist();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let provenance = if allow.contains(&maker_lower) {
        crate::rfq::MakerProvenance::Allowlist
    } else {
        let min_score = crate::rfq::agent_maker_min_score_bps();
        match state.reputation_cache.get(&maker_lower) {
            Some(r) if r.expires_at_ms > now_ms && r.score_bps >= min_score => {
                crate::rfq::MakerProvenance::Reputation {
                    score_bps: r.score_bps,
                }
            }
            _ => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(ApiResponse::<()>::err(
                        "maker not admitted: not on VELA_RFQ_MAKERS and no reputation attestation ≥ VELA_RFQ_MAKER_MIN_SCORE_BPS on file",
                    )),
                )
                    .into_response();
            }
        }
    };

    // Fetch the request to validate against.
    let req = match state.rfq.requests.get(&body.rfq_id) {
        Some(r) => r.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("rfq_id not found")),
            )
                .into_response()
        }
    };
    if !matches!(req.status, crate::rfq::RfqStatus::Open) {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err("rfq is not open")),
        )
            .into_response();
    }
    if body.quantity != req.quantity {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "quote quantity must equal RFQ quantity (partials are v2)",
            )),
        )
            .into_response();
    }

    let msg = rfq_quote_signing_message(
        body.rfq_id,
        body.price,
        body.quantity,
        body.expires_at_ms,
        body.nonce,
    );
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.maker.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "maker signature did not match payload",
            )),
        )
            .into_response();
    }

    let quote_id = crate::rfq::next_quote_id();
    let quote = crate::rfq::RfqQuote {
        quote_id,
        rfq_id: body.rfq_id,
        maker: maker_lower.clone(),
        price: body.price,
        quantity: body.quantity,
        expires_at_ms: body.expires_at_ms,
        created_at_ms: now_ms,
        provenance,
    };
    state
        .rfq
        .quotes
        .insert((body.rfq_id, quote_id), quote.clone());

    tracing::info!(
        target: "rfq",
        rfq_id = body.rfq_id,
        quote_id,
        maker = %maker_lower,
        price = body.price,
        provenance = ?quote.provenance,
        "rfq quote posted"
    );

    (StatusCode::OK, Json(ApiResponse::ok(quote))).into_response()
}

#[derive(serde::Deserialize)]
struct RfqAcceptBody {
    rfq_id: u64,
    quote_id: u64,
    requester: String,
    nonce: u64,
    /// Signature over `vela:rfq:accept:{rfq_id}:{quote_id}:{nonce}`.
    signature: String,
}

async fn rfq_accept(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RfqAcceptBody>,
) -> impl IntoResponse {
    let request = match state.rfq.requests.get(&body.rfq_id) {
        Some(r) => r.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("rfq_id not found")),
            )
                .into_response()
        }
    };
    if !matches!(request.status, crate::rfq::RfqStatus::Open) {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err("rfq is not open")),
        )
            .into_response();
    }
    if request.requester != body.requester.to_lowercase() {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::<()>::err("not the requester of this rfq")),
        )
            .into_response();
    }
    let msg = format!(
        "vela:rfq:accept:{}:{}:{}",
        body.rfq_id, body.quote_id, body.nonce
    );
    if crate::auth::verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.requester.clone(),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "requester signature did not match payload",
            )),
        )
            .into_response();
    }

    let quote = match state.rfq.quotes.get(&(body.rfq_id, body.quote_id)) {
        Some(q) => q.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("quote_id not found for rfq_id")),
            )
                .into_response()
        }
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if quote.expires_at_ms <= now_ms {
        return (
            StatusCode::GONE,
            Json(ApiResponse::<()>::err("quote has expired")),
        )
            .into_response();
    }

    // Book-improvement gate: quote must be no worse than current touch.
    {
        let engine = state.engine.lock().await;
        if let Some(book) = engine.order_books.get(&MarketId(request.market.clone())) {
            let touch = match request.side {
                types::OrderSide::Bid => book.best_ask(),
                types::OrderSide::Ask => book.best_bid(),
            };
            if let Some(t) = touch {
                let improves = match request.side {
                    types::OrderSide::Bid => quote.price <= t,
                    types::OrderSide::Ask => quote.price >= t,
                };
                if !improves {
                    return (
                        StatusCode::CONFLICT,
                        Json(ApiResponse::<()>::err(format!(
                            "quote price {} does not improve on public book touch {}",
                            quote.price, t
                        ))),
                    )
                        .into_response();
                }
            }
        }
    }

    // Atomic settlement. Bypass the CLOB matcher: both parties have
    // already signed off on price + quantity. Move balances directly.
    let requester_id = UserId::from_hex(&request.requester).unwrap();
    let maker_id = UserId::from_hex(&quote.maker).unwrap();
    let (base_str, quote_str) = match request.market.split_once('-') {
        Some(p) => p,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("malformed market_id")),
            )
                .into_response();
        }
    };
    let base_asset = AssetId::from_str(base_str);
    let quote_asset = AssetId::from_str(quote_str);

    // Notional in USDC micro = price × quantity / 1_000_000.
    let notional_micro = (quote.price as u128 * quote.quantity as u128 / 1_000_000u128) as u64;

    // Direction: request.side is the requester's side. Bid = requester
    // buys base, pays quote. Ask = requester sells base, receives quote.
    let (buyer_id, seller_id) = match request.side {
        types::OrderSide::Bid => (requester_id.clone(), maker_id.clone()),
        types::OrderSide::Ask => (maker_id.clone(), requester_id.clone()),
    };

    {
        let mut us = state.shards.user_state.write().await;

        // Buyer needs `notional_micro` of quote; seller needs `quantity` of base.
        let buyer_quote_key = (buyer_id.clone(), quote_asset);
        let seller_base_key = (seller_id.clone(), base_asset);

        let buyer_bal = us
            .balances
            .entry(buyer_quote_key)
            .or_insert_with(|| types::Balance {
                user: buyer_id.clone(),
                asset: quote_asset,
                available: 0,
                locked: 0,
            });
        if buyer_bal.available < notional_micro {
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(ApiResponse::<()>::err(format!(
                    "buyer needs {} quote micro; has {}",
                    notional_micro, buyer_bal.available
                ))),
            )
                .into_response();
        }
        buyer_bal.available -= notional_micro;

        // Check seller balance without holding the entry borrow across
        // the rollback path.
        let seller_available = us
            .balances
            .get(&seller_base_key)
            .map(|b| b.available)
            .unwrap_or(0);
        if seller_available < quote.quantity {
            // Rollback the buyer's debit.
            if let Some(b) = us.balances.get_mut(&(buyer_id.clone(), quote_asset)) {
                b.available += notional_micro;
            }
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(ApiResponse::<()>::err(format!(
                    "seller needs {} base micro; has {}",
                    quote.quantity, seller_available
                ))),
            )
                .into_response();
        }
        let seller_bal = us
            .balances
            .entry(seller_base_key)
            .or_insert_with(|| types::Balance {
                user: seller_id.clone(),
                asset: base_asset,
                available: 0,
                locked: 0,
            });
        seller_bal.available -= quote.quantity;

        // Credit the other side.
        us.balances
            .entry((seller_id.clone(), quote_asset))
            .or_insert_with(|| types::Balance {
                user: seller_id.clone(),
                asset: quote_asset,
                available: 0,
                locked: 0,
            })
            .available += notional_micro;
        us.balances
            .entry((buyer_id.clone(), base_asset))
            .or_insert_with(|| types::Balance {
                user: buyer_id.clone(),
                asset: base_asset,
                available: 0,
                locked: 0,
            })
            .available += quote.quantity;
    }

    // Flip request status.
    if let Some(mut r) = state.rfq.requests.get_mut(&body.rfq_id) {
        r.status = crate::rfq::RfqStatus::Filled;
        r.filled_by_quote_id = Some(body.quote_id);
    }

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "rfq_id": body.rfq_id,
            "quote_id": body.quote_id,
            "market": request.market,
            "buyer": format!("0x{}", hex::encode(buyer_id.0)),
            "seller": format!("0x{}", hex::encode(seller_id.0)),
            "price": quote.price,
            "quantity": quote.quantity,
            "notional_micro": notional_micro,
        }))),
    )
        .into_response()
}

async fn list_rfq_requests(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut all: Vec<crate::rfq::RfqRequest> = state
        .rfq
        .requests
        .iter()
        .map(|e| e.value().clone())
        .collect();
    all.sort_by_key(|r| std::cmp::Reverse(r.created_at_ms));
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "requests": all,
            "min_notional_micro": crate::rfq::min_notional_micro(),
            "maker_allowlist_size": crate::rfq::maker_allowlist().len(),
        }))),
    )
        .into_response()
}

async fn list_rfq_quotes(
    Path(rfq_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let quotes = state.rfq.quotes_for(rfq_id);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "rfq_id": rfq_id,
            "quotes": quotes,
        }))),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Sub-accounts (v1 MVP — see api::subaccounts module docs).
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CreateSubaccountBody {
    master: String,
    subaccount_id: u32,
    name: String,
    /// Signature by master over
    /// `vela:subaccount:create:{subaccount_id}:{name}:{nonce}`.
    nonce: u64,
    signature: String,
}

fn subaccount_create_signing_message(subaccount_id: u32, name: &str, nonce: u64) -> Vec<u8> {
    format!(
        "vela:subaccount:create:{}:{}:{}",
        subaccount_id, name, nonce
    )
    .into_bytes()
}

async fn create_subaccount(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateSubaccountBody>,
) -> impl IntoResponse {
    let master = match UserId::from_hex(&body.master) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid master address")),
            )
                .into_response()
        }
    };
    let msg = subaccount_create_signing_message(body.subaccount_id, &body.name, body.nonce);
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.master.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "master signature did not match payload",
            )),
        )
            .into_response();
    }

    let key = (body.master.to_lowercase(), body.subaccount_id);
    if state.subaccounts.subs.contains_key(&key) {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err(
                "subaccount_id already exists for this master",
            )),
        )
            .into_response();
    }

    let sub_user_id = crate::subaccounts::derive_sub_user_id(&master, body.subaccount_id);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Register master as an agent for the sub's derived user_id — same
    // pattern as vaults. Master signs orders with `user = sub_user_id`
    // and the existing agent path passes verification.
    state.agents.register(crate::agents::AgentDelegation {
        master: sub_user_id.clone(),
        agent: master.clone(),
        expires_at_ms: now_ms + 10 * 365 * 24 * 60 * 60 * 1_000,
        max_notional_per_order: u64::MAX,
        revoked: false,
        nonce: body.nonce,
        scope: crate::agents::CapabilityScope::default(),
    });

    let sub = crate::subaccounts::SubAccount {
        master: body.master.to_lowercase(),
        subaccount_id: body.subaccount_id,
        name: body.name,
        user_id: sub_user_id.clone(),
        created_at_ms: now_ms,
    };
    state.subaccounts.subs.insert(key, sub.clone());

    (StatusCode::OK, Json(ApiResponse::ok(sub))).into_response()
}

#[derive(serde::Deserialize)]
struct TransferSubaccountBody {
    master: String,
    subaccount_id: u32,
    /// USDC × 1e6 to move. Positive = master → sub. Negative = sub → master.
    amount_micro: i64,
    nonce: u64,
    /// Signature by master over
    /// `vela:subaccount:transfer:{subaccount_id}:{amount_micro}:{nonce}`.
    signature: String,
}

async fn transfer_subaccount(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TransferSubaccountBody>,
) -> impl IntoResponse {
    let master = match UserId::from_hex(&body.master) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid master address")),
            )
                .into_response()
        }
    };
    let msg = format!(
        "vela:subaccount:transfer:{}:{}:{}",
        body.subaccount_id, body.amount_micro, body.nonce
    );
    if crate::auth::verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.master.clone(),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "master signature did not match payload",
            )),
        )
            .into_response();
    }

    let sub_user_id = crate::subaccounts::derive_sub_user_id(&master, body.subaccount_id);
    let key = (body.master.to_lowercase(), body.subaccount_id);
    if !state.subaccounts.subs.contains_key(&key) {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err(
                "subaccount does not exist — create it first",
            )),
        )
            .into_response();
    }

    if body.amount_micro == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("amount_micro must be non-zero")),
        )
            .into_response();
    }

    let (from_id, to_id, amount) = if body.amount_micro > 0 {
        (
            master.clone(),
            sub_user_id.clone(),
            body.amount_micro as u64,
        )
    } else {
        (
            sub_user_id.clone(),
            master.clone(),
            (-body.amount_micro) as u64,
        )
    };

    let mut us = state.shards.user_state.write().await;
    let from_key = (from_id.clone(), types::AssetId::from_str("USDC"));
    let to_key = (to_id.clone(), types::AssetId::from_str("USDC"));

    let from_bal = us
        .balances
        .entry(from_key)
        .or_insert_with(|| types::Balance {
            user: from_id.clone(),
            asset: types::AssetId::from_str("USDC"),
            available: 0,
            locked: 0,
        });
    if from_bal.available < amount {
        return (
            StatusCode::PAYMENT_REQUIRED,
            Json(ApiResponse::<()>::err(format!(
                "source needs {} USDC micro available; has {}",
                amount, from_bal.available
            ))),
        )
            .into_response();
    }
    from_bal.available -= amount;

    let to_bal = us.balances.entry(to_key).or_insert_with(|| types::Balance {
        user: to_id.clone(),
        asset: types::AssetId::from_str("USDC"),
        available: 0,
        locked: 0,
    });
    to_bal.available += amount;

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "master": body.master.to_lowercase(),
            "subaccount_id": body.subaccount_id,
            "amount_micro": body.amount_micro,
            "direction": if body.amount_micro > 0 { "master_to_sub" } else { "sub_to_master" },
        }))),
    )
        .into_response()
}

async fn list_subaccounts(
    Path(master): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let subs = state.subaccounts.list_for(&master);
    let us = state.shards.user_state.read().await;
    let with_balances: Vec<serde_json::Value> = subs
        .into_iter()
        .map(|s| {
            let usdc_bal = us
                .balances
                .get(&(s.user_id.clone(), types::AssetId::from_str("USDC")))
                .map(|b| (b.available, b.locked))
                .unwrap_or((0, 0));
            serde_json::json!({
                "subaccount_id": s.subaccount_id,
                "name": s.name,
                "user_id": format!("0x{}", hex::encode(s.user_id.0)),
                "usdc_available_micro": usdc_bal.0,
                "usdc_locked_micro": usdc_bal.1,
                "created_at_ms": s.created_at_ms,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "master": master.to_lowercase(),
            "subaccounts": with_balances,
        }))),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// MM credit vaults.
//
// LPs deposit USDC into an operator-managed vault; operator trades using
// the vault's balance plus the shared MM credit ratio; PnL streams
// pro-rata to LP shares. The operator is authorized to sign orders
// on the vault's behalf via the existing agent-wallet path (a delegation
// from vault.user_id → operator is registered at vault creation).
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct CreateVaultBody {
    /// Human-readable vault name.
    name: String,
    /// Operator address (Ethereum hex). Authorised to submit orders
    /// using the vault's derived user_id via agent-wallet path.
    operator: String,
    /// Per-vault credit ratio the credit system will apply. Defaults
    /// to 5× if omitted; capped at 10× at accept time.
    #[serde(default)]
    credit_ratio: Option<f64>,
    /// Delegation validity window in ms from now.
    /// Defaults to 10 years; operator can refresh by re-registering.
    #[serde(default)]
    delegation_ttl_ms: Option<u64>,
    /// Operator's signature over `vela:vault:create:{name}:{operator}:{nonce}`.
    nonce: u64,
    signature: String,
}

fn vault_create_signing_message(name: &str, operator: &str, nonce: u64) -> Vec<u8> {
    format!("vela:vault:create:{}:{}:{}", name, operator, nonce).into_bytes()
}

async fn create_vault(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateVaultBody>,
) -> impl IntoResponse {
    let operator = match UserId::from_hex(&body.operator) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid operator address")),
            )
                .into_response()
        }
    };
    let msg = vault_create_signing_message(&body.name, &body.operator, body.nonce);
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.operator.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "operator signature did not match payload",
            )),
        )
            .into_response();
    }

    let credit_ratio = body.credit_ratio.unwrap_or(5.0).clamp(1.0, 10.0);

    let vault_id = crate::vaults::next_vault_id();
    let user_id = crate::vaults::derive_vault_user_id(vault_id);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    // Register the operator as an agent for the vault's derived
    // user_id. This lets operator submit orders with `user = vault.user_id`
    // and pass sig check via the agent path — reusing all the machinery
    // we already ship, no new sig plumbing.
    let ttl = body
        .delegation_ttl_ms
        .unwrap_or(10 * 365 * 24 * 60 * 60 * 1_000);
    state.agents.register(crate::agents::AgentDelegation {
        master: user_id.clone(),
        agent: operator.clone(),
        expires_at_ms: now_ms + ttl,
        // Unlimited notional per order — the vault operator needs full
        // deployment of AUM, and the credit_system.check_credit path
        // already enforces the vault's per-order collateral bounds.
        max_notional_per_order: u64::MAX,
        revoked: false,
        nonce: body.nonce,
        scope: crate::agents::CapabilityScope::default(),
    });

    // Set the vault's credit ratio in the credit system.
    {
        let mut engine = state.engine.lock().await;
        engine.set_credit_ratio(user_id.clone(), credit_ratio);
    }

    let vault = crate::vaults::Vault {
        vault_id,
        name: body.name.clone(),
        operator: body.operator.to_lowercase(),
        user_id: user_id.clone(),
        credit_ratio,
        total_shares_micro: 0,
        cumulative_deposits_micro: 0,
        cumulative_withdrawals_micro: 0,
        created_at_ms: now_ms,
    };
    state.vaults.vaults.insert(vault_id, vault.clone());

    (StatusCode::OK, Json(ApiResponse::ok(vault))).into_response()
}

async fn list_vaults(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Snapshot AUM per vault for the response so LPs can see current
    // aum and share pricing without a second round-trip.
    let mut out: Vec<serde_json::Value> = Vec::new();
    let vaults: Vec<crate::vaults::Vault> = state
        .vaults
        .vaults
        .iter()
        .map(|v| v.value().clone())
        .collect();

    let us = state.shards.user_state.read().await;
    for v in vaults {
        let aum = us
            .balances
            .get(&(v.user_id.clone(), types::AssetId::from_str("USDC")))
            .map(|b| b.available.saturating_add(b.locked))
            .unwrap_or(0);
        out.push(serde_json::json!({
            "vault_id": v.vault_id,
            "name": v.name,
            "operator": v.operator,
            "user_id": format!("0x{}", hex::encode(v.user_id.0)),
            "credit_ratio": v.credit_ratio,
            "aum_usdc_micro": aum,
            "total_shares_micro": v.total_shares_micro.to_string(),
            "cumulative_deposits_micro": v.cumulative_deposits_micro.to_string(),
            "cumulative_withdrawals_micro": v.cumulative_withdrawals_micro.to_string(),
            "created_at_ms": v.created_at_ms,
        }));
    }
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({ "vaults": out }))),
    )
        .into_response()
}

async fn get_vault(
    Path(vault_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let vault = match state.vaults.vaults.get(&vault_id) {
        Some(v) => v.value().clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("vault_id not found")),
            )
                .into_response()
        }
    };
    let us = state.shards.user_state.read().await;
    let aum = us
        .balances
        .get(&(vault.user_id.clone(), types::AssetId::from_str("USDC")))
        .map(|b| b.available.saturating_add(b.locked))
        .unwrap_or(0);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "vault": vault,
            "aum_usdc_micro": aum,
            "share_price_micro": if vault.total_shares_micro == 0 {
                "1000000".to_string() // 1.0 baseline
            } else {
                (aum as u128 * 1_000_000 / vault.total_shares_micro).to_string()
            },
        }))),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct VaultDepositBody {
    /// LP address that debits USDC and credits shares.
    lp: String,
    /// USDC × 1e6 to deposit.
    amount_micro: u64,
    nonce: u64,
    /// Signature by LP over
    /// `vela:vault:deposit:{vault_id}:{amount_micro}:{nonce}`.
    signature: String,
}

fn vault_deposit_signing_message(vault_id: u64, amount_micro: u64, nonce: u64) -> Vec<u8> {
    format!("vela:vault:deposit:{}:{}:{}", vault_id, amount_micro, nonce).into_bytes()
}

async fn vault_deposit(
    Path(vault_id): Path<u64>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<VaultDepositBody>,
) -> impl IntoResponse {
    let lp = match UserId::from_hex(&body.lp) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid lp address")),
            )
                .into_response()
        }
    };
    let msg = vault_deposit_signing_message(vault_id, body.amount_micro, body.nonce);
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.lp.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("lp signature did not match payload")),
        )
            .into_response();
    }
    if body.amount_micro == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("amount_micro must be > 0")),
        )
            .into_response();
    }

    let vault_user_id = match state.vaults.vaults.get(&vault_id) {
        Some(v) => v.value().user_id.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("vault_id not found")),
            )
                .into_response()
        }
    };

    // Move USDC from LP.available → vault.available, issue shares.
    let (aum_before, total_shares_before) = {
        let mut us = state.shards.user_state.write().await;
        let lp_key = (lp.clone(), types::AssetId::from_str("USDC"));
        let vault_key = (vault_user_id.clone(), types::AssetId::from_str("USDC"));

        let lp_bal = us.balances.entry(lp_key).or_insert_with(|| types::Balance {
            user: lp.clone(),
            asset: types::AssetId::from_str("USDC"),
            available: 0,
            locked: 0,
        });
        if lp_bal.available < body.amount_micro {
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(ApiResponse::<()>::err(format!(
                    "lp needs {} USDC micro available; has {}",
                    body.amount_micro, lp_bal.available
                ))),
            )
                .into_response();
        }
        lp_bal.available -= body.amount_micro;

        let vault_bal = us
            .balances
            .entry(vault_key)
            .or_insert_with(|| types::Balance {
                user: vault_user_id.clone(),
                asset: types::AssetId::from_str("USDC"),
                available: 0,
                locked: 0,
            });
        let aum_before = vault_bal.available.saturating_add(vault_bal.locked);
        vault_bal.available += body.amount_micro;

        let vault = state.vaults.vaults.get(&vault_id).unwrap();
        (aum_before, vault.total_shares_micro)
    };

    let shares_issued =
        crate::vaults::shares_for_deposit(body.amount_micro, aum_before, total_shares_before);

    {
        let mut v = state.vaults.vaults.get_mut(&vault_id).unwrap();
        v.total_shares_micro += shares_issued;
        v.cumulative_deposits_micro += body.amount_micro as u128;
    }
    {
        let lp_key = (vault_id, body.lp.to_lowercase());
        let mut pos = state.vaults.positions.entry(lp_key).or_default();
        pos.shares_micro += shares_issued;
        pos.cumulative_deposits_micro += body.amount_micro as u128;
    }

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "vault_id": vault_id,
            "lp": body.lp.to_lowercase(),
            "amount_deposited_micro": body.amount_micro,
            "shares_issued_micro": shares_issued.to_string(),
        }))),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct VaultWithdrawBody {
    lp: String,
    shares_micro: String, // u128 as string to survive JSON
    nonce: u64,
    signature: String,
}

fn vault_withdraw_signing_message(vault_id: u64, shares_str: &str, nonce: u64) -> Vec<u8> {
    format!("vela:vault:withdraw:{}:{}:{}", vault_id, shares_str, nonce).into_bytes()
}

async fn vault_withdraw(
    Path(vault_id): Path<u64>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<VaultWithdrawBody>,
) -> impl IntoResponse {
    let lp = match UserId::from_hex(&body.lp) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid lp address")),
            )
                .into_response()
        }
    };
    let msg = vault_withdraw_signing_message(vault_id, &body.shares_micro, body.nonce);
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.lp.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("lp signature did not match payload")),
        )
            .into_response();
    }
    let shares_to_burn: u128 = match body.shares_micro.parse() {
        Ok(v) if v > 0 => v,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(
                    "shares_micro must be a positive integer",
                )),
            )
                .into_response();
        }
    };

    let vault_user_id = match state.vaults.vaults.get(&vault_id) {
        Some(v) => v.value().user_id.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("vault_id not found")),
            )
                .into_response()
        }
    };

    let lp_key = (vault_id, body.lp.to_lowercase());
    {
        let pos = state.vaults.positions.get(&lp_key);
        let shares_available = pos.map(|p| p.shares_micro).unwrap_or(0);
        if shares_available < shares_to_burn {
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(ApiResponse::<()>::err(format!(
                    "lp has {} shares_micro, requested {}",
                    shares_available, shares_to_burn
                ))),
            )
                .into_response();
        }
    }

    // Compute payout under a write lock so AUM and shares snapshot
    // together, and vault.available >= payout is atomically true.
    let payout = {
        let mut us = state.shards.user_state.write().await;
        let vault_key = (vault_user_id.clone(), types::AssetId::from_str("USDC"));
        let vault_bal = us
            .balances
            .entry(vault_key)
            .or_insert_with(|| types::Balance {
                user: vault_user_id.clone(),
                asset: types::AssetId::from_str("USDC"),
                available: 0,
                locked: 0,
            });
        let total_shares = state
            .vaults
            .vaults
            .get(&vault_id)
            .unwrap()
            .total_shares_micro;
        let aum = vault_bal.available.saturating_add(vault_bal.locked);
        let payout = crate::vaults::usdc_for_shares(shares_to_burn, aum, total_shares);

        if vault_bal.available < payout {
            return (
                StatusCode::CONFLICT,
                Json(ApiResponse::<()>::err(format!(
                    "vault has {} USDC available but withdrawal requires {} (locked in open positions)",
                    vault_bal.available, payout
                ))),
            )
                .into_response();
        }
        vault_bal.available -= payout;

        let lp_bal_key = (lp.clone(), types::AssetId::from_str("USDC"));
        let lp_bal = us
            .balances
            .entry(lp_bal_key)
            .or_insert_with(|| types::Balance {
                user: lp.clone(),
                asset: types::AssetId::from_str("USDC"),
                available: 0,
                locked: 0,
            });
        lp_bal.available += payout;

        payout
    };

    {
        let mut v = state.vaults.vaults.get_mut(&vault_id).unwrap();
        v.total_shares_micro -= shares_to_burn;
        v.cumulative_withdrawals_micro += payout as u128;
    }
    {
        let mut pos = state.vaults.positions.get_mut(&lp_key).unwrap();
        pos.shares_micro -= shares_to_burn;
        pos.cumulative_withdrawals_micro += payout as u128;
    }

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "vault_id": vault_id,
            "lp": body.lp.to_lowercase(),
            "shares_burned_micro": shares_to_burn.to_string(),
            "usdc_paid_out_micro": payout,
        }))),
    )
        .into_response()
}

async fn get_lp_position(
    Path((vault_id, lp)): Path<(u64, String)>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let key = (vault_id, lp.to_lowercase());
    let pos = state
        .vaults
        .positions
        .get(&key)
        .map(|p| p.value().clone())
        .unwrap_or_default();
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "vault_id": vault_id,
            "lp": lp.to_lowercase(),
            "shares_micro": pos.shares_micro.to_string(),
            "cumulative_deposits_micro": pos.cumulative_deposits_micro.to_string(),
            "cumulative_withdrawals_micro": pos.cumulative_withdrawals_micro.to_string(),
        }))),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Permissionless market listing.
//
// Anyone posts a bond in USDC + market spec. Proposal enters a challenge
// window (24h default). Operator or governance can reject and slash the
// bond; otherwise the market auto-registers when the window elapses and
// the bond is refunded to the proposer's available USDC balance.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ProposeListingBody {
    proposer: String,
    /// Format: "BASE-QUOTE" (must be USDC for v1).
    market_id: String,
    base: String,
    quote: String,
    max_orders: usize,
    min_order_size: u64,
    price_tick: u64,
    quantity_tick: u64,
    nonce: u64,
    signature: String,
}

fn listing_signing_message(market_id: &str, base: &str, quote: &str, nonce: u64) -> Vec<u8> {
    format!(
        "vela:listing:propose:{}:{}:{}:{}",
        market_id, base, quote, nonce
    )
    .into_bytes()
}

async fn propose_listing(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProposeListingBody>,
) -> impl IntoResponse {
    let proposer = match UserId::from_hex(&body.proposer) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid proposer address")),
            )
                .into_response()
        }
    };

    // Signature check: proposer authorises the listing.
    let msg = listing_signing_message(&body.market_id, &body.base, &body.quote, body.nonce);
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.proposer.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "proposer signature did not match payload",
            )),
        )
            .into_response();
    }

    // Basic sanity on the market spec.
    if body.quote != "USDC" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("quote must be USDC in v1")),
        )
            .into_response();
    }
    if body.max_orders == 0
        || body.min_order_size == 0
        || body.price_tick == 0
        || body.quantity_tick == 0
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "max_orders / min_order_size / price_tick / quantity_tick must be > 0",
            )),
        )
            .into_response();
    }
    if body.market_id != format!("{}-{}", body.base, body.quote) {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "market_id must equal '{base}-{quote}'",
            )),
        )
            .into_response();
    }

    // Reject duplicate market_id (existing market or pending listing).
    {
        let engine = state.engine.lock().await;
        if engine
            .markets
            .contains_key(&MarketId(body.market_id.clone()))
        {
            return (
                StatusCode::CONFLICT,
                Json(ApiResponse::<()>::err("market already exists")),
            )
                .into_response();
        }
    }
    let dup = state.listings.iter().any(|l| {
        l.market_id == body.market_id
            && matches!(
                l.status,
                crate::listings::ListingStatus::Pending | crate::listings::ListingStatus::Accepted
            )
    });
    if dup {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err(
                "market_id already has a pending or accepted proposal",
            )),
        )
            .into_response();
    }

    // Debit bond from proposer's USDC balance.
    let bond = crate::listings::bond_amount_micro();
    {
        let mut us = state.shards.user_state.write().await;
        let key = (proposer.clone(), AssetId::from_str("USDC"));
        let bal = us
            .balances
            .entry(key.clone())
            .or_insert_with(|| types::Balance {
                user: proposer.clone(),
                asset: AssetId::from_str("USDC"),
                available: 0,
                locked: 0,
            });
        if bal.available < bond {
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(ApiResponse::<()>::err(format!(
                    "proposer needs at least {} USDC micro available; has {}",
                    bond, bal.available
                ))),
            )
                .into_response();
        }
        bal.available -= bond;
        bal.locked += bond;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let challenge_ms = crate::listings::challenge_hours() * 3_600_000;

    let listing_id = crate::listings::next_listing_id();
    let proposal = crate::listings::ListingProposal {
        listing_id,
        proposer: body.proposer.to_lowercase(),
        market_id: body.market_id.clone(),
        base: body.base.clone(),
        quote: body.quote.clone(),
        max_orders: body.max_orders,
        min_order_size: body.min_order_size,
        price_tick: body.price_tick,
        quantity_tick: body.quantity_tick,
        bond_micro: bond,
        proposed_at_ms: now_ms,
        challenge_deadline_ms: now_ms + challenge_ms,
        status: crate::listings::ListingStatus::Pending,
        reject_reason: None,
    };
    state.listings.insert(listing_id, proposal.clone());

    (StatusCode::OK, Json(ApiResponse::ok(proposal))).into_response()
}

async fn list_listings(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut all: Vec<crate::listings::ListingProposal> = state
        .listings
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    all.sort_by_key(|l| std::cmp::Reverse(l.proposed_at_ms));
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "listings": all,
            "bond_micro": crate::listings::bond_amount_micro(),
            "challenge_hours": crate::listings::challenge_hours(),
        }))),
    )
        .into_response()
}

async fn get_listing(
    Path(listing_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match state.listings.get(&listing_id) {
        Some(p) => (StatusCode::OK, Json(ApiResponse::ok(p.value().clone()))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("listing_id not found")),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct RejectListingBody {
    listing_id: u64,
    reason: String,
}

async fn admin_reject_listing(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<RejectListingBody>,
) -> impl IntoResponse {
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
    }

    let (proposer_hex, bond_micro) = match state.listings.get_mut(&body.listing_id) {
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("listing_id not found")),
            )
                .into_response()
        }
        Some(mut p) => {
            if !matches!(p.status, crate::listings::ListingStatus::Pending) {
                return (
                    StatusCode::CONFLICT,
                    Json(ApiResponse::<()>::err("listing is not pending")),
                )
                    .into_response();
            }
            p.status = crate::listings::ListingStatus::Rejected;
            p.reject_reason = Some(body.reason);
            (p.proposer.clone(), p.bond_micro)
        }
    };

    // Slash the bond: locked → fee_balances["USDC"] (operator take).
    let proposer_id = match UserId::from_hex(&proposer_hex) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err(
                    "proposer address unparseable — bond stuck",
                )),
            )
                .into_response();
        }
    };
    {
        let mut us = state.shards.user_state.write().await;
        let key = (proposer_id, AssetId::from_str("USDC"));
        if let Some(bal) = us.balances.get_mut(&key) {
            let slash = bond_micro.min(bal.locked);
            bal.locked -= slash;
            *us.fee_balances.entry("USDC".to_string()).or_insert(0) += slash;
        }
    }

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "listing_id": body.listing_id,
            "status": "rejected",
            "bond_slashed_micro": bond_micro,
        }))),
    )
        .into_response()
}

/// Long-running task: every minute, walk pending listings whose
/// challenge window has expired, add the market to the engine, refund
/// the bond, and flip status to Accepted.
pub async fn run_listing_task(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    ticker.tick().await; // Skip immediate first tick.

    loop {
        ticker.tick().await;

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let ready: Vec<crate::listings::ListingProposal> = state
            .listings
            .iter()
            .filter(|l| {
                matches!(l.status, crate::listings::ListingStatus::Pending)
                    && l.challenge_deadline_ms <= now_ms
            })
            .map(|l| l.value().clone())
            .collect();

        for p in ready {
            // Add market to the engine.
            let market = types::Market {
                id: MarketId(p.market_id.clone()),
                base: AssetId::from_str(&p.base),
                quote: AssetId::from_str(&p.quote),
                max_orders: p.max_orders,
                min_order_size: p.min_order_size,
                price_tick: p.price_tick,
                quantity_tick: p.quantity_tick,
                maker_fee_bps: -1,
                taker_fee_bps: 5,
            };
            {
                let mut engine = state.engine.lock().await;
                engine.add_market(market.clone());
            }
            {
                let mut us = state.shards.user_state.write().await;
                us.add_market(market);
            }

            // Refund bond: locked → available on proposer's USDC.
            if let Ok(proposer_id) = UserId::from_hex(&p.proposer) {
                let mut us = state.shards.user_state.write().await;
                let key = (proposer_id, AssetId::from_str("USDC"));
                if let Some(bal) = us.balances.get_mut(&key) {
                    let refund = p.bond_micro.min(bal.locked);
                    bal.locked -= refund;
                    bal.available += refund;
                }
            }

            // Flip status. Update happens under DashMap's fine-grained lock.
            if let Some(mut entry) = state.listings.get_mut(&p.listing_id) {
                entry.status = crate::listings::ListingStatus::Accepted;
            }

            tracing::info!(
                "permissionless listing {} auto-accepted: {}",
                p.listing_id,
                p.market_id
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Server-side execution algos (TWAP for now; more to follow).
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct TwapAlgoBody {
    address: String,
    market: String,
    side: types::OrderSide,
    /// Total base-asset quantity to work, fixed-point 1e6.
    quantity: u64,
    /// How long to spread execution over.
    duration_secs: u64,
    /// Optional worst-price bound (fixed-point 1e6). Slices execute IOC
    /// against this bound; miss on any slice is left unfilled.
    #[serde(default)]
    price_limit: Option<u64>,
    /// Number of equal slices. Defaults to 12. Capped at 240 to avoid
    /// pathological configurations that saturate the child-order path.
    #[serde(default)]
    slices: Option<u32>,
    /// Nonce for the delegation signature that authorizes this algo run.
    /// The client signs `vela:algo:twap:{market}:{qty}:{duration}:{nonce}`
    /// with their master or an authorized agent key.
    nonce: u64,
    signature: String,
}

fn twap_signing_message(market: &str, quantity: u64, duration_secs: u64, nonce: u64) -> Vec<u8> {
    format!(
        "vela:algo:twap:{}:{}:{}:{}",
        market, quantity, duration_secs, nonce
    )
    .into_bytes()
}

async fn post_twap_algo(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TwapAlgoBody>,
) -> impl IntoResponse {
    let user = match UserId::from_hex(&body.address) {
        Ok(u) => u,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid address")),
            )
                .into_response()
        }
    };
    if body.quantity == 0 || body.duration_secs == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "quantity and duration_secs must be > 0",
            )),
        )
            .into_response();
    }
    let slices = body.slices.unwrap_or(12).clamp(1, 240);
    if body.quantity < slices as u64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "quantity must be >= slices (need at least 1 unit per slice)",
            )),
        )
            .into_response();
    }

    // Authorize via master-or-agent signature. TWAP counts as a single
    // authorization event; individual child orders bypass sig verify
    // (they're synthesized internally and inherit this authorization).
    // Notional cap enforced against total quantity.
    let notional_micro = match body.side {
        types::OrderSide::Bid => body
            .price_limit
            .and_then(|p| p.checked_mul(body.quantity))
            .map(|n| n / 1_000_000)
            .unwrap_or(u64::MAX),
        types::OrderSide::Ask => body.quantity,
    };
    let msg = twap_signing_message(&body.market, body.quantity, body.duration_secs, body.nonce);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if crate::agents::verify_master_or_agent_async(
        msg,
        body.signature.clone(),
        body.address.clone(),
        notional_micro,
        now_ms,
        Arc::clone(&state.agents),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("invalid signature")),
        )
            .into_response();
    }

    // Reserve a nonce block for child orders — starts one above the
    // client's supplied nonce so master↔child nonces don't collide.
    let parent_id = crate::algos::next_parent_id();
    let parent = std::sync::Arc::new(crate::algos::TwapParent {
        parent_id,
        user_address: body.address.to_lowercase(),
        market: body.market.clone(),
        side: body.side,
        total_quantity: body.quantity,
        filled_quantity: std::sync::atomic::AtomicU64::new(0),
        price_limit: body.price_limit,
        duration_secs: body.duration_secs,
        slices,
        started_at_ms: now_ms,
        status: std::sync::Mutex::new(crate::algos::AlgoStatus::Running),
        cancel_flag: std::sync::atomic::AtomicBool::new(false),
        child_nonce_base: body.nonce.saturating_add(1),
        _phantom_signature: (),
    });

    state
        .algos
        .insert(parent_id, std::sync::Arc::clone(&parent));
    tokio::spawn(crate::algos::run_twap_task(
        Arc::clone(&state),
        std::sync::Arc::clone(&parent),
    ));

    let snapshot = crate::algos::TwapParentSnapshot::from(&parent);
    let user_hex = user.to_hex();
    let _ = user_hex;
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "parent_id": parent_id,
            "status": snapshot.status,
            "slice_count": snapshot.slices,
            "note": "TWAP running; poll GET /orders/algo/{parent_id} for progress.",
        }))),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct CancelAlgoBody {
    parent_id: u64,
    address: String,
    /// Signature over `vela:algo:cancel:{parent_id}:{nonce}` by master
    /// or agent.
    nonce: u64,
    signature: String,
}

async fn cancel_algo(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CancelAlgoBody>,
) -> impl IntoResponse {
    let parent = match state.algos.get(&body.parent_id) {
        Some(p) => Arc::clone(&*p),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("parent_id not found")),
            )
                .into_response()
        }
    };
    if parent.user_address != body.address.to_lowercase() {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::<()>::err("not the owner of this algo")),
        )
            .into_response();
    }
    let msg = format!("vela:algo:cancel:{}:{}", body.parent_id, body.nonce);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if crate::agents::verify_master_or_agent_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.address.clone(),
        0,
        now_ms,
        Arc::clone(&state.agents),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("invalid signature")),
        )
            .into_response();
    }

    parent
        .cancel_flag
        .store(true, std::sync::atomic::Ordering::Relaxed);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "parent_id": body.parent_id,
            "canceled": true,
        }))),
    )
        .into_response()
}

async fn get_algo_status(
    Path(parent_id): Path<u64>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match state.algos.get(&parent_id) {
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("parent_id not found")),
        )
            .into_response(),
        Some(p) => {
            let snap = crate::algos::TwapParentSnapshot::from(&p);
            (StatusCode::OK, Json(ApiResponse::ok(snap))).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Session keys / agent wallets.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct AgentRegisterBody {
    /// Master wallet delegating trading authority.
    master: String,
    /// Ephemeral agent wallet the master is authorizing.
    agent: String,
    /// Unix ms after which the delegation is no longer valid.
    expires_at_ms: u64,
    /// USDC × 1e6 cap per individual order this agent may submit.
    max_notional_per_order: u64,
    /// Registration nonce (dedupes replays of the same signed message).
    nonce: u64,
    /// 65-byte ECDSA signature by `master` over
    /// `vela:agent:register:0x{agent}:{expires_at_ms}:{max_notional}:{nonce}:{scope_hash}`.
    signature: String,
    /// Optional capability grammar: allow-listed markets, order types,
    /// sides, and rolling notional caps. Omit for a permissive (v1-style)
    /// delegation that only enforces `max_notional_per_order` + expiry.
    #[serde(default)]
    scope: Option<crate::agents::CapabilityScope>,
}

async fn agents_register(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AgentRegisterBody>,
) -> impl IntoResponse {
    let agent_id = match UserId::from_hex(&body.agent) {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid agent address")),
            )
                .into_response()
        }
    };
    let master_id = match UserId::from_hex(&body.master) {
        Ok(m) => m,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid master address")),
            )
                .into_response()
        }
    };

    let scope = body.scope.clone().unwrap_or_default();
    let msg = crate::agents::delegation_signing_message(
        &agent_id,
        body.expires_at_ms,
        body.max_notional_per_order,
        body.nonce,
        &scope,
    );
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.master.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "master signature did not match delegation payload",
            )),
        )
            .into_response();
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    if body.expires_at_ms <= now_ms {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "expires_at_ms must be in the future",
            )),
        )
            .into_response();
    }
    if body.max_notional_per_order == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("max_notional_per_order must be > 0")),
        )
            .into_response();
    }

    state.agents.register(crate::agents::AgentDelegation {
        master: master_id,
        agent: agent_id,
        expires_at_ms: body.expires_at_ms,
        max_notional_per_order: body.max_notional_per_order,
        revoked: false,
        nonce: body.nonce,
        scope,
    });

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "agent": body.agent.to_lowercase(),
            "master": body.master.to_lowercase(),
            "expires_at_ms": body.expires_at_ms,
        }))),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct AgentRevokeBody {
    master: String,
    agent: String,
    nonce: u64,
    /// 65-byte ECDSA signature by `master` over
    /// `vela:agent:revoke:0x{agent}:{nonce}`.
    signature: String,
}

async fn agents_revoke(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AgentRevokeBody>,
) -> impl IntoResponse {
    let agent_id = match UserId::from_hex(&body.agent) {
        Ok(a) => a,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid agent address")),
            )
                .into_response()
        }
    };

    let msg = crate::agents::revocation_signing_message(&agent_id, body.nonce);
    if crate::auth::verify_matches_async(msg, body.signature.clone(), body.master.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err(
                "master signature did not match revocation payload",
            )),
        )
            .into_response();
    }

    let removed = state.agents.revoke(&agent_id);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "agent": body.agent.to_lowercase(),
            "revoked": removed,
        }))),
    )
        .into_response()
}

async fn agents_list(
    Path(master): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let master_id = match UserId::from_hex(&master) {
        Ok(m) => m,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("invalid master address")),
            )
                .into_response()
        }
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let agents: Vec<serde_json::Value> = state
        .agents
        .agents_for(&master_id)
        .into_iter()
        .map(|d| {
            let active = !d.revoked && d.expires_at_ms > now_ms;
            serde_json::json!({
                "agent": format!("0x{}", hex::encode(d.agent.0)),
                "expires_at_ms": d.expires_at_ms,
                "max_notional_per_order": d.max_notional_per_order,
                "revoked": d.revoked,
                "active": active,
                "nonce": d.nonce,
            })
        })
        .collect();

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "master": master.to_lowercase(),
            "agents": agents,
        }))),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Agent-flow toxicity tier (Tier 3.5).
// ---------------------------------------------------------------------------

async fn agent_tier_handler(
    Path(master): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let addr = master.to_ascii_lowercase();
    let tc = crate::agent_tox::compute_tier(&state, &addr).await;
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "address": addr,
            "tier": tc.tier.as_str(),
            "avg_toxicity_30d": format!("{:.4}", tc.avg_toxicity),
            "taker_fill_count_30d": tc.taker_fill_count,
            "amber_threshold": crate::agent_tox::amber_threshold(),
            "red_threshold": crate::agent_tox::red_threshold(),
            "cleared_until_ms": tc.cleared_until_ms,
        }))),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct ClearAgentTierBody {
    address: String,
    /// Unix ms until which the address is treated as green regardless
    /// of raw toxicity score. Default: 7 days from now if omitted.
    #[serde(default)]
    cleared_until_ms: Option<u64>,
    /// Free-text reason for the audit log.
    #[serde(default)]
    reason: Option<String>,
}

async fn admin_clear_agent_tier(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<ClearAgentTierBody>,
) -> impl IntoResponse {
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
    }
    let addr = body.address.to_ascii_lowercase();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let default_until = now_ms + 7 * 24 * 60 * 60 * 1_000;
    let until = body.cleared_until_ms.unwrap_or(default_until);
    state.agent_tier_clears.insert(addr.clone(), until);

    tracing::info!(
        "agent tier cleared: addr={} until_ms={} reason={:?}",
        addr,
        until,
        body.reason
    );

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "address": addr,
            "cleared_until_ms": until,
            "reason": body.reason,
        }))),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Portfolio dashboard — realized/unrealized PnL, FIFO/HIFO cost basis, CSV.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CostBasisMethod {
    Fifo,
    Hifo,
}

impl CostBasisMethod {
    fn parse(s: Option<&str>) -> Self {
        match s.map(|v| v.to_ascii_lowercase()) {
            Some(v) if v == "hifo" => Self::Hifo,
            _ => Self::Fifo,
        }
    }
    fn as_str(&self) -> &'static str {
        match self {
            Self::Fifo => "FIFO",
            Self::Hifo => "HIFO",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Lot {
    /// Base quantity in fixed-point 1e6.
    quantity: u64,
    /// Price per unit of base, fixed-point 1e6.
    price: u64,
    /// Retained for future FIFO-with-holding-period reporting (tax
    /// long-term vs short-term). Not consumed by the v1 matcher.
    #[allow(dead_code)]
    timestamp: u64,
}

/// Split a market id like "BTC-USDC" into `(base, quote)`. Returns `None`
/// on malformed ids.
fn split_market(market_id: &str) -> Option<(&str, &str)> {
    market_id.split_once('-')
}

/// Notional in USDC as f64, from fixed-point price × quantity (each 1e6).
fn notional_from_fixed(price: u64, quantity: u64) -> f64 {
    (price as f64 * quantity as f64) / 1_000_000_000_000.0
}

#[derive(Debug, Default)]
struct MarketPnl {
    market_id: String,
    base_asset: String,
    /// Realized PnL in USDC.
    realized_usdc: f64,
    /// Base held right now (sum(buys) − sum(sells)).
    position_base: f64,
    /// Weighted-average cost basis per base unit for the current position.
    cost_basis_price: f64,
    /// Sum of remaining lots' quantity × price / QUANTITY_SCALE, in USDC.
    open_cost_basis_usdc: f64,
    /// If we have a mark price, current value of the open position in USDC.
    mark_price_usdc: Option<f64>,
    /// Unrealized PnL = current_value − open_cost_basis_usdc.
    unrealized_usdc: Option<f64>,
    fill_count: u64,
}

/// Compute per-market PnL for `address` using the chosen cost-basis method.
///
/// For each fill in the market where the address participated, apply
/// buy/sell to a per-market lot queue. Realized PnL accumulates when a
/// sell consumes buy lots (or vice versa if the user was short — v1
/// treats short exposure symmetrically).
fn compute_market_pnl(
    address: &str,
    market_id: &str,
    fills: &[&StoredFill],
    mark_price_fp: Option<u64>,
    method: CostBasisMethod,
) -> MarketPnl {
    let addr = address.to_ascii_lowercase();
    let (base, _quote) = match split_market(market_id) {
        Some(p) => p,
        None => (market_id, "USDC"),
    };

    // Sorted-by-timestamp list of (is_buy_from_user_perspective, price, qty)
    let mut trades: Vec<(bool, u64, u64, u64)> = Vec::new();
    for f in fills {
        let is_maker = f.maker_address.to_ascii_lowercase() == addr;
        let is_taker = f.taker_address.to_ascii_lowercase() == addr;
        if !is_maker && !is_taker {
            continue;
        }
        // `fill.side` is the taker's side. If user is taker and side is bid,
        // user bought base. If user is maker and taker was bidding, user
        // sold base (the maker was on the ask side).
        let taker_was_bidding = f.side.eq_ignore_ascii_case("bid");
        let user_bought = (is_taker && taker_was_bidding) || (is_maker && !taker_was_bidding);
        trades.push((user_bought, f.price, f.quantity, f.timestamp));
    }
    trades.sort_by_key(|(_, _, _, ts)| *ts);

    let mut lots: std::collections::VecDeque<Lot> = std::collections::VecDeque::new();
    let mut realized_usdc: f64 = 0.0;
    let fill_count = trades.len() as u64;

    for (buy, price, qty, ts) in trades {
        if buy {
            lots.push_back(Lot {
                quantity: qty,
                price,
                timestamp: ts,
            });
        } else {
            // Sell: draw from lots according to method until qty exhausted.
            let mut remaining = qty;
            while remaining > 0 && !lots.is_empty() {
                let idx = match method {
                    CostBasisMethod::Fifo => 0,
                    CostBasisMethod::Hifo => lots
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, l)| l.price)
                        .map(|(i, _)| i)
                        .unwrap_or(0),
                };
                let mut lot = lots.remove(idx).unwrap();
                let matched = lot.quantity.min(remaining);
                let pnl_usdc =
                    ((price as f64 - lot.price as f64) * matched as f64) / 1_000_000_000_000.0;
                realized_usdc += pnl_usdc;
                lot.quantity -= matched;
                remaining -= matched;
                if lot.quantity > 0 {
                    // Push back in original position (front for FIFO, arbitrary
                    // for HIFO since it re-picks the max next iteration).
                    lots.insert(idx, lot);
                }
            }
            // If `remaining > 0` after exhausting lots, the user went short
            // (or opened a short). v1: silently ignore — treated as a fresh
            // negative position. Realized PnL on that portion accrues on the
            // eventual buy-to-cover.
        }
    }

    // Compute open position from remaining lots.
    let (open_qty_fp, open_notional_usdc) = lots.iter().fold((0u128, 0.0), |(q, n), l| {
        (
            q + l.quantity as u128,
            n + notional_from_fixed(l.price, l.quantity),
        )
    });
    let open_qty_f = open_qty_fp as f64 / 1_000_000.0;

    let cost_basis_price = if open_qty_fp > 0 {
        (open_notional_usdc / open_qty_f).max(0.0)
    } else {
        0.0
    };

    let mark_price_usdc = mark_price_fp.map(|p| p as f64 / 1_000_000.0);
    let unrealized_usdc = mark_price_usdc.map(|mp| open_qty_f * mp - open_notional_usdc);

    MarketPnl {
        market_id: market_id.to_string(),
        base_asset: base.to_string(),
        realized_usdc,
        position_base: open_qty_f,
        cost_basis_price,
        open_cost_basis_usdc: open_notional_usdc,
        mark_price_usdc,
        unrealized_usdc,
        fill_count,
    }
}

/// Return the current mark price (best_bid+best_ask midpoint) per market
/// as a `HashMap<market_id, mid_price_fp>`. Falls back to best_bid or
/// best_ask if one side is empty. `None` in the map if both sides empty.
async fn mark_prices(state: &AppState) -> std::collections::HashMap<String, Option<u64>> {
    let engine = state.engine.lock().await;
    let mut out = std::collections::HashMap::new();
    for (mid, book) in &engine.order_books {
        let mid_price = match (book.best_bid(), book.best_ask()) {
            (Some(b), Some(a)) => Some((b + a) / 2),
            (Some(b), None) => Some(b),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        out.insert(mid.0.clone(), mid_price);
    }
    out
}

#[derive(serde::Deserialize)]
struct PortfolioQuery {
    method: Option<String>,
}

async fn get_portfolio_handler(
    Path(address): Path<String>,
    Query(q): Query<PortfolioQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let addr = address.to_ascii_lowercase();
    let method = CostBasisMethod::parse(q.method.as_deref());

    let marks = mark_prices(&state).await;
    let fills = state.fills.lock().await;

    // Group user's fills by market_id.
    let mut per_market: std::collections::HashMap<String, Vec<&StoredFill>> =
        std::collections::HashMap::new();
    for f in fills.iter() {
        if f.maker_address.to_ascii_lowercase() == addr
            || f.taker_address.to_ascii_lowercase() == addr
        {
            per_market.entry(f.market_id.clone()).or_default().push(f);
        }
    }

    let mut markets: Vec<serde_json::Value> = per_market
        .into_iter()
        .map(|(mid, fs)| {
            let mark = marks.get(&mid).copied().flatten();
            let p = compute_market_pnl(&addr, &mid, &fs, mark, method);
            serde_json::json!({
                "market_id": p.market_id,
                "base_asset": p.base_asset,
                "position_base": format!("{:.6}", p.position_base),
                "cost_basis_price_usdc": format!("{:.6}", p.cost_basis_price),
                "open_cost_basis_usdc": format!("{:.6}", p.open_cost_basis_usdc),
                "mark_price_usdc": p.mark_price_usdc.map(|v| format!("{:.6}", v)),
                "realized_pnl_usdc": format!("{:.6}", p.realized_usdc),
                "unrealized_pnl_usdc": p.unrealized_usdc.map(|v| format!("{:.6}", v)),
                "fill_count": p.fill_count,
            })
        })
        .collect();
    markets.sort_by(|a, b| {
        a["market_id"]
            .as_str()
            .unwrap_or("")
            .cmp(b["market_id"].as_str().unwrap_or(""))
    });

    let total_realized: f64 = markets
        .iter()
        .filter_map(|m| m["realized_pnl_usdc"].as_str())
        .filter_map(|s| s.parse::<f64>().ok())
        .sum();
    let total_unrealized: f64 = markets
        .iter()
        .filter_map(|m| m["unrealized_pnl_usdc"].as_str())
        .filter_map(|s| s.parse::<f64>().ok())
        .sum();

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "address": addr,
            "cost_basis_method": method.as_str(),
            "total_realized_pnl_usdc": format!("{:.6}", total_realized),
            "total_unrealized_pnl_usdc": format!("{:.6}", total_unrealized),
            "total_pnl_usdc": format!("{:.6}", total_realized + total_unrealized),
            "markets": markets,
        }))),
    )
        .into_response()
}

async fn get_portfolio_csv_handler(
    Path(address): Path<String>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let addr = address.to_ascii_lowercase();
    let fills = state.fills.lock().await;

    let mut out = String::from(
        "timestamp_ms,market,side,price_usdc,quantity_base,notional_usdc,counterparty,fill_id\n",
    );
    // Sort user's fills by timestamp for stable, replayable CSV output.
    let mut user_fills: Vec<&StoredFill> = fills
        .iter()
        .filter(|f| {
            f.maker_address.to_ascii_lowercase() == addr
                || f.taker_address.to_ascii_lowercase() == addr
        })
        .collect();
    user_fills.sort_by_key(|f| f.timestamp);

    for f in user_fills {
        let is_taker = f.taker_address.to_ascii_lowercase() == addr;
        let taker_was_bidding = f.side.eq_ignore_ascii_case("bid");
        let user_bought = (is_taker && taker_was_bidding) || (!is_taker && !taker_was_bidding);
        let side = if user_bought { "buy" } else { "sell" };
        let counterparty = if is_taker {
            &f.maker_address
        } else {
            &f.taker_address
        };
        let price_usdc = f.price as f64 / 1_000_000.0;
        let qty_base = f.quantity as f64 / 1_000_000.0;
        let notional = notional_from_fixed(f.price, f.quantity);
        out.push_str(&format!(
            "{},{},{},{:.6},{:.6},{:.6},{},{}\n",
            f.timestamp, f.market_id, side, price_usdc, qty_base, notional, counterparty, f.id
        ));
    }

    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=vela-fills.csv",
            ),
        ],
        out,
    )
        .into_response()
}

async fn get_leaderboard(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Points and volume leaderboards both roll off the same 30-day window
    // computed from `state.fills`. Points is the primary ranking now;
    // volume stays exposed for consumers who want the raw number.
    let by_addr = points_by_address(&state).await;

    let mut traders: Vec<serde_json::Value> = by_addr
        .iter()
        .map(|(addr, b)| {
            serde_json::json!({
                "address": addr,
                "total_points": format!("{:.2}", b.total()),
                "maker_points": format!("{:.2}", b.maker_points),
                "taker_points": format!("{:.2}", b.taker_points),
                "volume_usdc": format!("{:.2}", b.volume_usdc),
                "fill_count": b.maker_count + b.taker_count,
                "maker_count": b.maker_count,
                "taker_count": b.taker_count,
                "toxic_taker_count": b.toxic_taker_count,
            })
        })
        .collect();
    traders.sort_by(|a, b| {
        let pa: f64 = a["total_points"]
            .as_str()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        let pb: f64 = b["total_points"]
            .as_str()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
    });
    traders.truncate(20);

    let us = state.shards.user_state.read().await;
    let mut referrers: Vec<serde_json::Value> = us
        .metadata
        .iter()
        .filter(|(_, m)| !m.referred_users.is_empty() || m.ref_earnings > 0)
        .map(|(user, m)| {
            serde_json::json!({
                "address": user.to_hex(),
                "referred_count": m.referred_users.len(),
                "earnings_usdc": format!("{:.6}", m.ref_earnings as f64 / 1_000_000.0),
                "referral_points": format!("{:.2}",
                    (m.ref_earnings as f64 / 1_000_000.0) * POINTS_REFERRAL_MULTIPLIER),
            })
        })
        .collect();
    referrers.sort_by(|a, b| {
        let ra = a["referred_count"].as_u64().unwrap_or(0);
        let rb = b["referred_count"].as_u64().unwrap_or(0);
        rb.cmp(&ra)
    });
    referrers.truncate(10);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "top_traders": traders,
            "top_referrers": referrers,
            "period": "30d_rolling",
            "ranked_by": "total_points",
            "formula": {
                "maker_multiplier": POINTS_MAKER_MULTIPLIER,
                "referral_multiplier": POINTS_REFERRAL_MULTIPLIER,
                "taker_penalty": "notional * (1 - toxicity_score)",
            },
        }))),
    )
        .into_response()
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
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
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
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
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

    Json(ApiResponse::ok(
        serde_json::json!({ "market_makers": entries }),
    ))
}

async fn register_market_maker(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RegisterMMBody>,
) -> impl IntoResponse {
    if !body.address.starts_with("0x") || body.address.len() != 42 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("invalid address")),
        )
            .into_response();
    }

    if let Some(ref name) = body.display_name {
        if name.len() > 64 {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err("display_name exceeds 64 characters")),
            )
                .into_response();
        }
    }

    let msg = format!(
        "vela:mm-register:{}:{}",
        body.address.to_lowercase(),
        body.nonce
    )
    .into_bytes();
    if verify_matches_async(msg, body.signature.clone(), body.address.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("invalid signature")),
        )
            .into_response();
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut registered = state.registered_mms.lock().await;
    let addr_lower = body.address.to_lowercase();
    if registered
        .iter()
        .any(|mm| mm.address.to_lowercase() == addr_lower)
    {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err("address already registered")),
        )
            .into_response();
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
    let bound = if is_bid {
        mid * 99 / 100
    } else {
        mid * 101 / 100
    };
    let mut total_qty: u128 = 0;
    for &(price, qty) in levels {
        let in_range = if is_bid {
            price >= bound
        } else {
            price <= bound
        };
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
    let mut book_snaps: std::collections::HashMap<String, BookSnapshot> =
        std::collections::HashMap::new();
    for market_id in &market_ids {
        if let Some(shard_arc) = state.shards.shards.get(&MarketId(market_id.clone())) {
            let shard = shard_arc.lock().await;
            if let Some(book) = shard.engine.order_books.get(&MarketId(market_id.clone())) {
                book_snaps.insert(
                    market_id.clone(),
                    BookSnapshot {
                        best_bid: book.best_bid(),
                        best_ask: book.best_ask(),
                        asks_depth: book.depth_asks(500),
                        bids_depth: book.depth_bids(500),
                    },
                );
            }
        }
    }

    let fills = state.fills.lock().await;

    let mut markets = Vec::new();

    for market_id in &market_ids {
        let snap = book_snaps.get(market_id.as_str());

        let (best_bid, best_ask) = snap
            .map(|s| (s.best_bid, s.best_ask))
            .unwrap_or((None, None));

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
                (Some(s), Some(mid)) => (
                    compute_slippage_bps(&s.asks_depth, mid, 1_000),
                    compute_slippage_bps(&s.asks_depth, mid, 10_000),
                    compute_slippage_bps(&s.asks_depth, mid, 100_000),
                    compute_depth_usdc_1pct(&s.bids_depth, mid, true),
                    compute_depth_usdc_1pct(&s.asks_depth, mid, false),
                ),
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
        let avg_fill = if fill_count > 0 {
            total_volume / fill_count as f64
        } else {
            0.0
        };

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

async fn admin_export_trades_yesterday(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let ms_into_day = now_ms % 86_400_000;
    let start_of_today = now_ms - ms_into_day;
    let start_of_yesterday = start_of_today - 86_400_000;

    match crate::historical::dump_trades_for_day(
        Arc::clone(&state),
        start_of_yesterday,
        start_of_today,
    )
    .await
    {
        Ok((files, rows)) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serde_json::json!({
                "files": files,
                "rows": rows,
                "from_ms": start_of_yesterday,
                "to_ms": start_of_today,
            }))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err(e.to_string())),
        )
            .into_response(),
    }
}

async fn admin_export_l2_now(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let provided = headers
        .get("x-admin-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.verify_admin_token(provided) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    match crate::historical::dump_l2_snapshots(Arc::clone(&state), now_ms).await {
        Ok(count) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serde_json::json!({
                "markets": count,
                "timestamp_ms": now_ms,
            }))),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::err(e.to_string())),
        )
            .into_response(),
    }
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
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("unauthorized")),
        )
            .into_response();
    }

    let us = state.shards.user_state.read().await;
    let fee_balances: std::collections::HashMap<String, u64> = us
        .fee_balances
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let total_usdc = fee_balances.get("USDC").copied().unwrap_or(0);
    let total_fees_collected_usdc = format_amount(total_usdc, PRICE_DECIMALS);

    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "fee_balances": fee_balances,
            "total_fees_collected_usdc": total_fees_collected_usdc,
        }))),
    )
        .into_response()
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
    })))
    .into_response()
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

    let items: Vec<serde_json::Value> = filtered
        .iter()
        .map(|p| {
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
        })
        .collect();

    let next_cursor = filtered.last().map(|p| p.batch_id);
    drop(proofs_map);

    Json(ApiResponse::ok(serde_json::json!({
        "proofs": items,
        "next_cursor": next_cursor,
    })))
    .into_response()
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
    })))
    .into_response()
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

    let items: Vec<serde_json::Value> = filtered
        .iter()
        .map(|r| {
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
        })
        .collect();

    let next_cursor = filtered.last().map(|r| r.batch_id);
    drop(store);

    Json(ApiResponse::ok(serde_json::json!({
        "attestations": items,
        "next_cursor": next_cursor,
    })))
    .into_response()
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
        // AppState::new() panics without ADMIN_TOKEN; set a stub for the
        // in-process test harness. Safe here because these tests never
        // exercise the admin-token gated endpoints.
        if std::env::var("ADMIN_TOKEN").is_err() {
            std::env::set_var("ADMIN_TOKEN", "test-admin-token");
        }
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
        assert!(body["data"]["binary_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
        assert_eq!(body["data"]["total_batches"], 0);
    }
}

#[cfg(test)]
mod points_tests {
    use super::{fill_notional_usdc, fill_points, POINTS_MAKER_MULTIPLIER};
    use crate::types::StoredFill;

    fn mk_fill(price: u64, qty: u64, toxicity: f64) -> StoredFill {
        StoredFill {
            id: String::new(),
            market_id: String::new(),
            price,
            quantity: qty,
            maker_order_id: 0,
            taker_order_id: 0,
            maker_address: String::new(),
            taker_address: String::new(),
            timestamp: 0,
            side: String::new(),
            synthetic: false,
            toxicity_score: toxicity,
        }
    }

    #[test]
    fn notional_scales_from_fixed_point() {
        // price = 100 USDC (100 × 1e6), qty = 2 BTC (2 × 1e6) → 200 USDC
        let n = fill_notional_usdc(100 * 1_000_000, 2 * 1_000_000);
        assert!((n - 200.0).abs() < 1e-9);
    }

    #[test]
    fn clean_fill_gives_maker_bonus_and_full_taker() {
        let fill = mk_fill(100 * 1_000_000, 2 * 1_000_000, 0.0);
        let (m, t) = fill_points(&fill);
        assert!((m - 200.0 * POINTS_MAKER_MULTIPLIER).abs() < 1e-9);
        assert!((t - 200.0).abs() < 1e-9);
    }

    #[test]
    fn toxic_taker_earns_reduced_points() {
        // toxicity = 0.75 → taker gets 25% of notional
        let fill = mk_fill(100 * 1_000_000, 2 * 1_000_000, 0.75);
        let (_m, t) = fill_points(&fill);
        assert!((t - 200.0 * 0.25).abs() < 1e-9);
    }

    #[test]
    fn fully_toxic_taker_earns_zero() {
        let fill = mk_fill(100 * 1_000_000, 2 * 1_000_000, 1.0);
        let (m, t) = fill_points(&fill);
        // Maker still gets full points even when the fill was toxic —
        // makers aren't punished for being victims of adverse selection.
        assert!((m - 200.0 * POINTS_MAKER_MULTIPLIER).abs() < 1e-9);
        assert_eq!(t, 0.0);
    }

    #[test]
    fn toxicity_score_clamps_out_of_range() {
        // Guard against corrupted scores. (1 - 1.5).clamp(0, 1) = 0.
        let fill = mk_fill(100 * 1_000_000, 2 * 1_000_000, 1.5);
        let (_m, t) = fill_points(&fill);
        assert_eq!(t, 0.0);
        // Negative score: (1 - (-0.2)).clamp(0, 1) = 1, taker gets full.
        let fill_neg = mk_fill(100 * 1_000_000, 2 * 1_000_000, -0.2);
        let (_m, t2) = fill_points(&fill_neg);
        assert!((t2 - 200.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod portfolio_tests {
    use super::{compute_market_pnl, CostBasisMethod};
    use crate::types::StoredFill;

    /// Helper: make a StoredFill where `user_is_taker` and taker was on the
    /// given side ("bid" for buy, "ask" for sell).
    fn mk_fill(
        market: &str,
        user_is_taker: bool,
        taker_side: &str,
        price: u64,
        qty: u64,
        ts: u64,
    ) -> StoredFill {
        let user = "0xuser".to_string();
        let counter = "0xcounter".to_string();
        StoredFill {
            id: format!("fill-{}", ts),
            market_id: market.to_string(),
            price,
            quantity: qty,
            maker_order_id: 0,
            taker_order_id: 0,
            maker_address: if user_is_taker {
                counter.clone()
            } else {
                user.clone()
            },
            taker_address: if user_is_taker {
                user.clone()
            } else {
                counter.clone()
            },
            timestamp: ts,
            side: taker_side.to_string(),
            synthetic: false,
            toxicity_score: 0.0,
        }
    }

    #[test]
    fn fifo_buy_then_sell_at_higher_price_realizes_profit() {
        // User buys 1 BTC @ 100, then sells 1 BTC @ 120. Realized = +20 USDC.
        let f1 = mk_fill("BTC-USDC", true, "bid", 100 * 1_000_000, 1_000_000, 1);
        let f2 = mk_fill("BTC-USDC", true, "ask", 120 * 1_000_000, 1_000_000, 2);
        let fills = vec![&f1, &f2];
        let p = compute_market_pnl("0xuser", "BTC-USDC", &fills, None, CostBasisMethod::Fifo);
        assert!((p.realized_usdc - 20.0).abs() < 1e-6);
        assert_eq!(p.position_base, 0.0);
    }

    #[test]
    fn fifo_partial_sell_leaves_remaining_lot() {
        // Buy 2 @ 100, sell 1 @ 150 → realized = 50, 1 left at cost 100.
        let f1 = mk_fill("BTC-USDC", true, "bid", 100 * 1_000_000, 2_000_000, 1);
        let f2 = mk_fill("BTC-USDC", true, "ask", 150 * 1_000_000, 1_000_000, 2);
        let fills = vec![&f1, &f2];
        let p = compute_market_pnl("0xuser", "BTC-USDC", &fills, None, CostBasisMethod::Fifo);
        assert!((p.realized_usdc - 50.0).abs() < 1e-6);
        assert!((p.position_base - 1.0).abs() < 1e-9);
        assert!((p.cost_basis_price - 100.0).abs() < 1e-6);
    }

    #[test]
    fn fifo_vs_hifo_disagree_on_realized() {
        // Two buys at different prices, one sell — FIFO takes older/cheaper
        // lot first, HIFO takes higher-priced lot first.
        // Buy 1 @ 100 (t=1), Buy 1 @ 200 (t=2), Sell 1 @ 150 (t=3).
        let f1 = mk_fill("BTC-USDC", true, "bid", 100 * 1_000_000, 1_000_000, 1);
        let f2 = mk_fill("BTC-USDC", true, "bid", 200 * 1_000_000, 1_000_000, 2);
        let f3 = mk_fill("BTC-USDC", true, "ask", 150 * 1_000_000, 1_000_000, 3);
        let fills = vec![&f1, &f2, &f3];

        let fifo = compute_market_pnl("0xuser", "BTC-USDC", &fills, None, CostBasisMethod::Fifo);
        // FIFO matches sell vs. lot @ 100 → +50 realized. Remaining 1 @ 200.
        assert!((fifo.realized_usdc - 50.0).abs() < 1e-6);
        assert!((fifo.cost_basis_price - 200.0).abs() < 1e-6);

        let hifo = compute_market_pnl("0xuser", "BTC-USDC", &fills, None, CostBasisMethod::Hifo);
        // HIFO matches sell vs. lot @ 200 → -50 realized. Remaining 1 @ 100.
        assert!((hifo.realized_usdc - (-50.0)).abs() < 1e-6);
        assert!((hifo.cost_basis_price - 100.0).abs() < 1e-6);
    }

    #[test]
    fn unrealized_pnl_uses_mark_price() {
        // Buy 1 @ 100, hold. Mark @ 120 → unrealized = +20.
        let f1 = mk_fill("BTC-USDC", true, "bid", 100 * 1_000_000, 1_000_000, 1);
        let fills = vec![&f1];
        let p = compute_market_pnl(
            "0xuser",
            "BTC-USDC",
            &fills,
            Some(120 * 1_000_000),
            CostBasisMethod::Fifo,
        );
        assert!((p.unrealized_usdc.unwrap() - 20.0).abs() < 1e-6);
        assert!((p.position_base - 1.0).abs() < 1e-9);
        assert!((p.cost_basis_price - 100.0).abs() < 1e-6);
    }

    #[test]
    fn user_as_maker_bought_when_taker_was_asking() {
        // Taker's side = "ask" (selling), maker (user) was on the bid side
        // → user bought 1 @ 100.
        let f1 = mk_fill("BTC-USDC", false, "ask", 100 * 1_000_000, 1_000_000, 1);
        let fills = vec![&f1];
        let p = compute_market_pnl(
            "0xuser",
            "BTC-USDC",
            &fills,
            Some(100 * 1_000_000),
            CostBasisMethod::Fifo,
        );
        assert!((p.position_base - 1.0).abs() < 1e-9);
        assert!((p.cost_basis_price - 100.0).abs() < 1e-6);
    }
}
