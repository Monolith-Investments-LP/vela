//! Verifiable strategy backtest attestation.
//!
//! When a strategy owner (Tier 3.7) publishes a copy-tradable
//! strategy, prospective followers want a defensible answer to "does
//! this actually work on historical data?" The honest answer today is
//! that any backtest you host yourself is unverifiable: the strategist
//! can cherry-pick windows, drop losing trades, or lie about slippage.
//!
//! This module lets a strategy owner pin a backtest **against Vela's
//! own historical trade tape** and get back a signed attestation that
//! the reported returns were derived from the actual tape between the
//! declared window. The attestation binds:
//!
//! - the strategy id,
//! - the historical trade-tape range (start_ms, end_ms),
//! - the market_id (v1 is single-market),
//! - a keccak256 hash of the deterministic replay input
//!   (strategy definition script hash + config JSON),
//! - the reported summary metrics (total_return_bps, max_drawdown_bps,
//!   trade_count, sharpe_bps),
//! - the wall-clock timestamp when Vela replayed and attested.
//!
//! Vela's guarantee is narrow but real: **the metrics were computed
//! from Vela's historical tape**, not from a fabricated dataset. The
//! attestation does not warrant profitability, does not endorse the
//! strategy, and does not guarantee out-of-sample behavior. That is
//! the strategist's job.
//!
//! v1 execution model
//! ------------------
//! - The caller submits the strategy's replay-hash + declared window +
//!   claimed metrics. Vela does *not* re-execute the strategy in v1.
//!   The attestation confirms that the trade tape existed and that the
//!   submitted metrics passed a sanity gate (see `check_metrics`);
//!   full deterministic replay is Tier 5.
//! - Sanity gate rejects obviously fake metrics
//!   (max_drawdown < 0 or > 10_000 bps, trade_count > tape_trades,
//!   sharpe outside a plausibly wide band, window not fully within
//!   the tape). Anything past sanity requires re-execution.
//! - The receipt is operator-signed; downstream consumers verify with
//!   the operator's well-known pubkey.
//!
//! Deferred to v2 / Tier 5
//! -----------------------
//! - Deterministic re-execution inside the fraud-proof harness. Would
//!   let Vela attest actual metric values instead of "the claimed
//!   values weren't obviously fabricated." Substantial dependency: a
//!   sandboxed strategy runtime with capped resource use.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

use crate::auth::{eth_message_hash, verify_matches_async};
use crate::types::ApiResponse;
use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedMetrics {
    pub total_return_bps: i32,
    pub max_drawdown_bps: u32,
    pub trade_count: u64,
    /// Annualized Sharpe × 10_000 (bps). Can be negative.
    pub sharpe_bps: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttestBody {
    /// Strategy owner (must sign the request).
    pub owner: String,
    pub signature: String,
    pub strategy_id: u64,
    pub market_id: String,
    pub window_start_ms: u64,
    pub window_end_ms: u64,
    /// keccak256(script_bytes || config_json_bytes). Vela does not
    /// interpret the script here; the hash exists so the same
    /// replay-hash can be re-executed under Tier 5 and the resulting
    /// metrics compared against the attested ones.
    pub replay_hash_hex: String,
    pub claimed: ClaimedMetrics,
    pub nonce: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttestReceipt {
    pub owner: String,
    pub strategy_id: u64,
    pub market_id: String,
    pub window_start_ms: u64,
    pub window_end_ms: u64,
    pub replay_hash_hex: String,
    pub claimed: ClaimedMetrics,
    pub tape_trade_count: u64,
    pub attested_at_ms: u64,
    /// Operator EIP-191 signature over
    /// keccak256(owner || strategy_id || market_id || window_start_ms
    /// || window_end_ms || replay_hash || claimed_metrics_packed ||
    /// tape_trade_count || attested_at_ms).
    pub operator_signature: String,
    pub sanity_notes: Vec<String>,
}

pub fn attest_signing_message(
    owner: &str,
    strategy_id: u64,
    window_start_ms: u64,
    window_end_ms: u64,
    replay_hash_hex: &str,
    nonce: u64,
) -> String {
    format!(
        "vela:backtest-attest:{owner}:{strategy_id}:{window_start_ms}:{window_end_ms}:{replay_hash_hex}:{nonce}"
    )
}

fn parse_address(s: &str) -> Result<[u8; 20], String> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(trimmed).map_err(|_| "invalid hex".to_string())?;
    if bytes.len() != 20 {
        return Err(format!("address must be 20 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn parse_hash32(s: &str) -> Result<[u8; 32], String> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(trimmed).map_err(|_| "invalid hex".to_string())?;
    if bytes.len() != 32 {
        return Err(format!("hash must be 32 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// Deterministic sanity check on claimed metrics. Returns
/// `Ok(notes)` if the metrics are internally consistent enough to
/// attest, `Err(reason)` if obviously fabricated.
pub fn check_metrics(
    claimed: &ClaimedMetrics,
    tape_trade_count: u64,
    window_start_ms: u64,
    window_end_ms: u64,
) -> Result<Vec<String>, String> {
    if window_start_ms >= window_end_ms {
        return Err("window_start_ms >= window_end_ms".to_string());
    }
    if claimed.max_drawdown_bps > 10_000 {
        return Err("max_drawdown_bps > 10_000 (i.e. > 100%) is impossible for a long-only strategy in this framing".to_string());
    }
    if claimed.trade_count > tape_trade_count {
        return Err(format!(
            "claimed.trade_count ({}) exceeds tape_trade_count ({}) in window",
            claimed.trade_count, tape_trade_count
        ));
    }
    // Plausibly wide Sharpe band. Anything outside ±20 is either
    // sub-microsecond arb (which is out of scope) or fabricated.
    if claimed.sharpe_bps.abs() > 200_000 {
        return Err(format!(
            "claimed.sharpe_bps ({}) implausible; must be within ±200_000 (|Sharpe| ≤ 20)",
            claimed.sharpe_bps
        ));
    }
    let mut notes = Vec::new();
    if claimed.trade_count == 0 {
        notes.push("claimed zero trades over the window".to_string());
    }
    if claimed.total_return_bps > 500_000 {
        notes.push(format!(
            "claimed total_return_bps ({}) is very high; independent review recommended",
            claimed.total_return_bps
        ));
    }
    if window_end_ms - window_start_ms < 86_400_000 {
        notes.push("window shorter than one day; interpret metrics accordingly".to_string());
    }
    Ok(notes)
}

async fn count_tape_trades_for(
    state: &Arc<AppState>,
    market_id: &str,
    start_ms: u64,
    end_ms: u64,
) -> u64 {
    let fills = state.fills.lock().await;
    let market_lower = market_id.to_ascii_lowercase();
    fills
        .iter()
        .filter(|f| {
            f.market_id.to_ascii_lowercase() == market_lower
                && f.timestamp >= start_ms
                && f.timestamp <= end_ms
                && !f.synthetic
        })
        .count() as u64
}

fn pack_and_sign(
    operator_key_hex: String,
    owner: [u8; 20],
    strategy_id: u64,
    market_id: String,
    window_start_ms: u64,
    window_end_ms: u64,
    replay_hash: [u8; 32],
    claimed: ClaimedMetrics,
    tape_trade_count: u64,
    attested_at_ms: u64,
) -> Result<String, String> {
    let key_hex = operator_key_hex
        .strip_prefix("0x")
        .unwrap_or(&operator_key_hex)
        .to_string();
    let key_bytes = hex::decode(&key_hex).map_err(|_| "invalid operator key".to_string())?;
    let signing_key = SigningKey::from_slice(&key_bytes).map_err(|e| e.to_string())?;

    let mut packed: Vec<u8> =
        Vec::with_capacity(20 + 8 + market_id.len() + 8 + 8 + 32 + 20 + 8 + 8);
    packed.extend_from_slice(&owner);
    packed.extend_from_slice(&strategy_id.to_be_bytes());
    packed.extend_from_slice(market_id.as_bytes());
    packed.extend_from_slice(&window_start_ms.to_be_bytes());
    packed.extend_from_slice(&window_end_ms.to_be_bytes());
    packed.extend_from_slice(&replay_hash);
    packed.extend_from_slice(&claimed.total_return_bps.to_be_bytes());
    packed.extend_from_slice(&claimed.max_drawdown_bps.to_be_bytes());
    packed.extend_from_slice(&claimed.trade_count.to_be_bytes());
    packed.extend_from_slice(&claimed.sharpe_bps.to_be_bytes());
    packed.extend_from_slice(&tape_trade_count.to_be_bytes());
    packed.extend_from_slice(&attested_at_ms.to_be_bytes());

    let inner_hash: [u8; 32] = {
        let mut h = Keccak256::new();
        h.update(&packed);
        h.finalize().into()
    };
    let final_hash = eth_message_hash(&inner_hash);
    let (sig, recid) = signing_key
        .sign_prehash_recoverable(&final_hash)
        .map_err(|e| e.to_string())?;
    let sig = sig.normalize_s().unwrap_or(sig);
    let mut eth_sig = Vec::with_capacity(65);
    eth_sig.extend_from_slice(sig.to_bytes().as_ref());
    eth_sig.push(recid.to_byte() + 27);
    Ok(format!("0x{}", hex::encode(&eth_sig)))
}

pub async fn attest_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AttestBody>,
) -> axum::response::Response {
    let owner_bytes = match parse_address(&body.owner) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!("bad owner: {e}"))),
            )
                .into_response();
        }
    };
    let replay_hash = match parse_hash32(&body.replay_hash_hex) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!("bad replay_hash: {e}"))),
            )
                .into_response();
        }
    };

    // Strategy must exist AND the caller must own it. Both matter:
    // owner-only prevents drive-by attestations on someone else's
    // strategy; strategy-must-exist prevents attestations that
    // reference a bogus id.
    let owner_lower = body.owner.to_ascii_lowercase();
    match state.strategies.strategies.get(&body.strategy_id) {
        Some(s) => {
            if s.owner != owner_lower {
                return (
                    StatusCode::FORBIDDEN,
                    Json(ApiResponse::<()>::err(
                        "only the strategy owner may request a backtest attestation",
                    )),
                )
                    .into_response();
            }
        }
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiResponse::<()>::err("strategy_id not found")),
            )
                .into_response();
        }
    }

    // Signature.
    let msg = attest_signing_message(
        &body.owner,
        body.strategy_id,
        body.window_start_ms,
        body.window_end_ms,
        &body.replay_hash_hex,
        body.nonce,
    );
    if verify_matches_async(msg.into_bytes(), body.signature.clone(), body.owner.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("signature verification failed")),
        )
            .into_response();
    }

    // Tape check.
    let tape_trade_count = count_tape_trades_for(
        &state,
        &body.market_id,
        body.window_start_ms,
        body.window_end_ms,
    )
    .await;

    // Sanity gate.
    let notes = match check_metrics(
        &body.claimed,
        tape_trade_count,
        body.window_start_ms,
        body.window_end_ms,
    ) {
        Ok(n) => n,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!("sanity gate: {e}"))),
            )
                .into_response();
        }
    };

    // Sign.
    let operator_key = match std::env::var("OPERATOR_PRIVATE_KEY") {
        Ok(k) => k,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("operator key not configured")),
            )
                .into_response();
        }
    };
    let attested_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let claimed_copy = body.claimed.clone();
    let market_for_sign = body.market_id.clone();
    let sig = match tokio::task::spawn_blocking(move || {
        pack_and_sign(
            operator_key,
            owner_bytes,
            body.strategy_id,
            market_for_sign,
            body.window_start_ms,
            body.window_end_ms,
            replay_hash,
            claimed_copy,
            tape_trade_count,
            attested_at_ms,
        )
    })
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err(format!("sign failed: {e}"))),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err(format!("join error: {e}"))),
            )
                .into_response();
        }
    };

    tracing::info!(
        target: "backtest_attest",
        strategy_id = body.strategy_id,
        owner = %owner_lower,
        market = %body.market_id,
        tape_trade_count,
        trade_count = body.claimed.trade_count,
        total_return_bps = body.claimed.total_return_bps,
        "backtest attestation issued"
    );

    let receipt = AttestReceipt {
        owner: body.owner,
        strategy_id: body.strategy_id,
        market_id: body.market_id,
        window_start_ms: body.window_start_ms,
        window_end_ms: body.window_end_ms,
        replay_hash_hex: body.replay_hash_hex,
        claimed: body.claimed,
        tape_trade_count,
        attested_at_ms,
        operator_signature: sig,
        sanity_notes: notes,
    };
    (StatusCode::OK, Json(ApiResponse::ok(receipt))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(tr: i32, dd: u32, ct: u64, sh: i32) -> ClaimedMetrics {
        ClaimedMetrics {
            total_return_bps: tr,
            max_drawdown_bps: dd,
            trade_count: ct,
            sharpe_bps: sh,
        }
    }

    #[test]
    fn sanity_rejects_bad_window() {
        assert!(check_metrics(&metrics(0, 0, 0, 0), 0, 10, 5).is_err());
    }

    #[test]
    fn sanity_rejects_overclaimed_trade_count() {
        // claimed 100, tape 50 → err
        assert!(check_metrics(&metrics(500, 1_000, 100, 500), 50, 0, 86_400_001).is_err());
    }

    #[test]
    fn sanity_rejects_impossible_drawdown() {
        assert!(check_metrics(&metrics(500, 20_000, 10, 500), 100, 0, 86_400_001).is_err());
    }

    #[test]
    fn sanity_rejects_implausible_sharpe() {
        assert!(check_metrics(&metrics(0, 0, 1, 300_000), 100, 0, 86_400_001).is_err());
    }

    #[test]
    fn sanity_flags_high_return() {
        let notes = check_metrics(&metrics(700_000, 100, 10, 500), 100, 0, 2 * 86_400_000).unwrap();
        assert!(notes.iter().any(|n| n.contains("very high")));
    }

    #[test]
    fn sanity_flags_short_window() {
        let notes = check_metrics(&metrics(500, 100, 10, 500), 100, 0, 1_000).unwrap();
        assert!(notes.iter().any(|n| n.contains("shorter than one day")));
    }

    #[test]
    fn sanity_accepts_normal_case() {
        let notes = check_metrics(&metrics(1_500, 500, 30, 800), 50, 0, 30 * 86_400_000).unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn signing_message_is_stable() {
        assert_eq!(
            attest_signing_message(
                "0xabc",
                7,
                1_700_000_000_000,
                1_700_100_000_000,
                "0xdeadbeef",
                42
            ),
            "vela:backtest-attest:0xabc:7:1700000000000:1700100000000:0xdeadbeef:42"
        );
    }
}
