//! Permissionless market listing.
//!
//! Anyone can propose a new market by posting a USDC bond and a market
//! spec. Proposals enter a challenge window (default 24h; overridable
//! via `VELA_LISTING_CHALLENGE_HOURS`). During the window, the operator
//! or a challenge holder can reject the proposal and slash the bond
//! (via `/admin/listings/reject`). Unchallenged proposals auto-register
//! when the window expires and the bond is refunded to the proposer's
//! exchange balance.
//!
//! Sixteen markets is a rounding error vs Hyperliquid's 200+. This is
//! the wedge to scale market count without hiring a listings team, and
//! on-brand for Vela's verifiable positioning: bond + open challenge
//! is trust-minimised whereas a whitelisted listings team isn't.
//!
//! v1 keeps bond escrow in-engine via the existing user balance
//! (deducted at propose time, refunded on accept). A follow-up wires
//! this to `VelaSettlement.sol` so the bond lives on chain and can be
//! challenged by any Ethereum address, not only Vela-authenticated
//! addresses.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Bond amount in USDC micro-USD (default 100k USDC × 1e6 = 100 000 000 000).
pub fn bond_amount_micro() -> u64 {
    std::env::var("VELA_LISTING_BOND_MICRO")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000_000_000u64)
}

pub fn challenge_hours() -> u64 {
    std::env::var("VELA_LISTING_CHALLENGE_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24)
}

/// Monotonic listing-id source.
pub static NEXT_LISTING_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_listing_id() -> u64 {
    NEXT_LISTING_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ListingStatus {
    /// Bond posted; challenge window open.
    Pending,
    /// Window elapsed; market registered on the engine.
    Accepted,
    /// Rejected by the operator; bond slashed.
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListingProposal {
    pub listing_id: u64,
    /// Address that posted the bond and proposed the market.
    pub proposer: String,
    /// Spec of the proposed market.
    pub market_id: String,
    pub base: String,
    pub quote: String,
    pub max_orders: usize,
    pub min_order_size: u64,
    pub price_tick: u64,
    pub quantity_tick: u64,
    /// USDC × 1e6 bonded on this proposal.
    pub bond_micro: u64,
    /// Unix ms when the proposal was posted.
    pub proposed_at_ms: u64,
    /// Unix ms after which the market auto-accepts.
    pub challenge_deadline_ms: u64,
    pub status: ListingStatus,
    /// Rejection reason if `status == Rejected`.
    #[serde(default)]
    pub reject_reason: Option<String>,
}

pub type ListingRegistry = Arc<DashMap<u64, ListingProposal>>;
