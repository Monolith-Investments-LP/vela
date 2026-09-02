//! Integration tests exercising the TEE public surface.

use tee::{AttestationRequest, AttestationStatus, PlaceholderAttester, TeeAttester, TeePlatform};

#[tokio::test]
async fn placeholder_attester_produces_simulated_record() {
    let att = PlaceholderAttester::new();
    let req = AttestationRequest {
        batch_id: 99,
        state_root: "0xabc".into(),
        binary_hash: att.binary_hash(),
        fill_count: 3,
        orders_processed: 10,
        timestamp: 1_000,
        operator_address: "0x0000000000000000000000000000000000000001".into(),
    };
    let out = att.attest_batch(req).await;
    assert!(matches!(out.record.status, AttestationStatus::Simulated));
    assert_eq!(out.record.platform, TeePlatform::Placeholder);
    assert_eq!(out.record.fill_count, 3);
    assert!(out.error.is_none());
    // Attestation records must always name a verification note so
    // downstream disclosure UIs can render it verbatim.
    assert!(!out.record.verification_note.is_empty());
}

/// The env factory refuses to hand back the placeholder attester on
/// production. Guarding this behaviour prevents a mainnet operator
/// disclosure page from ever showing `Simulated` records.
#[test]
#[should_panic(expected = "TEE_PLATFORM=placeholder")]
fn placeholder_is_refused_on_production_env() {
    std::env::set_var("ENVIRONMENT", "production");
    std::env::set_var("TEE_PLATFORM", "placeholder");
    let _ = tee::attester_from_env();
}
