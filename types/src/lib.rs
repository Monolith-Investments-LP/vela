use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

// --------------------------------------------------------------------------
// BLS12-381 / TEOB types
// --------------------------------------------------------------------------

mod hex_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub mod bytes48 {
        use super::*;
        pub fn serialize<S: Serializer>(b: &[u8; 48], s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&hex::encode(b))
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 48], D::Error> {
            let s = String::deserialize(d)?;
            let v = hex::decode(s.strip_prefix("0x").unwrap_or(&s))
                .map_err(serde::de::Error::custom)?;
            if v.len() != 48 {
                return Err(serde::de::Error::custom("expected 48 bytes for G1Affine"));
            }
            let mut out = [0u8; 48];
            out.copy_from_slice(&v);
            Ok(out)
        }
    }

    pub mod bytes32 {
        use super::*;
        pub fn serialize<S: Serializer>(b: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&hex::encode(b))
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
            let s = String::deserialize(d)?;
            let v = hex::decode(s.strip_prefix("0x").unwrap_or(&s))
                .map_err(serde::de::Error::custom)?;
            if v.len() != 32 {
                return Err(serde::de::Error::custom("expected 32 bytes"));
            }
            let mut out = [0u8; 32];
            out.copy_from_slice(&v);
            Ok(out)
        }
    }

    pub mod bytes_vec {
        use super::*;
        pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&hex::encode(b))
        }
        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
            let s = String::deserialize(d)?;
            hex::decode(s.strip_prefix("0x").unwrap_or(&s)).map_err(serde::de::Error::custom)
        }
    }
}

/// Compressed BLS12-381 G1 affine point (48 bytes), hex-serialized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct G1Affine(
    #[serde(with = "hex_serde::bytes48")]
    pub [u8; 48],
);

/// Partial decryption share from one committee node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecryptionShare {
    pub node_index: u8,
    pub point: G1Affine,
}

/// ElGamal hybrid-encrypted order.
///
/// - `r`: ephemeral G1 point `r*G`
/// - `c`: ciphertext point `m*G + r*pk`  (m is a random key capsule)
/// - `order_hash`: SHA3-256 commitment to the plaintext order bytes
/// - `ciphertext`: order bytes XOR SHA3-CTR(SHA3-256(m*G))
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedOrder {
    pub r: G1Affine,
    pub c: G1Affine,
    #[serde(with = "hex_serde::bytes32")]
    pub order_hash: [u8; 32],
    #[serde(with = "hex_serde::bytes_vec")]
    pub ciphertext: Vec<u8>,
}

/// A threshold-decrypted order; a newtype over [`PostOrderRequest`].
///
/// After decryption the inner value can be submitted directly to the matching engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaintextOrder(pub PostOrderRequest);

/// Proof that threshold decryption was performed correctly and honestly.
///
/// Attests:
///   1. The [`EncryptedOrder`] was received at `t_recv_ms`.
///   2. Threshold was met and decryption completed at `t_decrypt_ms`.
///   3. `t_decrypt_ms >= t_recv_ms` (no pre-decryption).
///   4. The plaintext hashes to `order_hash`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecryptionProof {
    #[serde(with = "hex_serde::bytes32")]
    pub order_hash: [u8; 32],
    /// Unix timestamp (ms) when the encrypted order was received by the sequencer.
    pub t_recv_ms: u64,
    /// Unix timestamp (ms) when the t-th share arrived and decryption completed.
    pub t_decrypt_ms: u64,
    /// Batch sequence number in which this order was included.
    pub batch_seq: u64,
    /// Pre-computed validity flag (set by the verifier; false = fraud proof triggered).
    pub valid: bool,
}

impl DecryptionProof {
    /// Returns `true` iff the proof is well-formed:
    ///   - decryption did not precede submission
    ///   - the `valid` field (hash match etc.) is set
    pub fn is_valid(&self) -> bool {
        self.valid && self.t_decrypt_ms >= self.t_recv_ms
    }
}

/// Committee configuration (public parameters only — no secret material).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitteeConfig {
    /// Minimum number of shares required for decryption.
    pub t: u8,
    /// Total number of committee nodes.
    pub n: u8,
    /// Group public key (shared by all nodes).
    pub pub_key: G1Affine,
}

pub type Price = u64;
pub type Quantity = u64;
pub type OrderId = u64;
pub type Nonce = u64;
pub type Timestamp = u64;

pub const PRICE_DECIMALS: u32 = 8;
pub const QUANTITY_DECIMALS: u32 = 8;
pub const PRICE_SCALE: u64 = 10u64.pow(PRICE_DECIMALS);
pub const QUANTITY_SCALE: u64 = 10u64.pow(QUANTITY_DECIMALS);
pub const NONCE_WINDOW_SIZE: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub [u8; 20]);

impl UserId {
    pub fn from_hex(s: &str) -> Result<Self, VelaError> {
        let s = s.strip_prefix("0x").unwrap_or(s);
        let bytes = hex::decode(s).map_err(|_| VelaError::InvalidAddress)?;
        if bytes.len() != 20 {
            return Err(VelaError::InvalidAddress);
        }
        let mut arr = [0u8; 20];
        arr.copy_from_slice(&bytes);
        Ok(UserId(arr))
    }

    pub fn to_hex(&self) -> String {
        format!("0x{}", hex::encode(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MarketId(pub String);

impl MarketId {
    pub fn new(base: &str, quote: &str) -> Self {
        MarketId(format!("{}-{}", base, quote))
    }
}

impl std::fmt::Display for MarketId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Bid,
    Ask,
}

impl OrderSide {
    pub fn opposite(&self) -> Self {
        match self {
            OrderSide::Bid => OrderSide::Ask,
            OrderSide::Ask => OrderSide::Bid,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    GoodTillCanceled,
    PostOnly,
    ImmediateOrCancel,
    FillOrKill,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    Open,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub user: UserId,
    pub market: MarketId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: Price,
    pub quantity: Quantity,
    pub filled_quantity: Quantity,
    pub nonce: Nonce,
    pub client_order_id: Option<String>,
    pub timestamp: Timestamp,
    pub status: OrderStatus,
}

impl Order {
    pub fn remaining_quantity(&self) -> Quantity {
        self.quantity.saturating_sub(self.filled_quantity)
    }

    pub fn is_fully_filled(&self) -> bool {
        self.filled_quantity >= self.quantity
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub maker_order_id: OrderId,
    pub taker_order_id: OrderId,
    pub maker: UserId,
    pub taker: UserId,
    pub market: MarketId,
    pub side: OrderSide,
    pub price: Price,
    pub quantity: Quantity,
    pub maker_fee: i64,
    pub taker_fee: i64,
    pub timestamp: Timestamp,
    /// Adverse-selection toxicity score in [0.0, 1.0] for the taker order.
    /// 0.0 for non-taker fills (e.g., resting orders posted without a match).
    #[serde(default)]
    pub toxicity_score: f64,
    /// Signed OFI ring-buffer snapshot at the time the score was computed.
    #[serde(default)]
    pub ofi_snapshot: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub user: UserId,
    pub asset: AssetId,
    pub available: u64,
    pub locked: u64,
}

impl Balance {
    pub fn total(&self) -> u64 {
        self.available.saturating_add(self.locked)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMetadata {
    pub user: UserId,
    pub nonce_window: NonceWindow,
    pub open_order_ids: Vec<OrderId>,
    pub credit_ratio: f64,
    pub total_quoted_notional: u64,
    /// Actual quote-asset collateral: deposits minus fills consumed (excludes credit ghost).
    /// Used by the credit auto-cancel check to avoid ghost-balance inflation.
    pub actual_collateral: u64,
    #[serde(default)]
    pub ref_by: Option<String>,
    #[serde(default)]
    pub ref_earnings: u64,
    #[serde(default)]
    pub referred_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonceWindow {
    window: BTreeSet<Nonce>,
}

impl NonceWindow {
    pub fn new() -> Self {
        NonceWindow {
            window: BTreeSet::new(),
        }
    }

    pub fn accept(&mut self, nonce: Nonce) -> bool {
        if self.window.len() >= NONCE_WINDOW_SIZE {
            let min = match self.window.iter().next().copied() {
                Some(m) => m,
                None => return false,
            };
            if nonce <= min || self.window.contains(&nonce) {
                return false;
            }
            self.window.remove(&min);
        } else if self.window.contains(&nonce) {
            return false;
        }
        self.window.insert(nonce);
        true
    }

    pub fn min(&self) -> Option<Nonce> {
        self.window.iter().next().copied()
    }
}

impl Default for NonceWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeConfig {
    pub maker_fee_bps: i32,
    pub taker_fee_bps: i32,
}

impl Default for FeeConfig {
    fn default() -> Self {
        FeeConfig {
            maker_fee_bps: -1,
            taker_fee_bps: 5,
        }
    }
}

fn default_maker_fee_bps() -> i64 { -1 }
fn default_taker_fee_bps() -> i64 { 5 }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Market {
    pub id: MarketId,
    pub base: AssetId,
    pub quote: AssetId,
    pub max_orders: usize,
    pub min_order_size: Quantity,
    pub price_tick: Price,
    pub quantity_tick: Quantity,
    #[serde(default = "default_maker_fee_bps")]
    pub maker_fee_bps: i64,
    #[serde(default = "default_taker_fee_bps")]
    pub taker_fee_bps: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    PostOrder(PostOrderRequest),
    CancelOrder(CancelOrderRequest),
    Deposit(DepositRequest),
    Withdrawal(WithdrawalRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostOrderRequest {
    pub user: UserId,
    pub market: MarketId,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: Price,
    pub quantity: Quantity,
    pub nonce: Nonce,
    pub client_order_id: Option<String>,
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrderRequest {
    pub user: UserId,
    pub order_id: Option<OrderId>,
    pub client_order_id: Option<String>,
    pub nonce: Nonce,
    pub signature: Vec<u8>,
}

/// Credit a user's exchange balance from an on-chain deposit event.
///
/// # Deposit flow (L1 → exchange)
///
/// 1. **Lock on L1**: the user calls `deposit(asset, amount)` on the Vela
///    bridge contract.  The asset is transferred into the contract and an event
///    is emitted containing the L1 transaction hash.
/// 2. **Relayer picks up the event**: an off-chain relayer (or the sequencer
///    itself) observes the L1 event and constructs a `DepositRequest` with the
///    matching `l1_tx_hash` as proof.
/// 3. **Sequencer includes the request**: the `DepositRequest` is added to the
///    next batch.  The matching engine credits `amount` to `user`'s available
///    balance for `asset`.  Because `l1_tx_hash` uniquely identifies the L1
///    event, double-crediting is prevented.
/// 4. **State committed**: the new balance is committed to the MPT and posted
///    to the DA layer.  ZK / optimistic provers can verify the credit matches
///    the on-chain event.
///
/// Deposits may also be submitted via the forced-inclusion (delayed inbox) path
/// if the sequencer is censoring the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositRequest {
    pub user: UserId,
    pub asset: AssetId,
    pub amount: u64,
    /// Hash of the L1 transaction that locked the funds in the bridge contract.
    /// Acts as a unique nonce to prevent replay.
    pub l1_tx_hash: [u8; 32],
}

/// Initiate an on-chain settlement from the user's exchange balance.
///
/// # Withdrawal flow (exchange → L1)
///
/// 1. **User initiates**: the user signs a `WithdrawalRequest` and submits it
///    to the sequencer API.  The ECDSA `signature` covers `(user, asset,
///    amount, nonce)` so the sequencer can verify the request is authentic
///    without a round-trip to L1.
/// 2. **Sequencer deducts balance**: the matching engine checks `available ≥
///    amount`, deducts the balance, and includes the request in the next batch.
///    The `nonce` prevents replay.
/// 3. **State committed and proven**: the updated balance is committed to the
///    MPT.  Once the batch is either (a) past its 7-day optimistic challenge
///    window without dispute, or (b) covered by a fast-finality ZK proof, the
///    withdrawal is considered final from the L1 perspective.
/// 4. **L1 settlement**: a relayer (or the user directly) submits the
///    withdrawal proof to the Vela bridge contract.  The contract verifies the
///    MPT inclusion proof against the committed root and releases the funds to
///    the user's L1 address.
///
/// Fast-finality proofs (see `zkvm::OptimisticProver::request_fast_finality_proof`)
/// allow withdrawals to bypass the 7-day window, making the UX comparable to
/// a centralized exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalRequest {
    pub user: UserId,
    pub asset: AssetId,
    pub amount: u64,
    pub nonce: Nonce,
    /// ECDSA signature over `(user, asset, amount, nonce)`.
    pub signature: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    OrderPosted(OrderPostedResponse),
    OrderCanceled(OrderCanceledResponse),
    OrderFilled(Fill),
    BalanceUpdated(BalanceUpdatedResponse),
    Error(ErrorResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPostedResponse {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub status: OrderStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCanceledResponse {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BalanceUpdatedResponse {
    pub user: UserId,
    pub asset: AssetId,
    pub available: u64,
    pub locked: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    InvalidNonce,
    InsufficientBalance,
    CreditLimitExceeded,
    OrderNotFound,
    MarketNotFound,
    OrderBookFull,
    PostOnlyWouldMatch,
    FokNotFilled,
    InvalidSignature,
    InvalidMarket,
    InvalidPrice,
    InvalidQuantity,
    DuplicateClientOrderId,
    InvalidClientOrderId,
    InternalError,
}

#[derive(Debug, Error)]
pub enum VelaError {
    #[error("invalid address")]
    InvalidAddress,
    #[error("invalid nonce")]
    InvalidNonce,
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("credit limit exceeded")]
    CreditLimitExceeded,
    #[error("order not found")]
    OrderNotFound,
    #[error("market not found: {0}")]
    MarketNotFound(String),
    #[error("order book full")]
    OrderBookFull,
    #[error("post-only order would match")]
    PostOnlyWouldMatch,
    #[error("fill-or-kill order not fully filled")]
    FokNotFilled,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("duplicate client order id")]
    DuplicateClientOrderId,
    #[error("invalid client order id")]
    InvalidClientOrderId,
    #[error("internal error: {0}")]
    Internal(String),
}

impl From<VelaError> for ErrorResponse {
    fn from(e: VelaError) -> Self {
        let code = match &e {
            VelaError::InvalidNonce => ErrorCode::InvalidNonce,
            VelaError::InsufficientBalance => ErrorCode::InsufficientBalance,
            VelaError::CreditLimitExceeded => ErrorCode::CreditLimitExceeded,
            VelaError::OrderNotFound => ErrorCode::OrderNotFound,
            VelaError::MarketNotFound(_) => ErrorCode::MarketNotFound,
            VelaError::OrderBookFull => ErrorCode::OrderBookFull,
            VelaError::PostOnlyWouldMatch => ErrorCode::PostOnlyWouldMatch,
            VelaError::FokNotFilled => ErrorCode::FokNotFilled,
            VelaError::InvalidSignature => ErrorCode::InvalidSignature,
            VelaError::DuplicateClientOrderId => ErrorCode::DuplicateClientOrderId,
            VelaError::InvalidClientOrderId => ErrorCode::InvalidClientOrderId,
            VelaError::InvalidAddress | VelaError::Internal(_) => ErrorCode::InternalError,
        };
        ErrorResponse {
            code,
            message: e.to_string(),
        }
    }
}
