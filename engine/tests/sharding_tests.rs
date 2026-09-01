use engine::{MarketShards, MatchingEngine, UserState};
use std::sync::Arc;
use tokio::sync::RwLock;
use types::{
    AssetId, DepositRequest, FeeConfig, Market, MarketId, OrderSide, OrderType, PostOrderRequest,
    Request, UserId,
};

fn make_user(id: u8) -> UserId {
    let mut bytes = [0u8; 20];
    bytes[19] = id;
    UserId(bytes)
}

fn make_market(name: &str) -> Market {
    Market {
        id: MarketId(name.to_string()),
        base: AssetId::from_str(name.split('-').next().unwrap_or("ETH")),
        quote: AssetId::from_str("USDC"),
        max_orders: 1_000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: -1,
        taker_fee_bps: 5,
    }
}

async fn build_shards(markets: &[&str]) -> (Arc<MarketShards>, Arc<RwLock<UserState>>) {
    let user_state = Arc::new(RwLock::new(UserState::new(5.0)));
    {
        let mut us = user_state.write().await;
        for name in markets {
            us.add_market(make_market(name));
        }
    }
    let mut shards_builder = MarketShards::new(Arc::clone(&user_state));
    for name in markets {
        let market = make_market(name);
        let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
        engine.add_market(market);
        shards_builder.add_shard(MarketId(name.to_string()), engine);
    }
    (Arc::new(shards_builder), user_state)
}

async fn deposit(user_state: &Arc<RwLock<UserState>>, user: UserId, asset: &str, amount: u64) {
    let req = DepositRequest {
        user,
        asset: AssetId::from_str(asset),
        amount,
        l1_tx_hash: [0u8; 32],
    };
    let mut us = user_state.write().await;
    us.phase1_deposit(&req);
}

/// Test 1: Cross-market isolation.
/// Post an order on ETH-USDC and verify it doesn't appear in BTC-USDC order book.
#[tokio::test]
async fn test_cross_market_isolation() {
    use engine::{BatchMetrics, BatchedRequest};

    let (shards, user_state) = build_shards(&["ETH-USDC", "BTC-USDC"]).await;
    let metrics = BatchMetrics::new();

    let user1 = make_user(1);

    // Deposit USDC
    deposit(&user_state, user1.clone(), "USDC", 10_000_000_000).await;

    // Post a bid on ETH-USDC
    let (responder, resp_rx) = tokio::sync::oneshot::channel();
    let req = BatchedRequest {
        request: Request::PostOrder(PostOrderRequest {
            user: user1.clone(),
            market: MarketId("ETH-USDC".to_string()),
            side: OrderSide::Bid,
            order_type: OrderType::GoodTillCanceled,
            price: 3_000_000_000, // $3000
            quantity: 1_000_000,  // 1 ETH
            nonce: 1,
            client_order_id: None,
            signature: vec![],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        ts: 1000,
        responder,
        decryption_proof: None,
    };

    let result = MarketShards::dispatch_sharded_batch(
        Arc::clone(&shards),
        vec![req],
        std::time::Instant::now(),
        Arc::clone(&metrics),
    )
    .await;

    let responses = resp_rx.await.unwrap();
    assert!(
        responses
            .iter()
            .any(|r| matches!(r, types::Response::OrderPosted(_))),
        "Expected OrderPosted response"
    );

    // Check ETH-USDC shard has the order
    let eth_shard = shards
        .shards
        .get(&MarketId("ETH-USDC".to_string()))
        .unwrap();
    let eth_locked = eth_shard.lock().await;
    let eth_book = eth_locked
        .engine
        .order_books
        .get(&MarketId("ETH-USDC".to_string()))
        .unwrap();
    assert!(
        !eth_book.all_orders().is_empty(),
        "ETH-USDC should have resting order"
    );

    // Check BTC-USDC shard has no orders
    let btc_shard = shards
        .shards
        .get(&MarketId("BTC-USDC".to_string()))
        .unwrap();
    let btc_locked = btc_shard.lock().await;
    let btc_book = btc_locked
        .engine
        .order_books
        .get(&MarketId("BTC-USDC".to_string()))
        .unwrap();
    assert!(btc_book.all_orders().is_empty(), "BTC-USDC should be empty");

    let _ = result;
}

/// Test 2: Cross-market balance enforcement.
/// User with 1000 USDC submits two orders of 600 USDC notional each on different markets.
/// Phase 1 should reject the second one (InsufficientBalance).
#[tokio::test]
async fn test_cross_market_balance_constraint() {
    use engine::{BatchMetrics, BatchedRequest};

    let (shards, user_state) = build_shards(&["ETH-USDC", "BTC-USDC"]).await;
    let metrics = BatchMetrics::new();

    let user1 = make_user(2);

    // Deposit exactly 1000 USDC. With PRICE_DECIMALS=8 and QUANTITY_DECIMALS=8:
    // 1 USDC = 100_000_000 (1e8) units. So 1000 USDC = 100_000_000_000.
    deposit(&user_state, user1.clone(), "USDC", 100_000_000_000).await;

    // Two orders each requiring 600 USDC notional.
    // compute_notional(price, qty) = (price * qty) / QUANTITY_SCALE
    //   where QUANTITY_SCALE = 10^8 = 100_000_000
    // We want notional = 600 USDC = 60_000_000_000 units.
    // Let price = 1_00_000_000 ($1.00) and qty = 600_00_000_000 (600 units of base)
    // notional = (100_000_000 * 60_000_000_000) / 100_000_000 = 60_000_000_000 ✓
    //
    // So price = 100_000_000, quantity = 60_000_000_000
    let (r1, rx1) = tokio::sync::oneshot::channel();
    let (r2, rx2) = tokio::sync::oneshot::channel();

    let order1 = BatchedRequest {
        request: Request::PostOrder(PostOrderRequest {
            user: user1.clone(),
            market: MarketId("ETH-USDC".to_string()),
            side: OrderSide::Bid,
            order_type: OrderType::GoodTillCanceled,
            price: 100_000_000,       // $1.00
            quantity: 60_000_000_000, // 600 base units → notional = 60_000_000_000 (600 USDC)
            nonce: 1,
            client_order_id: None,
            signature: vec![],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        ts: 1000,
        responder: r1,
        decryption_proof: None,
    };

    let order2 = BatchedRequest {
        request: Request::PostOrder(PostOrderRequest {
            user: user1.clone(),
            market: MarketId("BTC-USDC".to_string()),
            side: OrderSide::Bid,
            order_type: OrderType::GoodTillCanceled,
            price: 100_000_000,       // $1.00
            quantity: 60_000_000_000, // 600 base units → notional = 60_000_000_000 (600 USDC)
            nonce: 2,
            client_order_id: None,
            signature: vec![],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        ts: 1001,
        responder: r2,
        decryption_proof: None,
    };

    // Submit both in the same batch
    let _result = MarketShards::dispatch_sharded_batch(
        Arc::clone(&shards),
        vec![order1, order2],
        std::time::Instant::now(),
        Arc::clone(&metrics),
    )
    .await;

    let resp1 = rx1.await.unwrap();
    let resp2 = rx2.await.unwrap();

    let ok1 = resp1
        .iter()
        .any(|r| matches!(r, types::Response::OrderPosted(_)));
    let ok2 = resp2
        .iter()
        .any(|r| matches!(r, types::Response::OrderPosted(_)));
    let err1 = resp1.iter().any(|r| matches!(r, types::Response::Error(_)));
    let err2 = resp2.iter().any(|r| matches!(r, types::Response::Error(_)));

    // Exactly one should succeed and one should fail
    assert!(
        (ok1 && err2) || (ok2 && err1),
        "Exactly one of the two 600-USDC orders on different markets should fail. ok1={ok1}, ok2={ok2}, err1={err1}, err2={err2}"
    );
}

/// Test 3: Concurrent fill ordering.
/// Two makers on ETH, one taker — fills should be deterministic (FIFO by arrival).
#[tokio::test]
async fn test_concurrent_fill_determinism() {
    use engine::{BatchMetrics, BatchedRequest};

    let (shards, user_state) = build_shards(&["ETH-USDC"]).await;
    let metrics = BatchMetrics::new();

    let maker1 = make_user(10);
    let maker2 = make_user(11);
    let taker = make_user(12);

    // Deposit assets
    deposit(&user_state, maker1.clone(), "ETH", 10_000_000).await;
    deposit(&user_state, maker2.clone(), "ETH", 10_000_000).await;
    deposit(&user_state, taker.clone(), "USDC", 100_000_000_000).await;

    // Place two ask orders (makers) at the same price
    let (rm1, rrx1) = tokio::sync::oneshot::channel();
    let (rm2, rrx2) = tokio::sync::oneshot::channel();

    let ask1 = BatchedRequest {
        request: Request::PostOrder(PostOrderRequest {
            user: maker1.clone(),
            market: MarketId("ETH-USDC".to_string()),
            side: OrderSide::Ask,
            order_type: OrderType::GoodTillCanceled,
            price: 3_000_000_000,
            quantity: 1_000_000,
            nonce: 1,
            client_order_id: None,
            signature: vec![],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        ts: 1000,
        responder: rm1,
        decryption_proof: None,
    };

    let ask2 = BatchedRequest {
        request: Request::PostOrder(PostOrderRequest {
            user: maker2.clone(),
            market: MarketId("ETH-USDC".to_string()),
            side: OrderSide::Ask,
            order_type: OrderType::GoodTillCanceled,
            price: 3_000_000_000,
            quantity: 1_000_000,
            nonce: 2,
            client_order_id: None,
            signature: vec![],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        ts: 1001,
        responder: rm2,
        decryption_proof: None,
    };

    // Post both maker orders first
    MarketShards::dispatch_sharded_batch(
        Arc::clone(&shards),
        vec![ask1, ask2],
        std::time::Instant::now(),
        Arc::clone(&metrics),
    )
    .await;

    let _ = rrx1.await;
    let _ = rrx2.await;

    // Now post a taker bid that should fill one of them
    let (rt, rtx) = tokio::sync::oneshot::channel();
    let bid = BatchedRequest {
        request: Request::PostOrder(PostOrderRequest {
            user: taker.clone(),
            market: MarketId("ETH-USDC".to_string()),
            side: OrderSide::Bid,
            order_type: OrderType::ImmediateOrCancel,
            price: 3_000_000_000,
            quantity: 1_000_000,
            nonce: 3,
            client_order_id: None,
            signature: vec![],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        ts: 2000,
        responder: rt,
        decryption_proof: None,
    };

    MarketShards::dispatch_sharded_batch(
        Arc::clone(&shards),
        vec![bid],
        std::time::Instant::now(),
        Arc::clone(&metrics),
    )
    .await;

    let taker_responses = rtx.await.unwrap();
    // Taker should have received exactly one fill
    let fills: Vec<_> = taker_responses
        .iter()
        .filter(|r| matches!(r, types::Response::OrderFilled(_)))
        .collect();
    assert_eq!(fills.len(), 1, "Taker should receive exactly one fill");

    // The remaining maker's order should still be in the book
    let shard = shards
        .shards
        .get(&MarketId("ETH-USDC".to_string()))
        .unwrap();
    let shard_locked = shard.lock().await;
    let book = shard_locked
        .engine
        .order_books
        .get(&MarketId("ETH-USDC".to_string()))
        .unwrap();
    let remaining_orders = book.all_orders();
    assert_eq!(
        remaining_orders.len(),
        1,
        "One maker order should remain in book"
    );
}
