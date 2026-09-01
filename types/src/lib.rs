use serde::{Deserialize, Serialize};
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
pub struct G1Affine(#[serde(with = "hex_serde::bytes48")] pub [u8; 48]);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetId(pub [u8; 16]);

impl AssetId {
    pub const fn from_str(s: &str) -> Self {
        let bytes = s.as_bytes();
        let mut arr = [0u8; 16];
        let len = if bytes.len() > 16 { 16 } else { bytes.len() };
        let mut i = 0;
        while i < len {
            arr[i] = bytes[i];
            i += 1;
        }
        AssetId(arr)
    }

    pub fn as_str(&self) -> &str {
        let end = self
            .0
            .iter()
            .rposition(|&b| b != 0)
            .map(|i| i + 1)
            .unwrap_or(0);
        std::str::from_utf8(&self.0[..end]).unwrap_or("")
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<&str> for AssetId {
    fn from(s: &str) -> Self {
        AssetId::from_str(s)
    }
}

impl From<String> for AssetId {
    fn from(s: String) -> Self {
        AssetId::from_str(&s)
    }
}

impl serde::Serialize for AssetId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for AssetId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(AssetId::from_str(&s))
    }
}

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

/// Behavior when an incoming taker order would match against a resting
/// order posted by the same user.
///
/// Default is `None`, which preserves the historical behavior (silently
/// skip the self-match and continue). Configurable per-order so a
/// market-making desk can pick the policy that matches its compliance
/// or strategy requirements.
///
/// `CancelTaker` and `CancelBoth` short-circuit matching entirely and
/// leave the taker order marked `OrderStatus::Canceled`. `CancelMaker`
/// removes the resting order and lets the taker continue matching
/// against the next order at that level or the next price level.
/// `DecrementAndCancel` cancels whichever order has the smaller
/// remaining quantity and decrements the other; if both remaining are
/// equal it cancels both (v1: when the taker is strictly smaller than
/// the maker we cancel both to avoid a fill-less partial-decrement of
/// the maker, which downstream systems don't yet model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SelfTradePreventionMode {
    #[default]
    None,
    CancelTaker,
    CancelMaker,
    CancelBoth,
    DecrementAndCancel,
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
    /// Self-trade prevention policy for this order. Defaults to `None`
    /// (skip self-matches silently) for wire back-compat.
    #[serde(default)]
    pub stp: SelfTradePreventionMode,
    /// If set, the total filled quantity across the initial dispatch
    /// must be at least this large or the order is rejected atomically
    /// (all delta writes rolled back).
    #[serde(default)]
    pub min_quantity: Option<Quantity>,
    /// Iceberg display quantity. When set, only `display_quantity` worth
    /// of the order shows up in public depth queries; the rest is hidden.
    /// Matching still uses the full remaining quantity, so a large taker
    /// can consume the hidden reserve in one sweep. Must satisfy
    /// `0 < display_quantity <= quantity` when present.
    #[serde(default)]
    pub display_quantity: Option<Quantity>,
}

impl Order {
    pub fn remaining_quantity(&self) -> Quantity {
        self.quantity.saturating_sub(self.filled_quantity)
    }

    pub fn is_fully_filled(&self) -> bool {
        self.filled_quantity >= self.quantity
    }

    /// Quantity that should appear in public depth queries.
    ///
    /// For a regular order this is just `remaining_quantity()`. For an
    /// iceberg order it's `min(display_quantity, remaining_quantity)`,
    /// which naturally refills as fills consume the visible slice: after
    /// each partial fill the newly-computed visible amount is the same
    /// display size until the remaining reserve drops below it.
    pub fn visible_quantity(&self) -> Quantity {
        let remaining = self.remaining_quantity();
        match self.display_quantity {
            Some(display) if display > 0 => display.min(remaining),
            _ => remaining,
        }
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

mod open_order_ids_serde {
    use super::OrderId;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(arr: &[OrderId; 64], serializer: S) -> Result<S::Ok, S::Error> {
        let v: Vec<OrderId> = arr.iter().copied().filter(|&id| id != 0).collect();
        v.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[OrderId; 64], D::Error> {
        let v = Vec::<OrderId>::deserialize(deserializer)?;
        let mut arr = [0u64; 64];
        for (i, &id) in v.iter().take(64).enumerate() {
            arr[i] = id;
        }
        Ok(arr)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMetadata {
    pub user: UserId,
    pub nonce_window: NonceWindow,
    #[serde(with = "open_order_ids_serde")]
    pub open_order_ids: [OrderId; 64],
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
    /// Volume-based fee tier (0 = default, higher = better rebates).
    /// Recomputed periodically by the api layer from a rolling 30-day
    /// volume window and cached here so the hot-path fee application
    /// stays O(1). See `types::fee_tiers` for the schedule.
    #[serde(default)]
    pub fee_tier: u8,
}

/// Volume-based fee tier schedule.
///
/// Applied by the matching engine at fill time in place of
/// `market.maker_fee_bps` / `market.taker_fee_bps` when the user's
/// cached `fee_tier` is non-zero. Tier 0 is unchanged from the
/// market defaults so pre-tier users keep seeing the same fees.
///
/// Thresholds are 30-day USDC volume in fixed-point 1e6 (so
/// 10_000_000 * 1_000_000 = 10M USDC).
pub mod fee_tiers {
    pub const TIER_COUNT: usize = 4;

    /// Minimum 30-day USDC volume (× 1e6) required for each tier.
    pub const THRESHOLDS_MICRO: [u64; TIER_COUNT] = [
        0,
        10_000_000_000_000,    // 10M USDC
        100_000_000_000_000,   // 100M USDC
        1_000_000_000_000_000, // 1B USDC
    ];

    /// Maker fee in basis points per tier. Negative = rebate.
    pub const MAKER_BPS: [i64; TIER_COUNT] = [-1, -2, -3, -4];

    /// Taker fee in basis points per tier.
    pub const TAKER_BPS: [i64; TIER_COUNT] = [5, 4, 3, 2];

    /// Look up the tier index for a 30-day volume, saturating at the
    /// highest tier the volume qualifies for.
    pub fn tier_for_volume(volume_micro: u64) -> u8 {
        let mut tier = 0u8;
        for (i, threshold) in THRESHOLDS_MICRO.iter().enumerate() {
            if volume_micro >= *threshold {
                tier = i as u8;
            }
        }
        tier
    }

    /// (maker_bps, taker_bps) for a tier, saturating at the top tier
    /// if a corrupt-high value comes back from storage.
    pub fn fees_for_tier(tier: u8) -> (i64, i64) {
        let idx = (tier as usize).min(TIER_COUNT - 1);
        (MAKER_BPS[idx], TAKER_BPS[idx])
    }
}

impl UserMetadata {
    pub fn push_order_id(&mut self, id: OrderId) {
        if let Some(slot) = self.open_order_ids.iter_mut().find(|&&mut s| s == 0) {
            *slot = id;
        }
    }

    pub fn remove_order_id(&mut self, id: OrderId) {
        if let Some(slot) = self.open_order_ids.iter_mut().find(|&&mut s| s == id) {
            *slot = 0;
        }
    }

    pub fn iter_order_ids(&self) -> impl Iterator<Item = OrderId> + '_ {
        self.open_order_ids.iter().copied().filter(|&id| id != 0)
    }

    pub fn order_id_count(&self) -> usize {
        self.open_order_ids.iter().filter(|&&id| id != 0).count()
    }

    pub fn contains_order_id(&self, id: OrderId) -> bool {
        self.open_order_ids.contains(&id)
    }
}

/// Fixed-size ring buffer for replay-protection nonces.
///
/// Stores the last `NONCE_WINDOW_SIZE` accepted nonces in a fixed array.
/// When full, the oldest entry is evicted before inserting the new one.
/// A nonce is rejected if it is already present or (when full) not strictly
/// greater than the minimum nonce in the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonceWindow {
    window: [Nonce; NONCE_WINDOW_SIZE],
    len: usize,
}

impl NonceWindow {
    pub fn new() -> Self {
        NonceWindow {
            window: [0u64; NONCE_WINDOW_SIZE],
            len: 0,
        }
    }

    pub fn accept(&mut self, nonce: Nonce) -> bool {
        if self.len < NONCE_WINDOW_SIZE {
            // Window not yet full — reject duplicates only.
            if self.window[..self.len].contains(&nonce) {
                return false;
            }
            self.window[self.len] = nonce;
            self.len += 1;
            return true;
        }

        // Window full: find minimum and enforce monotonic advance.
        let (min_idx, &min_val) = self
            .window
            .iter()
            .enumerate()
            .min_by_key(|(_, &v)| v)
            .unwrap();

        if nonce <= min_val || self.window.contains(&nonce) {
            return false;
        }

        // Evict oldest (minimum) entry and insert new nonce in its slot.
        self.window[min_idx] = nonce;
        true
    }

    pub fn min(&self) -> Option<Nonce> {
        if self.len == 0 {
            None
        } else {
            self.window[..self.len].iter().copied().min()
        }
    }

    pub fn contains(&self, nonce: Nonce) -> bool {
        let active = if self.len < NONCE_WINDOW_SIZE {
            &self.window[..self.len]
        } else {
            &self.window
        };
        active.contains(&nonce)
    }

    pub fn iter_active(&self) -> impl Iterator<Item = Nonce> + '_ {
        let active = if self.len < NONCE_WINDOW_SIZE {
            &self.window[..self.len]
        } else {
            &self.window
        };
        active.iter().copied()
    }

    /// Merge `other` into `self`: add every nonce in `other` that is not
    /// already in `self`.
    ///
    /// Used in the phase-3 shard-delta fold to ensure that nonces accepted by
    /// different shards for the same user are all preserved, regardless of
    /// iteration order.
    ///
    /// When the union of both windows exceeds `NONCE_WINDOW_SIZE`, the
    /// smallest-valued (oldest) nonces are evicted — the same policy as
    /// `accept()`.  The result is always a valid `NonceWindow`.
    pub fn merge(&mut self, other: &NonceWindow) {
        let self_active: &[Nonce] = if self.len < NONCE_WINDOW_SIZE {
            &self.window[..self.len]
        } else {
            &self.window
        };
        let other_active: &[Nonce] = if other.len < NONCE_WINDOW_SIZE {
            &other.window[..other.len]
        } else {
            &other.window
        };

        // Collect the union into a stack buffer (at most 2 × NONCE_WINDOW_SIZE entries).
        let mut combined = [0u64; NONCE_WINDOW_SIZE * 2];
        let mut combined_len = 0usize;

        for &n in self_active {
            combined[combined_len] = n;
            combined_len += 1;
        }
        for &n in other_active {
            if !combined[..combined_len].contains(&n) {
                combined[combined_len] = n;
                combined_len += 1;
            }
        }

        if combined_len <= NONCE_WINDOW_SIZE {
            // Fits — store all entries.
            self.window = [0u64; NONCE_WINDOW_SIZE];
            self.len = combined_len;
            for (i, &n) in combined[..combined_len].iter().enumerate() {
                self.window[i] = n;
            }
        } else {
            // Overflow: keep the NONCE_WINDOW_SIZE largest nonces.
            // Evicting the smallest is consistent with accept()'s eviction
            // policy, and prevents the floor from being lowered by a merge.
            combined[..combined_len].sort_unstable();
            let keep_start = combined_len - NONCE_WINDOW_SIZE;
            self.window = [0u64; NONCE_WINDOW_SIZE];
            self.len = NONCE_WINDOW_SIZE;
            for (i, &n) in combined[keep_start..combined_len].iter().enumerate() {
                self.window[i] = n;
            }
        }
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

fn default_maker_fee_bps() -> i64 {
    -1
}
fn default_taker_fee_bps() -> i64 {
    5
}

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
    #[serde(default)]
    pub stp: SelfTradePreventionMode,
    #[serde(default)]
    pub min_quantity: Option<Quantity>,
    #[serde(default)]
    pub display_quantity: Option<Quantity>,
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
    /// A `SelfTradePreventionMode::CancelTaker` or `CancelBoth` policy
    /// short-circuited the incoming order.
    StpTakerCanceled,
    /// `min_quantity` was set on the order and the total filled quantity
    /// across the initial dispatch did not meet the threshold.
    MinQuantityNotMet,
    /// `display_quantity` was zero, negative, or greater than the order
    /// quantity.
    InvalidDisplayQuantity,
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
    #[error("self-trade prevention canceled the taker order")]
    StpTakerCanceled,
    #[error("min_quantity not met: filled {filled}, minimum {min}")]
    MinQuantityNotMet { filled: u64, min: u64 },
    #[error("display_quantity must be > 0 and <= quantity")]
    InvalidDisplayQuantity,
    #[error("internal error: {0}")]
    Internal(String),
}

/// Aggregated result produced by one batch dispatch window.
///
/// Created by [`engine::BatchDispatcher`] after processing all requests in a
/// batch. The `CommitBatch` in the committer layer should be constructed from
/// this so one dispatch window → one DA commit entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    /// Flattened responses from all requests in the batch, in submission order.
    pub responses: Vec<Response>,
    /// Number of requests in this batch.
    pub batch_size: usize,
    /// Wall-clock time from window open (first arrival) to delta commit, in nanoseconds.
    pub dispatch_latency_ns: u64,
    /// Decryption proofs attached to TEOB (threshold-encrypted) orders in this batch.
    pub decryption_proofs: Vec<DecryptionProof>,
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
            VelaError::StpTakerCanceled => ErrorCode::StpTakerCanceled,
            VelaError::MinQuantityNotMet { .. } => ErrorCode::MinQuantityNotMet,
            VelaError::InvalidDisplayQuantity => ErrorCode::InvalidDisplayQuantity,
            VelaError::InvalidAddress | VelaError::Internal(_) => ErrorCode::InternalError,
        };
        ErrorResponse {
            code,
            message: e.to_string(),
        }
    }
}

#[cfg(test)]
mod fee_tier_tests {
    use super::fee_tiers::*;

    #[test]
    fn tier_zero_below_threshold() {
        assert_eq!(tier_for_volume(0), 0);
        assert_eq!(tier_for_volume(THRESHOLDS_MICRO[1] - 1), 0);
    }

    #[test]
    fn tier_boundaries() {
        assert_eq!(tier_for_volume(THRESHOLDS_MICRO[1]), 1);
        assert_eq!(tier_for_volume(THRESHOLDS_MICRO[2]), 2);
        assert_eq!(tier_for_volume(THRESHOLDS_MICRO[3]), 3);
    }

    #[test]
    fn tier_saturates_at_top() {
        assert_eq!(tier_for_volume(u64::MAX), (TIER_COUNT - 1) as u8);
    }

    #[test]
    fn tier_fees_monotonic() {
        // Maker rebate strictly increases with tier (more negative bps).
        for i in 1..TIER_COUNT {
            assert!(
                MAKER_BPS[i] < MAKER_BPS[i - 1],
                "maker rebate must improve with tier"
            );
            assert!(
                TAKER_BPS[i] < TAKER_BPS[i - 1],
                "taker fee must decrease with tier"
            );
        }
    }

    #[test]
    fn fees_for_tier_clamps_out_of_range() {
        let (mb0, tb0) = fees_for_tier(0);
        assert_eq!(mb0, MAKER_BPS[0]);
        assert_eq!(tb0, TAKER_BPS[0]);
        // Corrupt-high tier value clamps to top tier, doesn't panic.
        let (mb_top, tb_top) = fees_for_tier(255);
        assert_eq!(mb_top, MAKER_BPS[TIER_COUNT - 1]);
        assert_eq!(tb_top, TAKER_BPS[TIER_COUNT - 1]);
    }
}
