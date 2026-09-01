//! MM credit vaults.
//!
//! LPs deposit USDC into a vault run by a whitelisted operator. The vault
//! has its own engine-side UserId and its own balance and open orders.
//! The operator trades using the vault's balance and its (per-vault)
//! credit ratio, and PnL naturally accrues to the vault's USDC balance,
//! and thus pro-rata to LP shares.
//!
//! Why Vela is the only exchange that can ship this cleanly
//! ---------------------------------------------------------
//! The 5× MM credit system is the substrate: operators can quote beyond
//! deposit, which is exactly what makes a credit-backed LP vault
//! competitive with HLP (Hyperliquid's public vault). Nobody else has
//! verifiable credit as an engine primitive, so nobody else can offer
//! this without rebuilding the credit machinery Vela already ships.
//!
//! Operator authorization via existing session-keys / agent-wallets
//! ----------------------------------------------------------------
//! At vault creation, Vela registers an AgentDelegation from
//! `vault.user_id → operator_address`, with the vault's own scope
//! (unlimited notional to allow full deployment of AUM). The operator
//! submits orders with `user = vault.user_id`; the existing agent path
//! verifies operator's signature and executes. No new sig path.
//!
//! v1 limitations, documented for follow-up
//! ----------------------------------------
//! - Solidity vault contract not yet shipped. Vault state is in-engine
//!   memory and survives via the normal snapshot / WAL path.
//! - No withdrawal queue delay. Withdrawals are instant. A real vault
//!   needs a delay to prevent LPs racing an operator's losing trade.
//! - No drawdown circuit breaker. A separate task can watch vault AUM
//!   deltas and freeze new deposits if drawdown > threshold.
//! - No operator bond / slashing. The operator can drain the vault by
//!   crossing spreads badly. Operator whitelisting is the current
//!   mitigation; a bond + slash pipeline is a v2 requirement.
//! - Fee-share to operator not implemented; PnL flows entirely to LP
//!   shares. Fee model TBD (typically 2/20 with high-water-mark).

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use types::UserId;

pub static NEXT_VAULT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_vault_id() -> u64 {
    NEXT_VAULT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Derive a stable UserId from a vault_id. First 20 bytes of
/// `keccak256("vela:vault:" + vault_id.to_be_bytes())`. The derived id
/// has no private key — the operator authorizes orders on the vault's
/// behalf via an AgentDelegation registered at vault creation.
pub fn derive_vault_user_id(vault_id: u64) -> UserId {
    let mut h = Keccak256::new();
    h.update(b"vela:vault:");
    h.update(vault_id.to_be_bytes());
    let hash = h.finalize();
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[..20]);
    UserId(addr)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vault {
    pub vault_id: u64,
    pub name: String,
    /// Operator address (Ethereum-style hex). Authorized via agents
    /// system to submit orders using the vault's derived user_id.
    pub operator: String,
    /// Engine UserId derived from vault_id; carries the vault's balance
    /// and open orders.
    #[serde(with = "user_id_hex")]
    pub user_id: UserId,
    /// Credit ratio applied to the vault by the credit system. Default
    /// 5×, overridable at creation.
    pub credit_ratio: f64,
    /// Total LP shares outstanding, fixed-point 1e6.
    pub total_shares_micro: u128,
    /// Cumulative deposits, USDC × 1e6. Informational.
    pub cumulative_deposits_micro: u128,
    /// Cumulative withdrawals, USDC × 1e6. Informational.
    pub cumulative_withdrawals_micro: u128,
    pub created_at_ms: u64,
}

/// Serde helper: `UserId([u8; 20])` as 0x-prefixed hex.
mod user_id_hex {
    use serde::{Deserialize, Deserializer, Serializer};
    use types::UserId;
    pub fn serialize<S: Serializer>(u: &UserId, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("0x{}", hex::encode(u.0)))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<UserId, D::Error> {
        let s = String::deserialize(d)?;
        UserId::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// Per-LP position in a specific vault.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LpPosition {
    pub shares_micro: u128,
    pub cumulative_deposits_micro: u128,
    pub cumulative_withdrawals_micro: u128,
}

/// Vault registry: vault_id → Vault, and (vault_id, lp_address) → position.
#[derive(Default)]
pub struct VaultRegistry {
    pub vaults: DashMap<u64, Vault>,
    pub positions: DashMap<(u64, String), LpPosition>,
}

impl VaultRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

/// Compute shares to issue for a deposit.
///
/// - First deposit: shares equal USDC amount (1:1 baseline).
/// - Subsequent deposits: `amount × total_shares / aum` so the LP's
///   claim on AUM is exactly proportional to what they added, even if
///   AUM has drifted (up or down) since prior deposits.
pub fn shares_for_deposit(amount_micro: u64, aum_micro: u64, total_shares_micro: u128) -> u128 {
    if total_shares_micro == 0 || aum_micro == 0 {
        return amount_micro as u128;
    }
    (amount_micro as u128 * total_shares_micro) / (aum_micro as u128)
}

/// Compute USDC to return when burning shares.
///
/// `usdc = shares_burned × aum / total_shares`.
pub fn usdc_for_shares(shares_burned_micro: u128, aum_micro: u64, total_shares_micro: u128) -> u64 {
    if total_shares_micro == 0 {
        return 0;
    }
    let out = (shares_burned_micro * aum_micro as u128) / total_shares_micro;
    out as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_deposit_issues_1_to_1() {
        assert_eq!(shares_for_deposit(1_000, 0, 0), 1_000);
    }

    #[test]
    fn second_deposit_at_par() {
        // After 1000 deposit, aum=1000, shares=1000.
        // Second 500 at par: shares = 500 * 1000 / 1000 = 500.
        assert_eq!(shares_for_deposit(500, 1_000, 1_000), 500);
    }

    #[test]
    fn deposit_after_gain_gets_fewer_shares() {
        // Initial 1000; AUM grew to 2000 from PnL. Total shares still 1000.
        // New 500 deposit: shares = 500 * 1000 / 2000 = 250.
        assert_eq!(shares_for_deposit(500, 2_000, 1_000), 250);
    }

    #[test]
    fn withdraw_after_gain_gets_more_usdc() {
        // Shares 1000, AUM 2000. Burn 500 shares → 1000 USDC.
        assert_eq!(usdc_for_shares(500, 2_000, 1_000), 1_000);
    }

    #[test]
    fn withdraw_from_empty_vault_returns_zero() {
        assert_eq!(usdc_for_shares(100, 0, 0), 0);
    }

    #[test]
    fn derive_vault_user_id_is_stable() {
        let a = derive_vault_user_id(42);
        let b = derive_vault_user_id(42);
        let c = derive_vault_user_id(43);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
