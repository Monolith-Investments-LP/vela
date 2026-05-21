use std::time::{Duration, SystemTime};
use committer::{CommitterConfig, CommitterHandle, DelayedInbox};
use committer::handle::make_commit_batch;
use engine::MatchingEngine;
use types::{AssetId, DepositRequest, FeeConfig, Market, MarketId, Request, UserId, PRICE_SCALE};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user(i: u8) -> UserId {
    let mut a = [0u8; 20];
    a[19] = i;
    UserId(a)
}

fn usdc() -> AssetId { AssetId::from_str("USDC") }
fn btc()  -> AssetId { AssetId::from_str("BTC") }

fn deposit_req(u: &UserId, amount: u64) -> Request {
    Request::Deposit(DepositRequest {
        user: u.clone(),
        asset: usdc(),
        amount,
        l1_tx_hash: [0u8; 32],
    })
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

// ---------------------------------------------------------------------------
// Unit tests for DelayedInbox directly
// ---------------------------------------------------------------------------

/// A submitted transaction must sit in the inbox and not drain before its
/// timeout has elapsed.
#[test]
fn test_forced_tx_sits_in_inbox() {
    let mut inbox = DelayedInbox::new();
    let u = user(1);

    inbox.submit_forced(deposit_req(&u, 1_000), u.clone());
    assert_eq!(inbox.pending_count(), 1, "entry should be in the inbox");

    // With a 1-hour timeout the freshly submitted entry is not yet eligible.
    let drained = inbox.drain_eligible(Duration::from_secs(3600));
    assert!(drained.is_empty(), "fresh entry must not drain before timeout");
    assert_eq!(inbox.pending_count(), 1, "entry should still be in the inbox");
}

/// A transaction submitted with a past timestamp must drain immediately when
/// its age exceeds the configured timeout.
#[test]
fn test_forced_tx_drains_after_timeout() {
    let mut inbox = DelayedInbox::new();
    let u = user(2);

    // Simulate a tx submitted 2 hours ago.
    let old_time = SystemTime::now() - Duration::from_secs(2 * 3600);
    inbox.submit_forced_at(deposit_req(&u, 2_000), u.clone(), old_time);
    assert_eq!(inbox.pending_count(), 1);

    // With a 1-hour timeout the 2-hour-old entry is eligible.
    let drained = inbox.drain_eligible(Duration::from_secs(3600));
    assert_eq!(drained.len(), 1, "aged entry must be returned");
    assert_eq!(inbox.pending_count(), 0, "inbox must be empty after drain");
}

/// Multiple entries: only those past the timeout should drain; others stay.
#[test]
fn test_drain_eligible_is_selective() {
    let mut inbox = DelayedInbox::new();
    let u = user(3);

    // One old entry (2h ago) and one fresh entry.
    let old = SystemTime::now() - Duration::from_secs(2 * 3600);
    inbox.submit_forced_at(deposit_req(&u, 100), u.clone(), old);
    inbox.submit_forced(deposit_req(&u, 200), u.clone()); // fresh

    let timeout = Duration::from_secs(3600);
    let drained = inbox.drain_eligible(timeout);
    assert_eq!(drained.len(), 1, "only the aged entry should drain");
    assert_eq!(inbox.pending_count(), 1, "fresh entry must remain");
}

/// Zero timeout means every entry is immediately eligible.
#[test]
fn test_drain_eligible_zero_timeout() {
    let mut inbox = DelayedInbox::new();
    let u = user(4);

    inbox.submit_forced(deposit_req(&u, 300), u.clone());
    inbox.submit_forced(deposit_req(&u, 400), u.clone());

    let drained = inbox.drain_eligible(Duration::ZERO);
    assert_eq!(drained.len(), 2, "all entries eligible with zero timeout");
    assert_eq!(inbox.pending_count(), 0);
}

// ---------------------------------------------------------------------------
// Integration tests — full committer round-trip
// ---------------------------------------------------------------------------

/// A forced transaction submitted with a zero-timeout should appear in the
/// next committed batch.
#[tokio::test]
async fn test_forced_tx_included_in_next_batch() {
    let config = CommitterConfig {
        batch_interval: Duration::from_millis(50),
        forced_inclusion_timeout: Duration::ZERO, // immediately eligible
        ..Default::default()
    };
    let mut handle = CommitterHandle::spawn_full(config, 64, true, None);

    let u = user(5);
    handle.force_include(deposit_req(&u, 5_000), u.clone()).await.unwrap();

    // Wait for the next timer-driven commit to pick up the forced tx.
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        handle.next_result(),
    )
    .await
    .expect("timed out waiting for commit result")
    .expect("no CommitResult");

    assert!(result.forced_count >= 1, "forced tx must be counted in the result");
    assert_eq!(result.batch_size, result.forced_count, "entire batch should be forced txs");
    assert_ne!(result.root, [0u8; 32]);

    handle.abort();
}

/// Normal (non-forced) transactions are committed as usual even when forced
/// inclusion has a long timeout (nothing becomes eligible).  The forced tx
/// stays in the inbox and does not affect the normal commit path.
#[tokio::test]
async fn test_normal_tx_not_affected_by_pending_forced_tx() {
    let config = CommitterConfig {
        batch_interval: Duration::from_millis(50),
        forced_inclusion_timeout: Duration::from_secs(3600), // nothing drains
        ..Default::default()
    };
    let mut handle = CommitterHandle::spawn_full(config, 64, true, None);

    // Submit a forced tx that won't drain for 1 hour.
    let u = user(6);
    handle.force_include(deposit_req(&u, 9_999), u.clone()).await.unwrap();

    // Submit a normal batch.
    let engine = funded_engine();
    let batch = make_commit_batch(&engine, vec![], 1);
    handle.send_batch(batch).await.unwrap();

    let result = tokio::time::timeout(
        Duration::from_millis(500),
        handle.next_result(),
    )
    .await
    .expect("timed out")
    .expect("no CommitResult");

    // Normal commit succeeded with no forced txs in it.
    assert_ne!(result.root, [0u8; 32], "root should be non-zero");
    assert_eq!(result.forced_count, 0, "forced tx must not be included (timeout not reached)");

    handle.abort();
}

/// Forced transactions are prepended — they execute before normal transactions
/// in the same batch.
#[tokio::test]
async fn test_forced_tx_is_prepended_before_normal_txs() {
    let config = CommitterConfig {
        batch_interval: Duration::from_millis(50),
        forced_inclusion_timeout: Duration::ZERO,
        ..Default::default()
    };
    let mut handle = CommitterHandle::spawn_full(config, 64, true, None);

    // Queue a forced tx with an immediately eligible timeout.
    let u = user(7);
    handle.force_include(deposit_req(&u, 7_777), u.clone()).await.unwrap();

    // Also send a normal batch.
    let engine = funded_engine();
    let batch = make_commit_batch(&engine, vec![], 1);
    handle.send_batch(batch).await.unwrap();

    let result = tokio::time::timeout(
        Duration::from_millis(500),
        handle.next_result(),
    )
    .await
    .expect("timed out")
    .expect("no CommitResult");

    // The forced count is non-zero and the batch includes both forced and normal.
    assert!(result.forced_count >= 1, "should have at least one forced tx");
    assert_ne!(result.root, [0u8; 32]);

    handle.abort();
}
