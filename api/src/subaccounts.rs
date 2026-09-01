//! Sub-accounts (v1 MVP).
//!
//! Prop desks running multi-strategy books need logical isolation between
//! sub-accounts: separate balances, separate positions, separate PnL,
//! but the same master wallet controlling all of them. Standard on
//! Binance (up to 200 subs), OKX (unlimited), dYdX v4 (`owner+number`),
//! Hyperliquid (native), Paradex (up to 10 with PnL segregation).
//!
//! v1 design (this file)
//! ---------------------
//! Rather than a deep refactor of `UserState` to use a `(wallet,
//! subaccount_id)` composite key everywhere (touches auth, credit, WAL,
//! rate limits, WS topics — multi-day work), v1 uses the same trick that
//! ships MM credit vaults: derive a stable per-master sub-account
//! UserId, register an agent delegation from `sub_user_id → master`,
//! and let all existing engine paths run unchanged.
//!
//! The functional guarantees are identical for the common case:
//! - Each sub-account has its own isolated balance and open orders in
//!   the engine (they're separate UserIds).
//! - PnL, points, portfolio, and toxicity all attribute per sub-account
//!   because they key off UserId.
//! - The master can transfer USDC between master ↔ sub via a signed
//!   `POST /subaccounts/transfer`.
//! - Master signs orders for sub-accounts through the agent path
//!   (delegation set up at sub-account create time).
//!
//! Gaps vs. a real composite-key refactor
//! --------------------------------------
//! - Cross-sub credit netting: v1 credit ratio is per-UserId, so a
//!   master's credit doesn't span its subs. Follow-up work in the
//!   composite-key refactor.
//! - Rate limits are per-UserId; a busy sub doesn't consume the
//!   master's rate budget. This is arguably correct behaviour, but
//!   institutions sometimes want a global cap.
//! - Withdrawal from a sub-account requires transferring back to the
//!   master first (v1 keeps L1 settlement master-only for simplicity).

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::sync::Arc;
use types::UserId;

/// Derive a stable UserId for `(master, subaccount_id)`. First 20 bytes of
/// `keccak256("vela:sub:" + master.0 + subaccount_id.to_be_bytes())`.
pub fn derive_sub_user_id(master: &UserId, subaccount_id: u32) -> UserId {
    let mut h = Keccak256::new();
    h.update(b"vela:sub:");
    h.update(master.0);
    h.update(subaccount_id.to_be_bytes());
    let hash = h.finalize();
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&hash[..20]);
    UserId(addr)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAccount {
    pub master: String,
    pub subaccount_id: u32,
    pub name: String,
    #[serde(with = "user_id_hex")]
    pub user_id: UserId,
    pub created_at_ms: u64,
}

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

#[derive(Default)]
pub struct SubaccountRegistry {
    /// (master_address_lowercase, subaccount_id) → SubAccount
    pub subs: DashMap<(String, u32), SubAccount>,
}

impl SubaccountRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn list_for(&self, master: &str) -> Vec<SubAccount> {
        let m = master.to_ascii_lowercase();
        self.subs
            .iter()
            .filter(|entry| entry.key().0 == m)
            .map(|entry| entry.value().clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_stable_and_unique() {
        let m = UserId([1u8; 20]);
        let a = derive_sub_user_id(&m, 1);
        let b = derive_sub_user_id(&m, 1);
        let c = derive_sub_user_id(&m, 2);
        let m2 = UserId([2u8; 20]);
        let d = derive_sub_user_id(&m2, 1);
        assert_eq!(a, b, "same (master, id) must derive same UserId");
        assert_ne!(
            a, c,
            "different subaccount_id under same master derives different id"
        );
        assert_ne!(
            a, d,
            "same subaccount_id under different master derives different id"
        );
    }
}
