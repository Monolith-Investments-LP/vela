//! OpenAPI 3.1 spec + Redoc docs page.
//!
//! This module publishes a machine-readable schema of Vela's HTTP API so
//! institutional clients can generate typed SDKs (py / ts / rust) via
//! `openapi-generator-cli` and CI-verify their integration against a
//! versioned contract. The spec is hand-authored (not runtime-generated)
//! because we want tight control over descriptions, examples, and error
//! shapes.
//!
//! Consumers:
//! - `GET /openapi.json` — machine-readable spec.
//! - `GET /docs` — human-readable Redoc-rendered docs (CDN, no deps).
//!
//! Versioning contract:
//! - Every documented route is stable within the current major version.
//! - Breaking changes bump to `/v2` (and this spec's `info.version`).
//! - New response fields and new endpoints are additive within a version.

use axum::response::{Html, IntoResponse};
use axum::Json;

/// The OpenAPI document. Kept inline as a big JSON literal so this file
/// stays in the same review context as the routing table it documents.
/// If the spec grows past a few hundred lines this should move into
/// `api/openapi.json` and be `include_str!`'d.
pub fn openapi_spec() -> serde_json::Value {
    serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Vela Exchange API",
            "version": "0.3.0",
            "description": "HTTP + WebSocket API for the Vela verifiable spot DEX. See https://github.com/Monolith-Investments-LP/vela for source and https://vela.monolithsystematic.com for live beta.",
            "contact": { "email": "asomu@ucsd.edu" }
        },
        "servers": [
            { "url": "https://vela-engine.fly.dev", "description": "Live beta (Sepolia)" },
            { "url": "http://127.0.0.1:3001", "description": "Local dev" }
        ],
        "paths": {
            "/health": {
                "get": { "summary": "Liveness check", "responses": { "200": { "description": "ok" } } }
            },
            "/metrics": {
                "get": { "summary": "Prometheus metrics", "responses": { "200": { "description": "Prometheus text 0.0.4" } } }
            },
            "/markets": {
                "get": {
                    "summary": "List all markets",
                    "responses": { "200": { "description": "Markets with best bid/ask/spread" } }
                }
            },
            "/markets/{market}/book": {
                "get": {
                    "summary": "Order book snapshot",
                    "parameters": [{ "name": "market", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "Bids + asks up to N levels" } }
                }
            },
            "/orders": {
                "post": {
                    "summary": "Place a signed order (master or agent-wallet)",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/PostOrderBody" } } } },
                    "responses": {
                        "200": { "description": "Order accepted; response contains fills + status" },
                        "401": { "description": "Signature invalid or agent cap exceeded" },
                        "504": { "description": "Engine dispatch timed out" }
                    }
                }
            },
            "/orders/cancel": {
                "post": {
                    "summary": "Cancel an order by id or client_order_id",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CancelOrderBody" } } } },
                    "responses": { "200": { "description": "Cancel result" } }
                }
            },
            "/orders/{order_id}": {
                "get": {
                    "summary": "Order lifecycle with fill history",
                    "parameters": [{ "name": "order_id", "in": "path", "required": true, "schema": { "type": "integer" } }],
                    "responses": { "200": { "description": "Order details" } }
                }
            },
            "/orders/algo/twap": {
                "post": {
                    "summary": "Start a server-side TWAP",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/TwapAlgoBody" } } } },
                    "responses": { "200": { "description": "parent_id + status" } }
                }
            },
            "/orders/algo/cancel": {
                "post": {
                    "summary": "Cancel a running algo by parent_id",
                    "responses": { "200": { "description": "Canceled" } }
                }
            },
            "/orders/algo/{parent_id}": {
                "get": {
                    "summary": "Algo snapshot",
                    "parameters": [{ "name": "parent_id", "in": "path", "required": true, "schema": { "type": "integer" } }],
                    "responses": { "200": { "description": "TWAP parent status" } }
                }
            },
            "/agents/register": {
                "post": {
                    "summary": "Register a session-key / agent wallet",
                    "responses": { "200": { "description": "Delegation stored" } }
                }
            },
            "/agents/revoke": {
                "post": { "summary": "Revoke an agent", "responses": { "200": { "description": "Revoked" } } }
            },
            "/agents/{master}": {
                "get": {
                    "summary": "List agents for a master wallet",
                    "parameters": [{ "name": "master", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "Agents + status" } }
                }
            },
            "/account/{address}/balances": {
                "get": {
                    "summary": "Balances by asset",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "Balances" } }
                }
            },
            "/account/{address}/orders": {
                "get": {
                    "summary": "Open orders for an account",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "Open orders" } }
                }
            },
            "/portfolio/{address}": {
                "get": {
                    "summary": "Realized + unrealized PnL, cost basis, per market",
                    "parameters": [
                        { "name": "address", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "method", "in": "query", "required": false, "schema": { "type": "string", "enum": ["fifo", "hifo"] } }
                    ],
                    "responses": { "200": { "description": "Portfolio breakdown" } }
                }
            },
            "/portfolio/{address}/csv": {
                "get": {
                    "summary": "Full fill history as CSV (Koinly/CoinTracker)",
                    "responses": { "200": { "description": "text/csv", "content": { "text/csv": {} } } }
                }
            },
            "/points/{address}": {
                "get": {
                    "summary": "Points + toxicity-gated fill counts",
                    "responses": { "200": { "description": "Points breakdown" } }
                }
            },
            "/leaderboard": {
                "get": { "summary": "Top traders by points, 30d rolling", "responses": { "200": { "description": "Leaderboard" } } }
            },
            "/fees/schedule": {
                "get": { "summary": "Volume-tiered fee schedule", "responses": { "200": { "description": "Tier table" } } }
            },
            "/fees/tier/{address}": {
                "get": {
                    "summary": "Current fee tier for an address",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "Tier + volume + next threshold" } }
                }
            },
            "/deposit": {
                "post": {
                    "summary": "Credit engine balance from L1 Ethereum event",
                    "responses": { "200": { "description": "Credited" } }
                }
            },
            "/deposit/bridge": {
                "post": {
                    "summary": "Cross-chain bridge-attested deposit",
                    "responses": { "200": { "description": "Credited" }, "401": { "description": "Bridge sig invalid or not allowlisted" } }
                }
            },
            "/deposit/bridges": {
                "get": { "summary": "Public bridge allowlist", "responses": { "200": { "description": "Bridges + pubkeys" } } }
            },
            "/withdrawals": {
                "post": { "summary": "Submit signed withdrawal", "responses": { "200": { "description": "Queued" } } }
            },
            "/vaults": {
                "get": { "summary": "List MM credit vaults + AUM", "responses": { "200": { "description": "Vaults" } } }
            },
            "/vaults/create": {
                "post": { "summary": "Create a vault (operator-signed)", "responses": { "200": { "description": "Vault created" } } }
            },
            "/vaults/{vault_id}": {
                "get": {
                    "summary": "Vault snapshot + share price",
                    "parameters": [{ "name": "vault_id", "in": "path", "required": true, "schema": { "type": "integer" } }],
                    "responses": { "200": { "description": "Vault" } }
                }
            },
            "/vaults/{vault_id}/deposit": {
                "post": { "summary": "LP deposits USDC, receives shares", "responses": { "200": { "description": "Deposited" } } }
            },
            "/vaults/{vault_id}/withdraw": {
                "post": { "summary": "LP burns shares, receives USDC", "responses": { "200": { "description": "Withdrawn" } } }
            },
            "/vaults/{vault_id}/positions/{lp}": {
                "get": { "summary": "LP position in a vault", "responses": { "200": { "description": "Position" } } }
            },
            "/listings": {
                "get": { "summary": "Pending / accepted / rejected permissionless listings", "responses": { "200": { "description": "Listings" } } }
            },
            "/listings/propose": {
                "post": { "summary": "Propose a market (posts bond)", "responses": { "200": { "description": "Proposal" } } }
            },
            "/trades": { "get": { "summary": "Recent fills (all markets)", "responses": { "200": { "description": "Fills" } } } },
            "/trades/{market_id}": {
                "get": {
                    "summary": "Recent fills for one market",
                    "parameters": [{ "name": "market_id", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "Fills" } }
                }
            },
            "/ohlcv/{market_id}": {
                "get": {
                    "summary": "OHLCV candles derived from real fills",
                    "parameters": [{ "name": "market_id", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": { "200": { "description": "Candles" } }
                }
            },
            "/status": {
                "get": { "summary": "Engine status + today's counters", "responses": { "200": { "description": "Status" } } }
            },
            "/ws": {
                "get": {
                    "summary": "WebSocket upgrade — see WS section for channel protocol",
                    "responses": { "101": { "description": "Switching Protocols" } }
                }
            }
        },
        "components": {
            "schemas": {
                "PostOrderBody": {
                    "type": "object",
                    "required": ["address", "market", "side", "order_type", "price", "quantity", "nonce", "signature"],
                    "properties": {
                        "address": { "type": "string", "description": "Master wallet (order signer may be master or authorized agent)" },
                        "market": { "type": "string", "example": "BTC-USDC" },
                        "side": { "type": "string", "enum": ["Bid", "Ask"] },
                        "order_type": { "type": "string", "enum": ["GoodTillCanceled", "PostOnly", "ImmediateOrCancel", "FillOrKill"] },
                        "price": { "type": "integer", "description": "USDC × 1e6" },
                        "quantity": { "type": "integer", "description": "Base × 1e6" },
                        "nonce": { "type": "integer" },
                        "client_order_id": { "type": "string" },
                        "signature": { "type": "string", "description": "0x-prefixed 65-byte ECDSA" }
                    }
                },
                "CancelOrderBody": {
                    "type": "object",
                    "required": ["address", "nonce", "signature"],
                    "properties": {
                        "address": { "type": "string" },
                        "order_id": { "type": "integer" },
                        "client_order_id": { "type": "string" },
                        "nonce": { "type": "integer" },
                        "signature": { "type": "string" }
                    }
                },
                "TwapAlgoBody": {
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
        },
        "x-websocket": {
            "url": "wss://vela-engine.fly.dev/ws",
            "channels": {
                "orderbook:{market}": "Top-50 depth snapshots per second",
                "trades:{market}": "Fills as they land",
                "markets": "Best bid/ask/spread across all markets every 5s",
                "account:{address}": "Authenticated: balances, orders, fills for one account",
                "dropcopy:{address}": "Authenticated: fills-only, delivered on connection independent of trading",
                "feed/toxicity": "Authenticated: per-fill toxicity scores"
            },
            "auth": "Send { type: 'auth', address, signature, timestamp } after connect. `signature` covers `vela:ws:{address}:{timestamp}` via personal_sign."
        }
    })
}

/// Serve the OpenAPI JSON.
pub async fn openapi_handler() -> impl IntoResponse {
    Json(openapi_spec())
}

/// Redoc-rendered HTML for the OpenAPI spec. Uses Redoc's public CDN so
/// there's no bundling / hosting overhead.
pub async fn docs_handler() -> impl IntoResponse {
    Html(
        r#"<!doctype html>
<html>
  <head>
    <title>Vela API — reference</title>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width, initial-scale=1"/>
    <style>body { margin: 0; padding: 0; }</style>
  </head>
  <body>
    <redoc spec-url="/openapi.json"></redoc>
    <script src="https://cdn.redoc.ly/redoc/latest/bundles/redoc.standalone.js"></script>
  </body>
</html>"#,
    )
}
