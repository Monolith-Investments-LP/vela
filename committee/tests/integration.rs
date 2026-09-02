//! Integration tests exercising the public committee surface end-to-end
//! from a downstream crate's perspective (mirrors what api/ does at
//! runtime).

use committee::{generate_committee_keypair, CommitteeNode, ThresholdDecryptor};
use rand::SeedableRng;

fn make_rng(seed: u64) -> rand::rngs::StdRng {
    rand::rngs::StdRng::seed_from_u64(seed)
}

fn dummy_order() -> types::PostOrderRequest {
    types::PostOrderRequest {
        user: types::UserId([9u8; 20]),
        market: types::MarketId("BTC-USDC".to_string()),
        side: types::OrderSide::Ask,
        order_type: types::OrderType::GoodTillCanceled,
        price: 60_000_000_000,
        quantity: 5_000_000,
        nonce: 7,
        client_order_id: Some("ci-int-1".to_string()),
        signature: vec![7u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    }
}

/// Full (t, n) round-trip using only the public API: keygen → per-node
/// encrypt-share → threshold decryptor → plaintext.
#[test]
fn threshold_decryption_end_to_end() {
    let (t, n) = (3u8, 5u8);
    let mut rng = make_rng(0xdead_beef);
    let kp = generate_committee_keypair(t, n, &mut rng).unwrap();

    let nodes: Vec<CommitteeNode> = kp
        .shares
        .iter()
        .map(|s| CommitteeNode::new(s.index, s.value))
        .collect();

    let plain = types::PlaintextOrder(dummy_order());
    let ciphertext = committee::crypto::encrypt(&plain, &kp.pub_key, &mut rng).unwrap();

    let mut decryptor = ThresholdDecryptor::new(t, n);
    let mut decrypted = None;
    for (i, node) in nodes.iter().enumerate() {
        let share = node.decrypt_share(&ciphertext).unwrap();
        let result = decryptor.submit_share(&ciphertext, share).unwrap();
        if i + 1 < t as usize {
            assert!(result.is_none(), "below threshold must not decrypt");
        }
        if result.is_some() {
            decrypted = result;
            break;
        }
    }
    let decrypted = decrypted.expect("must decrypt at or above threshold");
    assert_eq!(decrypted.0.market.0, plain.0.market.0);
    assert_eq!(decrypted.0.price, plain.0.price);
    assert_eq!(decrypted.0.nonce, plain.0.nonce);
}

/// Submitting more than `t` shares still yields the same plaintext
/// (idempotent within one order).
#[test]
fn extra_shares_beyond_threshold_are_safe() {
    let mut rng = make_rng(0x1234_5678);
    let (t, n) = (2u8, 3u8);
    let kp = generate_committee_keypair(t, n, &mut rng).unwrap();
    let nodes: Vec<CommitteeNode> = kp
        .shares
        .iter()
        .map(|s| CommitteeNode::new(s.index, s.value))
        .collect();
    let plain = types::PlaintextOrder(dummy_order());
    let ciphertext = committee::crypto::encrypt(&plain, &kp.pub_key, &mut rng).unwrap();

    let mut decryptor = ThresholdDecryptor::new(t, n);
    let mut seen_plaintext = None;
    for node in &nodes {
        let share = node.decrypt_share(&ciphertext).unwrap();
        if let Some(pt) = decryptor.submit_share(&ciphertext, share).unwrap() {
            match &seen_plaintext {
                None => seen_plaintext = Some(pt),
                Some(prev) => assert_eq!(prev.0.nonce, pt.0.nonce),
            }
        }
    }
    assert!(seen_plaintext.is_some());
}

/// Public key shares Lagrange-reconstruct back to the group public key.
/// Guards against a rotated share set silently drifting.
#[test]
fn pub_key_shares_reconstruct_group_key() {
    let mut rng = make_rng(0xabcd_0001);
    let kp = generate_committee_keypair(3, 5, &mut rng).unwrap();
    let indexed: Vec<(u8, types::G1Affine)> = (1..=3u8)
        .map(|i| (i, kp.pub_key_shares[(i - 1) as usize].clone()))
        .collect();
    committee::verify_pk_shares_reconstruct_group(&indexed, &kp.pub_key)
        .expect("first t pub_key_shares must reconstruct");
}
