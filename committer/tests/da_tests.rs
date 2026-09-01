use committer::da::keccak256;
use committer::handle::make_commit_batch;
use committer::{CommitterHandle, MockDaClient};
use engine::MatchingEngine;
use std::time::Duration;
use types::{AssetId, DepositRequest, FeeConfig, Market, MarketId, Request, PRICE_SCALE};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user(i: u8) -> types::UserId {
    let mut a = [0u8; 20];
    a[19] = i;
    types::UserId(a)
}
fn usdc() -> AssetId {
    AssetId::from_str("USDC")
}
fn btc() -> AssetId {
    AssetId::from_str("BTC")
}

fn funded_engine() -> MatchingEngine {
    let mut e = MatchingEngine::new(FeeConfig::default(), 1.0);
    e.add_market(Market {
        id: MarketId::new("BTC", "USDC"),
        base: btc(),
        quote: usdc(),
        max_orders: 1000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: -1,
        taker_fee_bps: 5,
    });
    e.process(
        Request::Deposit(DepositRequest {
            user: user(1),
            asset: usdc(),
            amount: 100_000 * PRICE_SCALE,
            l1_tx_hash: [0u8; 32],
        }),
        1,
    );
    e
}

async fn commit_one(da: MockDaClient) -> committer::CommitResult {
    let da_box: Box<dyn committer::DataAvailabilityClient> = Box::new(da);
    let mut handle =
        CommitterHandle::spawn_with_da(Duration::from_millis(50), None, 64, true, Some(da_box));
    let batch = make_commit_batch(&funded_engine(), vec![], 1);
    handle.send_batch(batch).await.unwrap();

    tokio::time::timeout(Duration::from_millis(500), handle.next_result())
        .await
        .expect("timed out")
        .expect("no result")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A commit with a wired DA client should produce a non-None DaRecord.
#[tokio::test]
async fn test_batch_commit_produces_da_receipt() {
    let mock = MockDaClient::new();
    let result = commit_one(mock.clone()).await;

    assert!(
        result.da_record.is_some(),
        "CommitResult should contain a DA record when a client is wired"
    );
    let record = result.da_record.unwrap();
    assert_eq!(record.backend, "mock");
    assert_eq!(record.receipt.sequence, result.sequence);
    assert_ne!(record.receipt.content_hash, [0u8; 32]);
}

/// The content hash in the receipt must equal keccak256 of the bytes actually
/// submitted to the DA client.
#[tokio::test]
async fn test_receipt_content_hash_matches_submitted_data() {
    let mock = MockDaClient::new();
    let result = commit_one(mock.clone()).await;

    let subs = mock.submissions();
    assert_eq!(subs.len(), 1, "exactly one submission expected");

    let sub = &subs[0];
    let expected_hash = keccak256(&sub.data);
    assert_eq!(
        result.da_record.unwrap().receipt.content_hash,
        expected_hash,
        "receipt content hash must be keccak256 of the submitted bytes"
    );
}

/// Two batches with different engine state should produce different content hashes.
#[tokio::test]
async fn test_two_different_batches_produce_different_receipts() {
    // Batch 1: funded engine (100 k USDC)
    let mock1 = MockDaClient::new();
    let r1 = commit_one(mock1.clone()).await;

    // Batch 2: different deposit amount
    let mock2 = MockDaClient::new();
    let da_box2: Box<dyn committer::DataAvailabilityClient> = Box::new(mock2.clone());
    let mut handle2 =
        CommitterHandle::spawn_with_da(Duration::from_millis(50), None, 64, true, Some(da_box2));
    let mut e2 = MatchingEngine::new(FeeConfig::default(), 1.0);
    e2.add_market(Market {
        id: MarketId::new("BTC", "USDC"),
        base: btc(),
        quote: usdc(),
        max_orders: 1000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: -1,
        taker_fee_bps: 5,
    });
    e2.process(
        Request::Deposit(DepositRequest {
            user: user(2),
            asset: usdc(),
            amount: 50_000 * PRICE_SCALE, // different amount
            l1_tx_hash: [1u8; 32],
        }),
        1,
    );
    let batch2 = make_commit_batch(&e2, vec![], 1);
    handle2.send_batch(batch2).await.unwrap();
    let r2 = tokio::time::timeout(Duration::from_millis(500), handle2.next_result())
        .await
        .expect("timed out")
        .expect("no result");
    handle2.abort();

    let hash1 = r1.da_record.unwrap().receipt.content_hash;
    let hash2 = r2.da_record.unwrap().receipt.content_hash;
    assert_ne!(
        hash1, hash2,
        "different batches must produce different content hashes"
    );
}

/// A failing DA client should not prevent the commit from completing.
/// The CommitResult should arrive with da_record = None and a valid root.
#[tokio::test]
async fn test_da_failure_does_not_block_commit() {
    let failing_da: Box<dyn committer::DataAvailabilityClient> = Box::new(MockDaClient::failing());
    let mut handle =
        CommitterHandle::spawn_with_da(Duration::from_millis(50), None, 64, true, Some(failing_da));
    let batch = make_commit_batch(&funded_engine(), vec![], 1);
    handle.send_batch(batch).await.unwrap();

    let result = tokio::time::timeout(Duration::from_millis(500), handle.next_result())
        .await
        .expect("timed out waiting for commit result despite DA failure")
        .expect("no CommitResult received");

    // Commit itself succeeded.
    assert_ne!(result.root, [0u8; 32], "commit root should be non-zero");
    assert_eq!(result.sequence, 1);
    // DA record absent because the client failed.
    assert!(
        result.da_record.is_none(),
        "da_record should be None when DA submission fails"
    );

    handle.abort();
}
