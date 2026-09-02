//! Process-wide price cache.
//!
//! Consumers (borrow-lend, portfolio margin, perp mark-price) read the
//! freshest observation from this cache instead of maintaining their
//! own stubs. The Pyth Hermes feed task (`pyth::pyth_feed_task`) writes
//! into the cache on every tick.
//!
//! Design notes
//! ------------
//! - Reads are lock-free via `DashMap`. No async in the read path so it
//!   can be called from inside the matching engine hot path if we ever
//!   need to gate an order on a mark price.
//! - Stale-guard is caller-driven via `price_fresh(asset, max_ms)`. The
//!   default helper `price(asset)` uses `DEFAULT_STALENESS_MS = 60s` —
//!   generous enough to survive a Pyth blip, tight enough to catch a
//!   feed outage before mass liquidations trigger.
//! - We deliberately do NOT auto-invalidate stale entries. Borrow-lend
//!   opts to keep the last observed price under a Pyth outage rather
//!   than fall to zero (which would liquidate every open position).
//!   Callers that need strict freshness use `price_fresh(...)` and
//!   handle the `None` themselves.
//! - Missing / stale counters are exposed via `/metrics` so operators
//!   see a Pyth outage in Grafana before it becomes a liquidation
//!   incident.

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Default staleness window for `price(asset)`. Chosen at 60s to survive
/// a Pyth blip; tight enough that a real outage surfaces before any
/// liquidation cascade.
pub const DEFAULT_STALENESS_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy)]
pub struct PriceEntry {
    /// Mid price in micro-USDC (1e6 per USD).
    pub price_micro_usdc: u128,
    /// Wall-clock timestamp when this price was written (ms since UNIX
    /// epoch).
    pub timestamp_ms: u64,
}

pub struct PriceOracle {
    /// Uppercase asset ticker (e.g. "ETH") → last observation.
    prices: DashMap<String, PriceEntry>,
    stale_reads: AtomicU64,
    missing_reads: AtomicU64,
}

impl PriceOracle {
    pub fn new() -> Arc<Self> {
        let inst = Self {
            prices: DashMap::new(),
            stale_reads: AtomicU64::new(0),
            missing_reads: AtomicU64::new(0),
        };
        // Seed the stable pool at par so LTV / margin math resolves
        // without waiting for a first Pyth tick. Pyth doesn't feed the
        // stable→USD leg anyway.
        let now = now_ms();
        for stable in ["USDC", "USDT", "DAI"] {
            inst.prices.insert(
                stable.to_string(),
                PriceEntry {
                    price_micro_usdc: 1_000_000,
                    timestamp_ms: now,
                },
            );
        }
        Arc::new(inst)
    }

    /// Publish a fresh observation. Overwrites any previous entry.
    pub fn publish(&self, asset: &str, price_micro_usdc: u128) {
        self.prices.insert(
            asset.to_ascii_uppercase(),
            PriceEntry {
                price_micro_usdc,
                timestamp_ms: now_ms(),
            },
        );
    }

    /// Convenience: publish from a Vela market id such as `"ETH-USDC"`.
    /// The quote leg is stripped; only the base is stored.
    pub fn publish_from_market(&self, market_id: &str, price_micro_usdc: u128) {
        if let Some(base) = market_id.split('-').next() {
            self.publish(base, price_micro_usdc);
        }
    }

    /// Latest price if it's fresher than `max_staleness_ms`. Increments
    /// the `stale_reads` / `missing_reads` counters on miss.
    pub fn price_fresh(&self, asset: &str, max_staleness_ms: u64) -> Option<u128> {
        let key = asset.to_ascii_uppercase();
        match self.prices.get(&key) {
            None => {
                self.missing_reads.fetch_add(1, Ordering::Relaxed);
                None
            }
            Some(entry) => {
                if now_ms().saturating_sub(entry.timestamp_ms) > max_staleness_ms {
                    self.stale_reads.fetch_add(1, Ordering::Relaxed);
                    None
                } else {
                    Some(entry.price_micro_usdc)
                }
            }
        }
    }

    /// Latest price with the default staleness bound.
    pub fn price(&self, asset: &str) -> Option<u128> {
        self.price_fresh(asset, DEFAULT_STALENESS_MS)
    }

    /// Latest price regardless of staleness — for callers that would
    /// rather use a stale price than none at all (e.g. read endpoints
    /// that just display a value).
    pub fn price_any(&self, asset: &str) -> Option<u128> {
        self.prices
            .get(&asset.to_ascii_uppercase())
            .map(|e| e.price_micro_usdc)
    }

    pub fn entry(&self, asset: &str) -> Option<PriceEntry> {
        self.prices.get(&asset.to_ascii_uppercase()).map(|e| *e)
    }

    pub fn stale_reads(&self) -> u64 {
        self.stale_reads.load(Ordering::Relaxed)
    }

    pub fn missing_reads(&self) -> u64 {
        self.missing_reads.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> Vec<(String, PriceEntry)> {
        self.prices
            .iter()
            .map(|e| (e.key().clone(), *e.value()))
            .collect()
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stables_seeded_at_par() {
        let o = PriceOracle::new();
        assert_eq!(o.price("USDC"), Some(1_000_000));
        // Case-insensitive lookup.
        assert_eq!(o.price("usdc"), Some(1_000_000));
    }

    #[test]
    fn missing_asset_returns_none_and_bumps_counter() {
        let o = PriceOracle::new();
        assert!(o.price("BTC").is_none());
        assert_eq!(o.missing_reads(), 1);
    }

    #[test]
    fn publish_then_read() {
        let o = PriceOracle::new();
        o.publish("BTC", 60_000_000_000);
        assert_eq!(o.price("BTC"), Some(60_000_000_000));
    }

    #[test]
    fn publish_from_market_strips_quote() {
        let o = PriceOracle::new();
        o.publish_from_market("ETH-USDC", 3_000_000_000);
        assert_eq!(o.price("ETH"), Some(3_000_000_000));
        // The quote leg is not published as its own key.
        assert_eq!(o.price_any("USDC"), Some(1_000_000)); // still the seeded par.
    }

    #[test]
    fn stale_read_is_flagged() {
        let o = PriceOracle::new();
        o.publish("SOL", 145_000_000);
        // Sleep long enough to age past the 1-ms staleness window.
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(o.price_fresh("SOL", 1).is_none());
        assert!(o.stale_reads() >= 1);
        // `price_any` bypasses the staleness check.
        assert_eq!(o.price_any("SOL"), Some(145_000_000));
    }

    #[test]
    fn snapshot_lists_all_observations() {
        let o = PriceOracle::new();
        o.publish("BTC", 60_000_000_000);
        let snap = o.snapshot();
        assert!(snap.iter().any(|(k, _)| k == "BTC"));
        assert!(snap.iter().any(|(k, _)| k == "USDC"));
    }
}
