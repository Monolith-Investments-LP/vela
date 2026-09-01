//! Machine-readable schema for the WebSocket message stream.
//!
//! Agents (Claude, GPT, Gemini, custom bots) benefit from a strict,
//! versioned JSON Schema for every message shape they receive. Two
//! concrete uses:
//!
//! 1. **Structured-output constraints on LLMs.** OpenAI, Anthropic,
//!    and Google all support JSON-Schema-guided decoding. Publishing
//!    the schema means an agent that generates trading responses can
//!    constrain its output to valid message envelopes at inference
//!    time, eliminating a class of hallucinations.
//! 2. **Client-side validation.** Any agent runtime with a JSON
//!    Schema validator (ajv, jsonschema, etc.) can reject malformed
//!    incoming messages instead of accepting them and crashing later.
//!
//! Transport unchanged
//! -------------------
//! Vela's existing `/ws` transport is already JSON. This module does
//! not ship a new WS route; it publishes a schema for the messages
//! already flowing on the existing channels (`orderbook:`, `trades:`,
//! `markets`, `account:`, `dropcopy:`, `feed/toxicity`). Consumers
//! reference the schema via `GET /agent-stream/schema.json`.
//!
//! Versioning
//! ----------
//! The schema document carries a `schema_version` field. Additive
//! changes (new message variants, new optional fields) don't bump
//! the version. Breaking changes (removed fields, renamed enums,
//! narrowed types) bump the major version.

use axum::response::IntoResponse;
use axum::Json;
use serde_json::{json, Value};

pub const SCHEMA_VERSION: &str = "1.0.0";

/// Full JSON Schema (draft 2020-12) for Vela's WebSocket message
/// envelope. Every message the server sends over `/ws` conforms to one
/// of the variants under `oneOf`.
pub fn ws_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://vela-engine.fly.dev/agent-stream/schema.json",
        "title": "Vela WebSocket message envelope",
        "schema_version": SCHEMA_VERSION,
        "type": "object",
        "required": ["type"],
        "oneOf": [
            {
                "title": "OrderBook snapshot",
                "properties": {
                    "type": { "const": "snapshot" },
                    "channel": { "type": "string", "pattern": "^orderbook:.+" },
                    "seq": { "type": "integer", "minimum": 0 },
                    "timestamp": { "type": "integer", "minimum": 0 },
                    "data": {
                        "type": "object",
                        "required": ["bids", "asks"],
                        "properties": {
                            "bids": { "$ref": "#/$defs/PriceLevelArray" },
                            "asks": { "$ref": "#/$defs/PriceLevelArray" }
                        }
                    }
                },
                "required": ["type", "channel", "seq", "timestamp", "data"]
            },
            {
                "title": "Trade tick",
                "properties": {
                    "type": { "const": "trade" },
                    "channel": { "type": "string", "pattern": "^trades:.+" },
                    "seq": { "type": "integer", "minimum": 0 },
                    "timestamp": { "type": "integer", "minimum": 0 },
                    "data": { "$ref": "#/$defs/TradeData" }
                },
                "required": ["type", "channel", "seq", "timestamp", "data"]
            },
            {
                "title": "Markets summary",
                "properties": {
                    "type": { "const": "markets" },
                    "channel": { "const": "markets" },
                    "seq": { "type": "integer", "minimum": 0 },
                    "timestamp": { "type": "integer", "minimum": 0 },
                    "data": {
                        "type": "object",
                        "required": ["markets"],
                        "properties": {
                            "markets": {
                                "type": "array",
                                "items": { "$ref": "#/$defs/MarketSummary" }
                            }
                        }
                    }
                },
                "required": ["type", "channel", "seq", "timestamp", "data"]
            },
            {
                "title": "Account envelope (fills, orders, balances for an authenticated account)",
                "properties": {
                    "type": { "enum": ["fill", "order_update", "balance_update", "account_snapshot"] },
                    "channel": { "type": "string", "pattern": "^account:0x[0-9a-fA-F]{40}$" },
                    "seq": { "type": "integer", "minimum": 0 },
                    "timestamp": { "type": "integer", "minimum": 0 },
                    "data": { "type": "object" }
                },
                "required": ["type", "channel", "seq", "timestamp", "data"]
            },
            {
                "title": "Drop-copy fill (fills-only, mirrored from account channel)",
                "properties": {
                    "type": { "const": "fill" },
                    "channel": { "type": "string", "pattern": "^dropcopy:0x[0-9a-fA-F]{40}$" },
                    "seq": { "type": "integer", "minimum": 0 },
                    "timestamp": { "type": "integer", "minimum": 0 },
                    "data": { "type": "object" }
                },
                "required": ["type", "channel", "seq", "timestamp", "data"]
            },
            {
                "title": "Bare server message (auth handshake / errors / pong)",
                "properties": {
                    "type": { "enum": ["subscribed", "challenge", "authenticated", "error", "pong"] }
                },
                "required": ["type"]
            }
        ],
        "$defs": {
            "PriceLevel": {
                "type": "array",
                "prefixItems": [
                    { "type": "string", "description": "Price as decimal string in native units" },
                    { "type": "string", "description": "Quantity as decimal string in native units" }
                ],
                "minItems": 2,
                "maxItems": 2
            },
            "PriceLevelArray": {
                "type": "array",
                "items": { "$ref": "#/$defs/PriceLevel" }
            },
            "TradeData": {
                "type": "object",
                "required": ["id", "market_id", "price", "quantity", "side", "timestamp"],
                "properties": {
                    "id": { "type": "string" },
                    "market_id": { "type": "string" },
                    "price": { "type": "string" },
                    "quantity": { "type": "string" },
                    "side": { "enum": ["bid", "ask", "buy", "sell"] },
                    "maker_order_id": { "type": "integer" },
                    "taker_order_id": { "type": "integer" },
                    "maker_address": { "type": "string" },
                    "taker_address": { "type": "string" },
                    "timestamp": { "type": "integer" }
                }
            },
            "MarketSummary": {
                "type": "object",
                "required": ["id", "base", "quote"],
                "properties": {
                    "id": { "type": "string" },
                    "base": { "type": "string" },
                    "quote": { "type": "string" },
                    "best_bid": { "type": ["string", "null"] },
                    "best_ask": { "type": ["string", "null"] },
                    "spread": { "type": ["string", "null"] }
                }
            }
        }
    })
}

/// Serve the schema JSON.
pub async fn schema_handler() -> impl IntoResponse {
    Json(ws_schema())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_valid_json() {
        let v = ws_schema();
        assert!(v.is_object());
        assert_eq!(v["schema_version"], SCHEMA_VERSION);
        assert_eq!(v["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert!(v["oneOf"].is_array());
        assert!(v["oneOf"].as_array().unwrap().len() >= 5);
        assert!(v["$defs"]["PriceLevel"].is_object());
        assert!(v["$defs"]["TradeData"].is_object());
    }
}
