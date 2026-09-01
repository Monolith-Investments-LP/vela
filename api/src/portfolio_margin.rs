//! Portfolio margin / cross-collateralization for spot.
//!
//! Vela's per-market isolated margin is safe but capital-inefficient.
//! An MM quoting BTC/USDC and ETH/USDC has to post USDC twice, even
//! though a drawdown in one market is highly correlated with the other
//! and mostly offsets. Portfolio margin pools all of a user's positions
//! into a single margin account and applies a SPAN-style scenario
//! sweep: the account is solvent iff *every* stress scenario nets to
//! ≥ 0 equity.
//!
//! v1 design
//! ---------
//! - **Scenario set**: 12 shocks per risk-factor (asset). For each
//!   asset we sweep {-30%, -20%, -10%, -5%, -2%, 0, +2%, +5%, +10%,
//!   +20%, +30%, custom_extreme_bps}. Correlation matrix applied so a
//!   BTC -20% scenario implies ETH ≈ -18% (correlation 0.9).
//! - **Portfolio equity** in a scenario = Σ position_notional_shocked
//!   + cash. If any scenario drives portfolio equity below the
//!   maintenance margin buffer (default 5% of gross notional), the
//!   account is under-margined and new positions are rejected.
//! - **Maintenance vs initial** margin: initial = 2 × maintenance,
//!   default 10% of gross notional. Opening a new position requires
//!   surviving the scenario sweep at the *initial* threshold; keeping
//!   it open only requires surviving at the *maintenance* threshold.
//! - **Two consumers**: (a) borrow-lend HF check reuses this to allow
//!   correlated collateral to net; (b) the standard order path can
//!   optionally opt into portfolio margin via an `AccountMode` flag
//!   (Isolated | Portfolio), gated behind a signed opt-in.
//!
//! v1 scope
//! --------
//! - Positions in `state.borrow_lend` (supply/borrow) feed the
//!   scenario sweep. Direct spot balances (from the matching engine's
//!   UserState) are treated as "cash + long inventory" — captured via
//!   a caller-supplied `SpotPosition` list to avoid a cross-crate lock
//!   in this scaffold. Real integration reads UserState in the caller.
//! - Correlations are configurable per-asset via env
//!   (`VELA_PM_CORR_ETH_BTC=90`, `VELA_PM_CORR_SOL_BTC=80`, …). Default
//!   0.85 for majors, 0 for anything unmodeled.

use serde::{Deserialize, Serialize};

pub const BPS_DENOM: i128 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountMode {
    /// Legacy isolated margin: each market checked independently.
    Isolated,
    /// Portfolio margin: cross-collateralization with scenario sweep.
    Portfolio,
}

impl Default for AccountMode {
    fn default() -> Self {
        AccountMode::Isolated
    }
}

/// One spot position as fed into the scenario sweep. Positive `qty` is
/// long, negative is short (only reachable via borrow-lend borrow
/// against collateral). Prices are micro-USDC.
#[derive(Debug, Clone)]
pub struct SpotPosition {
    pub asset: String,
    pub qty: i128,
    pub price_micro_usdc: i128,
}

/// Shock definitions in bps. Positive = up, negative = down. 0
/// scenario is included so we always exercise the current state.
pub const DEFAULT_SHOCKS_BPS: [i32; 12] = [
    -3_000, -2_000, -1_000, -500, -200, 0, 200, 500, 1_000, 2_000, 3_000, -5_000,
];

/// Maintenance margin as a fraction of gross notional, bps. Default 5%.
pub fn maintenance_bps() -> u32 {
    std::env::var("VELA_PM_MAINT_BPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
}

/// Initial margin, default 10%. Must be ≥ maintenance.
pub fn initial_bps() -> u32 {
    std::env::var("VELA_PM_INITIAL_BPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000)
}

/// Correlation of `asset` to the primary risk factor (BTC), in bps.
/// Returns 10_000 for BTC itself, env-configured for known majors,
/// 0 otherwise.
pub fn correlation_to_btc_bps(asset: &str) -> i32 {
    match asset.to_ascii_uppercase().as_str() {
        "BTC" | "WBTC" => 10_000,
        "ETH" | "WETH" => std::env::var("VELA_PM_CORR_ETH_BTC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(9_000),
        "SOL" => std::env::var("VELA_PM_CORR_SOL_BTC")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7_500),
        _ => 0,
    }
}

/// Shock a single position: `shock_bps` is the BTC shock, scaled by
/// this asset's correlation. Returns the P&L in micro-USDC (signed).
pub fn pnl_under_shock(pos: &SpotPosition, shock_bps: i32) -> i128 {
    let corr = correlation_to_btc_bps(&pos.asset) as i128;
    let scaled_shock = (shock_bps as i128 * corr) / BPS_DENOM;
    // notional × qty at scaled shock; qty is signed.
    let notional = pos.price_micro_usdc.saturating_mul(pos.qty) / 1_000_000;
    (notional * scaled_shock) / BPS_DENOM
}

/// Gross notional in micro-USDC = Σ |qty × price|.
pub fn gross_notional_micro_usdc(positions: &[SpotPosition]) -> u128 {
    positions
        .iter()
        .map(|p| {
            let n = (p.qty.saturating_mul(p.price_micro_usdc) / 1_000_000).unsigned_abs();
            n
        })
        .sum()
}

#[derive(Debug, Clone, Serialize)]
pub struct MarginReport {
    pub gross_notional_micro_usdc: u128,
    pub initial_requirement_micro_usdc: u128,
    pub maintenance_requirement_micro_usdc: u128,
    pub worst_scenario_shock_bps: i32,
    pub worst_scenario_equity_micro_usdc: i128,
    pub current_equity_micro_usdc: i128,
    pub passes_initial: bool,
    pub passes_maintenance: bool,
}

/// Run the full scenario sweep for a set of positions plus a cash
/// balance in micro-USDC. Cash is unshocked.
pub fn compute_margin(cash_micro_usdc: i128, positions: &[SpotPosition]) -> MarginReport {
    let gross = gross_notional_micro_usdc(positions);
    let init_req = (gross * initial_bps() as u128) / BPS_DENOM as u128;
    let maint_req = (gross * maintenance_bps() as u128) / BPS_DENOM as u128;

    let mut worst_shock = 0i32;
    let mut worst_equity = i128::MAX;
    let mut current_equity = 0i128;
    for &shock in DEFAULT_SHOCKS_BPS.iter() {
        let pnl: i128 = positions.iter().map(|p| pnl_under_shock(p, shock)).sum();
        let equity = cash_micro_usdc + pnl;
        if shock == 0 {
            current_equity = equity;
        }
        if equity < worst_equity {
            worst_equity = equity;
            worst_shock = shock;
        }
    }
    let passes_initial = worst_equity >= init_req as i128;
    let passes_maintenance = worst_equity >= maint_req as i128;
    MarginReport {
        gross_notional_micro_usdc: gross,
        initial_requirement_micro_usdc: init_req,
        maintenance_requirement_micro_usdc: maint_req,
        worst_scenario_shock_bps: worst_shock,
        worst_scenario_equity_micro_usdc: worst_equity,
        current_equity_micro_usdc: current_equity,
        passes_initial,
        passes_maintenance,
    }
}

// ---------- HTTP handler ----------

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use std::sync::Arc;

use crate::borrow_lend::{from_scaled, BorrowLendRegistry};
use crate::types::ApiResponse;
use crate::AppState;

/// Build a `Vec<SpotPosition>` for `user` from the borrow-lend
/// registry: supplies become long positions, borrows become shorts.
/// Direct engine-side spot balances are not modeled here — callers
/// that want the fuller picture pass `additional` on top.
pub fn positions_from_borrow_lend(reg: &BorrowLendRegistry, user_lower: &str) -> Vec<SpotPosition> {
    let mut out = Vec::new();
    for entry in reg.positions.iter() {
        let (u, asset) = entry.key();
        if u != user_lower {
            continue;
        }
        let market = match reg.markets.get(asset) {
            Some(m) => m,
            None => continue,
        };
        let m = market.value();
        let long = from_scaled(entry.value().supply_scaled, m.supply_index) as i128;
        let short = from_scaled(entry.value().borrow_scaled, m.borrow_index) as i128;
        let net = long - short;
        if net != 0 {
            out.push(SpotPosition {
                asset: asset.clone(),
                qty: net,
                price_micro_usdc: m.price_micro_usdc as i128,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreviewBody {
    /// Optional cash balance override. If unset, defaults to 0 (v1
    /// caller supplies from client-side view of matching-engine
    /// balance).
    #[serde(default)]
    pub cash_micro_usdc: Option<i128>,
    #[serde(default)]
    pub extra_positions: Vec<PreviewPosition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreviewPosition {
    pub asset: String,
    pub qty: i128,
    pub price_micro_usdc: i128,
}

pub async fn account_margin_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(address): axum::extract::Path<String>,
) -> axum::response::Response {
    let user_lower = address.to_ascii_lowercase();
    let positions = positions_from_borrow_lend(&state.borrow_lend, &user_lower);
    let report = compute_margin(0, &positions);
    (StatusCode::OK, Json(ApiResponse::ok(report))).into_response()
}

pub async fn preview_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(address): axum::extract::Path<String>,
    Json(body): Json<PreviewBody>,
) -> axum::response::Response {
    let user_lower = address.to_ascii_lowercase();
    let mut positions = positions_from_borrow_lend(&state.borrow_lend, &user_lower);
    for p in body.extra_positions {
        positions.push(SpotPosition {
            asset: p.asset,
            qty: p.qty,
            price_micro_usdc: p.price_micro_usdc,
        });
    }
    let report = compute_margin(body.cash_micro_usdc.unwrap_or(0), &positions);
    (StatusCode::OK, Json(ApiResponse::ok(report))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(asset: &str, qty: i128, price: i128) -> SpotPosition {
        SpotPosition {
            asset: asset.to_string(),
            qty,
            price_micro_usdc: price,
        }
    }

    #[test]
    fn btc_shock_scales_by_full_correlation() {
        let p = pos("BTC", 1_000_000, 60_000_000_000); // 1 BTC @ $60k
                                                       // -1000 bps = -10% → -6000 μUSDC in notional × qty terms
                                                       // notional = qty * price / 1e6 = 60_000_000_000 μUSDC = $60k
                                                       // -10% × 60k = -6k = -6_000_000_000 μUSDC
        assert_eq!(pnl_under_shock(&p, -1_000), -6_000_000_000);
    }

    #[test]
    fn eth_shock_dampened_by_correlation() {
        let p = pos("ETH", 1_000_000, 3_000_000_000); // 1 ETH @ $3k
                                                      // With default corr 90%, a -1000 bps BTC shock → -900 bps ETH
                                                      // notional = 3_000_000_000 μUSDC → -9% × 3k = -270 → -270_000_000
        assert_eq!(pnl_under_shock(&p, -1_000), -270_000_000);
    }

    #[test]
    fn short_position_gains_on_downshock() {
        let p = pos("BTC", -500_000, 60_000_000_000); // short half a BTC
                                                      // -1000 bps × short 0.5 BTC × $60k = +$3k = +3_000_000_000
        assert_eq!(pnl_under_shock(&p, -1_000), 3_000_000_000);
    }

    #[test]
    fn uncorrelated_asset_pnl_is_zero() {
        let p = pos("XYZ", 1_000_000, 1_000_000);
        assert_eq!(pnl_under_shock(&p, -5_000), 0);
    }

    #[test]
    fn hedged_book_passes_maintenance() {
        // Long 1 BTC + short 0.9 BTC-equivalent notional in ETH.
        // ETH corr 90% means the short offsets BTC 1:1 in shock terms.
        let long_btc = pos("BTC", 1_000_000, 60_000_000_000);
        let short_eth = pos("ETH", -20_000_000, 3_000_000_000); // short 20 ETH @ $3k = $60k
        let report = compute_margin(10_000_000_000, &[long_btc, short_eth]);
        assert!(report.passes_maintenance);
        // Not necessarily initial — depends on maint vs init thresholds
        // and residual scenario risk. We only assert maintenance holds
        // for the hedged book with $10k cash cushion.
    }

    #[test]
    fn unhedged_leveraged_book_fails_initial() {
        // $100 cash, 1 BTC long ($60k notional) → 600× leverage. No
        // shock scenario has equity ≥ init_req.
        let long_btc = pos("BTC", 1_000_000, 60_000_000_000);
        let report = compute_margin(100_000_000, &[long_btc]);
        assert!(!report.passes_initial);
        // Worst shock should be the -5000 bps custom extreme.
        assert_eq!(report.worst_scenario_shock_bps, -5_000);
    }

    #[test]
    fn gross_notional_sums_absolute_values() {
        let a = pos("BTC", 1_000_000, 60_000_000_000);
        let b = pos("ETH", -20_000_000, 3_000_000_000);
        assert_eq!(gross_notional_micro_usdc(&[a, b]), 120_000_000_000);
    }
}
