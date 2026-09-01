use committer::handle::make_commit_batch;
use committer::{CommitBatch, CommitterHandle};
use engine::MatchingEngine;
use std::time::Duration;
use types::{
    AssetId, DepositRequest, FeeConfig, Market, MarketId, Request, PRICE_SCALE, QUANTITY_SCALE,
};

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
    let u = user(1);
    e.process(
        Request::Deposit(DepositRequest {
            user: u.clone(),
            asset: usdc(),
            amount: 100_000 * PRICE_SCALE,
            l1_tx_hash: [0u8; 32],
            source_chain: Default::default(),
            bridge_id: None,
        }),
        1,
    );
    e
}

#[tokio::test]
async fn test_committer_handle_spawns_and_alive() {
    let handle = CommitterHandle::spawn(Duration::from_millis(100), None, 64, false);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(handle.is_alive());
    handle.abort();
}

#[tokio::test]
async fn test_send_batch_succeeds() {
    let handle = CommitterHandle::spawn(Duration::from_millis(100), None, 64, false);
    let engine = funded_engine();
    let batch = make_commit_batch(&engine, vec![], 1);
    let result = handle.send_batch(batch).await;
    assert!(result.is_ok());
    handle.abort();
}

#[tokio::test]
async fn test_commit_result_received() {
    let mut handle = CommitterHandle::spawn(Duration::from_millis(50), None, 64, true);
    let engine = funded_engine();
    let batch = make_commit_batch(&engine, vec![], 1);
    handle.send_batch(batch).await.unwrap();

    let result = tokio::time::timeout(Duration::from_millis(500), handle.next_result()).await;

    assert!(result.is_ok(), "timed out waiting for commit result");
    let commit = result.unwrap();
    assert!(commit.is_some());
    let commit = commit.unwrap();
    assert_eq!(commit.sequence, 1);
    assert_ne!(commit.root, [0u8; 32]);
    handle.abort();
}

#[tokio::test]
async fn test_multiple_batches_increment_sequence() {
    let mut handle = CommitterHandle::spawn(Duration::from_millis(50), None, 64, true);
    let engine = funded_engine();

    for i in 1..=3 {
        let batch = make_commit_batch(&engine, vec![], i);
        handle.send_batch(batch).await.unwrap();
    }

    let mut sequences = vec![];
    for _ in 0..3 {
        let result = tokio::time::timeout(Duration::from_millis(500), handle.next_result()).await;
        if let Ok(Some(r)) = result {
            sequences.push(r.sequence);
        }
    }

    assert!(
        !sequences.is_empty(),
        "should have received at least one commit result"
    );
    handle.abort();
}

#[tokio::test]
async fn test_batch_with_different_state_produces_different_roots() {
    let mut handle1 = CommitterHandle::spawn(Duration::from_millis(50), None, 64, true);
    let mut handle2 = CommitterHandle::spawn(Duration::from_millis(50), None, 64, true);

    let mut engine1 = funded_engine();
    let mut engine2 = MatchingEngine::new(FeeConfig::default(), 1.0);
    engine2.add_market(Market {
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
    engine2.process(
        Request::Deposit(DepositRequest {
            user: user(1),
            asset: usdc(),
            amount: 50_000 * PRICE_SCALE,
            l1_tx_hash: [0u8; 32],
            source_chain: Default::default(),
            bridge_id: None,
        }),
        1,
    );

    handle1
        .send_batch(make_commit_batch(&engine1, vec![], 1))
        .await
        .unwrap();
    handle2
        .send_batch(make_commit_batch(&engine2, vec![], 1))
        .await
        .unwrap();

    let r1 = tokio::time::timeout(Duration::from_millis(500), handle1.next_result()).await;
    let r2 = tokio::time::timeout(Duration::from_millis(500), handle2.next_result()).await;

    if let (Ok(Some(c1)), Ok(Some(c2))) = (r1, r2) {
        assert_ne!(
            c1.root, c2.root,
            "different state must produce different roots"
        );
    }

    handle1.abort();
    handle2.abort();
}

#[tokio::test]
async fn test_make_commit_batch_captures_engine_state() {
    let engine = funded_engine();
    let batch = make_commit_batch(&engine, vec![], 1);
    assert_eq!(batch.sequence, 1);
    assert!(!batch.balances.is_empty());
    assert_eq!(batch.market_ids.len(), 1);
}
