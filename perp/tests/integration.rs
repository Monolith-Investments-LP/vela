//! Integration tests exercising the perp public surface.

use perp::{
    accrue_funding, apply_fill, funding_rate_bps_per_hour, liquidation_price_micro_usdc,
    margin_report, unrealized_pnl_micro_usdc, MarketConfig, MarketState, Position,
};

fn btc_market(index_price: u64, leverage: u32) -> MarketState {
    MarketState::new(MarketConfig::default_for("BTC-PERP", leverage), index_price)
}

/// Open 1 BTC long at $60k on a 20× market, watch the mark drop, verify
/// the maintenance check flips to `false` and `liquidation_price` sits
/// in a plausible band.
#[test]
fn long_fails_maintenance_when_mark_drops() {
    let mut m = btc_market(60_000_000_000, 20);
    let mut p = Position::default();
    apply_fill(&mut p, 1_000_000, 60_000_000_000); // 1 BTC long

    // Post enough cash to pass initial ($4k > 5% × $60k = $3k).
    let cash = 4_000_000_000i128;
    let liq_px = liquidation_price_micro_usdc(&p, &m, cash).unwrap();
    assert!(liq_px > 0 && liq_px < 60_000_000_000);

    // Drop mark below the trigger — maintenance must fail.
    m.mark_price_micro_usdc = liq_px.saturating_sub(500_000_000);
    let rep = margin_report(&p, &m, cash);
    assert!(!rep.passes_maintenance);
    assert!(rep.unrealized_pnl_micro_usdc < 0);
}

/// Short + funding accrual: a positive premium should tick the funding
/// index up, and a long's `funding_owed` should follow.
#[test]
fn funding_index_accrues_and_penalizes_long() {
    let mut m = btc_market(60_000_000_000, 20);
    m.mark_price_micro_usdc = 60_600_000_000; // +1% premium → clamped to 100 bps/hr
    let start = m.last_funding_ts_ms;
    let delta = accrue_funding(&mut m, start + 3_600_000);
    assert!(delta > 0);
    assert_eq!(funding_rate_bps_per_hour(&m), 100);

    let mut long = Position::default();
    apply_fill(&mut long, 1_000_000, 60_000_000_000);
    assert!(perp::funding_owed(&long, m.funding_index) > 0);
}

/// Close-flip-realize: short 1 BTC then buy 2 BTC. First unit closes at
/// profit, second opens a long at the new price.
#[test]
fn close_flip_realizes_and_reopens_long() {
    let mut p = Position::default();
    apply_fill(&mut p, -1_000_000, 60_000_000_000); // short 1 @ 60k
    apply_fill(&mut p, 2_000_000, 58_000_000_000); // buy 2 @ 58k
    assert_eq!(p.size, 1_000_000);
    assert_eq!(p.entry_price, 58_000_000_000);
    assert_eq!(p.realized_pnl_micro_usdc, 2_000_000_000);
    assert_eq!(
        unrealized_pnl_micro_usdc(&p, 60_000_000_000),
        2_000_000_000
    );
}
