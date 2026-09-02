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
                    "responses": {
                        "200": {
                            "description": "Markets with best bid/ask/spread",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "type": "array", "items": { "$ref": "#/components/schemas/MarketResponse" } }
                                }
                            } } }
                        }
                    }
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
                    "responses": {
                        "200": {
                            "description": "Balances",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "type": "array", "items": { "$ref": "#/components/schemas/BalanceResponse" } }
                                }
                            } } }
                        }
                    }
                }
            },
            "/perp/markets": {
                "get": {
                    "summary": "Perp market state (mark/index/funding)",
                    "responses": {
                        "200": {
                            "description": "Perp markets",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "type": "array", "items": { "$ref": "#/components/schemas/PerpMarket" } }
                                }
                            } } }
                        }
                    }
                }
            },
            "/perp/account/{address}": {
                "get": {
                    "summary": "Perp positions + margin report for one address",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": {
                        "200": {
                            "description": "Perp account",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "$ref": "#/components/schemas/PerpAccount" }
                                }
                            } } }
                        }
                    }
                }
            },
            "/perp/liquidatable": {
                "get": {
                    "summary": "Positions below maintenance margin (eligible for public liquidator)",
                    "responses": {
                        "200": {
                            "description": "Candidates",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "type": "array", "items": { "$ref": "#/components/schemas/PerpLiquidationCandidate" } }
                                }
                            } } }
                        }
                    }
                }
            },
            "/perp/liquidate": {
                "post": {
                    "summary": "Execute a public perp liquidation",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/PerpLiquidateBody" } } } },
                    "responses": {
                        "200": { "description": "Liquidation applied" },
                        "401": { "description": "Signature invalid" },
                        "404": { "description": "Unknown market" },
                        "409": { "description": "Borrower not liquidatable / no open position" }
                    }
                }
            },
            "/borrow-lend/markets": {
                "get": {
                    "summary": "Borrow-lend market state",
                    "responses": {
                        "200": {
                            "description": "Markets",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "type": "array", "items": { "$ref": "#/components/schemas/BorrowLendMarket" } }
                                }
                            } } }
                        }
                    }
                }
            },
            "/borrow-lend/account/{address}": {
                "get": {
                    "summary": "Borrow-lend positions + health factor",
                    "parameters": [{ "name": "address", "in": "path", "required": true, "schema": { "type": "string" } }],
                    "responses": {
                        "200": {
                            "description": "Account",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "$ref": "#/components/schemas/BorrowLendAccount" }
                                }
                            } } }
                        }
                    }
                }
            },
            "/tee/stats": {
                "get": {
                    "summary": "TEE attestation counts + platform label",
                    "responses": {
                        "200": {
                            "description": "TEE stats",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "$ref": "#/components/schemas/TeeStats" }
                                }
                            } } }
                        }
                    }
                }
            },
            "/proofs/stats": {
                "get": {
                    "summary": "ZK proof counts + provider label",
                    "responses": {
                        "200": {
                            "description": "Proof stats",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "$ref": "#/components/schemas/ProofStats" }
                                }
                            } } }
                        }
                    }
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
                    "responses": {
                        "200": {
                            "description": "Portfolio breakdown",
                            "content": { "application/json": { "schema": {
                                "type": "object",
                                "properties": {
                                    "ok": { "type": "boolean" },
                                    "data": { "$ref": "#/components/schemas/PortfolioResponse" }
                                }
                            } } }
                        }
                    }
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
                "ApiResponse": {
                    "type": "object",
                    "required": ["ok"],
                    "properties": {
                        "ok": { "type": "boolean" },
                        "data": {},
                        "error": { "type": "string" }
                    }
                },
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
                },
                "MarketResponse": {
                    "type": "object",
                    "required": ["id", "base", "quote"],
                    "properties": {
                        "id": { "type": "string", "example": "BTC-USDC" },
                        "base": { "type": "string" },
                        "quote": { "type": "string" },
                        "best_bid": { "type": "string" },
                        "best_ask": { "type": "string" },
                        "spread": { "type": "string" }
                    }
                },
                "BalanceResponse": {
                    "type": "object",
                    "required": ["asset", "available", "locked", "total"],
                    "properties": {
                        "asset": { "type": "string" },
                        "available": { "type": "string" },
                        "locked": { "type": "string" },
                        "total": { "type": "string" }
                    }
                },
                "PortfolioLot": {
                    "type": "object",
                    "required": ["asset", "quantity", "cost_basis_usdc", "acquired_at"],
                    "properties": {
                        "asset": { "type": "string" },
                        "quantity": { "type": "string" },
                        "cost_basis_usdc": { "type": "string", "description": "µUSDC" },
                        "acquired_at": { "type": "integer", "description": "Unix seconds" }
                    }
                },
                "PortfolioPerMarket": {
                    "type": "object",
                    "required": ["market", "realized_usdc", "unrealized_usdc"],
                    "properties": {
                        "market": { "type": "string" },
                        "realized_usdc": { "type": "string" },
                        "unrealized_usdc": { "type": "string" }
                    }
                },
                "PortfolioResponse": {
                    "type": "object",
                    "required": [
                        "address",
                        "realized_pnl_usdc",
                        "unrealized_pnl_usdc",
                        "cost_basis_method",
                        "tax_lots",
                        "per_market"
                    ],
                    "properties": {
                        "address": { "type": "string" },
                        "realized_pnl_usdc": { "type": "string" },
                        "unrealized_pnl_usdc": { "type": "string" },
                        "cost_basis_method": { "type": "string", "enum": ["FIFO", "HIFO"] },
                        "tax_lots": { "type": "array", "items": { "$ref": "#/components/schemas/PortfolioLot" } },
                        "per_market": { "type": "array", "items": { "$ref": "#/components/schemas/PortfolioPerMarket" } }
                    }
                },
                "PerpMarket": {
                    "type": "object",
                    "required": [
                        "market",
                        "mark_price_micro_usdc",
                        "index_price_micro_usdc",
                        "funding_index",
                        "funding_rate_bps_per_hour",
                        "gross_open_interest",
                        "net_open_interest",
                        "initial_margin_bps",
                        "maintenance_margin_bps",
                        "max_leverage"
                    ],
                    "properties": {
                        "market": { "type": "string", "example": "BTC-PERP" },
                        "mark_price_micro_usdc": { "type": "integer" },
                        "index_price_micro_usdc": { "type": "integer" },
                        "funding_index": { "type": "integer" },
                        "funding_rate_bps_per_hour": { "type": "integer" },
                        "gross_open_interest": { "type": "integer" },
                        "net_open_interest": { "type": "integer" },
                        "initial_margin_bps": { "type": "integer" },
                        "maintenance_margin_bps": { "type": "integer" },
                        "max_leverage": { "type": "integer" }
                    }
                },
                "PerpPosition": {
                    "type": "object",
                    "required": [
                        "market",
                        "size",
                        "entry_price_micro_usdc",
                        "realized_pnl_micro_usdc",
                        "notional_micro_usdc",
                        "unrealized_pnl_micro_usdc",
                        "initial_requirement_micro_usdc",
                        "maintenance_requirement_micro_usdc",
                        "mark_price_micro_usdc"
                    ],
                    "properties": {
                        "market": { "type": "string" },
                        "size": { "type": "string", "description": "Signed size, base × 1e6 (positive = long)" },
                        "entry_price_micro_usdc": { "type": "integer" },
                        "realized_pnl_micro_usdc": { "type": "string" },
                        "notional_micro_usdc": { "type": "string" },
                        "unrealized_pnl_micro_usdc": { "type": "string" },
                        "initial_requirement_micro_usdc": { "type": "string" },
                        "maintenance_requirement_micro_usdc": { "type": "string" },
                        "mark_price_micro_usdc": { "type": "integer" }
                    }
                },
                "PerpAccount": {
                    "type": "object",
                    "required": ["user", "positions"],
                    "properties": {
                        "user": { "type": "string" },
                        "positions": { "type": "array", "items": { "$ref": "#/components/schemas/PerpPosition" } }
                    }
                },
                "PerpLiquidationCandidate": {
                    "type": "object",
                    "required": [
                        "user",
                        "market",
                        "size",
                        "entry_price_micro_usdc",
                        "mark_price_micro_usdc",
                        "notional_micro_usdc",
                        "maintenance_requirement_micro_usdc",
                        "equity_micro_usdc"
                    ],
                    "properties": {
                        "user": { "type": "string" },
                        "market": { "type": "string" },
                        "size": { "type": "string" },
                        "entry_price_micro_usdc": { "type": "integer" },
                        "mark_price_micro_usdc": { "type": "integer" },
                        "notional_micro_usdc": { "type": "string" },
                        "maintenance_requirement_micro_usdc": { "type": "string" },
                        "equity_micro_usdc": { "type": "string" }
                    }
                },
                "PerpLiquidateBody": {
                    "type": "object",
                    "required": ["liquidator", "signature", "borrower", "market", "nonce"],
                    "properties": {
                        "liquidator": { "type": "string" },
                        "signature": { "type": "string" },
                        "borrower": { "type": "string" },
                        "market": { "type": "string" },
                        "nonce": { "type": "integer" }
                    }
                },
                "BorrowLendMarket": {
                    "type": "object",
                    "required": [
                        "asset",
                        "total_supply",
                        "total_borrows",
                        "utilization_bps",
                        "borrow_rate_apr_bps",
                        "supply_rate_apr_bps",
                        "collateral_factor_bps",
                        "liquidation_bonus_bps",
                        "price_micro_usdc"
                    ],
                    "properties": {
                        "asset": { "type": "string" },
                        "total_supply": { "type": "string" },
                        "total_borrows": { "type": "string" },
                        "utilization_bps": { "type": "integer" },
                        "borrow_rate_apr_bps": { "type": "integer" },
                        "supply_rate_apr_bps": { "type": "integer" },
                        "collateral_factor_bps": { "type": "integer" },
                        "liquidation_bonus_bps": { "type": "integer" },
                        "price_micro_usdc": { "type": "integer" }
                    }
                },
                "BorrowLendPosition": {
                    "type": "object",
                    "required": [
                        "asset",
                        "supply_native",
                        "borrow_native",
                        "supply_value_micro_usdc",
                        "borrow_value_micro_usdc"
                    ],
                    "properties": {
                        "asset": { "type": "string" },
                        "supply_native": { "type": "string" },
                        "borrow_native": { "type": "string" },
                        "supply_value_micro_usdc": { "type": "string" },
                        "borrow_value_micro_usdc": { "type": "string" }
                    }
                },
                "BorrowLendAccount": {
                    "type": "object",
                    "required": [
                        "user",
                        "positions",
                        "borrowing_power_micro_usdc",
                        "total_borrow_value_micro_usdc",
                        "health_factor_bps"
                    ],
                    "properties": {
                        "user": { "type": "string" },
                        "positions": { "type": "array", "items": { "$ref": "#/components/schemas/BorrowLendPosition" } },
                        "borrowing_power_micro_usdc": { "type": "string" },
                        "total_borrow_value_micro_usdc": { "type": "string" },
                        "health_factor_bps": { "type": "string" }
                    }
                },
                "TeeStats": {
                    "type": "object",
                    "required": [
                        "total_batches",
                        "attested",
                        "simulated",
                        "pending",
                        "failed",
                        "platform",
                        "binary_hash",
                        "platform_status"
                    ],
                    "properties": {
                        "total_batches": { "type": "integer" },
                        "attested": { "type": "integer" },
                        "simulated": { "type": "integer" },
                        "pending": { "type": "integer" },
                        "failed": { "type": "integer" },
                        "platform": { "type": "string" },
                        "binary_hash": { "type": "string" },
                        "platform_status": { "type": "string" }
                    }
                },
                "ProofStats": {
                    "type": "object",
                    "required": ["total", "proven", "pending", "skipped", "failed", "provider"],
                    "properties": {
                        "total": { "type": "integer" },
                        "proven": { "type": "integer" },
                        "pending": { "type": "integer" },
                        "skipped": { "type": "integer" },
                        "failed": { "type": "integer" },
                        "provider": { "type": "string" }
                    }
                },
                "OraclePriceEntry": {
                    "type": "object",
                    "required": ["asset", "price_micro_usdc", "timestamp_ms"],
                    "properties": {
                        "asset": { "type": "string" },
                        "price_micro_usdc": { "type": "integer" },
                        "timestamp_ms": { "type": "integer" }
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
