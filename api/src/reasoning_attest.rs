//! Reasoning-trace attestation endpoint.
//!
//! Agents (and the humans supervising them) increasingly need to prove
//! *when* a given model-reasoning trace was committed. The trace itself
//! lives in the caller's own storage (S3, GCS, whatever); Vela commits
//! only the keccak256 hash. This endpoint returns an operator-signed
//! receipt that binds:
//!
//! - the trace hash,
//! - the caller's master address,
//! - the optional agent identifier string,
//! - the wall-clock timestamp the operator saw it,
//!
//! into a single ECDSA signature over an EIP-191 personal-sign
//! envelope. Auditors can later verify the receipt with the operator's
//! well-known public key without trusting Vela's own storage.
//!
//! Storage
//! -------
//! v1 is stateless: we hash, sign, and return. If the caller wants a
//! durable copy of the receipt they must persist the response
//! themselves. Persisting on-chain / on-DA is a follow-up if operators
//! start seeing repeated re-attestations for the same hash.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use k256::ecdsa::SigningKey;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use std::sync::Arc;

use crate::auth::eth_message_hash;
use crate::types::ApiResponse;
use crate::AppState;

#[derive(Debug, Clone, Deserialize)]
pub struct AttestBody {
    /// Master wallet address (0x-prefixed hex, 20 bytes).
    pub address: String,
    /// keccak256(reasoning_trace_bytes) as 0x-prefixed 32-byte hex.
    pub reasoning_trace_hash: String,
    /// Optional free-text agent identifier
    /// (`"claude-opus-4.7"`, `"gpt-5-turbo"`, `"internal-mm-v3"`).
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Optional order_id this trace was attached to. Not required for
    /// pre-flight attestations (agent wants a receipt before the order
    /// is submitted).
    #[serde(default)]
    pub order_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttestReceipt {
    pub address: String,
    pub reasoning_trace_hash: String,
    pub agent_id: Option<String>,
    pub order_id: Option<u64>,
    /// Server wall-clock ms when the operator signed this receipt.
    pub attested_at_ms: u64,
    /// Operator signature over the EIP-191 personal-sign envelope of
    /// keccak256(address || reasoning_trace_hash || agent_id_bytes ||
    /// order_id_be || attested_at_ms_be). 65 bytes, 0x-prefixed hex.
    pub operator_signature: String,
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

fn sign_attestation(
    operator_key_hex: String,
    address: [u8; 20],
    hash: [u8; 32],
    agent_id_bytes: Vec<u8>,
    order_id: u64,
    attested_at_ms: u64,
) -> Result<String, String> {
    let key_hex = operator_key_hex
        .strip_prefix("0x")
        .unwrap_or(&operator_key_hex)
        .to_string();
    let key_bytes = hex::decode(&key_hex).map_err(|_| "invalid operator key".to_string())?;
    let signing_key = SigningKey::from_slice(&key_bytes).map_err(|e| e.to_string())?;

    let mut packed: Vec<u8> = Vec::with_capacity(20 + 32 + agent_id_bytes.len() + 8 + 8);
    packed.extend_from_slice(&address);
    packed.extend_from_slice(&hash);
    packed.extend_from_slice(&agent_id_bytes);
    packed.extend_from_slice(&order_id.to_be_bytes());
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
    State(_state): State<Arc<AppState>>,
    Json(body): Json<AttestBody>,
) -> axum::response::Response {
    let address = match parse_address(&body.address) {
        Ok(a) => a,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!("bad address: {e}"))),
            )
                .into_response()
        }
    };
    let hash = match parse_hash32(&body.reasoning_trace_hash) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiResponse::<()>::err(format!("bad hash: {e}"))),
            )
                .into_response()
        }
    };

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

    let agent_id_bytes = body.agent_id.as_deref().unwrap_or("").as_bytes().to_vec();
    let order_id = body.order_id.unwrap_or(0);
    let attested_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let signature = match tokio::task::spawn_blocking(move || {
        sign_attestation(
            operator_key,
            address,
            hash,
            agent_id_bytes,
            order_id,
            attested_at_ms,
        )
    })
    .await
    {
        Ok(Ok(sig)) => sig,
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

    tracing::info!(
        target: "reasoning_trace",
        address = %body.address.to_lowercase(),
        agent_id = ?body.agent_id,
        reasoning_trace_hash = %body.reasoning_trace_hash,
        order_id = ?body.order_id,
        attested_at_ms,
        "reasoning attestation issued"
    );

    let receipt = AttestReceipt {
        address: body.address,
        reasoning_trace_hash: body.reasoning_trace_hash,
        agent_id: body.agent_id,
        order_id: body.order_id,
        attested_at_ms,
        operator_signature: signature,
    };
    (StatusCode::OK, Json(ApiResponse::ok(receipt))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::{signature::hazmat::PrehashVerifier, VerifyingKey};

    #[test]
    fn sign_and_verify_round_trip() {
        // Fixed 32-byte key so the test is deterministic.
        let key_hex = "1111111111111111111111111111111111111111111111111111111111111111";
        let sk = SigningKey::from_slice(&hex::decode(key_hex).unwrap()).unwrap();
        let vk = VerifyingKey::from(&sk);

        let addr = [0xAAu8; 20];
        let hash = [0xBBu8; 32];
        let agent = b"claude-opus-4.7".to_vec();
        let order_id: u64 = 42;
        let attested_at: u64 = 1_700_000_000_000;

        let sig_hex = sign_attestation(
            key_hex.to_string(),
            addr,
            hash,
            agent.clone(),
            order_id,
            attested_at,
        )
        .unwrap();
        let raw = hex::decode(sig_hex.strip_prefix("0x").unwrap()).unwrap();
        assert_eq!(raw.len(), 65);

        // Reconstruct the message and verify the (r, s) prefix.
        let mut packed = Vec::new();
        packed.extend_from_slice(&addr);
        packed.extend_from_slice(&hash);
        packed.extend_from_slice(&agent);
        packed.extend_from_slice(&order_id.to_be_bytes());
        packed.extend_from_slice(&attested_at.to_be_bytes());
        let inner: [u8; 32] = {
            let mut h = Keccak256::new();
            h.update(&packed);
            h.finalize().into()
        };
        let final_hash = eth_message_hash(&inner);
        let sig = k256::ecdsa::Signature::from_slice(&raw[..64]).unwrap();
        vk.verify_prehash(&final_hash, &sig).unwrap();
    }
}
