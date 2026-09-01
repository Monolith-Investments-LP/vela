//! ERC-8004-style agent reputation adapter.
//!
//! ERC-8004 (draft, Q4 2025) standardises how autonomous agents publish
//! and verify reputation across venues. The core primitive is a
//! *portable reputation attestation*: any registered issuer can sign a
//! `(subject, score, dimension, expires_at)` tuple, and any relying
//! party can verify the signature against the issuer's on-chain
//! identity commitment. Reputation stops being locked to one venue.
//!
//! Vela is a natural issuer. Every fill it processes is signed, batched,
//! and (eventually) rolled into a fraud-proof-verifiable state root, so
//! the raw evidence for a reputation score is already public. Emitting
//! signed attestations on top costs almost nothing and lets external
//! agents use Vela's historical behavior as reputation collateral when
//! interacting with other venues.
//!
//! v1 scope
//! --------
//! - **Issue**: compute an aggregate reputation score for an address
//!   from local state (avg toxicity, fill notional, uptime, cancel
//!   ratio), sign it with the operator key, return an attestation
//!   record. `POST /reputation/attest/:address`.
//! - **Fetch cached**: read back the last attestation without
//!   re-signing. `GET /reputation/:address`.
//! - **Relying-party verify**: publish the ABI-encoded attestation so
//!   external contracts / off-chain verifiers can pull it in.
//!
//! On-chain registry integration (queueing a Merkle root of attestation
//! digests into VelaSettlement.sol) is deferred to Tier 3.9 (credit
//! lines actually consume this).
//!
//! Score model (v1)
//! ---------------
//! Weighted average of:
//! - `toxicity_component = 1.0 - avg_toxicity` in [0, 1]
//!   (higher = cleaner flow)
//! - `volume_component = min(1.0, taker_notional_usdc / 1_000_000)`
//! - `activity_component = min(1.0, taker_fill_count / 100)`
//! Final score = 0.5 * toxicity + 0.3 * volume + 0.2 * activity, in
//! `[0.0, 1.0]`. Encoded on-wire as `u16` in `[0, 10_000]` bps so
//! ERC-8004 relying parties don't have to deal with float encoding.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

use crate::agent_tox;
use crate::auth::eth_message_hash;
use crate::types::ApiResponse;
use crate::AppState;

pub const DIMENSION_TRADING: &str = "trading.execution-quality";
pub const SCORE_SCALE_BPS: u32 = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationScore {
    pub address: String,
    pub dimension: String,
    /// Score in bps (0 = worst, 10_000 = best). u16 fits and stays
    /// ABI-friendly for ERC-8004 relying parties.
    pub score_bps: u16,
    /// Underlying components for transparency. Not signed; only the
    /// score_bps + address + dimension + expires_at are covered by the
    /// operator signature.
    pub components: ReputationComponents,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    /// EIP-191 operator signature over
    /// keccak256(address || dimension_bytes || score_bps_be ||
    /// expires_at_ms_be).
    pub operator_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationComponents {
    pub avg_toxicity: f64,
    pub taker_notional_usdc: f64,
    pub taker_fill_count: u64,
    pub toxicity_component: f64,
    pub volume_component: f64,
    pub activity_component: f64,
}

/// TTL for issued attestations, in ms. Relying parties should treat
/// past expiry as "not valid," which forces re-issuance and prevents
/// stale scores from paying dividends after behavior degrades.
pub fn attestation_ttl_ms() -> u64 {
    std::env::var("VELA_REPUTATION_TTL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24 * 60 * 60 * 1_000)
}

pub fn score_message_digest(
    address: [u8; 20],
    dimension: &str,
    score_bps: u16,
    expires_at_ms: u64,
) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(address);
    h.update(dimension.as_bytes());
    h.update(score_bps.to_be_bytes());
    h.update(expires_at_ms.to_be_bytes());
    h.finalize().into()
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

fn sign_reputation(
    operator_key_hex: String,
    address: [u8; 20],
    dimension: String,
    score_bps: u16,
    expires_at_ms: u64,
) -> Result<String, String> {
    let key_hex = operator_key_hex
        .strip_prefix("0x")
        .unwrap_or(&operator_key_hex)
        .to_string();
    let key_bytes = hex::decode(&key_hex).map_err(|_| "invalid operator key".to_string())?;
    let signing_key = SigningKey::from_slice(&key_bytes).map_err(|e| e.to_string())?;
    let inner = score_message_digest(address, &dimension, score_bps, expires_at_ms);
    let final_hash = eth_message_hash(&inner);
    let (sig, recid) = signing_key
        .sign_prehash_recoverable(&final_hash)
        .map_err(|e| e.to_string())?;
    let sig = sig.normalize_s().unwrap_or(sig);
    let mut eth_sig = Vec::with_capacity(65);
    eth_sig.extend_from_slice(sig.to_bytes().as_ref());
    eth_sig.push(recid.to_byte() + 27);
    Ok(format!("0x{}", hex::encode(&eth_sig)))
}

async fn compute_score(state: &Arc<AppState>, address_lower: &str) -> ReputationComponents {
    let tier = agent_tox::compute_tier(state, address_lower).await;
    let avg_tox = tier.avg_toxicity;
    let fill_count = tier.taker_fill_count;

    // Sum taker notional over the same 30-day window used by
    // compute_tier. Re-scan fills; we're already amortising a full
    // lock in compute_tier so a second pass is cheap.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);
    let mut notional = 0.0f64;
    {
        let fills = state.fills.lock().await;
        for f in fills.iter() {
            if f.timestamp < cutoff {
                continue;
            }
            if f.taker_address.to_ascii_lowercase() != address_lower {
                continue;
            }
            notional += (f.price as f64 * f.quantity as f64) / 1_000_000_000_000.0;
        }
    }

    let toxicity_component = (1.0 - avg_tox).clamp(0.0, 1.0);
    let volume_component = (notional / 1_000_000.0).clamp(0.0, 1.0);
    let activity_component = ((fill_count as f64) / 100.0).clamp(0.0, 1.0);

    ReputationComponents {
        avg_toxicity: avg_tox,
        taker_notional_usdc: notional,
        taker_fill_count: fill_count,
        toxicity_component,
        volume_component,
        activity_component,
    }
}

fn score_bps_from(components: &ReputationComponents) -> u16 {
    let raw = 0.5 * components.toxicity_component
        + 0.3 * components.volume_component
        + 0.2 * components.activity_component;
    let scaled = (raw.clamp(0.0, 1.0) * (SCORE_SCALE_BPS as f64)).round() as u32;
    scaled.min(SCORE_SCALE_BPS) as u16
}

pub async fn attest_handler(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> axum::response::Response {
    let addr = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!("bad address: {e}"))),
            )
                .into_response()
        }
    };
    let addr_lower = address.to_ascii_lowercase();

    let components = compute_score(&state, &addr_lower).await;
    let score_bps = score_bps_from(&components);

    let issued_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let expires_at_ms = issued_at_ms + attestation_ttl_ms();

    let operator_key = match std::env::var("OPERATOR_PRIVATE_KEY") {
        Ok(k) => k,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err("operator key not configured")),
            )
                .into_response()
        }
    };

    let dim = DIMENSION_TRADING.to_string();
    let dim_for_sign = dim.clone();
    let sig = match tokio::task::spawn_blocking(move || {
        sign_reputation(operator_key, addr, dim_for_sign, score_bps, expires_at_ms)
    })
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err(format!("sign failed: {e}"))),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::err(format!("join error: {e}"))),
            )
                .into_response()
        }
    };

    let record = ReputationScore {
        address: format!("0x{}", hex::encode(addr)),
        dimension: dim,
        score_bps,
        components,
        issued_at_ms,
        expires_at_ms,
        operator_signature: sig,
    };

    state
        .reputation_cache
        .insert(addr_lower.clone(), record.clone());

    tracing::info!(
        target: "reputation",
        address = %addr_lower,
        score_bps,
        expires_at_ms,
        "reputation attestation issued"
    );

    (StatusCode::OK, Json(ApiResponse::ok(record))).into_response()
}

pub async fn get_handler(
    State(state): State<Arc<AppState>>,
    Path(address): Path<String>,
) -> axum::response::Response {
    let _ = match parse_address(&address) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!("bad address: {e}"))),
            )
                .into_response()
        }
    };
    let key = address.to_ascii_lowercase();
    match state.reputation_cache.get(&key) {
        Some(r) => (StatusCode::OK, Json(ApiResponse::ok(r.clone()))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err(
                "no attestation cached; call POST /reputation/attest/:address first",
            )),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{signature::hazmat::PrehashVerifier, VerifyingKey};

    #[test]
    fn score_bps_bounded() {
        let c = ReputationComponents {
            avg_toxicity: 0.0,
            taker_notional_usdc: 10_000_000.0,
            taker_fill_count: 1_000,
            toxicity_component: 1.0,
            volume_component: 1.0,
            activity_component: 1.0,
        };
        assert_eq!(score_bps_from(&c), 10_000);

        let z = ReputationComponents {
            avg_toxicity: 1.0,
            taker_notional_usdc: 0.0,
            taker_fill_count: 0,
            toxicity_component: 0.0,
            volume_component: 0.0,
            activity_component: 0.0,
        };
        assert_eq!(score_bps_from(&z), 0);
    }

    #[test]
    fn weighted_average() {
        let c = ReputationComponents {
            avg_toxicity: 0.5,
            taker_notional_usdc: 500_000.0,
            taker_fill_count: 50,
            toxicity_component: 0.5,
            volume_component: 0.5,
            activity_component: 0.5,
        };
        // 0.5 * 0.5 + 0.3 * 0.5 + 0.2 * 0.5 = 0.5 → 5000 bps
        assert_eq!(score_bps_from(&c), 5_000);
    }

    #[test]
    fn attestation_round_trip_verifies() {
        let key_hex = "2222222222222222222222222222222222222222222222222222222222222222";
        let sk = SigningKey::from_slice(&hex::decode(key_hex).unwrap()).unwrap();
        let vk = VerifyingKey::from(&sk);

        let addr = [0xCCu8; 20];
        let dim = "trading.execution-quality";
        let score_bps: u16 = 7_500;
        let expires_at_ms: u64 = 1_700_000_000_000;

        let sig_hex = sign_reputation(
            key_hex.to_string(),
            addr,
            dim.to_string(),
            score_bps,
            expires_at_ms,
        )
        .unwrap();
        let raw = hex::decode(sig_hex.strip_prefix("0x").unwrap()).unwrap();
        assert_eq!(raw.len(), 65);

        let inner = score_message_digest(addr, dim, score_bps, expires_at_ms);
        let final_hash = eth_message_hash(&inner);
        let sig = k256::ecdsa::Signature::from_slice(&raw[..64]).unwrap();
        vk.verify_prehash(&final_hash, &sig).unwrap();
    }
}
