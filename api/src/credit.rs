//! Reputation-collateralized credit lines.
//!
//! Agents (and vaults, and MMs) with a strong reputation attestation
//! (Tier 3.3) can borrow **short-term inventory credit** on Vela: they
//! can post orders that would otherwise fail the `funds_available`
//! check, up to a limit derived from their reputation score. Positions
//! must be closed inside a settlement window (default 5 minutes) or the
//! credit line reverts and any open exposure is force-flattened by the
//! operator.
//!
//! Why bother
//! ----------
//! The classic problem for agent trading is **collateral fragmentation**:
//! an agent that trades on ten venues has to pre-fund each one, and the
//! working capital that sits unused earns nothing. If Vela can extend
//! very short-duration credit to well-behaved agents, we underprice
//! that fragmentation cost for the agents we most want. Reputation is
//! the collateral that makes this work without a trusted lender.
//!
//! Credit sizing
//! -------------
//! - `credit_limit_usdc = score_bps * MAX_CREDIT_PER_BPS`
//!   where `MAX_CREDIT_PER_BPS` defaults to $10/bp, giving a max
//!   line of $100k for a perfect (10000 bps) score.
//! - Attestations expire (Tier 3.3), so a credit line auto-shrinks if
//!   the underlying reputation is stale.
//! - Only addresses with a non-expired attestation on file get any
//!   credit — silently zero for everyone else. No implicit credit.
//!
//! v1 mechanics
//! ------------
//! - `POST /credit/open { address, signature, requested_usdc, nonce }`
//!   → returns `{ granted_usdc, expires_at_ms, line_id }` or an error.
//! - The line is tracked in `AppState.credit_lines` (in-memory DashMap
//!   keyed by lowercase address). At most one live line per address.
//! - Consumption: this scaffold records the granted amount; the
//!   matching engine hook that actually spends it is left as a
//!   follow-up (needs UserState.available_balance to consult the line
//!   before rejecting `insufficient_funds`).
//! - `POST /credit/close { address, signature, line_id, nonce }`
//!   voluntarily returns the line. No penalty if closed before the
//!   window ends.
//! - Automatic expiry sweep runs every 10 s; overdue lines emit a
//!   `credit_line_expired` tracing event so the operator's ops
//!   channel can force-flatten off-line if needed.
//!
//! Not in v1
//! ---------
//! - Interest accrual (all lines are 5-minute at 0 bps).
//! - Multi-line per address.
//! - Cross-venue collateral netting.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::auth::verify_matches_async;
use crate::types::ApiResponse;
use crate::AppState;

pub fn max_credit_per_bp_micro_usdc() -> u64 {
    // Micro-USDC (1e-6). Default $10/bp → 10_000_000 μUSDC/bp.
    std::env::var("VELA_MAX_CREDIT_PER_BP_MICRO_USDC")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000_000)
}

pub fn credit_window_ms() -> u64 {
    std::env::var("VELA_CREDIT_WINDOW_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5 * 60 * 1_000)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditLine {
    pub line_id: String,
    pub address: String,
    /// Granted amount in micro-USDC (1e-6).
    pub granted_micro_usdc: u64,
    /// Amount already drawn against the line, in micro-USDC. v1 tracks
    /// the field but the matching-engine hook that debits it is a
    /// follow-up.
    pub drawn_micro_usdc: u64,
    pub opened_at_ms: u64,
    pub expires_at_ms: u64,
    /// score_bps at time of grant. Snapshotted so a later reputation
    /// downgrade doesn't retroactively shrink an active line.
    pub grant_score_bps: u16,
}

pub type CreditRegistry = Arc<DashMap<String, CreditLine>>;

pub fn new_registry() -> CreditRegistry {
    Arc::new(DashMap::new())
}

fn line_id_for(address_lower: &str, opened_at_ms: u64) -> String {
    use sha3::{Digest, Keccak256};
    let mut h = Keccak256::new();
    h.update(address_lower.as_bytes());
    h.update(opened_at_ms.to_be_bytes());
    let out: [u8; 32] = h.finalize().into();
    format!("cl_{}", hex::encode(&out[..12]))
}

pub fn credit_open_message(address: &str, requested_micro_usdc: u64, nonce: u64) -> String {
    format!("vela:credit-open:{address}:{requested_micro_usdc}:{nonce}")
}

pub fn credit_close_message(address: &str, line_id: &str, nonce: u64) -> String {
    format!("vela:credit-close:{address}:{line_id}:{nonce}")
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpenBody {
    pub address: String,
    pub signature: String,
    /// Amount requested, in micro-USDC (1e-6). Server clamps to the
    /// address's current cap; the response tells the caller what was
    /// actually granted.
    pub requested_micro_usdc: u64,
    pub nonce: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenResponse {
    pub line: CreditLine,
    /// Cap the caller would receive at the current reputation score.
    /// If `granted < cap`, the caller asked for less than the cap;
    /// they can re-open with a higher `requested_micro_usdc` up to
    /// `cap`.
    pub cap_micro_usdc: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloseBody {
    pub address: String,
    pub signature: String,
    pub line_id: String,
    pub nonce: u64,
}

pub async fn open_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<OpenBody>,
) -> axum::response::Response {
    let addr_lower = body.address.to_ascii_lowercase();

    let msg = credit_open_message(&body.address, body.requested_micro_usdc, body.nonce);
    if verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.address.clone(),
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

    // Consult cached reputation attestation. No implicit credit: caller
    // must call /reputation/attest/:address first.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let (score_bps, dimension) = match state.reputation_cache.get(&addr_lower) {
        Some(r) if r.expires_at_ms > now_ms => (r.score_bps, r.dimension.clone()),
        _ => {
            return (
                StatusCode::FORBIDDEN,
                Json(ApiResponse::<()>::err(
                    "no valid reputation attestation on file; call POST /reputation/attest/:address first",
                )),
            )
                .into_response();
        }
    };

    let per_bp = max_credit_per_bp_micro_usdc();
    let cap = (score_bps as u64).saturating_mul(per_bp);
    if cap == 0 {
        return (
            StatusCode::FORBIDDEN,
            Json(ApiResponse::<()>::err(
                "reputation score too low for any credit",
            )),
        )
            .into_response();
    }
    let granted = body.requested_micro_usdc.min(cap);

    // Enforce at most one live line per address. Re-opening overwrites
    // the previous line, which is fine for v1 (drawn amount always
    // reads the latest line).
    let opened_at_ms = now_ms;
    let expires_at_ms = opened_at_ms + credit_window_ms();
    let line = CreditLine {
        line_id: line_id_for(&addr_lower, opened_at_ms),
        address: body.address.clone(),
        granted_micro_usdc: granted,
        drawn_micro_usdc: 0,
        opened_at_ms,
        expires_at_ms,
        grant_score_bps: score_bps,
    };
    state.credit_lines.insert(addr_lower.clone(), line.clone());

    tracing::info!(
        target: "credit",
        address = %addr_lower,
        line_id = %line.line_id,
        granted_micro_usdc = granted,
        cap_micro_usdc = cap,
        score_bps,
        dimension = %dimension,
        expires_at_ms,
        "credit line opened"
    );

    (
        StatusCode::OK,
        Json(ApiResponse::ok(OpenResponse {
            line,
            cap_micro_usdc: cap,
        })),
    )
        .into_response()
}

pub async fn close_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CloseBody>,
) -> axum::response::Response {
    let addr_lower = body.address.to_ascii_lowercase();
    let msg = credit_close_message(&body.address, &body.line_id, body.nonce);
    if verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.address.clone(),
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

    let removed = state
        .credit_lines
        .remove_if(&addr_lower, |_, line| line.line_id == body.line_id);
    match removed {
        Some((_, line)) => {
            tracing::info!(
                target: "credit",
                address = %addr_lower,
                line_id = %line.line_id,
                drawn_micro_usdc = line.drawn_micro_usdc,
                "credit line closed voluntarily"
            );
            (StatusCode::OK, Json(ApiResponse::ok(line))).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err(
                "no matching credit line for that address / line_id",
            )),
        )
            .into_response(),
    }
}

pub async fn get_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(address): axum::extract::Path<String>,
) -> axum::response::Response {
    let key = address.to_ascii_lowercase();
    match state.credit_lines.get(&key) {
        Some(l) => (StatusCode::OK, Json(ApiResponse::ok(l.clone()))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("no live credit line")),
        )
            .into_response(),
    }
}

/// Background sweep: every 10 s, expire any line whose window is up
/// and emit a warn-level tracing event so ops can force-flatten
/// exposure if the address is still drawn.
pub async fn run_expiry_task(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    loop {
        ticker.tick().await;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut expired: Vec<CreditLine> = Vec::new();
        state.credit_lines.retain(|_, line| {
            if line.expires_at_ms <= now_ms {
                expired.push(line.clone());
                false
            } else {
                true
            }
        });
        for line in expired {
            let level_msg = if line.drawn_micro_usdc > 0 {
                "credit_line_expired_with_outstanding_draw"
            } else {
                "credit_line_expired_clean"
            };
            tracing::warn!(
                target: "credit",
                address = %line.address.to_ascii_lowercase(),
                line_id = %line.line_id,
                drawn_micro_usdc = line.drawn_micro_usdc,
                granted_micro_usdc = line.granted_micro_usdc,
                "{level_msg}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_id_is_deterministic() {
        let a = line_id_for("0xdeadbeef", 1_700_000_000_000);
        let b = line_id_for("0xdeadbeef", 1_700_000_000_000);
        assert_eq!(a, b);
        assert!(a.starts_with("cl_"));
        let c = line_id_for("0xdeadbeef", 1_700_000_000_001);
        assert_ne!(a, c);
    }

    #[test]
    fn credit_open_message_is_stable() {
        let m = credit_open_message("0xABC", 12_000_000, 42);
        assert_eq!(m, "vela:credit-open:0xABC:12000000:42");
    }

    #[test]
    fn cap_math() {
        let per_bp = 10_000_000u64; // $10/bp
        let cap: u64 = (7_500u64).saturating_mul(per_bp);
        assert_eq!(cap, 75_000_000_000); // $75_000 USDC
    }
}
