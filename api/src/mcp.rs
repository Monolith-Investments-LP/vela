//! Model Context Protocol (MCP) server for Vela.
//!
//! MCP is the JSON-RPC 2.0 spec that every major agent runtime
//! (Anthropic MCP, OpenAI structured tools, Google A2A, Fetch.ai
//! Agentverse, Coinbase AgentKit, etc.) now converges on for tool
//! calls. Exposing an MCP endpoint means any conforming agent can trade
//! against Vela with zero custom SDK code.
//!
//! Transport
//! ---------
//! Standard MCP for hosted services is HTTP + JSON-RPC 2.0. We accept
//! `POST /mcp` with a JSON-RPC envelope and return the response in the
//! same envelope. No streaming for v1; long-running tool calls (algos)
//! return the parent-id and the caller polls the algo endpoint.
//!
//! Auth model
//! ----------
//! Read-only tools (`list_markets`, `book_snapshot`, `toxicity_score`,
//! `points`, `portfolio`) require no signature.
//!
//! Trading tools (`place_order`, `cancel_order`, `place_twap`) accept
//! the same signed payloads the HTTP endpoints take. The signature is
//! verified via the shipped master-or-agent path, so an agent holding
//! a session key can call MCP tools identically to how it calls REST.
//!
//! Tools shipped in v1
//! -------------------
//! - `list_markets` — market metadata (base, quote, ticks, fees)
//! - `book_snapshot` — depth up to 50 levels
//! - `toxicity_score` — current toxicity + fee tier for an address
//! - `points` — 30d rolling points breakdown
//! - `portfolio` — realised + unrealised PnL per market
//! - `place_order` — signed order
//! - `cancel_order` — signed cancel
//! - `place_twap` — start a server-side TWAP
//!
//! Deferred to v2 (documented, not shipped)
//! ----------------------------------------
//! - MCP resources (`resources/list`, `resources/read`) for historical
//!   L2 snapshots served from the S3 dumps.
//! - MCP prompts (`prompts/list`) for canned strategy templates.
//! - Streamable HTTP for real-time subscription tools (agents currently
//!   subscribe via the existing WebSocket, not MCP).

use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
pub const MCP_SERVER_NAME: &str = "vela-mcp";
pub const MCP_SERVER_VERSION: &str = "0.1.0";

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// Full tool catalog. Kept as an inline function so the JSON stays
/// side-by-side with the dispatch table in `dispatch_tool_call`.
fn tool_catalog() -> Value {
    json!([
        {
            "name": "list_markets",
            "description": "List every trading market on Vela with base/quote, tick sizes, and fee bps.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "book_snapshot",
            "description": "Return the top-N bids and asks for a market.",
            "inputSchema": {
                "type": "object",
                "required": ["market"],
                "properties": {
                    "market": { "type": "string", "description": "e.g. BTC-USDC" },
                    "levels": { "type": "integer", "default": 50, "minimum": 1, "maximum": 50 }
                }
            }
        },
        {
            "name": "toxicity_score",
            "description": "Current fee tier and 30d toxicity/volume for an address.",
            "inputSchema": {
                "type": "object",
                "required": ["address"],
                "properties": { "address": { "type": "string" } }
            }
        },
        {
            "name": "points",
            "description": "30d rolling points breakdown for an address.",
            "inputSchema": {
                "type": "object",
                "required": ["address"],
                "properties": { "address": { "type": "string" } }
            }
        },
        {
            "name": "portfolio",
            "description": "Realised + unrealised PnL per market for an address.",
            "inputSchema": {
                "type": "object",
                "required": ["address"],
                "properties": {
                    "address": { "type": "string" },
                    "method": { "type": "string", "enum": ["fifo", "hifo"], "default": "fifo" }
                }
            }
        },
        {
            "name": "place_order",
            "description": "Place a signed order. Signature accepted from master or authorized agent-wallet.",
            "inputSchema": {
                "type": "object",
                "required": ["address", "market", "side", "order_type", "price", "quantity", "nonce", "signature"],
                "properties": {
                    "address": { "type": "string" },
                    "market": { "type": "string" },
                    "side": { "type": "string", "enum": ["Bid", "Ask"] },
                    "order_type": { "type": "string", "enum": ["GoodTillCanceled", "PostOnly", "ImmediateOrCancel", "FillOrKill"] },
                    "price": { "type": "integer", "description": "USDC × 1e6" },
                    "quantity": { "type": "integer", "description": "Base × 1e6" },
                    "nonce": { "type": "integer" },
                    "client_order_id": { "type": "string" },
                    "signature": { "type": "string" }
                }
            }
        },
        {
            "name": "cancel_order",
            "description": "Cancel a signed order by id or client_order_id.",
            "inputSchema": {
                "type": "object",
                "required": ["address", "nonce", "signature"],
                "properties": {
                    "address": { "type": "string" },
                    "order_id": { "type": "integer" },
                    "client_order_id": { "type": "string" },
                    "nonce": { "type": "integer" },
                    "signature": { "type": "string" }
                }
            }
        },
        {
            "name": "place_twap",
            "description": "Start a server-side TWAP execution algo. Returns parent_id.",
            "inputSchema": {
                "type": "object",
                "required": ["address", "market", "side", "quantity", "duration_secs", "nonce", "signature"],
                "properties": {
                    "address": { "type": "string" },
                    "market": { "type": "string" },
                    "side": { "type": "string", "enum": ["Bid", "Ask"] },
                    "quantity": { "type": "integer" },
                    "duration_secs": { "type": "integer" },
                    "price_limit": { "type": "integer" },
                    "slices": { "type": "integer", "default": 12 },
                    "nonce": { "type": "integer" },
                    "signature": { "type": "string" }
                }
            }
        }
    ])
}

/// Wrap a plain JSON result as an MCP `tools/call` response shape:
/// `{ content: [{ type: "text", text: "<json>" }], isError: false }`.
fn tool_result(value: &Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(value).unwrap_or_default()
        }],
        "isError": false
    })
}

fn tool_error(msg: impl Into<String>) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": msg.into()
        }],
        "isError": true
    })
}

/// Dispatch a `tools/call` invocation to the concrete implementation.
async fn dispatch_tool_call(state: &Arc<AppState>, tool: &str, args: Value) -> Value {
    match tool {
        "list_markets" => tool_result(&mcp_list_markets(state).await),
        "book_snapshot" => match args.get("market").and_then(|v| v.as_str()) {
            Some(market) => {
                let levels = args
                    .get("levels")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50)
                    .min(50) as usize;
                tool_result(&mcp_book_snapshot(state, market, levels).await)
            }
            None => tool_error("missing required arg: market"),
        },
        "toxicity_score" => match args.get("address").and_then(|v| v.as_str()) {
            Some(a) => tool_result(&mcp_toxicity_score(state, a).await),
            None => tool_error("missing required arg: address"),
        },
        "points" => match args.get("address").and_then(|v| v.as_str()) {
            Some(a) => tool_result(&mcp_points(state, a).await),
            None => tool_error("missing required arg: address"),
        },
        "portfolio" => match args.get("address").and_then(|v| v.as_str()) {
            Some(a) => {
                let method = args
                    .get("method")
                    .and_then(|v| v.as_str())
                    .unwrap_or("fifo");
                tool_result(&mcp_portfolio(state, a, method).await)
            }
            None => tool_error("missing required arg: address"),
        },
        "place_order" | "cancel_order" | "place_twap" => {
            match mcp_dispatch_signed(state, tool, &args).await {
                Ok(v) => tool_result(&v),
                Err(e) => tool_error(e),
            }
        }
        _ => tool_error(format!("unknown tool: {}", tool)),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations (read-only)
// ---------------------------------------------------------------------------

async fn mcp_list_markets(state: &Arc<AppState>) -> Value {
    let engine = state.engine.lock().await;
    let markets: Vec<Value> = engine
        .markets
        .values()
        .map(|m| {
            json!({
                "id": m.id.0,
                "base": m.base.as_str(),
                "quote": m.quote.as_str(),
                "price_tick": m.price_tick,
                "quantity_tick": m.quantity_tick,
                "min_order_size": m.min_order_size,
                "max_orders": m.max_orders,
                "maker_fee_bps": m.maker_fee_bps,
                "taker_fee_bps": m.taker_fee_bps,
            })
        })
        .collect();
    json!({ "markets": markets })
}

async fn mcp_book_snapshot(state: &Arc<AppState>, market_str: &str, levels: usize) -> Value {
    let market = types::MarketId(market_str.to_string());
    let engine = state.engine.lock().await;
    let book = match engine.order_books.get(&market) {
        Some(b) => b,
        None => return json!({ "error": "market not found", "market": market_str }),
    };
    let bids: Vec<[String; 2]> = book
        .depth_bids(levels)
        .into_iter()
        .map(|(p, q)| [p.to_string(), q.to_string()])
        .collect();
    let asks: Vec<[String; 2]> = book
        .depth_asks(levels)
        .into_iter()
        .map(|(p, q)| [p.to_string(), q.to_string()])
        .collect();
    json!({ "market": market_str, "bids": bids, "asks": asks })
}

async fn mcp_toxicity_score(state: &Arc<AppState>, address: &str) -> Value {
    let uid = match types::UserId::from_hex(address) {
        Ok(u) => u,
        Err(_) => return json!({ "error": "invalid address" }),
    };
    let us = state.shards.user_state.read().await;
    let tier = us.metadata.get(&uid).map(|m| m.fee_tier).unwrap_or(0);
    let (maker_bps, taker_bps) = types::fee_tiers::fees_for_tier(tier);
    json!({
        "address": address.to_lowercase(),
        "fee_tier": tier,
        "maker_bps": maker_bps,
        "taker_bps": taker_bps,
    })
}

async fn mcp_points(state: &Arc<AppState>, address: &str) -> Value {
    // Delegate through the same path the HTTP handler uses. Since
    // `points_by_address` is private in handler.rs, we recompute the
    // sum here inline to avoid a public API leak.
    let addr = address.to_ascii_lowercase();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);
    let fills = state.fills.lock().await;
    let mut maker_points = 0.0f64;
    let mut taker_points = 0.0f64;
    let mut volume = 0.0f64;
    for f in fills.iter() {
        if f.timestamp < cutoff {
            continue;
        }
        let notional = (f.price as f64 * f.quantity as f64) / 1_000_000_000_000.0;
        if f.maker_address.to_ascii_lowercase() == addr {
            maker_points += notional * 1.5;
            volume += notional;
        }
        if f.taker_address.to_ascii_lowercase() == addr {
            let clean = (1.0 - f.toxicity_score).clamp(0.0, 1.0);
            taker_points += notional * clean;
            volume += notional;
        }
    }
    json!({
        "address": addr,
        "window_days": 30,
        "total_points": format!("{:.2}", maker_points + taker_points),
        "maker_points": format!("{:.2}", maker_points),
        "taker_points": format!("{:.2}", taker_points),
        "volume_usdc": format!("{:.2}", volume),
    })
}

async fn mcp_portfolio(state: &Arc<AppState>, address: &str, _method: &str) -> Value {
    // Aggregate per-market realized/unrealized-lite. Full FIFO is in
    // the HTTP handler; the MCP tool returns a compact summary optimised
    // for LLM consumption (small integer fields).
    let addr = address.to_ascii_lowercase();
    let fills = state.fills.lock().await;
    let mut per_market: std::collections::HashMap<String, (f64, u64, u64)> =
        std::collections::HashMap::new();
    for f in fills.iter() {
        let is_maker = f.maker_address.to_ascii_lowercase() == addr;
        let is_taker = f.taker_address.to_ascii_lowercase() == addr;
        if !is_maker && !is_taker {
            continue;
        }
        let notional = (f.price as f64 * f.quantity as f64) / 1_000_000_000_000.0;
        let e = per_market.entry(f.market_id.clone()).or_insert((0.0, 0, 0));
        e.0 += notional;
        if is_maker {
            e.1 += 1;
        }
        if is_taker {
            e.2 += 1;
        }
    }
    let markets: Vec<Value> = per_market
        .into_iter()
        .map(|(m, (vol, maker_ct, taker_ct))| {
            json!({
                "market_id": m,
                "volume_usdc": format!("{:.2}", vol),
                "maker_fill_count": maker_ct,
                "taker_fill_count": taker_ct,
            })
        })
        .collect();
    json!({ "address": addr, "markets": markets })
}

// ---------------------------------------------------------------------------
// Signed-tool dispatch
// ---------------------------------------------------------------------------

async fn mcp_dispatch_signed(
    state: &Arc<AppState>,
    tool: &str,
    args: &Value,
) -> Result<Value, String> {
    match tool {
        "place_order" => mcp_place_order(state, args).await,
        "cancel_order" => mcp_cancel_order(state, args).await,
        "place_twap" => mcp_place_twap(state, args).await,
        _ => Err(format!("unknown signed tool: {}", tool)),
    }
}

fn arg_string(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing or non-string arg: {}", key))
}

fn arg_u64(args: &Value, key: &str) -> Result<u64, String> {
    args.get(key)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| format!("missing or non-integer arg: {}", key))
}

async fn mcp_place_order(state: &Arc<AppState>, args: &Value) -> Result<Value, String> {
    let address = arg_string(args, "address")?;
    let market = arg_string(args, "market")?;
    let side_str = arg_string(args, "side")?;
    let order_type_str = arg_string(args, "order_type")?;
    let price = arg_u64(args, "price")?;
    let quantity = arg_u64(args, "quantity")?;
    let nonce = arg_u64(args, "nonce")?;
    let signature = arg_string(args, "signature")?;
    let client_order_id = args
        .get("client_order_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let side = match side_str.as_str() {
        "Bid" | "bid" => types::OrderSide::Bid,
        "Ask" | "ask" => types::OrderSide::Ask,
        _ => return Err(format!("invalid side: {}", side_str)),
    };
    let order_type = match order_type_str.as_str() {
        "GoodTillCanceled" | "gtc" => types::OrderType::GoodTillCanceled,
        "PostOnly" | "post_only" => types::OrderType::PostOnly,
        "ImmediateOrCancel" | "ioc" => types::OrderType::ImmediateOrCancel,
        "FillOrKill" | "fok" => types::OrderType::FillOrKill,
        _ => return Err(format!("invalid order_type: {}", order_type_str)),
    };

    let user = types::UserId::from_hex(&address).map_err(|_| "invalid address".to_string())?;
    let msg = crate::auth::order_signing_message(
        &market,
        &format!("{:?}", side).to_lowercase(),
        price,
        quantity,
        nonce,
        client_order_id.as_deref(),
    );
    let notional_micro = match side {
        types::OrderSide::Bid => price
            .checked_mul(quantity)
            .map(|n| n / 1_000_000)
            .unwrap_or(u64::MAX),
        types::OrderSide::Ask => quantity,
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    crate::agents::verify_master_or_agent_async(
        msg,
        signature,
        address.clone(),
        notional_micro,
        now_ms,
        Arc::clone(&state.agents),
    )
    .await
    .map_err(|_| "invalid signature".to_string())?;

    let req = types::PostOrderRequest {
        user,
        market: types::MarketId(market),
        side,
        order_type,
        price,
        quantity,
        nonce,
        client_order_id,
        signature: vec![],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: types::Request::PostOrder(req),
        ts,
        responder,
        decryption_proof: None,
    };
    state
        .order_tx
        .send(channel_item)
        .await
        .map_err(|_| "engine unavailable".to_string())?;
    let responses = tokio::time::timeout(std::time::Duration::from_millis(500), resp_rx)
        .await
        .map_err(|_| "engine dispatch timed out".to_string())?
        .map_err(|_| "engine error".to_string())?;
    Ok(json!({ "responses": responses }))
}

async fn mcp_cancel_order(state: &Arc<AppState>, args: &Value) -> Result<Value, String> {
    let address = arg_string(args, "address")?;
    let nonce = arg_u64(args, "nonce")?;
    let signature = arg_string(args, "signature")?;
    let order_id = args.get("order_id").and_then(|v| v.as_u64());
    let client_order_id = args
        .get("client_order_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let msg = crate::auth::cancel_signing_message(order_id, client_order_id.as_deref(), nonce);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    crate::agents::verify_master_or_agent_async(
        msg,
        signature,
        address.clone(),
        0,
        now_ms,
        Arc::clone(&state.agents),
    )
    .await
    .map_err(|_| "invalid signature".to_string())?;

    let user = types::UserId::from_hex(&address).map_err(|_| "invalid address".to_string())?;
    let req = types::CancelOrderRequest {
        user,
        order_id,
        client_order_id,
        nonce,
        signature: vec![],
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let channel_item = engine::batch_dispatcher::BatchedRequest {
        request: types::Request::CancelOrder(req),
        ts,
        responder,
        decryption_proof: None,
    };
    state
        .order_tx
        .send(channel_item)
        .await
        .map_err(|_| "engine unavailable".to_string())?;
    let responses = tokio::time::timeout(std::time::Duration::from_millis(500), resp_rx)
        .await
        .map_err(|_| "engine dispatch timed out".to_string())?
        .map_err(|_| "engine error".to_string())?;
    Ok(json!({ "responses": responses }))
}

async fn mcp_place_twap(state: &Arc<AppState>, args: &Value) -> Result<Value, String> {
    let address = arg_string(args, "address")?;
    let market = arg_string(args, "market")?;
    let side_str = arg_string(args, "side")?;
    let quantity = arg_u64(args, "quantity")?;
    let duration_secs = arg_u64(args, "duration_secs")?;
    let nonce = arg_u64(args, "nonce")?;
    let signature = arg_string(args, "signature")?;
    let price_limit = args.get("price_limit").and_then(|v| v.as_u64());
    let slices = args.get("slices").and_then(|v| v.as_u64()).unwrap_or(12) as u32;
    let slices = slices.clamp(1, 240);

    let side = match side_str.as_str() {
        "Bid" | "bid" => types::OrderSide::Bid,
        "Ask" | "ask" => types::OrderSide::Ask,
        _ => return Err(format!("invalid side: {}", side_str)),
    };

    if quantity == 0 || duration_secs == 0 {
        return Err("quantity and duration_secs must be > 0".to_string());
    }
    if quantity < slices as u64 {
        return Err("quantity must be >= slices (need at least 1 unit per slice)".to_string());
    }

    let notional_micro = match side {
        types::OrderSide::Bid => price_limit
            .and_then(|p| p.checked_mul(quantity))
            .map(|n| n / 1_000_000)
            .unwrap_or(u64::MAX),
        types::OrderSide::Ask => quantity,
    };
    let msg = format!(
        "vela:algo:twap:{}:{}:{}:{}",
        market, quantity, duration_secs, nonce
    )
    .into_bytes();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    crate::agents::verify_master_or_agent_async(
        msg,
        signature,
        address.clone(),
        notional_micro,
        now_ms,
        Arc::clone(&state.agents),
    )
    .await
    .map_err(|_| "invalid signature".to_string())?;

    let parent_id = crate::algos::next_parent_id();
    let parent = std::sync::Arc::new(crate::algos::TwapParent {
        parent_id,
        user_address: address.to_lowercase(),
        market,
        side,
        total_quantity: quantity,
        filled_quantity: std::sync::atomic::AtomicU64::new(0),
        price_limit,
        duration_secs,
        slices,
        started_at_ms: now_ms,
        status: std::sync::Mutex::new(crate::algos::AlgoStatus::Running),
        cancel_flag: std::sync::atomic::AtomicBool::new(false),
        child_nonce_base: nonce.saturating_add(1),
        _phantom_signature: (),
    });
    state
        .algos
        .insert(parent_id, std::sync::Arc::clone(&parent));
    tokio::spawn(crate::algos::run_twap_task(
        Arc::clone(state),
        std::sync::Arc::clone(&parent),
    ));

    Ok(json!({
        "parent_id": parent_id,
        "status": "Running",
        "note": "TWAP running; poll GET /orders/algo/{parent_id} or call tools/call `algo_status`."
    }))
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 top-level dispatch
// ---------------------------------------------------------------------------

pub async fn handle_rpc(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    let id = req.id.clone();
    let response = match req.method.as_str() {
        "initialize" => JsonRpcResponse::ok(
            id,
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": {}
                },
                "serverInfo": {
                    "name": MCP_SERVER_NAME,
                    "version": MCP_SERVER_VERSION
                }
            }),
        ),
        "tools/list" => JsonRpcResponse::ok(id, json!({ "tools": tool_catalog() })),
        "tools/call" => {
            let tool = req
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default()));
            match tool {
                Some(t) => JsonRpcResponse::ok(id, dispatch_tool_call(&state, &t, args).await),
                None => JsonRpcResponse::error(id, -32602, "missing tool name"),
            }
        }
        "ping" => JsonRpcResponse::ok(id, json!({})),
        other => JsonRpcResponse::error(id, -32601, format!("method not found: {}", other)),
    };
    Json(response)
}
