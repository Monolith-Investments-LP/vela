//! Spot borrow-lend money market.
//!
//! Vela lets users borrow one asset against another as collateral, so a
//! trader can go long ETH by borrowing USDC against wBTC without
//! opening a perp, or short USDC by borrowing it against ETH. v1
//! supports two markets: `ETH/USDC` and `wBTC/USDC`, both with
//! conservative 60% LTV.
//!
//! Why now
//! -------
//! Leveraged spot is the missing rung between spot trading (no leverage,
//! all cash) and perps (linear futures with funding and liquidation
//! complexity). Every serious spot exchange offers it, and the mechanics
//! are far simpler than perps: no funding rate, no oracle-price
//! marking, no ADL. Interest accrues on borrows, that's it.
//!
//! v1 design
//! ---------
//! - **Pooled interest-rate model**: kink model
//!   (linear below 80% utilization, steep above). Borrow rate =
//!   base + slope1 * u/u_kink for u ≤ u_kink; base + slope1 +
//!   slope2 * (u - u_kink) / (1 - u_kink) otherwise. Supply rate =
//!   borrow rate × utilization × (1 - reserve factor).
//! - **Continuous accrual**: index-based, similar to Compound V2. Each
//!   supply / borrow position stores a scaled principal; the market
//!   maintains a per-asset borrow_index that ticks up on every
//!   interaction (lazy accrual, no per-block cron).
//! - **Collateral & LTV**: each asset has a
//!   collateral_factor_bps (default 6_000). Borrowing power =
//!   Σ (collateral_balance × price × collateral_factor). If total
//!   borrows exceed borrowing power, the position is liquidatable.
//! - **Liquidation**: public liquidator repays up to 50% of a
//!   borrower's borrow and seizes collateral at
//!   price × (1 + liquidation_bonus_bps), default 500 bps (5%).
//!
//! Prices
//! ------
//! `MarketState::price_micro_usdc` is refreshed from
//! `crate::oracle::PriceOracle` on every `accrue_market` call. Under a
//! Pyth outage the last-known price is deliberately kept in place — a
//! zeroed price would liquidate every open position, which is much
//! worse than trading briefly on a stale mark. Operators watch
//! `oracle.stale_reads` / `oracle.missing_reads` via `/metrics`.
//!
//! Not in v1
//! ---------
//! - Interest paid to LPs in real time to the LP (v1 accrues, LP
//!   claims on withdraw).
//! - Isolated / e-mode collateral (Aave-style) — one pool per asset,
//!   uniform LTV.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::auth::verify_matches_async;
use crate::types::ApiResponse;
use crate::AppState;

pub const BPS_DENOM: u128 = 10_000;
pub const RAY: u128 = 1_000_000_000_000_000_000; // 1e18, index precision.
pub const SECONDS_PER_YEAR: u128 = 365 * 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketConfig {
    pub asset: String,
    /// Fraction of asset value that counts toward borrowing power,
    /// in bps. Default 6_000 (60%).
    pub collateral_factor_bps: u16,
    /// Bonus to the liquidator on seized collateral, in bps.
    /// Default 500 (5%).
    pub liquidation_bonus_bps: u16,
    /// Portion of accrued borrow interest retained by the protocol,
    /// in bps. Default 1_000 (10%).
    pub reserve_factor_bps: u16,
    /// Kink utilization (u_kink) in bps. Default 8_000 (80%).
    pub kink_util_bps: u16,
    /// Slope of borrow-rate curve below the kink, annualized bps.
    /// Default 400 (4%).
    pub slope1_bps: u16,
    /// Slope above the kink, annualized bps. Default 20_000 (200%).
    pub slope2_bps: u16,
    /// Base borrow rate at zero utilization, annualized bps.
    /// Default 0.
    pub base_rate_bps: u16,
}

impl MarketConfig {
    pub fn default_for(asset: &str) -> Self {
        Self {
            asset: asset.to_string(),
            collateral_factor_bps: 6_000,
            liquidation_bonus_bps: 500,
            reserve_factor_bps: 1_000,
            kink_util_bps: 8_000,
            slope1_bps: 400,
            slope2_bps: 20_000,
            base_rate_bps: 0,
        }
    }
}

/// Per-asset money-market state. Locked behind a Mutex or accessed via
/// DashMap; all updates are supposed to accrue-then-mutate to keep the
/// index correct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    pub config: MarketConfig,
    /// Sum of all supplier deposits, in native asset units (1e6 scale).
    pub total_supply: u128,
    /// Sum of all borrower principal, in native asset units.
    pub total_borrows: u128,
    /// Cumulative borrow index, ray-scaled (1e18). Starts at RAY.
    pub borrow_index: u128,
    /// Cumulative supply index, ray-scaled (1e18). Starts at RAY.
    pub supply_index: u128,
    /// Timestamp (ms) of the last accrual.
    pub last_accrual_ms: u64,
    /// Reserve pool: interest retained by protocol, in native units.
    pub reserves: u128,
    /// Latest observed mid-price in micro-USDC. Refreshed from
    /// `crate::oracle::PriceOracle` on every accrue; falls back to the
    /// last observation when the oracle is stale/missing.
    pub price_micro_usdc: u128,
}

impl MarketState {
    pub fn new(config: MarketConfig, initial_price_micro_usdc: u128) -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            config,
            total_supply: 0,
            total_borrows: 0,
            borrow_index: RAY,
            supply_index: RAY,
            last_accrual_ms: now_ms,
            reserves: 0,
            price_micro_usdc: initial_price_micro_usdc,
        }
    }
}

/// Per-user, per-asset supply/borrow position. `principal_scaled` is
/// scaled by the market's cumulative index at deposit/borrow time so
/// current balances are `principal_scaled * index / RAY`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserPosition {
    pub supply_scaled: u128,
    pub borrow_scaled: u128,
}

pub struct BorrowLendRegistry {
    pub markets: DashMap<String, MarketState>,
    /// (user_lower, asset) → position
    pub positions: DashMap<(String, String), UserPosition>,
    /// Optional oracle handle. When wired, `refresh_prices_from_oracle`
    /// pulls each market's mark price before accrual. Left None in unit
    /// tests that construct a registry directly.
    oracle: Option<Arc<crate::oracle::PriceOracle>>,
}

impl BorrowLendRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            markets: DashMap::new(),
            positions: DashMap::new(),
            oracle: None,
        })
    }

    /// Constructor used by `AppState::new` — keeps a handle to the
    /// process-wide price oracle so every accrue can refresh prices.
    pub fn with_oracle(oracle: Arc<crate::oracle::PriceOracle>) -> Arc<Self> {
        Arc::new(Self {
            markets: DashMap::new(),
            positions: DashMap::new(),
            oracle: Some(oracle),
        })
    }

    pub fn seed_defaults(self: &Arc<Self>) {
        // Cold-start seed prices — used only until the first oracle
        // observation lands (typically < 1s after boot when Pyth is
        // enabled). USDC stays at par because Pyth doesn't feed
        // stable→USD.
        self.markets.insert(
            "USDC".to_string(),
            MarketState::new(MarketConfig::default_for("USDC"), 1_000_000),
        );
        self.markets.insert(
            "ETH".to_string(),
            MarketState::new(MarketConfig::default_for("ETH"), 3_000_000_000),
        );
        // Pull any prices already resident in the oracle cache.
        self.refresh_prices_from_oracle();
    }

    /// Pull the freshest price for each market from the oracle. If the
    /// oracle handle is unset (unit tests) or a market's price is
    /// stale/missing, the previous `price_micro_usdc` is preserved.
    pub fn refresh_prices_from_oracle(&self) {
        let oracle = match &self.oracle {
            Some(o) => o,
            None => return,
        };
        for mut entry in self.markets.iter_mut() {
            let asset = entry.key().clone();
            if let Some(px) = oracle.price(&asset) {
                entry.value_mut().price_micro_usdc = px;
            }
        }
    }
}

/// Utilization in bps: total_borrows / total_supply.
pub fn utilization_bps(m: &MarketState) -> u16 {
    if m.total_supply == 0 {
        return 0;
    }
    let u = (m.total_borrows * BPS_DENOM) / m.total_supply;
    u.min(BPS_DENOM) as u16
}

/// Annualized borrow rate in bps at the given utilization.
pub fn borrow_rate_bps(m: &MarketState, util_bps: u16) -> u32 {
    let cfg = &m.config;
    if util_bps <= cfg.kink_util_bps {
        // base + slope1 * u / u_kink
        let addend = if cfg.kink_util_bps == 0 {
            0u32
        } else {
            (cfg.slope1_bps as u32 * util_bps as u32) / cfg.kink_util_bps as u32
        };
        cfg.base_rate_bps as u32 + addend
    } else {
        let excess = util_bps as u32 - cfg.kink_util_bps as u32;
        let range = (BPS_DENOM as u32) - cfg.kink_util_bps as u32;
        let extra = if range == 0 {
            0
        } else {
            (cfg.slope2_bps as u32 * excess) / range
        };
        cfg.base_rate_bps as u32 + cfg.slope1_bps as u32 + extra
    }
}

/// Supply rate = borrow_rate × utilization × (1 - reserve_factor).
pub fn supply_rate_bps(m: &MarketState, util_bps: u16, borrow_rate_bps: u32) -> u32 {
    let rf = m.config.reserve_factor_bps as u32;
    let after_reserve = borrow_rate_bps.saturating_mul(BPS_DENOM as u32 - rf) / BPS_DENOM as u32;
    (after_reserve as u64 * util_bps as u64 / BPS_DENOM as u64) as u32
}

/// Accrue interest since `m.last_accrual_ms` up to `now_ms`. Updates
/// borrow_index, supply_index, total_borrows, and reserves in place.
/// Idempotent within the same millisecond.
pub fn accrue(m: &mut MarketState, now_ms: u64) {
    if now_ms <= m.last_accrual_ms {
        return;
    }
    let elapsed_ms = now_ms - m.last_accrual_ms;
    let util = utilization_bps(m);
    let br_bps = borrow_rate_bps(m, util);
    let sr_bps = supply_rate_bps(m, util, br_bps);

    // Interest factor over the elapsed period, ray-scaled.
    // factor = rate_bps / BPS_DENOM / SECONDS_PER_YEAR * elapsed_seconds
    // For millisecond precision we use elapsed_ms / (1000 * SPY).
    let borrow_factor =
        (br_bps as u128 * elapsed_ms as u128 * RAY) / (BPS_DENOM * 1_000 * SECONDS_PER_YEAR);
    let supply_factor =
        (sr_bps as u128 * elapsed_ms as u128 * RAY) / (BPS_DENOM * 1_000 * SECONDS_PER_YEAR);

    // New indices.
    let new_borrow_index = m.borrow_index + (m.borrow_index * borrow_factor / RAY);
    let new_supply_index = m.supply_index + (m.supply_index * supply_factor / RAY);

    // Interest accrued on outstanding borrows (native units).
    let interest_accrued =
        (m.total_borrows * (new_borrow_index - m.borrow_index)) / m.borrow_index.max(1);
    let reserve_share = (interest_accrued * m.config.reserve_factor_bps as u128) / BPS_DENOM;

    m.total_borrows += interest_accrued;
    m.reserves += reserve_share;
    m.borrow_index = new_borrow_index;
    m.supply_index = new_supply_index;
    m.last_accrual_ms = now_ms;
}

/// Convert a native-unit amount to a scaled principal at the current
/// index. `principal_scaled = amount * RAY / index`.
pub fn to_scaled(amount: u128, index: u128) -> u128 {
    if index == 0 {
        return 0;
    }
    amount.saturating_mul(RAY) / index
}

/// Inverse of `to_scaled`.
pub fn from_scaled(scaled: u128, index: u128) -> u128 {
    scaled.saturating_mul(index) / RAY
}

/// Borrowing power in micro-USDC for `user` across all markets.
/// Iterates markets; caller supplies price lookups via a closure.
pub fn borrowing_power_micro_usdc(reg: &BorrowLendRegistry, user_lower: &str) -> u128 {
    let mut sum = 0u128;
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
        let supply_native = from_scaled(entry.value().supply_scaled, m.supply_index);
        // supply_native × price / 1e6 × collateral_factor / BPS_DENOM
        let value_micro_usdc = supply_native * m.price_micro_usdc / 1_000_000;
        sum += value_micro_usdc * m.config.collateral_factor_bps as u128 / BPS_DENOM;
    }
    sum
}

/// Total borrow value in micro-USDC for `user` across all markets.
pub fn total_borrow_value_micro_usdc(reg: &BorrowLendRegistry, user_lower: &str) -> u128 {
    let mut sum = 0u128;
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
        let borrow_native = from_scaled(entry.value().borrow_scaled, m.borrow_index);
        sum += borrow_native * m.price_micro_usdc / 1_000_000;
    }
    sum
}

/// Health factor in bps: borrowing_power / total_borrows × BPS_DENOM.
/// > BPS_DENOM = safe, ≤ BPS_DENOM = liquidatable, u128::MAX for
/// no-borrow accounts.
pub fn health_factor_bps(reg: &BorrowLendRegistry, user_lower: &str) -> u128 {
    let borrows = total_borrow_value_micro_usdc(reg, user_lower);
    if borrows == 0 {
        return u128::MAX;
    }
    let power = borrowing_power_micro_usdc(reg, user_lower);
    power.saturating_mul(BPS_DENOM) / borrows
}

// ---------- Signing schemas ----------

pub fn supply_message(user: &str, asset: &str, amount: u128, nonce: u64) -> String {
    format!("vela:borrow-lend:supply:{user}:{asset}:{amount}:{nonce}")
}

pub fn withdraw_message(user: &str, asset: &str, amount: u128, nonce: u64) -> String {
    format!("vela:borrow-lend:withdraw:{user}:{asset}:{amount}:{nonce}")
}

pub fn borrow_message(user: &str, asset: &str, amount: u128, nonce: u64) -> String {
    format!("vela:borrow-lend:borrow:{user}:{asset}:{amount}:{nonce}")
}

pub fn repay_message(user: &str, asset: &str, amount: u128, nonce: u64) -> String {
    format!("vela:borrow-lend:repay:{user}:{asset}:{amount}:{nonce}")
}

pub fn liquidate_message(
    liquidator: &str,
    borrower: &str,
    repay_asset: &str,
    repay_amount: u128,
    seize_asset: &str,
    nonce: u64,
) -> String {
    format!(
        "vela:borrow-lend:liquidate:{liquidator}:{borrower}:{repay_asset}:{repay_amount}:{seize_asset}:{nonce}"
    )
}

// ---------- Request bodies ----------

#[derive(Debug, Clone, Deserialize)]
pub struct SupplyBody {
    pub user: String,
    pub signature: String,
    pub asset: String,
    /// Native amount, 1e6 scale for USDC / wBTC / ETH (matches Vela's
    /// existing spot balance units).
    pub amount: u128,
    pub nonce: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BorrowBody {
    pub user: String,
    pub signature: String,
    pub asset: String,
    pub amount: u128,
    pub nonce: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiquidateBody {
    pub liquidator: String,
    pub signature: String,
    pub borrower: String,
    pub repay_asset: String,
    pub repay_amount: u128,
    pub seize_asset: String,
    pub nonce: u64,
}

// ---------- Handlers ----------

fn err_response(code: StatusCode, msg: impl Into<String>) -> axum::response::Response {
    (code, Json(ApiResponse::<()>::err(msg.into()))).into_response()
}

async fn accrue_market(reg: &Arc<BorrowLendRegistry>, asset: &str) -> Option<()> {
    // Refresh every market's mark price from the oracle before we
    // accrue. Doing it here (rather than at each handler entry) keeps
    // all borrow / withdraw / liquidate paths oracle-fed by default.
    reg.refresh_prices_from_oracle();
    let mut entry = reg.markets.get_mut(asset)?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    accrue(entry.value_mut(), now_ms);
    Some(())
}

pub async fn supply_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SupplyBody>,
) -> axum::response::Response {
    let msg = supply_message(&body.user, &body.asset, body.amount, body.nonce);
    if verify_matches_async(msg.into_bytes(), body.signature.clone(), body.user.clone())
        .await
        .is_err()
    {
        return err_response(StatusCode::UNAUTHORIZED, "signature verification failed");
    }
    if accrue_market(&state.borrow_lend, &body.asset)
        .await
        .is_none()
    {
        return err_response(StatusCode::NOT_FOUND, "unsupported asset");
    }
    let market = state.borrow_lend.markets.get(&body.asset).unwrap();
    let supply_index = market.supply_index;
    drop(market);

    let user_lower = body.user.to_ascii_lowercase();
    let key = (user_lower.clone(), body.asset.clone());
    let added = to_scaled(body.amount, supply_index);
    state
        .borrow_lend
        .positions
        .entry(key)
        .or_default()
        .supply_scaled += added;
    if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.asset) {
        m.total_supply += body.amount;
    }

    let payload = serde_json::json!({
        "supplied_amount": body.amount,
        "supply_scaled_added": added,
        "user": user_lower,
        "asset": body.asset,
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

pub async fn withdraw_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SupplyBody>,
) -> axum::response::Response {
    let msg = withdraw_message(&body.user, &body.asset, body.amount, body.nonce);
    if verify_matches_async(msg.into_bytes(), body.signature.clone(), body.user.clone())
        .await
        .is_err()
    {
        return err_response(StatusCode::UNAUTHORIZED, "signature verification failed");
    }
    if accrue_market(&state.borrow_lend, &body.asset)
        .await
        .is_none()
    {
        return err_response(StatusCode::NOT_FOUND, "unsupported asset");
    }
    let market = state.borrow_lend.markets.get(&body.asset).unwrap();
    let supply_index = market.supply_index;
    drop(market);

    let user_lower = body.user.to_ascii_lowercase();
    let key = (user_lower.clone(), body.asset.clone());
    let mut pos = match state.borrow_lend.positions.get_mut(&key) {
        Some(p) => p,
        None => return err_response(StatusCode::NOT_FOUND, "no supply position"),
    };
    let native = from_scaled(pos.supply_scaled, supply_index);
    if body.amount > native {
        return err_response(
            StatusCode::BAD_REQUEST,
            format!("withdraw {} exceeds native supply {}", body.amount, native),
        );
    }
    // Simulate the post-withdraw health factor.
    let removed_scaled = to_scaled(body.amount, supply_index);
    let saved_scaled = pos.supply_scaled;
    pos.supply_scaled -= removed_scaled;
    drop(pos);

    let hf = health_factor_bps(&state.borrow_lend, &user_lower);
    if hf < BPS_DENOM {
        // Roll back.
        if let Some(mut p) = state.borrow_lend.positions.get_mut(&key) {
            p.supply_scaled = saved_scaled;
        }
        return err_response(
            StatusCode::CONFLICT,
            format!(
                "withdraw would push health factor to {} bps (< 10_000)",
                hf.min(u128::from(u32::MAX))
            ),
        );
    }
    if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.asset) {
        m.total_supply = m.total_supply.saturating_sub(body.amount);
    }

    let payload = serde_json::json!({
        "withdrawn": body.amount,
        "user": user_lower,
        "asset": body.asset,
        "post_health_bps": hf.min(u128::from(u64::MAX)),
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

pub async fn borrow_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BorrowBody>,
) -> axum::response::Response {
    let msg = borrow_message(&body.user, &body.asset, body.amount, body.nonce);
    if verify_matches_async(msg.into_bytes(), body.signature.clone(), body.user.clone())
        .await
        .is_err()
    {
        return err_response(StatusCode::UNAUTHORIZED, "signature verification failed");
    }
    if accrue_market(&state.borrow_lend, &body.asset)
        .await
        .is_none()
    {
        return err_response(StatusCode::NOT_FOUND, "unsupported asset");
    }
    let market = state.borrow_lend.markets.get(&body.asset).unwrap();
    let borrow_index = market.borrow_index;
    let available = market.total_supply.saturating_sub(market.total_borrows);
    let price = market.price_micro_usdc;
    drop(market);
    if body.amount > available {
        return err_response(
            StatusCode::CONFLICT,
            format!(
                "requested {} > available liquidity {}",
                body.amount, available
            ),
        );
    }

    let user_lower = body.user.to_ascii_lowercase();
    let key = (user_lower.clone(), body.asset.clone());
    let added_scaled = to_scaled(body.amount, borrow_index);
    let saved = state
        .borrow_lend
        .positions
        .get(&key)
        .map(|p| p.borrow_scaled)
        .unwrap_or(0);
    state
        .borrow_lend
        .positions
        .entry(key.clone())
        .or_default()
        .borrow_scaled += added_scaled;
    if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.asset) {
        m.total_borrows += body.amount;
    }
    let hf = health_factor_bps(&state.borrow_lend, &user_lower);
    if hf < BPS_DENOM {
        // Roll back.
        if let Some(mut p) = state.borrow_lend.positions.get_mut(&key) {
            p.borrow_scaled = saved;
        }
        if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.asset) {
            m.total_borrows = m.total_borrows.saturating_sub(body.amount);
        }
        return err_response(
            StatusCode::CONFLICT,
            format!(
                "borrow would push health factor to {} bps (< 10_000)",
                hf.min(u128::from(u64::MAX))
            ),
        );
    }

    let payload = serde_json::json!({
        "borrowed": body.amount,
        "borrow_value_micro_usdc": body.amount * price / 1_000_000,
        "user": user_lower,
        "asset": body.asset,
        "post_health_bps": hf.min(u128::from(u64::MAX)),
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

pub async fn repay_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BorrowBody>,
) -> axum::response::Response {
    let msg = repay_message(&body.user, &body.asset, body.amount, body.nonce);
    if verify_matches_async(msg.into_bytes(), body.signature.clone(), body.user.clone())
        .await
        .is_err()
    {
        return err_response(StatusCode::UNAUTHORIZED, "signature verification failed");
    }
    if accrue_market(&state.borrow_lend, &body.asset)
        .await
        .is_none()
    {
        return err_response(StatusCode::NOT_FOUND, "unsupported asset");
    }
    let market = state.borrow_lend.markets.get(&body.asset).unwrap();
    let borrow_index = market.borrow_index;
    drop(market);

    let user_lower = body.user.to_ascii_lowercase();
    let key = (user_lower.clone(), body.asset.clone());
    let mut pos = match state.borrow_lend.positions.get_mut(&key) {
        Some(p) => p,
        None => return err_response(StatusCode::NOT_FOUND, "no borrow position"),
    };
    let outstanding = from_scaled(pos.borrow_scaled, borrow_index);
    let repay_native = body.amount.min(outstanding);
    let repay_scaled = to_scaled(repay_native, borrow_index).min(pos.borrow_scaled);
    pos.borrow_scaled -= repay_scaled;
    drop(pos);
    if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.asset) {
        m.total_borrows = m.total_borrows.saturating_sub(repay_native);
    }
    let payload = serde_json::json!({
        "repaid": repay_native,
        "remaining_borrow_scaled": state
            .borrow_lend
            .positions
            .get(&key)
            .map(|p| p.borrow_scaled)
            .unwrap_or(0),
        "user": user_lower,
        "asset": body.asset,
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

pub async fn liquidate_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiquidateBody>,
) -> axum::response::Response {
    let msg = liquidate_message(
        &body.liquidator,
        &body.borrower,
        &body.repay_asset,
        body.repay_amount,
        &body.seize_asset,
        body.nonce,
    );
    if verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.liquidator.clone(),
    )
    .await
    .is_err()
    {
        return err_response(StatusCode::UNAUTHORIZED, "signature verification failed");
    }

    let borrower_lower = body.borrower.to_ascii_lowercase();
    // Must be liquidatable NOW.
    for asset in [&body.repay_asset, &body.seize_asset] {
        let _ = accrue_market(&state.borrow_lend, asset).await;
    }
    let hf = health_factor_bps(&state.borrow_lend, &borrower_lower);
    if hf >= BPS_DENOM {
        return err_response(
            StatusCode::CONFLICT,
            format!(
                "borrower health factor {} bps ≥ 10_000; not liquidatable",
                hf
            ),
        );
    }

    // Close-factor: liquidator may repay at most 50% of outstanding
    // borrow in repay_asset.
    let borrow_key = (borrower_lower.clone(), body.repay_asset.clone());
    let repay_market = state
        .borrow_lend
        .markets
        .get(&body.repay_asset)
        .ok_or(())
        .map(|m| m.clone());
    let seize_market = state
        .borrow_lend
        .markets
        .get(&body.seize_asset)
        .ok_or(())
        .map(|m| m.clone());
    let (rm, sm) = match (repay_market, seize_market) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return err_response(StatusCode::NOT_FOUND, "unsupported asset"),
    };

    let outstanding = state
        .borrow_lend
        .positions
        .get(&borrow_key)
        .map(|p| from_scaled(p.borrow_scaled, rm.borrow_index))
        .unwrap_or(0);
    let max_repay = outstanding / 2;
    let repay_native = body.repay_amount.min(max_repay);
    if repay_native == 0 {
        return err_response(
            StatusCode::CONFLICT,
            "close-factor cap = 0; nothing to repay",
        );
    }

    // Seize amount = repay_value_usdc × (1 + bonus) / seize_price.
    let repay_value_usdc = repay_native * rm.price_micro_usdc / 1_000_000;
    let seize_value_usdc =
        repay_value_usdc * (BPS_DENOM + sm.config.liquidation_bonus_bps as u128) / BPS_DENOM;
    let seize_native = seize_value_usdc * 1_000_000 / sm.price_micro_usdc;

    let seize_key = (borrower_lower.clone(), body.seize_asset.clone());
    let seize_scaled = to_scaled(seize_native, sm.supply_index);
    // Cap seizure at borrower's actual collateral.
    let available_seize_scaled = state
        .borrow_lend
        .positions
        .get(&seize_key)
        .map(|p| p.supply_scaled)
        .unwrap_or(0);
    let seize_scaled_capped = seize_scaled.min(available_seize_scaled);
    if seize_scaled_capped == 0 {
        return err_response(
            StatusCode::CONFLICT,
            "borrower has no seizable collateral in seize_asset",
        );
    }

    // Apply.
    if let Some(mut p) = state.borrow_lend.positions.get_mut(&borrow_key) {
        let repay_scaled = to_scaled(repay_native, rm.borrow_index).min(p.borrow_scaled);
        p.borrow_scaled -= repay_scaled;
    }
    if let Some(mut p) = state.borrow_lend.positions.get_mut(&seize_key) {
        p.supply_scaled = p.supply_scaled.saturating_sub(seize_scaled_capped);
    }
    if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.repay_asset) {
        m.total_borrows = m.total_borrows.saturating_sub(repay_native);
    }
    if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.seize_asset) {
        m.total_supply = m
            .total_supply
            .saturating_sub(from_scaled(seize_scaled_capped, m.supply_index));
    }
    // Credit the liquidator's own supply position in the seize asset.
    let liquidator_lower = body.liquidator.to_ascii_lowercase();
    let liq_key = (liquidator_lower.clone(), body.seize_asset.clone());
    state
        .borrow_lend
        .positions
        .entry(liq_key)
        .or_default()
        .supply_scaled += seize_scaled_capped;
    if let Some(mut m) = state.borrow_lend.markets.get_mut(&body.seize_asset) {
        m.total_supply += from_scaled(seize_scaled_capped, m.supply_index);
    }

    tracing::info!(
        target: "borrow_lend",
        borrower = %borrower_lower,
        liquidator = %liquidator_lower,
        repay_asset = %body.repay_asset,
        repay_native,
        seize_asset = %body.seize_asset,
        seize_native = from_scaled(seize_scaled_capped, sm.supply_index),
        "liquidation executed"
    );

    let payload = serde_json::json!({
        "repaid": repay_native,
        "seized_native": from_scaled(seize_scaled_capped, sm.supply_index),
        "borrower": borrower_lower,
        "liquidator": liquidator_lower,
        "repay_asset": body.repay_asset,
        "seize_asset": body.seize_asset,
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

pub async fn markets_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    // Trigger accrual on read so displayed indices are current.
    let assets: Vec<String> = state
        .borrow_lend
        .markets
        .iter()
        .map(|e| e.key().clone())
        .collect();
    for a in &assets {
        let _ = accrue_market(&state.borrow_lend, a).await;
    }
    let out: Vec<serde_json::Value> = state
        .borrow_lend
        .markets
        .iter()
        .map(|e| {
            let m = e.value();
            let util = utilization_bps(m);
            let br = borrow_rate_bps(m, util);
            let sr = supply_rate_bps(m, util, br);
            serde_json::json!({
                "asset": e.key(),
                "total_supply": m.total_supply,
                "total_borrows": m.total_borrows,
                "utilization_bps": util,
                "borrow_rate_apr_bps": br,
                "supply_rate_apr_bps": sr,
                "collateral_factor_bps": m.config.collateral_factor_bps,
                "liquidation_bonus_bps": m.config.liquidation_bonus_bps,
                "price_micro_usdc": m.price_micro_usdc,
            })
        })
        .collect();
    (StatusCode::OK, Json(ApiResponse::ok(out))).into_response()
}

pub async fn account_handler(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> axum::response::Response {
    let user_lower = address.to_ascii_lowercase();
    let assets: Vec<String> = state
        .borrow_lend
        .markets
        .iter()
        .map(|e| e.key().clone())
        .collect();
    for a in &assets {
        let _ = accrue_market(&state.borrow_lend, a).await;
    }
    let positions: Vec<serde_json::Value> = state
        .borrow_lend
        .positions
        .iter()
        .filter(|e| e.key().0 == user_lower)
        .map(|e| {
            let (_, asset) = e.key();
            let market = state.borrow_lend.markets.get(asset).unwrap();
            let m = market.value();
            let supply_native = from_scaled(e.value().supply_scaled, m.supply_index);
            let borrow_native = from_scaled(e.value().borrow_scaled, m.borrow_index);
            serde_json::json!({
                "asset": asset,
                "supply_native": supply_native,
                "borrow_native": borrow_native,
                "supply_value_micro_usdc": supply_native * m.price_micro_usdc / 1_000_000,
                "borrow_value_micro_usdc": borrow_native * m.price_micro_usdc / 1_000_000,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "user": user_lower,
        "positions": positions,
        "borrowing_power_micro_usdc": borrowing_power_micro_usdc(&state.borrow_lend, &user_lower),
        "total_borrow_value_micro_usdc": total_borrow_value_micro_usdc(&state.borrow_lend, &user_lower),
        "health_factor_bps": health_factor_bps(&state.borrow_lend, &user_lower).min(u128::from(u64::MAX)),
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_market(asset: &str) -> MarketState {
        MarketState::new(MarketConfig::default_for(asset), 1_000_000)
    }

    #[test]
    fn utilization_math() {
        let mut m = fresh_market("USDC");
        assert_eq!(utilization_bps(&m), 0);
        m.total_supply = 100_000;
        m.total_borrows = 50_000;
        assert_eq!(utilization_bps(&m), 5_000);
        m.total_borrows = 100_000;
        assert_eq!(utilization_bps(&m), 10_000);
    }

    #[test]
    fn borrow_rate_below_kink_is_linear() {
        let m = fresh_market("USDC");
        // At 0 → base (0).
        assert_eq!(borrow_rate_bps(&m, 0), 0);
        // At kink (80%) → base + slope1 = 400.
        assert_eq!(borrow_rate_bps(&m, 8_000), 400);
        // At 40% → half of slope1 = 200.
        assert_eq!(borrow_rate_bps(&m, 4_000), 200);
    }

    #[test]
    fn borrow_rate_above_kink_is_steep() {
        let m = fresh_market("USDC");
        // At 100% → base + slope1 + slope2 = 20_400.
        assert_eq!(borrow_rate_bps(&m, 10_000), 20_400);
        // At 90% → base + slope1 + slope2 * (0.5) = 400 + 10_000 = 10_400.
        assert_eq!(borrow_rate_bps(&m, 9_000), 10_400);
    }

    #[test]
    fn supply_rate_scales_with_utilization_and_reserve() {
        let m = fresh_market("USDC");
        let sr = supply_rate_bps(&m, 5_000, 200); // 50% util, 2% borrow rate
                                                  // 200 * (10000 - 1000)/10000 = 180 → * 5000/10000 = 90
        assert_eq!(sr, 90);
    }

    #[test]
    fn accrue_ticks_indices_forward() {
        let mut m = fresh_market("USDC");
        m.total_supply = 1_000_000;
        m.total_borrows = 500_000; // 50% utilization → 2.5% borrow rate
        let start = m.borrow_index;
        let start_supply = m.supply_index;
        // Advance one year.
        let advance_to = m.last_accrual_ms + 365 * 24 * 60 * 60 * 1_000;
        accrue(&mut m, advance_to);
        // After a year at 2.5% borrow_rate, index should have grown
        // by ~2.5%. Allow some rounding slack.
        let grew = m.borrow_index - start;
        assert!(grew > (start * 24) / 1000);
        assert!(grew < (start * 26) / 1000);
        // Supply grew by borrow_rate × util × (1-rf) = 250 × 0.5 × 0.9
        // = 112.5 bps → ~1.125%.
        let grew_s = m.supply_index - start_supply;
        assert!(grew_s > (start_supply * 10) / 1000);
        assert!(grew_s < (start_supply * 13) / 1000);
    }

    #[test]
    fn to_from_scaled_roundtrip() {
        let idx = RAY + RAY / 100; // 1.01
        let amt = 1_000_000u128;
        let s = to_scaled(amt, idx);
        let back = from_scaled(s, idx);
        // May be off by ≤1 due to integer division.
        assert!(back.abs_diff(amt) <= 1);
    }

    #[test]
    fn borrowing_power_respects_collateral_factor() {
        let reg = BorrowLendRegistry::new();
        reg.seed_defaults();
        let user = "0xabc";
        // Deposit 1 ETH ($3000). collateral_factor 60% → $1800 borrowing
        // power = 1_800_000_000 micro-USDC.
        {
            let m = reg.markets.get("ETH").unwrap();
            let scaled = to_scaled(1_000_000, m.supply_index); // 1 ETH = 1e6 native units at 1e6 scale
            drop(m);
            reg.positions.insert(
                (user.to_string(), "ETH".to_string()),
                UserPosition {
                    supply_scaled: scaled,
                    borrow_scaled: 0,
                },
            );
        }
        let bp = borrowing_power_micro_usdc(&reg, user);
        assert_eq!(bp, 1_800_000_000);
    }

    #[test]
    fn health_factor_infinite_without_borrows() {
        let reg = BorrowLendRegistry::new();
        reg.seed_defaults();
        assert_eq!(health_factor_bps(&reg, "0xabc"), u128::MAX);
    }
}
