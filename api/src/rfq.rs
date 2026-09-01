//! RFQ / block-trade venue.
//!
//! Off-book quote request for trades that are too big to work through
//! the public CLOB without moving the market. Requester posts an RFQ,
//! whitelisted MMs post signed quotes, requester accepts one, Vela
//! atomically debits and credits both sides.
//!
//! Why bypass the CLOB
//! -------------------
//! A $2M taker walking a spot book with 100k top-of-book depth pays
//! massive slippage and leaks the size to the whole market via fills
//! visible on the trade tape. Institutions currently route this flow
//! OTC (Wintermute, Cumberland) precisely to avoid that. Capturing it
//! on-venue requires an agent-native block-trade rail with:
//! - Requester privacy: quotes are private per-request until accepted.
//! - Better-than-book pricing: quote must improve on the current touch
//!   (enforced at accept time), so an MM can't collude to fill worse
//!   than the requester could have gotten in the book.
//! - Atomic settlement: no legs of the trade land without the other.
//!
//! v1 policy
//! ---------
//! - `VELA_RFQ_MAKERS` env is a comma-separated allowlist of MM
//!   addresses. Non-listed MMs' quotes are rejected.
//! - Every RFQ is single-market. Multi-leg / cross-asset requires v2.
//! - MMs post quotes with expiry timestamps; requester must accept
//!   before expiry. Vela does not automatically pick "best quote" —
//!   the requester picks.
//! - `min_notional_usdc_micro` (default 250k USDC) is the floor for
//!   posting an RFQ. Rationale: RFQ has more per-trade overhead than
//!   the CLOB and its point is precisely the trades too big for the
//!   book. Overridable via `VELA_RFQ_MIN_NOTIONAL_MICRO`.
//!
//! Deferred to v2
//! --------------
//! - MM SLA tracking (uptime, average spread) to publish an on-chain
//!   MM reputation for the whitelist.
//! - Multi-leg / basket RFQs.
//! - Explicit fee schedule for RFQ (currently reuses CLOB market
//!   fees).

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use types::OrderSide;

pub static NEXT_RFQ_ID: AtomicU64 = AtomicU64::new(1);
pub static NEXT_QUOTE_ID: AtomicU64 = AtomicU64::new(1);

pub fn min_notional_micro() -> u64 {
    std::env::var("VELA_RFQ_MIN_NOTIONAL_MICRO")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(250_000_000_000u64) // 250k USDC × 1e6
}

pub fn maker_allowlist() -> std::collections::HashSet<String> {
    match std::env::var("VELA_RFQ_MAKERS") {
        Ok(s) if !s.is_empty() => s
            .split(',')
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .collect(),
        _ => std::collections::HashSet::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RfqStatus {
    Open,
    Filled,
    Expired,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqRequest {
    pub rfq_id: u64,
    pub requester: String,
    pub market: String,
    pub side: OrderSide,
    /// Base quantity, fixed-point 1e6.
    pub quantity: u64,
    /// Wall-clock ms deadline for accepting a quote.
    pub expires_at_ms: u64,
    pub created_at_ms: u64,
    pub status: RfqStatus,
    /// If Filled, which quote won.
    pub filled_by_quote_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RfqQuote {
    pub quote_id: u64,
    pub rfq_id: u64,
    pub maker: String,
    /// Quote price in USDC × 1e6.
    pub price: u64,
    /// Quote quantity in base × 1e6 (must equal the RFQ's quantity;
    /// partial quotes are v2).
    pub quantity: u64,
    /// Wall-clock ms after which this quote is stale.
    pub expires_at_ms: u64,
    pub created_at_ms: u64,
}

#[derive(Default)]
pub struct RfqRegistry {
    pub requests: DashMap<u64, RfqRequest>,
    /// (rfq_id, quote_id) → quote
    pub quotes: DashMap<(u64, u64), RfqQuote>,
}

impl RfqRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn quotes_for(&self, rfq_id: u64) -> Vec<RfqQuote> {
        self.quotes
            .iter()
            .filter(|entry| entry.key().0 == rfq_id)
            .map(|entry| entry.value().clone())
            .collect()
    }
}

pub fn next_rfq_id() -> u64 {
    NEXT_RFQ_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn next_quote_id() -> u64 {
    NEXT_QUOTE_ID.fetch_add(1, Ordering::Relaxed)
}
