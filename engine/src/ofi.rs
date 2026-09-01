use std::collections::HashMap;
use types::MarketId;

pub const DEFAULT_OFI_WINDOW: usize = 50;

// Scorer weights: OFI imbalance, size fraction, book walk.
const W_OFI: f64 = 0.5;
const W_SIZE: f64 = 0.3;
const W_WALK: f64 = 0.2;

pub const TOXICITY_PENALTY_THRESHOLD: f64 = 0.75;
pub const CREDIT_PENALTY_MULTIPLIER: f64 = 0.8;
// 10 seconds in microseconds (engine timestamp unit).
pub const CREDIT_PENALTY_DURATION_US: u64 = 10_000_000;

/// Fixed-size ring buffer for signed order-flow imbalance.
///
/// +qty for buyer-initiated fills, -qty for seller-initiated fills.
/// All operations are O(1) with no heap allocation after construction.
pub struct OfiAccumulator {
    buf: [i64; DEFAULT_OFI_WINDOW],
    /// Next write slot; wraps at DEFAULT_OFI_WINDOW.
    head: usize,
    /// Number of valid items currently in the buffer [0, DEFAULT_OFI_WINDOW].
    count: usize,
    /// Running signed sum of all items in the buffer.
    pub sum: i64,
    /// Running sum of absolute values for normalization.
    abs_sum: i64,
}

impl Default for OfiAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl OfiAccumulator {
    pub const fn new() -> Self {
        Self {
            buf: [0i64; DEFAULT_OFI_WINDOW],
            head: 0,
            count: 0,
            sum: 0,
            abs_sum: 0,
        }
    }

    /// Push a signed flow entry. No allocation; O(1).
    pub fn push(&mut self, signed_qty: i64) {
        if self.count == DEFAULT_OFI_WINDOW {
            let evicted = self.buf[self.head];
            self.sum -= evicted;
            self.abs_sum -= evicted.abs();
        } else {
            self.count += 1;
        }
        self.buf[self.head] = signed_qty;
        self.sum += signed_qty;
        self.abs_sum += signed_qty.abs();
        self.head = (self.head + 1) % DEFAULT_OFI_WINDOW;
    }

    /// Normalized imbalance in [0.0, 1.0]: |signed_sum| / abs_sum.
    pub fn imbalance(&self) -> f64 {
        if self.abs_sum == 0 {
            0.0
        } else {
            self.sum.abs() as f64 / self.abs_sum as f64
        }
    }

    /// Current signed sum snapshot.
    pub fn snapshot(&self) -> i64 {
        self.sum
    }
}

/// Compute toxicity score in [0.0, 1.0] from three weighted components.
///
/// - ofi_imbalance: `OfiAccumulator::imbalance()` after the fill is pushed
/// - size_fraction: fill_qty / top_of_book_depth, clamped to [0, 1]
/// - walked_book: true when the order consumed liquidity at more than one price level
pub fn compute_toxicity(ofi_imbalance: f64, size_fraction: f64, walked_book: bool) -> f64 {
    let walk = if walked_book { 1.0 } else { 0.0 };
    (W_OFI * ofi_imbalance + W_SIZE * size_fraction.min(1.0) + W_WALK * walk).clamp(0.0, 1.0)
}

/// Maintains per-market OFI accumulators and computes toxicity on each taker fill.
pub struct ToxicityScorer {
    pub accumulators: HashMap<MarketId, OfiAccumulator>,
}

impl ToxicityScorer {
    pub fn new() -> Self {
        Self {
            accumulators: HashMap::new(),
        }
    }

    /// Score a taker fill event and update the OFI accumulator for the market.
    /// Returns `(toxicity_score, ofi_snapshot)`.
    ///
    /// - `signed_qty`: total filled quantity with sign — positive for bid (buy) taker,
    ///   negative for ask (sell) taker.
    /// - `top_depth`: total quantity available at the best opposing price level at
    ///   the moment the order arrived.
    /// - `total_fill_qty`: total quantity matched by this taker order.
    /// - `walked_book`: whether the order consumed liquidity at more than one price level.
    pub fn score_and_update(
        &mut self,
        market: &MarketId,
        signed_qty: i64,
        top_depth: u64,
        total_fill_qty: u64,
        walked_book: bool,
    ) -> (f64, i64) {
        let acc = self.accumulators.entry(market.clone()).or_default();
        acc.push(signed_qty);

        let ofi_imbalance = acc.imbalance();
        let size_fraction = if top_depth == 0 {
            1.0
        } else {
            total_fill_qty as f64 / top_depth as f64
        };
        let score = compute_toxicity(ofi_imbalance, size_fraction, walked_book);
        (score, acc.snapshot())
    }
}

impl Default for ToxicityScorer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_balanced_flow_score_zero() {
        let mut scorer = ToxicityScorer::new();
        let market = MarketId("ETH-USDC".to_string());
        // Alternating buyer- and seller-initiated fills of equal size.
        for _ in 0..10 {
            scorer.score_and_update(&market, 1_000_000_000, 10_000_000_000, 1_000_000_000, false);
            scorer.score_and_update(
                &market,
                -1_000_000_000,
                10_000_000_000,
                1_000_000_000,
                false,
            );
        }
        let (score, _) =
            scorer.score_and_update(&market, 1_000_000_000, 10_000_000_000, 1_000_000_000, false);
        // Balanced OFI → low imbalance; small fill relative to depth; no book walk.
        assert!(score < 0.15, "balanced flow score too high: {score}");
    }

    #[test]
    fn test_one_sided_walk_score_high() {
        let mut scorer = ToxicityScorer::new();
        let market = MarketId("BTC-USDC".to_string());
        // Fill the entire window with large one-sided buys that walk the book.
        for _ in 0..DEFAULT_OFI_WINDOW {
            scorer.score_and_update(&market, 10_000_000_000, 500_000_000, 10_000_000_000, true);
        }
        let (score, _) =
            scorer.score_and_update(&market, 10_000_000_000, 500_000_000, 10_000_000_000, true);
        // Max OFI imbalance (1.0) + fill 20× top depth (clamped 1.0) + walked.
        // Expected: 0.5 * 1.0 + 0.3 * 1.0 + 0.2 * 1.0 = 1.0
        assert!(score > 0.9, "one-sided large walk score too low: {score}");
    }

    #[test]
    fn test_ring_buffer_rollover() {
        let mut acc = OfiAccumulator::new();
        for _ in 0..DEFAULT_OFI_WINDOW {
            acc.push(1_000);
        }
        assert_eq!(acc.count, DEFAULT_OFI_WINDOW);
        assert_eq!(acc.sum, 1_000 * DEFAULT_OFI_WINDOW as i64);

        // 51st push: evicts oldest +1000, inserts +1000 → sum unchanged.
        acc.push(1_000);
        assert_eq!(
            acc.sum,
            1_000 * DEFAULT_OFI_WINDOW as i64,
            "sum unchanged after same-value rollover"
        );

        // 52nd push: evicts oldest +1000, inserts -500 → sum decreases by 1500.
        acc.push(-500);
        let expected = 1_000 * (DEFAULT_OFI_WINDOW as i64 - 1) - 500;
        assert_eq!(
            acc.sum, expected,
            "sum after mixed rollover: expected {expected}, got {}",
            acc.sum
        );
    }
}
