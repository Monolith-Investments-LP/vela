//! Tests for [`engine::BatchDispatcher`].
//!
//! Five tests required by VEL-BATCH spec:
//!   1. Response ordering matches submission order.
//!   2. Max batch size triggers early dispatch before window expires.
//!   3. Empty window does not spin (task stays alive and responsive).
//!   4. TEOB encrypted orders coalesce with plaintext orders in the same batch.
//!   5. Single-order batch produces the correct response.

use engine::{BatchDispatcher, BatchMetrics, BatchedRequest, MatchingEngine};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use types::{
    AssetId, DecryptionProof, DepositRequest, FeeConfig, Market, MarketId, OrderSide, OrderType,
    PostOrderRequest, Request, Response, UserId, PRICE_SCALE, QUANTITY_SCALE,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn usdc() -> AssetId {
    AssetId::from_str("USDC")
}
fn btc() -> AssetId {
    AssetId::from_str("BTC")
}

fn user(i: u8) -> UserId {
    let mut a = [0u8; 20];
    a[19] = i;
    UserId(a)
}

fn make_engine() -> Arc<tokio::sync::Mutex<MatchingEngine>> {
    let mut e = MatchingEngine::new(FeeConfig::default(), 5.0);
    e.add_market(Market {
        id: MarketId::new("BTC", "USDC"),
        base: btc(),
        quote: usdc(),
        max_orders: 10_000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: -1,
        taker_fee_bps: 5,
    });
    // Fund user 1 with USDC and BTC so orders can be posted.
    e.process(
        Request::Deposit(DepositRequest {
            user: user(1),
            asset: usdc(),
            amount: 1_000_000 * PRICE_SCALE,
            l1_tx_hash: [0u8; 32],
        }),
        1,
    );
    e.process(
        Request::Deposit(DepositRequest {
            user: user(1),
            asset: btc(),
            amount: 1_000 * QUANTITY_SCALE,
            l1_tx_hash: [1u8; 32],
        }),
        2,
    );
    Arc::new(tokio::sync::Mutex::new(e))
}

fn bid_request(price: u64, nonce: u64) -> Request {
    Request::PostOrder(PostOrderRequest {
        user: user(1),
        market: MarketId::new("BTC", "USDC"),
        side: OrderSide::Bid,
        order_type: OrderType::GoodTillCanceled,
        price: price * PRICE_SCALE,
        quantity: QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
    })
}

fn send_request(
    tx: &mpsc::Sender<BatchedRequest>,
    request: Request,
    nonce_for_ts: u64,
    decryption_proof: Option<DecryptionProof>,
) -> oneshot::Receiver<Vec<Response>> {
    let (responder, rx) = oneshot::channel();
    let item = BatchedRequest {
        request,
        ts: nonce_for_ts,
        responder,
        decryption_proof,
    };
    tx.try_send(item).expect("channel send failed");
    rx
}

fn dummy_proof(order_hash: [u8; 32]) -> DecryptionProof {
    DecryptionProof {
        order_hash,
        t_recv_ms: 100,
        t_decrypt_ms: 200,
        batch_seq: 0,
        valid: true,
    }
}

// ---------------------------------------------------------------------------
// Test 1: Response ordering matches submission order
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_response_ordering_matches_submission_order() {
    let engine = make_engine();
    let metrics = BatchMetrics::new();
    let (tx, rx) = mpsc::channel(64);

    // 500 µs window, max 256 — all 5 requests should coalesce.
    let dispatcher = BatchDispatcher::new(500, 256);
    tokio::spawn(dispatcher.run(rx, Arc::clone(&engine), Arc::clone(&metrics), None));

    // Submit 5 bids with different prices; nonces must be strictly increasing.
    let prices = [10_000u64, 10_001, 10_002, 10_003, 10_004];
    let mut receivers: Vec<oneshot::Receiver<Vec<Response>>> = prices
        .iter()
        .enumerate()
        .map(|(i, &price)| {
            send_request(
                &tx,
                bid_request(price, (i + 1) as u64),
                (i + 1) as u64,
                None,
            )
        })
        .collect();

    // Collect responses in submission order.
    let mut order_ids: Vec<u64> = Vec::new();
    for rx in receivers.iter_mut() {
        let resps = tokio::time::timeout(Duration::from_millis(200), rx)
            .await
            .expect("timed out waiting for response")
            .expect("oneshot closed");
        // Each response set must contain an OrderPosted.
        let posted = resps.iter().find_map(|r| {
            if let Response::OrderPosted(op) = r {
                Some(op.order_id)
            } else {
                None
            }
        });
        order_ids.push(posted.expect("no OrderPosted in response"));
    }

    // Order IDs must be strictly increasing (engine allocates sequentially).
    for w in order_ids.windows(2) {
        assert!(
            w[0] < w[1],
            "order IDs not strictly increasing: {:?}",
            order_ids
        );
    }
}

// ---------------------------------------------------------------------------
// Test 2: Max batch size triggers early dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_max_batch_size_triggers_early_dispatch() {
    let engine = make_engine();
    let metrics = BatchMetrics::new();
    let (tx, rx) = mpsc::channel(512);

    // Very long window (10 s) — we expect early dispatch at max_batch_size=4.
    let dispatcher = BatchDispatcher::new(10_000_000, 4);
    tokio::spawn(dispatcher.run(rx, Arc::clone(&engine), Arc::clone(&metrics), None));

    // Send exactly max_batch_size requests.
    let receivers: Vec<_> = (1u64..=4)
        .map(|nonce| send_request(&tx, bid_request(9_000, nonce), nonce, None))
        .collect();

    // All responses must arrive well before the 10-second window would expire.
    for mut rx in receivers {
        tokio::time::timeout(Duration::from_millis(500), &mut rx)
            .await
            .expect("batch did not fire early at max_batch_size")
            .expect("oneshot closed");
    }
}

// ---------------------------------------------------------------------------
// Test 3: Empty window does not spin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_empty_window_does_not_spin() {
    let engine = make_engine();
    let metrics = BatchMetrics::new();
    let (tx, rx) = mpsc::channel(64);

    let dispatcher = BatchDispatcher::new(1_000, 256); // 1 ms window
    let handle = tokio::spawn(dispatcher.run(rx, Arc::clone(&engine), Arc::clone(&metrics), None));

    // Wait 100 ms with no orders submitted.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Task must still be alive (not panicked or infinitely spinning to death).
    assert!(!handle.is_finished(), "dispatcher task exited unexpectedly");

    // It must also still be responsive to new orders.
    let mut rx = send_request(&tx, bid_request(8_000, 1), 1, None);
    let resps = tokio::time::timeout(Duration::from_millis(200), &mut rx)
        .await
        .expect("dispatcher unresponsive after idle")
        .expect("oneshot closed");
    assert!(
        resps.iter().any(|r| matches!(r, Response::OrderPosted(_))),
        "no OrderPosted after idle period"
    );

    drop(tx); // Close channel so task exits.
    let _ = handle.await;
}

// ---------------------------------------------------------------------------
// Test 4: TEOB encrypted orders coalesce with plaintext orders
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_teob_and_plaintext_coalesce_in_same_batch() {
    let engine = make_engine();
    let metrics = BatchMetrics::new();
    let (tx, rx) = mpsc::channel(64);

    // Collect BatchResults via an auxiliary channel.
    let (result_tx, mut result_rx) = mpsc::channel(8);

    let dispatcher = BatchDispatcher::new(500, 256);
    tokio::spawn(dispatcher.run(
        rx,
        Arc::clone(&engine),
        Arc::clone(&metrics),
        Some(result_tx),
    ));

    // Plaintext order.
    let rx_plain = send_request(&tx, bid_request(7_000, 1), 1, None);
    // TEOB-decrypted order (same structure but carries a DecryptionProof).
    let proof = dummy_proof([0xABu8; 32]);
    let rx_teob = send_request(&tx, bid_request(7_001, 2), 2, Some(proof.clone()));

    // Wait for both responses.
    let r1 = tokio::time::timeout(Duration::from_millis(200), rx_plain)
        .await
        .expect("timeout")
        .expect("closed");
    let r2 = tokio::time::timeout(Duration::from_millis(200), rx_teob)
        .await
        .expect("timeout")
        .expect("closed");

    assert!(
        r1.iter().any(|r| matches!(r, Response::OrderPosted(_))),
        "plain order not posted"
    );
    assert!(
        r2.iter().any(|r| matches!(r, Response::OrderPosted(_))),
        "teob order not posted"
    );

    // The BatchResult should carry exactly one decryption proof (from the TEOB order).
    let batch_result = tokio::time::timeout(Duration::from_millis(200), result_rx.recv())
        .await
        .expect("no BatchResult received")
        .expect("channel closed");

    assert_eq!(
        batch_result.decryption_proofs.len(),
        1,
        "expected 1 decryption proof in BatchResult, got {}",
        batch_result.decryption_proofs.len()
    );
    assert_eq!(
        batch_result.decryption_proofs[0].order_hash,
        proof.order_hash
    );
}

// ---------------------------------------------------------------------------
// Test 5: Single-order batch produces the correct response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_single_order_batch_correct_response() {
    let engine = make_engine();
    let metrics = BatchMetrics::new();
    let (tx, rx) = mpsc::channel(64);

    let dispatcher = BatchDispatcher::new(500, 256);
    tokio::spawn(dispatcher.run(rx, Arc::clone(&engine), Arc::clone(&metrics), None));

    let mut rx = send_request(&tx, bid_request(6_000, 1), 1, None);
    let resps = tokio::time::timeout(Duration::from_millis(200), &mut rx)
        .await
        .expect("timed out on single-order batch")
        .expect("oneshot closed");

    // Must contain exactly one OrderPosted with status Open (resting bid).
    let posted = resps.iter().find_map(|r| {
        if let Response::OrderPosted(op) = r {
            Some(op)
        } else {
            None
        }
    });
    let posted = posted.expect("no OrderPosted response in single-order batch");
    assert!(
        matches!(
            posted.status,
            types::OrderStatus::Open | types::OrderStatus::PartiallyFilled
        ),
        "unexpected order status {:?}",
        posted.status
    );
}
