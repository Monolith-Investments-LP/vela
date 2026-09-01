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
use std::sync::Arc;
use types::{UserId, VelaError};

use crate::auth::recover_signer;

/// Per-order notional cap is stored as USDC in fixed-point 1e6.
pub type NotionalMicro = u64;

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
}

/// Concurrent map: agent address → delegation. Master → agents lookup
/// scans the map; the beta expects at most a handful of agents per
/// master so an O(N) scan is fine.
#[derive(Default)]
pub struct AgentRegistry {
    inner: DashMap<UserId, AgentDelegation>,
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: DashMap::new(),
        })
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
pub fn delegation_signing_message(
    agent: &UserId,
    expires_at_ms: u64,
    max_notional_micro: NotionalMicro,
    nonce: u64,
) -> Vec<u8> {
    format!(
        "vela:agent:register:0x{}:{}:{}:{}",
        hex::encode(agent.0),
        expires_at_ms,
        max_notional_micro,
        nonce
    )
    .into_bytes()
}

/// Signing message the master signs to revoke an agent.
pub fn revocation_signing_message(agent: &UserId, nonce: u64) -> Vec<u8> {
    format!("vela:agent:revoke:0x{}:{}", hex::encode(agent.0), nonce).into_bytes()
}

/// Verify an order signature against either the master account or a
/// currently-active agent for that master.
///
/// - `message`: the signed message bytes (order or cancel).
/// - `signature_hex`: 0x-prefixed 65-byte ECDSA signature.
/// - `expected_master_hex`: the address whose balance is being used.
/// - `order_notional_micro`: notional in USDC × 1e6, checked against
///   agent cap when agent signed. Pass `0` for cancels (no cap check).
/// - `now_ms`: current wall-clock for expiry checks.
/// - `registry`: agent registry.
///
/// Returns Ok(signer) where signer is either the master or the agent.
pub fn verify_master_or_agent(
    message: &[u8],
    signature_hex: &str,
    expected_master_hex: &str,
    order_notional_micro: NotionalMicro,
    now_ms: u64,
    registry: &AgentRegistry,
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
        );
        assert!(
            err.is_err(),
            "agent cannot trade for a master it isn't delegated to"
        );
    }
}
