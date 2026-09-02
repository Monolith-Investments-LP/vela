//! Perp positions registry + HTTP surface.
//!
//! Thin service layer on top of the `perp` crate. In v1 this owns:
//!
//! - A `PerpMarketRegistry` (DashMap<market_id, MarketState>).
//! - A `PerpPositionRegistry` (DashMap<(user_lower, market_id),
//!   Position>).
//! - Read-only endpoints for `/perp/markets` and
//!   `/perp/account/:address`.
//! - Signed action endpoints for opening / adjusting a position and
//!   for the operator to bump mark/index prices (dev-mode until Pyth
//!   is wired).
//!
//! The **matching engine integration** — i.e. actually routing an
//! order into the CLOB and producing a fill — reuses the existing
//! spot matcher with a per-side leverage cap plugged in. That wiring
//! lives in a follow-up (needs order-type extension in
//! `types::OrderType` and a margin gate in the dispatcher). This
//! module handles the position-side of the ledger so the two halves
//! can be built in parallel.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use dashmap::DashMap;
use perp::{
    accrue_funding, apply_fill, funding_rate_bps_per_hour, margin_report, notional_micro_usdc,
    settle_funding, MarketConfig, MarketState, Position,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::auth::verify_matches_async;
use crate::types::ApiResponse;
use crate::AppState;

pub struct PerpRegistry {
    pub markets: DashMap<String, MarketState>,
    pub positions: DashMap<(String, String), Position>,
    /// Per-market insurance reserve, µUSDC. Fed by 70% of every
    /// liquidation penalty (the other 30% goes to the liquidator).
    pub insurance_reserve: DashMap<String, u128>,
}

impl PerpRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            markets: DashMap::new(),
            positions: DashMap::new(),
            insurance_reserve: DashMap::new(),
        })
    }

    /// Seed defaults matching the perp roadmap: 8 majors at 20×–50×.
    pub fn seed_defaults(self: &Arc<Self>) {
        // (market, max_lev, index_price_micro_usdc)
        for (m, lev, px) in [
            ("BTC-PERP", 50, 60_000_000_000u64),
            ("ETH-PERP", 50, 3_000_000_000u64),
            ("SOL-PERP", 20, 150_000_000u64),
            ("HYPE-PERP", 10, 30_000_000u64),
            ("SUI-PERP", 10, 4_000_000u64),
            ("DOGE-PERP", 10, 250_000u64),
            ("ARB-PERP", 10, 800_000u64),
            ("LINK-PERP", 20, 20_000_000u64),
        ] {
            self.markets.insert(
                m.to_string(),
                MarketState::new(MarketConfig::default_for(m, lev), px),
            );
            self.insurance_reserve.insert(m.to_string(), 0);
        }
    }

    /// Refresh every perp market's mark + index price from the oracle.
    /// The base ticker is derived by stripping the `-PERP` suffix
    /// (e.g. `BTC-PERP` → `BTC`). Missing/stale prices leave the last
    /// observation in place — same rationale as borrow-lend.
    pub fn refresh_marks_from_oracle(&self, oracle: &crate::oracle::PriceOracle) {
        for mut entry in self.markets.iter_mut() {
            let base = entry
                .key()
                .strip_suffix("-PERP")
                .unwrap_or(entry.key().as_str())
                .to_string();
            if let Some(px) = oracle.price(&base) {
                if let Ok(v) = u64::try_from(px) {
                    entry.value_mut().mark_price_micro_usdc = v;
                    entry.value_mut().index_price_micro_usdc = v;
                }
            }
        }
    }

    /// Enumerate positions currently below maintenance margin (i.e.
    /// eligible for public liquidation). Read-only snapshot; safe to
    /// serve from a GET endpoint.
    pub fn liquidatable_candidates(&self) -> Vec<LiquidationCandidate> {
        let mut out = Vec::new();
        for entry in self.positions.iter() {
            let (user_lower, market_id) = entry.key();
            let market = match self.markets.get(market_id) {
                Some(m) => m.clone(),
                None => continue,
            };
            let mut working = entry.value().clone();
            settle_funding(&mut working, market.funding_index);
            let rep = margin_report(&working, &market, 0);
            if !rep.passes_maintenance && working.size != 0 {
                out.push(LiquidationCandidate {
                    user: user_lower.clone(),
                    market: market_id.clone(),
                    size: working.size,
                    entry_price_micro_usdc: working.entry_price,
                    mark_price_micro_usdc: market.mark_price_micro_usdc,
                    notional_micro_usdc: rep.notional_micro_usdc,
                    maintenance_requirement_micro_usdc: rep.maintenance_requirement_micro_usdc,
                    equity_micro_usdc: rep.equity_micro_usdc,
                });
            }
        }
        out
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LiquidationCandidate {
    pub user: String,
    pub market: String,
    pub size: i128,
    pub entry_price_micro_usdc: u64,
    pub mark_price_micro_usdc: u64,
    pub notional_micro_usdc: u128,
    pub maintenance_requirement_micro_usdc: u128,
    pub equity_micro_usdc: i128,
}

/// Fraction of a liquidatable position that a single call may close.
/// Matches the 50% close-factor used by Aave / Compound and by our own
/// spot borrow-lend liquidator.
pub const LIQUIDATION_CLOSE_FACTOR_BPS: u128 = 5_000;

/// Fraction of the liquidation penalty routed to the liquidator. The
/// remainder feeds the insurance reserve.
pub const LIQUIDATOR_BONUS_BPS: u128 = 3_000;

/// Cumulative count of successful liquidations. Exposed via `/metrics`
/// so operators can chart liquidation cascades.
pub static PERP_LIQUIDATIONS_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

// ---------- Signing schemas ----------

pub fn open_message(user: &str, market: &str, size: i128, price: u64, nonce: u64) -> String {
    format!("vela:perp:open:{user}:{market}:{size}:{price}:{nonce}")
}

pub fn mark_price_message(operator: &str, market: &str, price: u64, nonce: u64) -> String {
    format!("vela:perp:mark:{operator}:{market}:{price}:{nonce}")
}

pub fn liquidate_message(liquidator: &str, borrower: &str, market: &str, nonce: u64) -> String {
    format!("vela:perp:liquidate:{liquidator}:{borrower}:{market}:{nonce}")
}

// ---------- Bodies ----------

#[derive(Debug, Clone, Deserialize)]
pub struct OpenBody {
    pub user: String,
    pub signature: String,
    pub market: String,
    /// Signed size in the perp crate's SIZE_SCALE (1e6). Positive =
    /// long, negative = short.
    pub size: i128,
    pub price_micro_usdc: u64,
    pub nonce: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarkBody {
    pub operator: String,
    pub signature: String,
    pub market: String,
    pub mark_price_micro_usdc: u64,
    pub nonce: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LiquidateBody {
    pub liquidator: String,
    pub signature: String,
    pub borrower: String,
    pub market: String,
    pub nonce: u64,
}

// ---------- Handlers ----------

pub async fn markets_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    // Accrue funding on read so displayed indices are current.
    let market_ids: Vec<String> = state.perp.markets.iter().map(|e| e.key().clone()).collect();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    for id in &market_ids {
        if let Some(mut m) = state.perp.markets.get_mut(id) {
            accrue_funding(&mut m, now_ms);
        }
    }
    let out: Vec<serde_json::Value> = state
        .perp
        .markets
        .iter()
        .map(|e| {
            let m = e.value();
            serde_json::json!({
                "market": e.key(),
                "mark_price_micro_usdc": m.mark_price_micro_usdc,
                "index_price_micro_usdc": m.index_price_micro_usdc,
                "funding_index": m.funding_index,
                "funding_rate_bps_per_hour": funding_rate_bps_per_hour(m),
                "gross_open_interest": m.gross_open_interest,
                "net_open_interest": m.net_open_interest,
                "initial_margin_bps": m.config.initial_margin_bps(),
                "maintenance_margin_bps": m.config.maintenance_margin_bps(),
                "max_leverage": m.config.max_leverage,
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
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let mut positions_out: Vec<serde_json::Value> = Vec::new();
    for entry in state.perp.positions.iter() {
        let (u, market_id) = entry.key();
        if u != &user_lower {
            continue;
        }
        let market_snapshot = match state.perp.markets.get(market_id) {
            Some(m) => m.clone(),
            None => continue,
        };
        // Settle funding into a working copy for the report; we do NOT
        // mutate the stored position on a GET.
        let mut working = entry.value().clone();
        settle_funding(&mut working, market_snapshot.funding_index);
        let rep = margin_report(&working, &market_snapshot, 0);
        positions_out.push(serde_json::json!({
            "market": market_id,
            "size": working.size,
            "entry_price_micro_usdc": working.entry_price,
            "realized_pnl_micro_usdc": working.realized_pnl_micro_usdc,
            "notional_micro_usdc": rep.notional_micro_usdc,
            "unrealized_pnl_micro_usdc": rep.unrealized_pnl_micro_usdc,
            "initial_requirement_micro_usdc": rep.initial_requirement_micro_usdc,
            "maintenance_requirement_micro_usdc": rep.maintenance_requirement_micro_usdc,
            "mark_price_micro_usdc": market_snapshot.mark_price_micro_usdc,
        }));
    }
    let _ = now_ms;
    let payload = serde_json::json!({
        "user": user_lower,
        "positions": positions_out,
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

pub async fn open_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenBody>,
) -> axum::response::Response {
    let msg = open_message(
        &body.user,
        &body.market,
        body.size,
        body.price_micro_usdc,
        body.nonce,
    );
    if verify_matches_async(msg.into_bytes(), body.signature.clone(), body.user.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("signature verification failed")),
        )
            .into_response();
    }
    let mut market = match state.perp.markets.get_mut(&body.market) {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("unknown perp market")),
            )
                .into_response();
        }
    };
    // Accrue funding, then update position + OI.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    accrue_funding(&mut market, now_ms);
    let funding_index = market.funding_index;
    let old_gross = market.gross_open_interest;
    let old_net = market.net_open_interest;
    drop(market);

    let user_lower = body.user.to_ascii_lowercase();
    let key = (user_lower.clone(), body.market.clone());
    let mut pos = state.perp.positions.entry(key.clone()).or_default();
    settle_funding(&mut pos, funding_index);
    let old_abs = pos.size.unsigned_abs() as u128;
    apply_fill(&mut pos, body.size, body.price_micro_usdc);
    let new_abs = pos.size.unsigned_abs() as u128;
    let net_delta = body.size;
    let post = serde_json::json!({
        "user": user_lower,
        "market": body.market,
        "size_after": pos.size,
        "entry_price_after": pos.entry_price,
        "realized_pnl_after": pos.realized_pnl_micro_usdc,
    });
    let notional = notional_micro_usdc(&pos, body.price_micro_usdc);
    drop(pos);

    if let Some(mut m) = state.perp.markets.get_mut(&body.market) {
        // Update OI: replace |old| with |new| in gross.
        m.gross_open_interest = old_gross + new_abs - old_abs.min(old_gross);
        m.net_open_interest = old_net + net_delta;
    }
    tracing::info!(
        target: "perp",
        user = %user_lower,
        market = %body.market,
        size = body.size,
        price = body.price_micro_usdc,
        notional_micro_usdc = notional as u64,
        "perp position updated"
    );
    (StatusCode::OK, Json(ApiResponse::ok(post))).into_response()
}

/// Public read of liquidatable positions. Liquidators poll this to
/// find candidates before submitting `POST /perp/liquidate`.
pub async fn liquidatable_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    // Refresh marks against the oracle before we compute candidates;
    // otherwise a stale mark could hide (or fabricate) liquidations.
    state.perp.refresh_marks_from_oracle(&state.oracle);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let market_ids: Vec<String> = state.perp.markets.iter().map(|e| e.key().clone()).collect();
    for id in &market_ids {
        if let Some(mut m) = state.perp.markets.get_mut(id) {
            accrue_funding(&mut m, now_ms);
        }
    }
    let candidates = state.perp.liquidatable_candidates();
    (StatusCode::OK, Json(ApiResponse::ok(candidates))).into_response()
}

/// Execute a public liquidation. The caller signs
/// `vela:perp:liquidate:{liquidator}:{borrower}:{market}:{nonce}` and
/// receives 30% of the maintenance-margin penalty; the remainder feeds
/// the market's insurance reserve.
pub async fn liquidate_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LiquidateBody>,
) -> axum::response::Response {
    let msg = liquidate_message(&body.liquidator, &body.borrower, &body.market, body.nonce);
    if verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.liquidator.clone(),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("signature verification failed")),
        )
            .into_response();
    }

    // Refresh marks from the oracle so we don't liquidate on a stale
    // price. Then accrue funding into the target market before the
    // maintenance check.
    state.perp.refresh_marks_from_oracle(&state.oracle);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    {
        if let Some(mut m) = state.perp.markets.get_mut(&body.market) {
            accrue_funding(&mut m, now_ms);
        } else {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("unknown perp market")),
            )
                .into_response();
        }
    }
    let market_snapshot = state
        .perp
        .markets
        .get(&body.market)
        .map(|m| m.clone())
        .expect("checked above");

    let borrower_lower = body.borrower.to_ascii_lowercase();
    let key = (borrower_lower.clone(), body.market.clone());

    // Take a snapshot for the maintenance check and to compute close size.
    let (pre_size, pre_entry, funding_index, mark_price) = {
        let pos = state
            .perp
            .positions
            .get(&key)
            .map(|p| p.clone())
            .unwrap_or_default();
        (
            pos.size,
            pos.entry_price,
            market_snapshot.funding_index,
            market_snapshot.mark_price_micro_usdc,
        )
    };
    if pre_size == 0 {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err(
                "borrower has no open position in this market",
            )),
        )
            .into_response();
    }

    // Re-run the maintenance check with the freshest snapshot.
    let mut check_pos = state
        .perp
        .positions
        .get(&key)
        .map(|p| p.clone())
        .unwrap_or_default();
    settle_funding(&mut check_pos, funding_index);
    let rep = margin_report(&check_pos, &market_snapshot, 0);
    if rep.passes_maintenance {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err(
                "borrower currently passes maintenance; not liquidatable",
            )),
        )
            .into_response();
    }

    // Close-factor: liquidator may close up to 50% of |size|. Sign is
    // opposite of the borrower's current side so the closing fill
    // realizes P&L against the mark.
    let close_abs =
        (pre_size.unsigned_abs() as u128 * LIQUIDATION_CLOSE_FACTOR_BPS / 10_000u128) as i128;
    if close_abs == 0 {
        return (
            StatusCode::CONFLICT,
            Json(ApiResponse::<()>::err("close-factor rounds to zero")),
        )
            .into_response();
    }
    let close_size = -pre_size.signum() * close_abs;

    // Compute the penalty from the notional actually being closed.
    let close_notional_micro_usdc =
        (close_abs.unsigned_abs() as u128 * mark_price as u128) / perp::PRICE_SCALE as u128;
    let penalty = (close_notional_micro_usdc
        * market_snapshot.config.maintenance_margin_bps() as u128)
        / 10_000u128;
    let liquidator_award = penalty * LIQUIDATOR_BONUS_BPS / 10_000u128;
    let insurance_share = penalty.saturating_sub(liquidator_award);

    // Apply the close to the borrower position; realize funding first.
    {
        let mut pos = state.perp.positions.entry(key.clone()).or_default();
        settle_funding(&mut pos, funding_index);
        apply_fill(&mut pos, close_size, mark_price);
        // Deduct the penalty from the borrower's realized P&L.
        pos.realized_pnl_micro_usdc -= penalty as i128;
    }

    // Credit the liquidator's insurance-like ledger by depositing to
    // their (user, market) position slot as realized P&L. This is a
    // scaffold — a real system would route to a stablecoin balance.
    let liquidator_lower = body.liquidator.to_ascii_lowercase();
    let liq_key = (liquidator_lower.clone(), body.market.clone());
    {
        let mut lp = state.perp.positions.entry(liq_key.clone()).or_default();
        lp.realized_pnl_micro_usdc += liquidator_award as i128;
    }
    if let Some(mut r) = state.perp.insurance_reserve.get_mut(&body.market) {
        *r += insurance_share;
    }

    PERP_LIQUIDATIONS_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tracing::info!(
        target: "perp",
        liquidator = %liquidator_lower,
        borrower = %borrower_lower,
        market = %body.market,
        pre_size = pre_size,
        pre_entry = pre_entry,
        close_size = close_size,
        mark_price,
        penalty_micro_usdc = penalty as u64,
        liquidator_award_micro_usdc = liquidator_award as u64,
        insurance_share_micro_usdc = insurance_share as u64,
        "perp liquidation executed"
    );

    let payload = serde_json::json!({
        "market": body.market,
        "borrower": borrower_lower,
        "liquidator": liquidator_lower,
        "closed_size": close_size,
        "mark_price_micro_usdc": mark_price,
        "penalty_micro_usdc": penalty as u64,
        "liquidator_award_micro_usdc": liquidator_award as u64,
        "insurance_reserve_share_micro_usdc": insurance_share as u64,
    });
    (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
}

/// Background watcher: every 60s, refresh marks from the oracle and log
/// the number of positions currently below maintenance. Does NOT
/// auto-liquidate — the bounty exists for public liquidators.
pub async fn run_liquidation_watcher(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        ticker.tick().await;
        state.perp.refresh_marks_from_oracle(&state.oracle);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let market_ids: Vec<String> = state.perp.markets.iter().map(|e| e.key().clone()).collect();
        for id in &market_ids {
            if let Some(mut m) = state.perp.markets.get_mut(id) {
                accrue_funding(&mut m, now_ms);
            }
        }
        let candidates = state.perp.liquidatable_candidates();
        if !candidates.is_empty() {
            tracing::warn!(
                target: "perp",
                count = candidates.len(),
                "perp positions below maintenance; awaiting public liquidator"
            );
        }
    }
}

pub async fn admin_mark_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MarkBody>,
) -> axum::response::Response {
    // Gate on admin token OR operator signature: for v1 we accept
    // either. Real Pyth wiring will replace this endpoint entirely.
    let msg = mark_price_message(
        &body.operator,
        &body.market,
        body.mark_price_micro_usdc,
        body.nonce,
    );
    if verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.operator.clone(),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("signature verification failed")),
        )
            .into_response();
    }
    let mut market = match state.perp.markets.get_mut(&body.market) {
        Some(m) => m,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("unknown perp market")),
            )
                .into_response();
        }
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    accrue_funding(&mut market, now_ms);
    market.mark_price_micro_usdc = body.mark_price_micro_usdc;
    // v1 uses mark == index unless the caller distinguishes; keep it
    // simple.
    market.index_price_micro_usdc = body.mark_price_micro_usdc;
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serde_json::json!({
            "market": body.market,
            "mark_price_micro_usdc": body.mark_price_micro_usdc,
            "funding_index": market.funding_index,
        }))),
    )
        .into_response()
}
