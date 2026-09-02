//! Perpetual futures core (v1: margin math + position accounting +
//! funding accrual).
//!
//! Scope of this crate
//! -------------------
//! - `Position`: per-user, per-market signed size + entry price + last
//!   funding index snapshot. Realized / unrealized PnL derived.
//! - `MarketState`: mark price, index price, cumulative funding index,
//!   open interest, and the config (tick, leverage tier, funding
//!   cadence).
//! - `Margin`: initial + maintenance margin math on cross-margin
//!   accounts. Returns a `MarginReport` similar in shape to the
//!   spot portfolio-margin engine so downstream risk logic can
//!   converge on one interface.
//! - `Funding`: hourly funding rate from `(mark - index) / index`,
//!   clamped ± cap. Continuous accrual by index snapshot.
//! - `Liquidation`: given a `Position` + current mark + maint margin,
//!   compute the liquidation trigger and the price at which the
//!   account first hits it.
//!
//! Out of scope in this crate (deferred to the perp *service* /
//! matching integration in api):
//! - Order-book matching (that's the existing spot matching engine
//!   with a per-side leverage cap plugged in).
//! - Oracle wiring (owned by `pyth` + `oracle` modules in api).
//! - Insurance fund / ADL accounting (owned by `api::insurance`
//!   scaffold in a follow-up).
//!
//! Units
//! -----
//! - Contract size = 1 unit of base asset. `size` is `i128`,
//!   positive = long, negative = short, 1e6 scale (matches Vela's
//!   spot balance units).
//! - Price / notional in `u64` micro-USDC (1e6 scale).
//! - Funding index in `i128`, arbitrary starting value; only diffs
//!   between snapshots matter.

use serde::{Deserialize, Serialize};

pub const BPS_DENOM: i128 = 10_000;
pub const PRICE_SCALE: i128 = 1_000_000;
pub const SIZE_SCALE: i128 = 1_000_000;
pub const SECONDS_PER_HOUR: i128 = 3_600;

// ---------- Config ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketConfig {
    pub market_id: String,
    /// Max leverage the exchange offers on this market.
    /// Initial margin = 1 / max_leverage. Maintenance margin =
    /// maint_leverage_ratio × initial.
    pub max_leverage: u32,
    /// Ratio of maintenance to initial margin, bps.
    /// Default 5_000 (50%): if init is 5%, maint is 2.5%.
    pub maint_ratio_bps: u16,
    /// Funding rate cap per hour, bps. Default 100 (1%/hr = 8760%/yr,
    /// deliberately loose so hourly ticks handle regime shifts).
    pub max_funding_bps_per_hour: i32,
    /// Interest-rate component of funding, bps per hour (constant
    /// carry cost, mimics Binance / Bybit). Default 0.
    pub interest_bps_per_hour: i32,
    /// Contract-size multiplier for size → base conversion. Kept
    /// separate from `SIZE_SCALE` so exotic contracts (e.g. inverse)
    /// can override later.
    pub contract_multiplier: u64,
}

impl MarketConfig {
    pub fn default_for(market_id: &str, max_leverage: u32) -> Self {
        Self {
            market_id: market_id.to_string(),
            max_leverage,
            maint_ratio_bps: 5_000,
            max_funding_bps_per_hour: 100,
            interest_bps_per_hour: 0,
            contract_multiplier: 1,
        }
    }

    pub fn initial_margin_bps(&self) -> u32 {
        (BPS_DENOM as u32) / self.max_leverage.max(1)
    }

    pub fn maintenance_margin_bps(&self) -> u32 {
        self.initial_margin_bps() * self.maint_ratio_bps as u32 / BPS_DENOM as u32
    }
}

// ---------- Position ----------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Position {
    pub size: i128,
    /// Volume-weighted entry price in micro-USDC. Zero when size is 0.
    pub entry_price: u64,
    /// Cumulative funding index at last settlement.
    pub funding_index_snapshot: i128,
    /// Realized P&L, micro-USDC. Includes accrued funding.
    pub realized_pnl_micro_usdc: i128,
}

impl Position {
    pub fn is_flat(&self) -> bool {
        self.size == 0
    }
}

// ---------- Market state ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketState {
    pub config: MarketConfig,
    pub mark_price_micro_usdc: u64,
    pub index_price_micro_usdc: u64,
    /// Cumulative funding index (i128 for room to grow). Longs pay
    /// (index_delta × size) when positive; shorts receive it.
    pub funding_index: i128,
    pub last_funding_ts_ms: u64,
    /// Net signed open interest, size units. Positive = more longs
    /// than shorts (should always net to zero for a v1 perp, but the
    /// field is here for observability).
    pub net_open_interest: i128,
    /// Gross open interest (Σ |size|) for margin capacity checks.
    pub gross_open_interest: u128,
}

impl MarketState {
    pub fn new(config: MarketConfig, index_price: u64) -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            config,
            mark_price_micro_usdc: index_price,
            index_price_micro_usdc: index_price,
            funding_index: 0,
            last_funding_ts_ms: now_ms,
            net_open_interest: 0,
            gross_open_interest: 0,
        }
    }
}

// ---------- Funding ----------

/// Instantaneous funding rate in bps per hour, clamped to
/// ±`config.max_funding_bps_per_hour`. Formula:
///   raw = (mark - index) / index + interest
pub fn funding_rate_bps_per_hour(m: &MarketState) -> i32 {
    let idx = m.index_price_micro_usdc.max(1) as i128;
    let premium_bps = ((m.mark_price_micro_usdc as i128 - idx) * BPS_DENOM) / idx;
    let raw = premium_bps as i64 + m.config.interest_bps_per_hour as i64;
    let cap = m.config.max_funding_bps_per_hour as i64;
    raw.clamp(-cap, cap) as i32
}

/// Advance the market's funding_index by the accrued rate since
/// `last_funding_ts_ms`. Returns the delta applied (bps per hour ×
/// hours elapsed, scaled to the index space).
pub fn accrue_funding(m: &mut MarketState, now_ms: u64) -> i128 {
    if now_ms <= m.last_funding_ts_ms {
        return 0;
    }
    let elapsed_ms = (now_ms - m.last_funding_ts_ms) as i128;
    let rate_bph = funding_rate_bps_per_hour(m) as i128;
    // Index delta = rate_bph × hours_elapsed × PRICE_SCALE / BPS_DENOM,
    // to keep the index in the same unit space as prices.
    let hours = elapsed_ms; // in ms; divide by ms/hr below.
    let ms_per_hour: i128 = 3_600_000;
    let delta = (rate_bph * hours * PRICE_SCALE) / (BPS_DENOM * ms_per_hour);
    m.funding_index += delta;
    m.last_funding_ts_ms = now_ms;
    delta
}

/// Funding owed by (or paid to) this position since its snapshot.
/// Positive = position owes; negative = position is credited.
pub fn funding_owed(pos: &Position, market_funding_index: i128) -> i128 {
    let delta = market_funding_index - pos.funding_index_snapshot;
    // Long positions with positive delta owe. Short with positive
    // delta receive. Multiply by signed size.
    (delta * pos.size) / SIZE_SCALE
}

/// Settle funding into realized_pnl on `pos`, updating the snapshot.
pub fn settle_funding(pos: &mut Position, market_funding_index: i128) {
    let owed = funding_owed(pos, market_funding_index);
    // Owed reduces P&L (i.e. is subtracted).
    pos.realized_pnl_micro_usdc -= owed;
    pos.funding_index_snapshot = market_funding_index;
}

// ---------- Fills ----------

/// Apply an incoming fill to a position (cross-margin, single market
/// slot). `fill_size` signed (positive = buy/long, negative =
/// sell/short); `fill_price` in micro-USDC. Realized P&L accrues on
/// reducing/closing trades. Entry price is a running volume-weighted
/// average across the current same-sign portion.
pub fn apply_fill(pos: &mut Position, fill_size: i128, fill_price: u64) {
    if fill_size == 0 {
        return;
    }
    let old_size = pos.size;
    let new_size = old_size + fill_size;

    // Closing / flipping paths first — realize P&L on the closed portion.
    if old_size != 0 && (old_size.signum() != fill_size.signum()) {
        // The trade reduces the position. Portion closed = min(|old|, |fill|).
        let closed = fill_size.unsigned_abs().min(old_size.unsigned_abs()) as i128;
        let close_dir = old_size.signum();
        // PnL per unit = (fill_price - entry_price) * close_dir
        let pnl_per_unit = (fill_price as i128 - pos.entry_price as i128) * close_dir;
        // PnL = pnl_per_unit × closed × contract_size_scale
        // (units convention: closed is scaled by SIZE_SCALE already).
        let pnl = (pnl_per_unit * closed) / PRICE_SCALE;
        pos.realized_pnl_micro_usdc += pnl;

        if new_size.signum() == 0 {
            pos.entry_price = 0;
            pos.size = 0;
            return;
        }
        if new_size.signum() != old_size.signum() {
            // Flipped: entry becomes the fill price on the residual.
            pos.entry_price = fill_price;
            pos.size = new_size;
            return;
        }
        // Purely reducing without flipping: entry stays.
        pos.size = new_size;
        return;
    }

    // Adding to (or opening) a position of same sign: VWAP.
    if old_size == 0 {
        pos.entry_price = fill_price;
        pos.size = fill_size;
        return;
    }
    let old_notional = pos.entry_price as i128 * old_size.abs();
    let add_notional = fill_price as i128 * fill_size.abs();
    let new_abs = old_size.abs() + fill_size.abs();
    let new_entry = (old_notional + add_notional) / new_abs;
    pos.entry_price = new_entry as u64;
    pos.size = new_size;
}

// ---------- PnL ----------

pub fn unrealized_pnl_micro_usdc(pos: &Position, mark_price: u64) -> i128 {
    if pos.size == 0 {
        return 0;
    }
    let per_unit = (mark_price as i128 - pos.entry_price as i128) * pos.size.signum();
    (per_unit * pos.size.abs()) / PRICE_SCALE
}

pub fn notional_micro_usdc(pos: &Position, mark_price: u64) -> u128 {
    (pos.size.unsigned_abs() * mark_price as u128) / PRICE_SCALE as u128
}

// ---------- Margin ----------

#[derive(Debug, Clone, Serialize)]
pub struct MarginReport {
    pub notional_micro_usdc: u128,
    pub initial_requirement_micro_usdc: u128,
    pub maintenance_requirement_micro_usdc: u128,
    pub unrealized_pnl_micro_usdc: i128,
    pub equity_micro_usdc: i128,
    pub passes_initial: bool,
    pub passes_maintenance: bool,
}

/// Margin report for a single position + a cash balance (equity that
/// backs this position). Cross-market portfolio margin composes these.
pub fn margin_report(pos: &Position, market: &MarketState, cash_micro_usdc: i128) -> MarginReport {
    let notional = notional_micro_usdc(pos, market.mark_price_micro_usdc);
    let init_req = (notional * market.config.initial_margin_bps() as u128) / BPS_DENOM as u128;
    let maint_req = (notional * market.config.maintenance_margin_bps() as u128) / BPS_DENOM as u128;
    let upnl = unrealized_pnl_micro_usdc(pos, market.mark_price_micro_usdc);
    let equity = cash_micro_usdc + upnl + pos.realized_pnl_micro_usdc;
    MarginReport {
        notional_micro_usdc: notional,
        initial_requirement_micro_usdc: init_req,
        maintenance_requirement_micro_usdc: maint_req,
        unrealized_pnl_micro_usdc: upnl,
        equity_micro_usdc: equity,
        passes_initial: equity >= init_req as i128,
        passes_maintenance: equity >= maint_req as i128,
    }
}

/// Liquidation trigger price: mark at which equity == maintenance.
/// Returns `None` for flat positions. For a long position, that's
/// the price where cash + (liq_price - entry) × size == maint_req,
/// solved for liq_price.
pub fn liquidation_price_micro_usdc(
    pos: &Position,
    market: &MarketState,
    cash_micro_usdc: i128,
) -> Option<u64> {
    if pos.size == 0 {
        return None;
    }
    let maint_bps = market.config.maintenance_margin_bps() as i128;
    let sign = pos.size.signum();
    // For a long: liq_price = entry - (cash - maint_req_at_entry) * PRICE_SCALE / size
    // But maint_req scales with |size| × price, so we approximate by
    // using entry-price notional as the maintenance base (conservative
    // for longs, tight for shorts). Fine as a UI signal in v1.
    let entry_notional = notional_micro_usdc(pos, pos.entry_price.max(1)) as i128;
    let maint_req_at_entry = (entry_notional * maint_bps) / BPS_DENOM;
    let delta_needed = (cash_micro_usdc - maint_req_at_entry) * PRICE_SCALE / pos.size.abs();
    let liq_price = (pos.entry_price as i128) - sign * delta_needed;
    if liq_price <= 0 {
        Some(0)
    } else {
        Some(liq_price as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market_at(price: u64, max_lev: u32) -> MarketState {
        MarketState::new(MarketConfig::default_for("BTC-PERP", max_lev), price)
    }

    #[test]
    fn initial_and_maintenance_margin_defaults() {
        let cfg = MarketConfig::default_for("BTC-PERP", 20);
        assert_eq!(cfg.initial_margin_bps(), 500); // 1/20 = 5%
        assert_eq!(cfg.maintenance_margin_bps(), 250); // 50% of init
    }

    #[test]
    fn open_position_sets_entry_and_size() {
        let mut p = Position::default();
        apply_fill(&mut p, 500_000, 60_000_000_000); // buy 0.5 BTC @ $60k
        assert_eq!(p.size, 500_000);
        assert_eq!(p.entry_price, 60_000_000_000);
    }

    #[test]
    fn add_to_position_computes_vwap() {
        let mut p = Position::default();
        apply_fill(&mut p, 1_000_000, 60_000_000_000);
        apply_fill(&mut p, 1_000_000, 62_000_000_000);
        assert_eq!(p.size, 2_000_000);
        assert_eq!(p.entry_price, 61_000_000_000);
    }

    #[test]
    fn close_realizes_pnl() {
        let mut p = Position::default();
        apply_fill(&mut p, 1_000_000, 60_000_000_000);
        apply_fill(&mut p, -1_000_000, 62_000_000_000);
        assert_eq!(p.size, 0);
        // profit = ($62k - $60k) × 1 BTC = $2k = 2_000_000_000 μUSDC
        assert_eq!(p.realized_pnl_micro_usdc, 2_000_000_000);
    }

    #[test]
    fn flip_short_to_long_realizes_and_reopens() {
        let mut p = Position::default();
        apply_fill(&mut p, -1_000_000, 60_000_000_000); // short 1 BTC @ 60k
        apply_fill(&mut p, 3_000_000, 58_000_000_000); // buy 3 BTC @ 58k
                                                       // Closes short at profit ($2k), residual = long 2 BTC @ $58k
        assert_eq!(p.size, 2_000_000);
        assert_eq!(p.entry_price, 58_000_000_000);
        assert_eq!(p.realized_pnl_micro_usdc, 2_000_000_000);
    }

    #[test]
    fn unrealized_pnl_signs_correctly() {
        let mut p = Position::default();
        apply_fill(&mut p, 1_000_000, 60_000_000_000);
        assert_eq!(unrealized_pnl_micro_usdc(&p, 65_000_000_000), 5_000_000_000);
        assert_eq!(
            unrealized_pnl_micro_usdc(&p, 55_000_000_000),
            -5_000_000_000
        );
    }

    #[test]
    fn margin_report_passes_at_5x() {
        // 1 BTC long @ $60k, 20× leverage → init margin = 5% = $3k.
        // Post $4k cash → passes both maint and init.
        let m = market_at(60_000_000_000, 20);
        let mut p = Position::default();
        apply_fill(&mut p, 1_000_000, 60_000_000_000);
        let r = margin_report(&p, &m, 4_000_000_000);
        assert!(r.passes_initial);
        assert!(r.passes_maintenance);
    }

    #[test]
    fn margin_report_fails_when_undermarginated() {
        let m = market_at(60_000_000_000, 20);
        let mut p = Position::default();
        apply_fill(&mut p, 1_000_000, 60_000_000_000);
        // Only $1k cash → below both requirements.
        let r = margin_report(&p, &m, 1_000_000_000);
        assert!(!r.passes_initial);
        assert!(!r.passes_maintenance);
    }

    #[test]
    fn funding_positive_when_mark_above_index() {
        let mut m = market_at(60_000_000_000, 20);
        m.mark_price_micro_usdc = 60_600_000_000; // +1% premium
        let rate = funding_rate_bps_per_hour(&m);
        // Clamped at 100 bps/hour.
        assert_eq!(rate, 100);
    }

    #[test]
    fn funding_negative_when_mark_below_index() {
        let mut m = market_at(60_000_000_000, 20);
        m.mark_price_micro_usdc = 59_700_000_000; // -0.5% premium
        let rate = funding_rate_bps_per_hour(&m);
        assert_eq!(rate, -50);
    }

    #[test]
    fn accrue_funding_ticks_index() {
        let mut m = market_at(60_000_000_000, 20);
        m.mark_price_micro_usdc = 60_600_000_000;
        let start = m.last_funding_ts_ms;
        let delta = accrue_funding(&mut m, start + 3_600_000); // 1 hour
        assert!(delta > 0);
    }

    #[test]
    fn funding_owed_signs_correctly_for_long_and_short() {
        let mut m = market_at(60_000_000_000, 20);
        m.funding_index = 1_000_000; // deliberate positive index
        let mut long = Position::default();
        apply_fill(&mut long, 1_000_000, 60_000_000_000);
        let mut short = Position::default();
        apply_fill(&mut short, -1_000_000, 60_000_000_000);
        assert!(funding_owed(&long, m.funding_index) > 0);
        assert!(funding_owed(&short, m.funding_index) < 0);
    }
}
