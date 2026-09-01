use serde::{Deserialize, Serialize};
use types::{OrderSide, OrderType, Price, Quantity};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFill {
    pub id: String,
    pub market_id: String,
    pub price: u64,
    pub quantity: u64,
    pub maker_order_id: u64,
    pub taker_order_id: u64,
    pub maker_address: String,
    pub taker_address: String,
    pub timestamp: u64,
    pub side: String,
    #[serde(default)]
    pub synthetic: bool,
    /// Toxicity score for the taker order this fill contributed to,
    /// in `[0.0, 1.0]`. Zero for synthetic / MM-bot fills that were not
    /// scored on the hot path. Used to gate points and leaderboard
    /// eligibility against wash-trading style behavior.
    #[serde(default)]
    pub toxicity_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderFillRecord {
    pub fill_id: String,
    pub counterparty_order_id: u64,
    pub counterparty_address: String,
    pub price: u64,
    pub quantity: u64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredOrder {
    pub id: u64,
    pub market_id: String,
    pub user: String,
    pub side: String,
    pub price: u64,
    pub quantity: u64,
    pub filled_quantity: u64,
    pub status: String,
    pub order_type: String,
    pub time_in_force: String,
    pub nonce: u64,
    pub client_order_id: Option<String>,
    pub signature: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub fills: Vec<OrderFillRecord>,
    pub da_hash: Option<String>,
    /// Optional keccak256 hash (0x-prefixed hex) of the agent's model
    /// reasoning trace at order-submission time. When present, links
    /// this order to a compliance-auditable trace stored in the
    /// caller's own S3 / private log. Vela commits the hash but does
    /// not store the trace itself.
    #[serde(default)]
    pub reasoning_trace_hash: Option<String>,
    /// Optional agent identifier string ("claude-opus-4.7",
    /// "gpt-5-turbo", "internal-mm-v3") the caller wants attached to
    /// audit records. Free-text; not verified.
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientMessage {
    Subscribe {
        channels: Vec<String>,
    },
    Unsubscribe {
        channels: Vec<String>,
    },
    RequestChallenge,
    Auth {
        address: String,
        signature: String,
        #[serde(default)]
        nonce: Option<String>,
        #[serde(default)]
        timestamp: Option<u64>,
    },
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsEnvelope {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub channel: String,
    pub seq: u64,
    pub data: serde_json::Value,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerMessage {
    Subscribed {
        channels: Vec<String>,
    },
    BookSnapshot {
        market: String,
        bids: Vec<[String; 2]>,
        asks: Vec<[String; 2]>,
    },
    Trade {
        market: String,
        price: String,
        quantity: String,
        side: String,
        timestamp: u64,
    },
    OrderUpdate {
        order_id: u64,
        status: String,
        filled_quantity: String,
    },
    Fill {
        maker_order_id: u64,
        taker_order_id: u64,
        price: String,
        quantity: String,
        side: String,
        maker_fee: String,
        taker_fee: String,
        timestamp: u64,
    },
    BalanceUpdate {
        asset: String,
        available: String,
        locked: String,
    },
    Challenge {
        nonce: String,
    },
    Authenticated {
        address: String,
    },
    Error {
        code: String,
        message: String,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostOrderBody {
    pub market: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: Price,
    pub quantity: Quantity,
    pub nonce: u64,
    pub client_order_id: Option<String>,
    pub address: String,
    pub signature: String,
    /// Optional 0x-prefixed 32-byte keccak256 hash of the agent's
    /// reasoning trace for this order. Vela records the hash in
    /// StoredOrder so post-hoc audit can prove the trace existed at
    /// submission time. The trace itself lives in the caller's own
    /// storage.
    #[serde(default)]
    pub reasoning_trace_hash: Option<String>,
    /// Optional model/agent identifier ("claude-opus-4.7", etc.).
    /// Free-text; not verified.
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderBody {
    pub order_id: Option<u64>,
    pub client_order_id: Option<String>,
    pub nonce: u64,
    pub address: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawBody {
    pub asset: String,
    pub amount: u64,
    pub nonce: u64,
    pub address: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositBody {
    pub user: String,
    pub asset: String,
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: String,
    pub quantity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookResponse {
    pub market: String,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketResponse {
    pub id: String,
    pub base: String,
    pub quote: String,
    pub best_bid: Option<String>,
    pub best_ask: Option<String>,
    pub spread: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceResponse {
    pub asset: String,
    pub available: String,
    pub locked: String,
    pub total: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        ApiResponse {
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> ApiResponse<()> {
        ApiResponse {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchSummary {
    pub batch_id: u64,
    pub timestamp: u64,
    pub fill_count: usize,
    pub order_count: usize,
    pub markets: Vec<String>,
    pub state_root: String,
    pub operator_signature: String,
    pub fills: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchDetail {
    pub batch_id: u64,
    pub timestamp: u64,
    pub fill_count: usize,
    pub order_count: usize,
    pub markets: Vec<String>,
    pub state_root: String,
    pub operator_signature: String,
    pub fills: Vec<StoredFill>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorRecord {
    pub anchor_id: u64,
    pub state_root: String,
    pub tx_hash: String,
    pub timestamp: u64,
    pub orders_processed: u64,
    pub block_number: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateRootData {
    pub state_root: String,
    pub timestamp: u64,
    pub order_count: usize,
    pub user_count: usize,
    pub block_number: Option<u64>,
    pub last_anchor_tx: Option<String>,
    pub last_anchor_time: Option<u64>,
    pub anchor_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incident {
    pub id: u64,
    pub incident_type: String,
    pub started_at: u64,
    pub resolved_at: Option<u64>,
    pub description: String,
    pub impact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    pub id: u64,
    pub decision_type: String,
    pub title: String,
    pub description: String,
    pub rationale: String,
    pub effective_date: u64,
    pub announced_at: u64,
    pub status: String,
    pub operator_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredMM {
    pub address: String,
    pub display_name: Option<String>,
    pub registered_at: u64,
    pub signature: String,
}

pub fn format_amount(raw: u64, decimals: u32) -> String {
    let scale = 10u64.pow(decimals);
    let whole = raw / scale;
    let frac = raw % scale;
    if frac == 0 {
        format!("{}", whole)
    } else {
        format!("{}.{:0>width$}", whole, frac, width = decimals as usize)
            .trim_end_matches('0')
            .to_string()
    }
}
