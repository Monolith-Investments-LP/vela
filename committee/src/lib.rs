//! Threshold Encrypted Order Book (TEOB) — committee crate.
//!
//! Provides (t, n)-threshold BLS12-381 ElGamal decryption for order book orders.
//! Orders are submitted as ciphertext; only at match time, after t committee nodes
//! cooperate, is the plaintext revealed.
//!
//! # Key types
//! - [`CommitteeNode`] — holds one Shamir secret share; produces [`DecryptionShare`]s.
//! - [`ThresholdDecryptor`] — aggregates shares and reconstructs plaintext when threshold met.
//! - [`EncryptedOrder`] / [`PlaintextOrder`] / [`CommitteeConfig`] live in `types::`.
//!
//! # Unsafe
//! `bls_ops` contains the minimum necessary `unsafe` blocks to call the blst C FFI for
//! BLS12-381 G1 operations (point multiplication and addition).  All other modules are
//! safe Rust.

pub mod bls_ops;
pub mod crypto;
pub mod shamir;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use rand::RngCore;
use types::{CommitteeConfig, DecryptionShare, EncryptedOrder, G1Affine, PlaintextOrder};

use bls_ops::{g1_generator_mul, g1_is_valid, G1};
use shamir::{generate_shares, Fr, Share};

// --------------------------------------------------------------------------
// Committee key generation
// --------------------------------------------------------------------------

/// Generated committee keypair: the group public key and n secret shares.
pub struct CommitteeKeypair {
    /// Public key (group key = secret * G).
    pub pub_key: G1Affine,
    /// Shamir shares, one per committee member.  Member i holds `shares[i-1]`.
    pub shares: Vec<Share>,
    /// Threshold configuration.
    pub config: CommitteeConfig,
}

/// Generate a fresh (t, n)-committee keypair using a random dealer secret.
///
/// # Environment variables
/// - `VELA_THRESHOLD_T` (default 3) — minimum shares needed.
/// - `VELA_THRESHOLD_N` (default 5) — total number of committee nodes.
pub fn generate_committee_keypair(
    t: u8,
    n: u8,
    rng: &mut impl RngCore,
) -> Result<CommitteeKeypair> {
    if t == 0 || t > n {
        bail!("invalid (t={t}, n={n}): need 1 ≤ t ≤ n");
    }

    // Random dealer secret
    let mut secret_bytes = [0u8; 32];
    rng.fill_bytes(&mut secret_bytes);
    let secret = Fr::from_le_bytes(&secret_bytes);

    // Public key: pk = secret * G
    let pk_g1 = g1_generator_mul(&secret.to_le_bytes())?;
    let pub_key = G1Affine(pk_g1.0);

    // Shamir shares
    let shares = generate_shares(secret, t, n, rng);

    let config = CommitteeConfig {
        t,
        n,
        pub_key: pub_key.clone(),
    };

    Ok(CommitteeKeypair {
        pub_key,
        shares,
        config,
    })
}

/// Read committee (t, n) from environment variables, defaulting to (3, 5).
pub fn committee_config_from_env() -> (u8, u8) {
    let t = std::env::var("VELA_THRESHOLD_T")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3u8);
    let n = std::env::var("VELA_THRESHOLD_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5u8);
    (t, n)
}

// --------------------------------------------------------------------------
// CommitteeNode — holds one key share
// --------------------------------------------------------------------------

/// A single committee node holding one Shamir secret share.
/// Exposes [`decrypt_share`] to produce a partial decryption for any incoming order.
pub struct CommitteeNode {
    /// 1-based index of this node's share (1..=n).
    pub index: u8,
    share: Fr,
}

impl CommitteeNode {
    /// Create a committee node with the given index and scalar share.
    pub fn new(index: u8, share: Fr) -> Self {
        CommitteeNode { index, share }
    }

    /// Produce a partial decryption share for `ciphertext`.
    ///
    /// Returns `DecryptionShare { node_index, point: sk_i * R }`.
    pub fn decrypt_share(&self, ciphertext: &EncryptedOrder) -> Result<DecryptionShare> {
        let point_g1 = crypto::partial_decrypt(&self.share, ciphertext)?;
        Ok(DecryptionShare {
            node_index: self.index,
            point: G1Affine(point_g1.0),
        })
    }
}

// --------------------------------------------------------------------------
// ThresholdDecryptor — share aggregation and Lagrange reconstruction
// --------------------------------------------------------------------------

/// Per-order aggregation state.
struct OrderEntry {
    /// Shares received so far, indexed by node_index to prevent duplicates.
    /// Pre-allocated with capacity n.
    shares: HashMap<u8, G1>,
    /// The encrypted order (needed for final decryption).
    encrypted: EncryptedOrder,
    /// Wall-clock time when the first share arrived.
    inserted_at: Instant,
}

/// Aggregates [`DecryptionShare`]s and decrypts once the threshold is met.
///
/// Internal state is keyed on `order_hash` so multiple concurrent orders are handled.
/// Entries older than 500 ms are evicted by the background task in `api/`.
pub struct ThresholdDecryptor {
    threshold: u8,
    total_nodes: u8,
    entries: HashMap<[u8; 32], OrderEntry>,
}

impl ThresholdDecryptor {
    /// Create a new decryptor with the given (t, n) parameters.
    pub fn new(threshold: u8, total_nodes: u8) -> Self {
        ThresholdDecryptor {
            threshold,
            total_nodes,
            entries: HashMap::new(),
        }
    }

    /// Submit a decryption share for `enc_order`.
    ///
    /// - Stores the share (deduplicated by node_index).
    /// - Returns `Some(PlaintextOrder)` exactly when the threshold is first met;
    ///   subsequent submissions for the same order return `None`.
    /// - Timing: we always attempt share storage; the threshold check comes after,
    ///   so partial-share paths incur similar work per call.
    pub fn submit_share(
        &mut self,
        enc_order: &EncryptedOrder,
        share: DecryptionShare,
    ) -> Result<Option<PlaintextOrder>> {
        // Validate the share point is on the curve and in the subgroup.
        if !g1_is_valid(&G1(share.point.0)) {
            bail!(
                "invalid G1 point in DecryptionShare from node {}",
                share.node_index
            );
        }

        let key = enc_order.order_hash;

        // Initialise entry on first share, pre-allocating capacity n.
        let entry = self.entries.entry(key).or_insert_with(|| OrderEntry {
            shares: HashMap::with_capacity(self.total_nodes as usize),
            encrypted: enc_order.clone(),
            inserted_at: Instant::now(),
        });

        // Deduplicate: ignore repeated shares from the same node.
        if entry.shares.contains_key(&share.node_index) {
            return Ok(None);
        }

        entry.shares.insert(share.node_index, G1(share.point.0));

        // Check threshold after storing.
        if entry.shares.len() < self.threshold as usize {
            return Ok(None);
        }

        // Threshold reached — collect exactly `threshold` shares for reconstruction.
        let chosen: Vec<(u8, G1)> = entry
            .shares
            .iter()
            .take(self.threshold as usize)
            .map(|(&idx, pt)| (idx, pt.clone()))
            .collect();

        // Attempt decryption.
        match crypto::threshold_decrypt(&chosen, &entry.encrypted) {
            Ok(plain) => {
                // Remove entry to free memory and prevent re-decryption.
                self.entries.remove(&key);
                Ok(Some(plain))
            }
            Err(e) => {
                // Hash mismatch or other error — remove poisoned entry.
                self.entries.remove(&key);
                Err(e)
            }
        }
    }

    /// Evict entries older than `max_age`.  Called periodically by the background task.
    pub fn evict_stale(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.entries
            .retain(|_, entry| now.duration_since(entry.inserted_at) < max_age);
    }

    /// Number of pending orders currently awaiting threshold.
    pub fn pending_count(&self) -> usize {
        self.entries.len()
    }
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn make_rng() -> rand::rngs::StdRng {
        rand::rngs::StdRng::seed_from_u64(0xdead_beef_cafe_babe)
    }

    fn dummy_order() -> types::PostOrderRequest {
        types::PostOrderRequest {
            user: types::UserId([1u8; 20]),
            market: types::MarketId("ETH-USDC".to_string()),
            side: types::OrderSide::Bid,
            order_type: types::OrderType::GoodTillCanceled,
            price: 3_200_00000000,
            quantity: 1_00000000,
            nonce: 42,
            client_order_id: None,
            signature: vec![0u8; 65],
            stp: Default::default(),
            min_quantity: None,
        }
    }

    /// Encrypt an order, generate n shares, submit exactly t shares, verify round-trip.
    #[test]
    fn test_full_roundtrip() {
        let mut rng = make_rng();
        let kp = generate_committee_keypair(3, 5, &mut rng).unwrap();

        let nodes: Vec<CommitteeNode> = kp
            .shares
            .iter()
            .map(|s| CommitteeNode::new(s.index, s.value))
            .collect();

        let order = PlaintextOrder(dummy_order());
        let enc = crypto::encrypt(&order, &kp.pub_key, &mut rng).unwrap();

        let mut decryptor = ThresholdDecryptor::new(3, 5);

        // Submit t-1 shares — should not decrypt yet.
        for node in nodes.iter().take(2) {
            let share = node.decrypt_share(&enc).unwrap();
            let result = decryptor.submit_share(&enc, share).unwrap();
            assert!(result.is_none(), "should not decrypt with t-1 shares");
        }

        // Submit the t-th share — should now decrypt.
        let share = nodes[2].decrypt_share(&enc).unwrap();
        let result = decryptor.submit_share(&enc, share).unwrap();
        let decrypted = result.expect("should decrypt at threshold");

        assert_eq!(decrypted.0.nonce, order.0.nonce);
        assert_eq!(decrypted.0.price, order.0.price);
        assert_eq!(decrypted.0.quantity, order.0.quantity);
        assert_eq!(decrypted.0.market.0, order.0.market.0);
    }

    /// Submit t-1 shares — verify None and no partial decryption leak.
    #[test]
    fn test_threshold_not_met_returns_none() {
        let mut rng = make_rng();
        let kp = generate_committee_keypair(3, 5, &mut rng).unwrap();
        let nodes: Vec<CommitteeNode> = kp
            .shares
            .iter()
            .map(|s| CommitteeNode::new(s.index, s.value))
            .collect();

        let enc = crypto::encrypt(&PlaintextOrder(dummy_order()), &kp.pub_key, &mut rng).unwrap();
        let mut decryptor = ThresholdDecryptor::new(3, 5);

        for node in nodes.iter().take(2) {
            let share = node.decrypt_share(&enc).unwrap();
            let r = decryptor.submit_share(&enc, share).unwrap();
            assert!(r.is_none());
        }

        // Still 1 pending entry (not evicted, not decrypted)
        assert_eq!(decryptor.pending_count(), 1);
    }

    /// Submit the same node's share twice — verify deduplication.
    #[test]
    fn test_duplicate_share_rejected() {
        let mut rng = make_rng();
        let kp = generate_committee_keypair(3, 5, &mut rng).unwrap();
        let node0 = CommitteeNode::new(kp.shares[0].index, kp.shares[0].value);

        let enc = crypto::encrypt(&PlaintextOrder(dummy_order()), &kp.pub_key, &mut rng).unwrap();
        let mut decryptor = ThresholdDecryptor::new(3, 5);

        // Submit node 0's share twice.
        let share1 = node0.decrypt_share(&enc).unwrap();
        let share2 = node0.decrypt_share(&enc).unwrap();
        decryptor.submit_share(&enc, share1).unwrap();
        decryptor.submit_share(&enc, share2).unwrap();

        // Should have exactly 1 unique share stored (pending, not decrypted).
        assert_eq!(decryptor.pending_count(), 1);
        let entry = decryptor.entries.get(&enc.order_hash).unwrap();
        assert_eq!(entry.shares.len(), 1, "duplicate share must not count");
    }

    /// Eviction: insert a share, then evict with max_age=0, verify entry removed.
    #[test]
    fn test_eviction() {
        let mut rng = make_rng();
        let kp = generate_committee_keypair(3, 5, &mut rng).unwrap();
        let node0 = CommitteeNode::new(kp.shares[0].index, kp.shares[0].value);

        let enc = crypto::encrypt(&PlaintextOrder(dummy_order()), &kp.pub_key, &mut rng).unwrap();
        let mut decryptor = ThresholdDecryptor::new(3, 5);

        let share = node0.decrypt_share(&enc).unwrap();
        decryptor.submit_share(&enc, share).unwrap();
        assert_eq!(decryptor.pending_count(), 1);

        // Evict immediately (max_age = 0).
        decryptor.evict_stale(Duration::ZERO);
        assert_eq!(decryptor.pending_count(), 0, "entry must be evicted");

        // Subsequent shares for the same order should return None (fresh start needed).
        let node1 = CommitteeNode::new(kp.shares[1].index, kp.shares[1].value);
        let node2 = CommitteeNode::new(kp.shares[2].index, kp.shares[2].value);
        let s1 = node1.decrypt_share(&enc).unwrap();
        let s2 = node2.decrypt_share(&enc).unwrap();
        let r1 = decryptor.submit_share(&enc, s1).unwrap();
        let r2 = decryptor.submit_share(&enc, s2).unwrap();
        assert!(r1.is_none());
        assert!(r2.is_none());
    }

    /// Hash integrity: tamper with ciphertext after encryption, verify decryption fails.
    #[test]
    fn test_hash_integrity_check() {
        let mut rng = make_rng();
        let kp = generate_committee_keypair(3, 5, &mut rng).unwrap();
        let nodes: Vec<CommitteeNode> = kp
            .shares
            .iter()
            .map(|s| CommitteeNode::new(s.index, s.value))
            .collect();

        let mut enc =
            crypto::encrypt(&PlaintextOrder(dummy_order()), &kp.pub_key, &mut rng).unwrap();

        // Tamper with one byte of the ciphertext.
        if let Some(b) = enc.ciphertext.get_mut(0) {
            *b ^= 0xff;
        }

        // Collect t shares.
        let shares: Vec<G1> = nodes
            .iter()
            .take(3)
            .map(|n| {
                let scalar = n.share.to_le_bytes();
                crate::bls_ops::g1_mul(&G1(enc.r.0), &scalar).unwrap()
            })
            .collect();

        let share_pairs: Vec<(u8, G1)> =
            nodes.iter().take(3).map(|n| n.index).zip(shares).collect();

        let result = crypto::threshold_decrypt(&share_pairs, &enc);
        assert!(result.is_err(), "tampered ciphertext must fail hash check");
    }

    /// Fraud proof trigger: DecryptionProof with T_decrypt < T_recv is invalid.
    #[test]
    fn test_fraud_proof_t_decrypt_before_t_recv() {
        let proof = types::DecryptionProof {
            order_hash: [0u8; 32],
            t_recv_ms: 1000,
            t_decrypt_ms: 500, // BEFORE receipt — fraudulent
            batch_seq: 1,
            valid: true,
        };
        assert!(
            !proof.is_valid(),
            "proof with T_decrypt < T_recv must be invalid"
        );
    }

    /// Verify a well-formed DecryptionProof is valid.
    #[test]
    fn test_valid_decryption_proof() {
        let proof = types::DecryptionProof {
            order_hash: [1u8; 32],
            t_recv_ms: 1000,
            t_decrypt_ms: 1500,
            batch_seq: 2,
            valid: true,
        };
        assert!(proof.is_valid());
    }
}
