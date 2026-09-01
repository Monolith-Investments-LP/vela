//! Session keys / agent wallets.
//!
//! A "master" wallet delegates trading authority to an ephemeral "agent"
//! wallet by signing a delegation message. The agent then signs
//! subsequent orders on behalf of the master, so traders don't have to
//! reach for MetaMask on every click. Sub-microsecond match latency is
//! invisible when a personal_sign popup gates every order.
//!
//! Model mirrors what Hyperliquid, dYdX v4, and Aevo ship today:
//!
//! - Each delegation is scoped: expires at a chosen timestamp, capped
//!   at a per-order notional in USDC (fixed-point 1e6).
//! - Revocation is either a signed revoke message from the master or
//!   simply letting the expiry pass.
//! - Master can always sign directly; the agent is additive, not
//!   replacement.
//!
//! v1 keeps the registry in memory only. Restart clears all agents and
//! users must re-authorize. Persistence via snapshot / WAL is a v2
//! follow-up.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use types::{MarketId, OrderSide, OrderType, UserId, VelaError};

use crate::auth::recover_signer;

/// Per-order notional cap is stored as USDC in fixed-point 1e6.
pub type NotionalMicro = u64;

/// Rich capability scope attached to a delegation. Every field defaults
/// to "no restriction" so an old-style delegation (only notional cap +
/// expiry) is expressible as the default `CapabilityScope`. Serialized
/// on the wire; the delegation-signing message includes a stable hash
/// of the scope so the master signs the *specific* capabilities being
/// granted, not an unbounded pointer to a mutable object.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilityScope {
    /// If set, orders may only be placed on these markets. Empty vec
    /// treated the same as `None` (unrestricted).
    #[serde(default)]
    pub allowed_markets: Option<Vec<MarketId>>,
    /// If set, orders must be one of these types.
    #[serde(default)]
    pub allowed_order_types: Option<Vec<OrderType>>,
    /// If set, orders must be on one of these sides (usually one of
    /// `[Bid]` for buy-only agents, or `[Ask]` for sell-only).
    #[serde(default)]
    pub allowed_sides: Option<Vec<OrderSide>>,
    /// Rolling per-hour notional cap in USDC × 1e6. Consumed by every
    /// order submitted through this delegation. Falls off after 3600s.
    #[serde(default)]
    pub max_notional_per_hour: Option<NotionalMicro>,
    /// Rolling per-day notional cap in USDC × 1e6.
    #[serde(default)]
    pub max_notional_per_day: Option<NotionalMicro>,
}

impl CapabilityScope {
    /// Stable hash of the scope, so the master's registration signature
    /// covers the specific capabilities being granted. keccak256 of the
    /// canonical JSON encoding.
    pub fn hash_hex(&self) -> String {
        use sha3::{Digest, Keccak256};
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        let h = Keccak256::digest(bytes);
        hex::encode(h)
    }

    /// Check a single order against the scope. Returns Ok if allowed,
    /// Err with a specific reason string if any restriction fires.
    /// Rate-limit checks live in `check_rate_and_record` because they
    /// mutate the running counter and shouldn't be duplicated.
    pub fn check_order_static(
        &self,
        market: &MarketId,
        side: OrderSide,
        order_type: OrderType,
    ) -> Result<(), &'static str> {
        if let Some(markets) = &self.allowed_markets {
            if !markets.is_empty() && !markets.iter().any(|m| m == market) {
                return Err("market not in allowed_markets");
            }
        }
        if let Some(types) = &self.allowed_order_types {
            if !types.is_empty() && !types.iter().any(|t| *t == order_type) {
                return Err("order_type not in allowed_order_types");
            }
        }
        if let Some(sides) = &self.allowed_sides {
            if !sides.is_empty() && !sides.iter().any(|s| *s == side) {
                return Err("side not in allowed_sides");
            }
        }
        Ok(())
    }
}

/// A single active delegation from `master` to `agent`.
#[derive(Debug, Clone)]
pub struct AgentDelegation {
    pub master: UserId,
    pub agent: UserId,
    /// Unix milliseconds after which the delegation is no longer valid.
    pub expires_at_ms: u64,
    /// Maximum notional per order this agent is allowed to submit,
    /// denominated in USDC × 1e6.
    pub max_notional_per_order: NotionalMicro,
    /// Set true when a signed revoke message from the master lands.
    /// Checked before expiry so a revoked-but-not-yet-expired agent
    /// stops working immediately.
    pub revoked: bool,
    /// Registration nonce; prevents delegation-replay attacks and
    /// dedupes duplicate registration attempts.
    pub nonce: u64,
    /// Optional richer capability grammar. Empty scope preserves the
    /// old behavior (only `max_notional_per_order` + expiry apply).
    #[allow(dead_code)]
    pub scope: CapabilityScope,
}

/// Concurrent map: agent address → delegation. Master → agents lookup
/// scans the map; the beta expects at most a handful of agents per
/// master so an O(N) scan is fine.
///
/// Additional per-agent rolling notional counters live in `rate` so we
/// can enforce `max_notional_per_hour` / `max_notional_per_day` without
/// touching the main `inner` map on every order.
#[derive(Default)]
pub struct AgentRegistry {
    inner: DashMap<UserId, AgentDelegation>,
    /// (agent_address, bucket_seconds, bucket_id) → cumulative notional in
    /// that bucket. bucket_seconds is 3600 for hourly, 86400 for daily.
    rate: DashMap<(UserId, u64, u64), AtomicU64>,
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: DashMap::new(),
            rate: DashMap::new(),
        })
    }

    /// Try to record `notional` against the agent's hourly + daily
    /// buckets, returning `Ok` iff both caps (when set) still fit.
    /// On any cap breach, the write is rolled back so the counter does
    /// not credit a would-be over-limit order.
    pub fn check_rate_and_record(
        &self,
        agent: &UserId,
        scope: &CapabilityScope,
        notional: NotionalMicro,
        now_ms: u64,
    ) -> Result<(), &'static str> {
        if scope.max_notional_per_hour.is_none() && scope.max_notional_per_day.is_none() {
            return Ok(());
        }
        let hour_bucket = now_ms / 1000 / 3_600;
        let day_bucket = now_ms / 1000 / 86_400;

        // Tentatively add to both buckets, check both, roll back if any
        // breach. Two-phase so a partial write on a breach doesn't hurt.
        if let Some(cap) = scope.max_notional_per_hour {
            let key = (agent.clone(), 3_600u64, hour_bucket);
            let entry = self.rate.entry(key).or_insert_with(|| AtomicU64::new(0));
            let new_val = entry.fetch_add(notional, Ordering::Relaxed) + notional;
            if new_val > cap {
                // Roll back before returning.
                entry.fetch_sub(notional, Ordering::Relaxed);
                return Err("hourly notional cap exceeded");
            }
        }
        if let Some(cap) = scope.max_notional_per_day {
            let key = (agent.clone(), 86_400u64, day_bucket);
            let entry = self.rate.entry(key).or_insert_with(|| AtomicU64::new(0));
            let new_val = entry.fetch_add(notional, Ordering::Relaxed) + notional;
            if new_val > cap {
                entry.fetch_sub(notional, Ordering::Relaxed);
                // Also roll back the hourly credit on failure to keep
                // both counters consistent.
                if scope.max_notional_per_hour.is_some() {
                    let hkey = (agent.clone(), 3_600u64, hour_bucket);
                    if let Some(h) = self.rate.get(&hkey) {
                        h.fetch_sub(notional, Ordering::Relaxed);
                    }
                }
                return Err("daily notional cap exceeded");
            }
        }
        Ok(())
    }

    pub fn register(&self, d: AgentDelegation) {
        self.inner.insert(d.agent.clone(), d);
    }

    pub fn revoke(&self, agent: &UserId) -> bool {
        if let Some(mut e) = self.inner.get_mut(agent) {
            e.revoked = true;
            true
        } else {
            false
        }
    }

    /// Look up an agent by its address. Returns `None` if unknown,
    /// revoked, or expired.
    pub fn get_active(&self, agent: &UserId, now_ms: u64) -> Option<AgentDelegation> {
        self.inner
            .get(agent)
            .filter(|d| !d.revoked && d.expires_at_ms > now_ms)
            .map(|d| d.clone())
    }

    pub fn agents_for(&self, master: &UserId) -> Vec<AgentDelegation> {
        self.inner
            .iter()
            .filter(|d| d.master == *master)
            .map(|d| d.clone())
            .collect()
    }
}

/// Signing message the master signs to register an agent.
///
/// v2 (Tier 3.2): includes the scope hash so the master's signature
/// binds to the specific capability grammar being granted. Old-style
/// delegations without richer scope pass `CapabilityScope::default()`
/// whose hash is stable and derivable client-side, preserving
/// interoperability.
pub fn delegation_signing_message(
    agent: &UserId,
    expires_at_ms: u64,
    max_notional_micro: NotionalMicro,
    nonce: u64,
    scope: &CapabilityScope,
) -> Vec<u8> {
    format!(
        "vela:agent:register:0x{}:{}:{}:{}:{}",
        hex::encode(agent.0),
        expires_at_ms,
        max_notional_micro,
        nonce,
        scope.hash_hex()
    )
    .into_bytes()
}

/// Signing message the master signs to revoke an agent.
pub fn revocation_signing_message(agent: &UserId, nonce: u64) -> Vec<u8> {
    format!("vela:agent:revoke:0x{}:{}", hex::encode(agent.0), nonce).into_bytes()
}

/// Optional scope context for a specific order. If provided, the agent
/// path additionally enforces `CapabilityScope` restrictions (allowed
/// markets, order types, sides) and rolling notional caps (per-hour,
/// per-day). Cancels and non-order calls pass `None` to skip.
#[derive(Debug, Clone)]
pub struct OrderScopeCheck<'a> {
    pub market: &'a MarketId,
    pub side: OrderSide,
    pub order_type: OrderType,
}

/// Verify an order signature against either the master account or a
/// currently-active agent for that master. When `order_scope` is
/// provided and the signer is an agent, the delegation's
/// `CapabilityScope` is enforced (allow-listed markets / order types /
/// sides + rolling notional caps).
pub fn verify_master_or_agent(
    message: &[u8],
    signature_hex: &str,
    expected_master_hex: &str,
    order_notional_micro: NotionalMicro,
    now_ms: u64,
    registry: &AgentRegistry,
    order_scope: Option<&OrderScopeCheck<'_>>,
) -> Result<UserId, VelaError> {
    let signer = recover_signer(message, signature_hex)?;
    let expected =
        UserId::from_hex(expected_master_hex).map_err(|_| VelaError::InvalidSignature)?;

    // Fast path: master signed directly.
    if signer == expected {
        return Ok(signer);
    }

    // Agent path: signer must be a registered, non-revoked, unexpired
    // agent whose master matches, and the order must be within the cap.
    let delegation = registry
        .get_active(&signer, now_ms)
        .ok_or(VelaError::InvalidSignature)?;
    if delegation.master != expected {
        return Err(VelaError::InvalidSignature);
    }
    if order_notional_micro > delegation.max_notional_per_order {
        return Err(VelaError::InvalidSignature);
    }
    // Scope + rate checks apply only to order-type calls.
    if let Some(scope_ctx) = order_scope {
        if delegation
            .scope
            .check_order_static(scope_ctx.market, scope_ctx.side, scope_ctx.order_type)
            .is_err()
        {
            return Err(VelaError::InvalidSignature);
        }
        if registry
            .check_rate_and_record(&signer, &delegation.scope, order_notional_micro, now_ms)
            .is_err()
        {
            return Err(VelaError::InvalidSignature);
        }
    }
    Ok(signer)
}

pub async fn verify_master_or_agent_async(
    message: Vec<u8>,
    signature: String,
    expected: String,
    order_notional_micro: NotionalMicro,
    now_ms: u64,
    registry: Arc<AgentRegistry>,
) -> Result<UserId, VelaError> {
    tokio::task::spawn_blocking(move || {
        verify_master_or_agent(
            &message,
            &signature,
            &expected,
            order_notional_micro,
            now_ms,
            &registry,
            None,
        )
    })
    .await
    .map_err(|_| VelaError::InvalidSignature)?
}

/// Async variant that also enforces the agent's `CapabilityScope`
/// against the incoming order (allow-listed markets / order types /
/// sides + hourly / daily notional caps). Used by `post_order`.
pub async fn verify_master_or_agent_scoped_async(
    message: Vec<u8>,
    signature: String,
    expected: String,
    order_notional_micro: NotionalMicro,
    now_ms: u64,
    registry: Arc<AgentRegistry>,
    market: MarketId,
    side: OrderSide,
    order_type: OrderType,
) -> Result<UserId, VelaError> {
    tokio::task::spawn_blocking(move || {
        let scope = OrderScopeCheck {
            market: &market,
            side,
            order_type,
        };
        verify_master_or_agent(
            &message,
            &signature,
            &expected,
            order_notional_micro,
            now_ms,
            &registry,
            Some(&scope),
        )
    })
    .await
    .map_err(|_| VelaError::InvalidSignature)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;
    use sha3::{Digest, Keccak256};

    /// Deterministic keypair from a fixed seed; return (SigningKey, address).
    fn key_addr(seed: u8) -> (SigningKey, UserId) {
        let seed_bytes = [seed; 32];
        let sk = SigningKey::from_slice(&seed_bytes).unwrap();
        let vk = sk.verifying_key();
        let pubkey = vk.to_encoded_point(false);
        let h = Keccak256::digest(&pubkey.as_bytes()[1..]);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&h[12..]);
        (sk, UserId(addr))
    }

    fn sign_eth(sk: &SigningKey, msg: &[u8]) -> String {
        use crate::auth::eth_message_hash;
        use k256::ecdsa::signature::hazmat::PrehashSigner;
        let hash = eth_message_hash(msg);
        let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
            sk.sign_prehash(&hash).unwrap();
        let mut bytes = Vec::with_capacity(65);
        bytes.extend_from_slice(sig.to_bytes().as_ref());
        bytes.push(recid.to_byte() + 27);
        format!("0x{}", hex::encode(bytes))
    }

    #[test]
    fn master_signature_always_accepted() {
        let (master_sk, master_addr) = key_addr(1);
        let registry = AgentRegistry::default();
        let msg = b"vela:order:BTC-USDC:bid:100000000:1000000:42";
        let sig = sign_eth(&master_sk, msg);
        let ok = verify_master_or_agent(
            msg,
            &sig,
            &format!("0x{}", hex::encode(master_addr.0)),
            0,
            0,
            &registry,
            None,
        );
        assert!(ok.is_ok(), "master signature must always verify");
    }

    #[test]
    fn active_agent_within_cap_accepted() {
        let (_, master_addr) = key_addr(2);
        let (agent_sk, agent_addr) = key_addr(3);
        let registry = AgentRegistry::default();
        registry.register(AgentDelegation {
            master: master_addr.clone(),
            agent: agent_addr.clone(),
            expires_at_ms: 10_000,
            max_notional_per_order: 500_000_000, // 500 USDC
            revoked: false,
            nonce: 1,
            scope: CapabilityScope::default(),
        });
        let msg = b"vela:order:BTC-USDC:bid:100000000:1000000:42";
        let sig = sign_eth(&agent_sk, msg);
        let ok = verify_master_or_agent(
            msg,
            &sig,
            &format!("0x{}", hex::encode(master_addr.0)),
            100_000_000, // 100 USDC, under cap
            5_000,       // before expiry
            &registry,
            None,
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn agent_over_notional_cap_rejected() {
        let (_, master_addr) = key_addr(4);
        let (agent_sk, agent_addr) = key_addr(5);
        let registry = AgentRegistry::default();
        registry.register(AgentDelegation {
            master: master_addr.clone(),
            agent: agent_addr.clone(),
            expires_at_ms: 10_000,
            max_notional_per_order: 100_000_000, // 100 USDC cap
            revoked: false,
            nonce: 1,
            scope: CapabilityScope::default(),
        });
        let msg = b"vela:order:BTC-USDC:bid:100000000:1000000:42";
        let sig = sign_eth(&agent_sk, msg);
        let err = verify_master_or_agent(
            msg,
            &sig,
            &format!("0x{}", hex::encode(master_addr.0)),
            500_000_000, // 500 USDC, over cap
            5_000,
            &registry,
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn expired_agent_rejected() {
        let (_, master_addr) = key_addr(6);
        let (agent_sk, agent_addr) = key_addr(7);
        let registry = AgentRegistry::default();
        registry.register(AgentDelegation {
            master: master_addr.clone(),
            agent: agent_addr.clone(),
            expires_at_ms: 1_000,
            max_notional_per_order: u64::MAX,
            revoked: false,
            nonce: 1,
            scope: CapabilityScope::default(),
        });
        let msg = b"vela:order:BTC-USDC:bid:100000000:1000000:42";
        let sig = sign_eth(&agent_sk, msg);
        let err = verify_master_or_agent(
            msg,
            &sig,
            &format!("0x{}", hex::encode(master_addr.0)),
            0,
            5_000, // after expiry
            &registry,
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn revoked_agent_rejected() {
        let (_, master_addr) = key_addr(8);
        let (agent_sk, agent_addr) = key_addr(9);
        let registry = AgentRegistry::default();
        registry.register(AgentDelegation {
            master: master_addr.clone(),
            agent: agent_addr.clone(),
            expires_at_ms: u64::MAX,
            max_notional_per_order: u64::MAX,
            revoked: false,
            nonce: 1,
            scope: CapabilityScope::default(),
        });
        assert!(registry.revoke(&agent_addr));

        let msg = b"vela:order:BTC-USDC:bid:100000000:1000000:42";
        let sig = sign_eth(&agent_sk, msg);
        let err = verify_master_or_agent(
            msg,
            &sig,
            &format!("0x{}", hex::encode(master_addr.0)),
            0,
            5_000,
            &registry,
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn agent_signing_for_wrong_master_rejected() {
        let (_, master_a) = key_addr(10);
        let (_, master_b) = key_addr(11);
        let (agent_sk, agent_addr) = key_addr(12);
        let registry = AgentRegistry::default();
        // Agent belongs to master_a.
        registry.register(AgentDelegation {
            master: master_a.clone(),
            agent: agent_addr.clone(),
            expires_at_ms: u64::MAX,
            max_notional_per_order: u64::MAX,
            revoked: false,
            nonce: 1,
            scope: CapabilityScope::default(),
        });

        let msg = b"vela:order:BTC-USDC:bid:100000000:1000000:42";
        let sig = sign_eth(&agent_sk, msg);
        // Try to trade on behalf of master_b.
        let err = verify_master_or_agent(
            msg,
            &sig,
            &format!("0x{}", hex::encode(master_b.0)),
            0,
            5_000,
            &registry,
            None,
        );
        assert!(
            err.is_err(),
            "agent cannot trade for a master it isn't delegated to"
        );
    }

    // -----------------------------------------------------------------
    // CapabilityScope tests (Tier 3.2)
    // -----------------------------------------------------------------

    #[test]
    fn scope_hash_is_stable() {
        // Same scope should hash the same across constructions; different
        // scopes must produce different hashes.
        let a = CapabilityScope {
            allowed_markets: Some(vec![MarketId::new("BTC", "USDC")]),
            ..Default::default()
        };
        let b = CapabilityScope {
            allowed_markets: Some(vec![MarketId::new("BTC", "USDC")]),
            ..Default::default()
        };
        let c = CapabilityScope {
            allowed_markets: Some(vec![MarketId::new("ETH", "USDC")]),
            ..Default::default()
        };
        assert_eq!(a.hash_hex(), b.hash_hex());
        assert_ne!(a.hash_hex(), c.hash_hex());
    }

    #[test]
    fn scope_market_allowlist_enforced() {
        let scope = CapabilityScope {
            allowed_markets: Some(vec![MarketId::new("BTC", "USDC")]),
            ..Default::default()
        };
        assert!(scope
            .check_order_static(
                &MarketId::new("BTC", "USDC"),
                OrderSide::Bid,
                OrderType::GoodTillCanceled
            )
            .is_ok());
        assert!(scope
            .check_order_static(
                &MarketId::new("ETH", "USDC"),
                OrderSide::Bid,
                OrderType::GoodTillCanceled
            )
            .is_err());
    }

    #[test]
    fn scope_order_type_and_side_gates() {
        let scope = CapabilityScope {
            allowed_order_types: Some(vec![OrderType::PostOnly]),
            allowed_sides: Some(vec![OrderSide::Ask]),
            ..Default::default()
        };
        assert!(scope
            .check_order_static(
                &MarketId::new("BTC", "USDC"),
                OrderSide::Ask,
                OrderType::PostOnly
            )
            .is_ok());
        // Wrong order type.
        assert!(scope
            .check_order_static(
                &MarketId::new("BTC", "USDC"),
                OrderSide::Ask,
                OrderType::ImmediateOrCancel
            )
            .is_err());
        // Wrong side.
        assert!(scope
            .check_order_static(
                &MarketId::new("BTC", "USDC"),
                OrderSide::Bid,
                OrderType::PostOnly
            )
            .is_err());
    }

    #[test]
    fn hourly_notional_cap_blocks_over_limit() {
        let registry = AgentRegistry::default();
        let (_, agent) = key_addr(30);
        let scope = CapabilityScope {
            max_notional_per_hour: Some(500), // 500 micro-USDC total per hour
            ..Default::default()
        };
        // Two orders of 200 each — fine.
        assert!(registry
            .check_rate_and_record(&agent, &scope, 200, 1_000_000_000)
            .is_ok());
        assert!(registry
            .check_rate_and_record(&agent, &scope, 200, 1_000_000_001)
            .is_ok());
        // Third order of 200 pushes us to 600, over the 500 cap.
        assert!(registry
            .check_rate_and_record(&agent, &scope, 200, 1_000_000_002)
            .is_err());
        // Rejected order must not have credited the counter (still 400).
        // A follow-up 100 should now fit.
        assert!(registry
            .check_rate_and_record(&agent, &scope, 100, 1_000_000_003)
            .is_ok());
    }

    #[test]
    fn hourly_bucket_rolls_over_after_3600s() {
        let registry = AgentRegistry::default();
        let (_, agent) = key_addr(31);
        let scope = CapabilityScope {
            max_notional_per_hour: Some(100),
            ..Default::default()
        };
        // Fill the bucket.
        assert!(registry
            .check_rate_and_record(&agent, &scope, 100, 0)
            .is_ok());
        assert!(registry
            .check_rate_and_record(&agent, &scope, 1, 0)
            .is_err());
        // Advance past the hour boundary (3600 seconds = 3_600_000 ms).
        assert!(registry
            .check_rate_and_record(&agent, &scope, 100, 3_600_001)
            .is_ok());
    }
}
