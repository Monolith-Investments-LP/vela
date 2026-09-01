//! ElGamal hybrid encryption over BLS12-381 G1.
//!
//! Scheme:
//!   - Pick random scalars `r` (ephemeral) and `m` (message key).
//!   - R = r * G   (ephemeral public key)
//!   - shared = r * pk  (DH shared secret)
//!   - C = m*G + shared   (ElGamal ciphertext point; m*G is the "key capsule")
//!   - K = SHA3-256(m*G.compress())   (symmetric key from the capsule)
//!   - ciphertext = order_bytes XOR SHA3-CTR(K)
//!   - order_hash = SHA3-256(order_bytes)
//!
//! Threshold decryption (given t partial decryptions of R):
//!   - Lagrange-reconstruct sk*R from {sk_i * R} shares
//!   - m*G = C - (sk*R)
//!   - K = SHA3-256((m*G).compress())
//!   - plaintext = ciphertext XOR SHA3-CTR(K)
//!   - verify SHA3-256(plaintext) == order_hash

use anyhow::{bail, Result};
use rand::RngCore;
use sha3::{Digest, Sha3_256};
use types::{EncryptedOrder, G1Affine, PlaintextOrder};

use crate::bls_ops::{g1_add, g1_generator_mul, g1_mul, g1_neg, G1};
use crate::shamir::{lagrange_coefficient, Fr};

// --------------------------------------------------------------------------
// SHA3 stream cipher (counter mode)
// --------------------------------------------------------------------------

fn sha3_keystream(key: &[u8; 32], length: usize) -> Vec<u8> {
    let mut stream = Vec::with_capacity(length + 32);
    let mut counter: u64 = 0;
    while stream.len() < length {
        let mut h = Sha3_256::new();
        h.update(key);
        h.update(counter.to_le_bytes());
        stream.extend_from_slice(&h.finalize());
        counter += 1;
    }
    stream.truncate(length);
    stream
}

pub fn xor_with_keystream(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let ks = sha3_keystream(key, data.len());
    data.iter().zip(ks.iter()).map(|(d, k)| d ^ k).collect()
}

fn sha3_256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha3_256::new();
    h.update(data);
    h.finalize().into()
}

// --------------------------------------------------------------------------
// Helpers to convert between G1 (48-byte compressed) and G1Affine (types crate)
// --------------------------------------------------------------------------

fn g1_to_affine(p: &G1) -> G1Affine {
    G1Affine(p.0)
}

fn affine_to_g1(a: &G1Affine) -> G1 {
    G1(a.0)
}

// --------------------------------------------------------------------------
// Encryption
// --------------------------------------------------------------------------

/// Encrypt a `PlaintextOrder` under `pub_key` using ElGamal hybrid encryption.
pub fn encrypt(
    order: &PlaintextOrder,
    pub_key: &G1Affine,
    rng: &mut impl RngCore,
) -> Result<EncryptedOrder> {
    let order_bytes =
        serde_json::to_vec(&order.0).map_err(|e| anyhow::anyhow!("serialize order: {e}"))?;
    let order_hash = sha3_256(&order_bytes);

    // Random ephemeral scalar r
    let mut r_bytes = [0u8; 32];
    rng.fill_bytes(&mut r_bytes);
    let r_fr = Fr::from_le_bytes(&r_bytes);
    let r_le = r_fr.to_le_bytes();

    // Random message key scalar m
    let mut m_bytes_raw = [0u8; 32];
    rng.fill_bytes(&mut m_bytes_raw);
    let m_fr = Fr::from_le_bytes(&m_bytes_raw);
    let m_le = m_fr.to_le_bytes();

    // R = r * G
    let r_point = g1_generator_mul(&r_le)?;

    // shared = r * pk
    let shared = g1_mul(&affine_to_g1(pub_key), &r_le)?;

    // M_key = m * G  (key capsule)
    let m_key_point = g1_generator_mul(&m_le)?;

    // C = M_key + shared  (ElGamal: encrypt M_key under pk)
    let c_point = g1_add(&m_key_point, &shared)?;

    // Symmetric key K = SHA3-256(M_key.compress())
    let sym_key = sha3_256(&m_key_point.0);

    // Encrypt order bytes
    let ciphertext = xor_with_keystream(&sym_key, &order_bytes);

    Ok(EncryptedOrder {
        r: g1_to_affine(&r_point),
        c: g1_to_affine(&c_point),
        order_hash,
        ciphertext,
    })
}

// --------------------------------------------------------------------------
// Partial decryption (committee node)
// --------------------------------------------------------------------------

/// Compute `sk_i * R` — the partial decryption share for this node.
pub fn partial_decrypt(sk_share: &Fr, enc: &EncryptedOrder) -> Result<G1> {
    let scalar = sk_share.to_le_bytes();
    let r_point = affine_to_g1(&enc.r);
    g1_mul(&r_point, &scalar)
}

// --------------------------------------------------------------------------
// Threshold reconstruction and final decryption
// --------------------------------------------------------------------------

/// Reconstruct the shared secret `sk * R` from at least `t` partial decryption shares
/// using Lagrange interpolation, then decrypt the `EncryptedOrder`.
///
/// `shares` is a slice of `(node_index, G1_share_point)` pairs.
/// Returns the decrypted `PlaintextOrder` if hash verification passes.
pub fn threshold_decrypt(shares: &[(u8, G1)], enc: &EncryptedOrder) -> Result<PlaintextOrder> {
    if shares.is_empty() {
        bail!("no shares provided");
    }

    let indices: Vec<u8> = shares.iter().map(|(i, _)| *i).collect();

    // Lagrange reconstruction: sk*R = ∑ λ_i * (sk_i * R)
    let mut sk_r: Option<G1> = None;
    for (idx, share_point) in shares {
        let lambda = lagrange_coefficient(*idx, &indices)?;
        let lambda_bytes = lambda.to_le_bytes();
        let lambda_share = g1_mul(share_point, &lambda_bytes)?;
        sk_r = Some(match sk_r {
            None => lambda_share,
            Some(acc) => g1_add(&acc, &lambda_share)?,
        });
    }

    let sk_r = sk_r.unwrap();

    // M_key = C - sk*R = C + (-(sk*R))
    let neg_sk_r = g1_neg(&sk_r)?;
    let m_key_point = g1_add(&affine_to_g1(&enc.c), &neg_sk_r)?;

    // Symmetric key
    let sym_key = sha3_256(&m_key_point.0);

    // Decrypt
    let order_bytes = xor_with_keystream(&sym_key, &enc.ciphertext);

    // Verify hash
    let computed_hash = sha3_256(&order_bytes);
    if computed_hash != enc.order_hash {
        bail!("decryption integrity check failed: order_hash mismatch");
    }

    // Deserialize
    let req: types::PostOrderRequest = serde_json::from_slice(&order_bytes)
        .map_err(|e| anyhow::anyhow!("deserialize plaintext order: {e}"))?;

    Ok(PlaintextOrder(req))
}
