//! HTTP handlers for the Threshold Encrypted Order Book (TEOB) endpoints.
//!
//! - `POST /order/encrypted`   — submit an encrypted order.
//! - `POST /committee/share`   — submit a partial decryption share (committee nodes only).
//!
//! Committee node authentication uses per-node HMAC-SHA256 keys loaded from
//! `VELA_COMMITTEE_KEY_<N>` environment variables at startup.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::Mutex;
use types::{DecryptionShare, EncryptedOrder, G1Affine, PostOrderRequest};

use crate::AppState;
use engine::batch_dispatcher::BatchedRequest;

type HmacSha256 = Hmac<Sha256>;

fn ct_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// --------------------------------------------------------------------------
// Pending encrypted orders queue
// --------------------------------------------------------------------------

/// An entry in the pending queue.
pub struct PendingEntry {
    pub order: EncryptedOrder,
    /// Instant for eviction age comparison.
    pub received_at: Instant,
    /// Unix timestamp (ms) for the engine submission (preserves fairness ordering).
    pub received_ts_ms: u64,
}

pub type PendingEncryptedOrders = Arc<Mutex<VecDeque<PendingEntry>>>;

pub fn new_pending_queue() -> PendingEncryptedOrders {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Background task: evict pending entries older than 500 ms and evict stale
/// ThresholdDecryptor share entries.
pub async fn eviction_task(pending: PendingEncryptedOrders, state: Arc<AppState>) {
    const MAX_AGE: Duration = Duration::from_millis(500);
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        {
            let mut q = pending.lock().await;
            let now = Instant::now();
            q.retain(|e| now.duration_since(e.received_at) < MAX_AGE);
        }
        {
            let mut dec = state.threshold_decryptor.lock().await;
            dec.evict_stale(MAX_AGE);
        }
    }
}

// --------------------------------------------------------------------------
// POST /order/encrypted
// --------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct EncryptedOrderBody {
    /// Hex-encoded compressed G1 point (48 bytes).
    pub r: String,
    /// Hex-encoded compressed G1 point (48 bytes).
    pub c: String,
    /// Hex-encoded 32-byte SHA3-256 hash.
    pub order_hash: String,
    /// Hex-encoded symmetrically-encrypted order bytes.
    pub ciphertext: String,
}

impl EncryptedOrderBody {
    fn into_encrypted_order(self) -> Result<EncryptedOrder, String> {
        let r = parse_g1(&self.r).map_err(|e| format!("r: {e}"))?;
        let c = parse_g1(&self.c).map_err(|e| format!("c: {e}"))?;
        let order_hash = parse_bytes32(&self.order_hash).map_err(|e| format!("order_hash: {e}"))?;
        let ciphertext = hex::decode(
            self.ciphertext
                .strip_prefix("0x")
                .unwrap_or(&self.ciphertext),
        )
        .map_err(|e| format!("ciphertext: {e}"))?;
        Ok(EncryptedOrder {
            r,
            c,
            order_hash,
            ciphertext,
        })
    }
}

fn parse_g1(s: &str) -> Result<G1Affine, String> {
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s)).map_err(|e| e.to_string())?;
    if bytes.len() != 48 {
        return Err(format!("expected 48 bytes, got {}", bytes.len()));
    }
    let mut arr = [0u8; 48];
    arr.copy_from_slice(&bytes);
    Ok(G1Affine(arr))
}

fn parse_bytes32(s: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(s.strip_prefix("0x").unwrap_or(s)).map_err(|e| e.to_string())?;
    if bytes.len() != 32 {
        return Err(format!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

/// Validate that a G1Affine point is properly encoded.
fn g1_is_well_formed(g: &G1Affine) -> bool {
    committee::bls_ops::g1_is_valid(&committee::bls_ops::G1(g.0))
}

#[derive(serde::Serialize)]
struct ApiOk<T: serde::Serialize> {
    success: bool,
    data: T,
}

fn ok<T: serde::Serialize>(data: T) -> Json<ApiOk<T>> {
    Json(ApiOk {
        success: true,
        data,
    })
}

pub async fn post_encrypted_order(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EncryptedOrderBody>,
) -> impl IntoResponse {
    let enc = match body.into_encrypted_order() {
        Ok(e) => e,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": msg })),
            )
                .into_response();
        }
    };

    // Validate curve points.
    if !g1_is_well_formed(&enc.r) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "R point is not a valid G1 element" })),
        )
            .into_response();
    }
    if !g1_is_well_formed(&enc.c) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "C point is not a valid G1 element" })),
        )
            .into_response();
    }
    if enc.order_hash == [0u8; 32] {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "order_hash must be non-zero" })),
        )
            .into_response();
    }
    if enc.ciphertext.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "ciphertext is empty" })),
        )
            .into_response();
    }

    let now_instant = Instant::now();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let order_hash_hex = hex::encode(enc.order_hash);

    state
        .pending_encrypted
        .lock()
        .await
        .push_back(PendingEntry {
            order: enc,
            received_at: now_instant,
            received_ts_ms: now_ms,
        });

    (
        StatusCode::OK,
        ok(serde_json::json!({ "order_hash": order_hash_hex, "status": "pending" })),
    )
        .into_response()
}

// --------------------------------------------------------------------------
// POST /committee/share
// --------------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub struct CommitteeShareBody {
    pub node_index: u8,
    pub order_hash: String,
    pub share_point: String,
}

/// Extract HMAC-SHA256 authentication from the `X-Committee-Auth` header.
/// Expected format: `<node_index>:<hex_hmac>`
fn verify_committee_auth(
    headers: &HeaderMap,
    body_bytes: &[u8],
    committee_keys: &std::collections::HashMap<u8, [u8; 32]>,
    expected_node: u8,
) -> bool {
    let auth_header = match headers.get("x-committee-auth") {
        Some(h) => match h.to_str() {
            Ok(s) => s,
            Err(_) => return false,
        },
        None => return false,
    };

    let (idx_str, hmac_hex) = match auth_header.split_once(':') {
        Some(parts) => parts,
        None => return false,
    };

    let node_idx: u8 = match idx_str.parse() {
        Ok(n) => n,
        Err(_) => return false,
    };
    if node_idx != expected_node {
        return false;
    }

    let key = match committee_keys.get(&node_idx) {
        Some(k) => k,
        None => return false,
    };

    let provided_mac = match hex::decode(hmac_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };

    // Compute expected HMAC-SHA256 over the request body bytes.
    let mut mac = HmacSha256::new_from_slice(key.as_slice()).expect("HMAC key always valid");
    mac.update(body_bytes);
    let expected = mac.finalize().into_bytes();
    let expected_bytes: &[u8] = &expected;

    // Constant-time comparison to prevent timing side channels.
    ct_eq_bytes(&provided_mac, expected_bytes)
}

pub async fn post_committee_share(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Parse JSON body.
    let share_body: CommitteeShareBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("invalid JSON: {e}") })),
            )
                .into_response();
        }
    };

    // Authenticate.
    if !verify_committee_auth(
        &headers,
        &body,
        &state.committee_keys,
        share_body.node_index,
    ) {
        tracing::warn!(
            node_index = share_body.node_index,
            "committee share rejected: authentication failed"
        );
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "committee node authentication failed" })),
        )
            .into_response();
    }

    // Parse the order_hash.
    let order_hash = match parse_bytes32(&share_body.order_hash) {
        Ok(h) => h,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("order_hash: {e}") })),
            )
                .into_response();
        }
    };

    // Parse the G1 share point.
    let share_point = match parse_g1(&share_body.share_point) {
        Ok(g) => g,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("share_point: {e}") })),
            )
                .into_response();
        }
    };

    // Look up the pending encrypted order.
    let enc_order = {
        let q = state.pending_encrypted.lock().await;
        q.iter()
            .find(|e| e.order.order_hash == order_hash)
            .map(|e| (e.order.clone(), e.received_ts_ms))
    };

    let (enc_order, received_ts_ms) = match enc_order {
        Some(pair) => pair,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "no pending encrypted order with that hash" })),
            )
                .into_response();
        }
    };

    let dec_share = DecryptionShare {
        node_index: share_body.node_index,
        point: share_point,
    };

    // Submit to the ThresholdDecryptor.
    let result = {
        let mut dec = state.threshold_decryptor.lock().await;
        dec.submit_share(&enc_order, dec_share)
    };

    match result {
        Err(e) => {
            tracing::error!("threshold decryption error: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::ACCEPTED,
            ok(serde_json::json!({ "status": "share_accepted" })),
        )
            .into_response(),
        Ok(Some(plain_order)) => {
            // Threshold reached — inject into the matching engine with the original submission ts.
            // Remove from pending queue.
            {
                let mut q = state.pending_encrypted.lock().await;
                q.retain(|e| e.order.order_hash != order_hash);
            }

            // Build the DecryptionProof.
            let t_decrypt_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let proof = types::DecryptionProof {
                order_hash,
                t_recv_ms: received_ts_ms,
                t_decrypt_ms,
                batch_seq: 0, // will be set by committer on batch inclusion
                valid: t_decrypt_ms >= received_ts_ms,
            };

            if !proof.is_valid() {
                tracing::error!(
                    "fraud detected: T_decrypt < T_recv for order {:?}",
                    hex::encode(order_hash)
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(
                        serde_json::json!({ "error": "fraud proof triggered: T_decrypt < T_recv" }),
                    ),
                )
                    .into_response();
            }

            // Store the proof for API reporting endpoints.
            {
                let mut proofs = state.decryption_proofs.lock().await;
                proofs.push(proof.clone());
            }

            // Inject into the engine with the ORIGINAL submission timestamp.
            // The decryption proof rides along so the batch dispatcher can
            // include it in the single CommitBatch for this dispatch window.
            let req: PostOrderRequest = plain_order.0;
            let (responder, response_rx) = tokio::sync::oneshot::channel();
            let channel_item = BatchedRequest {
                request: types::Request::PostOrder(req),
                ts: received_ts_ms,
                responder,
                decryption_proof: Some(proof),
            };

            if let Err(e) = state.order_tx.send(channel_item).await {
                tracing::error!("failed to send decrypted order to engine: {e}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "error": "engine channel full" })),
                )
                    .into_response();
            }

            // Wait for engine response (optional; fire-and-forget is also fine).
            let _engine_resp = response_rx.await;

            (
                StatusCode::OK,
                ok(serde_json::json!({ "status": "decrypted_and_submitted" })),
            )
                .into_response()
        }
    }
}
