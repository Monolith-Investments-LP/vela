//! Integration tests exercising the ZKVM public surface.

use zkvm::{
    verifier_from_env, PlaceholderProver, ProofFill, ProofRequest, ProofStatus, Sp1Prover,
    Sp1Verifier, ZkProver, ZkVerifier,
};

fn sample_request(batch_id: u64) -> ProofRequest {
    ProofRequest {
        batch_id,
        state_root_before: format!("0x{:064x}", batch_id),
        state_root_after: format!("0x{:064x}", batch_id + 1),
        fills: vec![ProofFill {
            fill_id: format!("f-{batch_id}"),
            market_id: "ETH-USDC".to_string(),
            price: 3_200_000_000,
            quantity: 1_000_000,
            maker_address: "0xmaker".to_string(),
            taker_address: "0xtaker".to_string(),
            timestamp: 1,
        }],
        orders_processed: 1,
        timestamp: 1,
    }
}

/// PlaceholderProver returns `Skipped`; PlaceholderVerifier accepts it.
#[tokio::test]
async fn placeholder_prove_and_verify() {
    let prover = PlaceholderProver;
    let result = prover.prove_batch(sample_request(1)).await;
    assert!(matches!(result.proof.status, ProofStatus::Skipped));

    // Force placeholder verifier by clearing envs (test-local).
    std::env::remove_var("ZKVM_PROVIDER");
    std::env::remove_var("VELA_PROVER");
    std::env::remove_var("ENVIRONMENT");
    let verifier = verifier_from_env();
    assert!(verifier.verify_proof(&result.proof).is_ok());
}

/// SP1 mock path emits a Proven record with a deterministic pseudo-proof
/// that the paired verifier accepts.
#[tokio::test]
async fn sp1_mock_round_trip() {
    let prover = Sp1Prover::new(None, "vela-matcher-v1");
    let out = prover.prove_batch(sample_request(42)).await;
    assert!(matches!(out.proof.status, ProofStatus::Proven));
    assert_eq!(out.proof.prover, "sp1-mock");
    let verifier = Sp1Verifier { verifier_url: None };
    verifier.verify_proof(&out.proof).unwrap();
}
