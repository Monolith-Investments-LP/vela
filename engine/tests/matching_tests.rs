use engine::{CreditSystem, MatchingEngine};
use types::{
    AssetId, CancelOrderRequest, DepositRequest, ErrorCode, FeeConfig, Market, MarketId,
    NonceWindow, OrderSide, OrderStatus, OrderType, PostOrderRequest, Request, Response, UserId,
    UserMetadata, PRICE_SCALE, QUANTITY_SCALE,
};

fn btc_usdc() -> MarketId {
    MarketId::new("BTC", "USDC")
}
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

fn default_market() -> Market {
    Market {
        id: btc_usdc(),
        base: btc(),
        quote: usdc(),
        max_orders: 10_000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: -1,
        taker_fee_bps: 5,
    }
}

fn engine() -> MatchingEngine {
    let mut e = MatchingEngine::new(FeeConfig::default(), 5.0);
    e.add_market(default_market());
    e
}

fn deposit(user: UserId, asset: AssetId, amount: u64) -> Request {
    Request::Deposit(DepositRequest {
        user,
        asset,
        amount,
        l1_tx_hash: [0u8; 32],
    })
}

fn post_bid(user: UserId, price: u64, qty: u64, nonce: u64) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::GoodTillCanceled,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    })
}

fn post_ask(user: UserId, price: u64, qty: u64, nonce: u64) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Ask,
        order_type: OrderType::GoodTillCanceled,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    })
}

fn post_ioc_bid(user: UserId, price: u64, qty: u64, nonce: u64) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::ImmediateOrCancel,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    })
}

fn post_fok_bid(user: UserId, price: u64, qty: u64, nonce: u64) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::FillOrKill,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    })
}

#[test]
fn test_deposit() {
    let mut e = engine();
    let u = user(1);
    let responses = e.process(deposit(u.clone(), usdc(), 1000 * PRICE_SCALE), 1);
    assert!(matches!(responses[0], Response::BalanceUpdated(_)));
    let bal = e.balances.get(&(u, usdc())).unwrap();
    assert_eq!(bal.available, 1000 * PRICE_SCALE);
}

#[test]
fn test_resting_bid_no_match() {
    let mut e = engine();
    let maker = user(1);
    e.process(deposit(maker.clone(), usdc(), 50_100 * PRICE_SCALE), 1);
    let responses = e.process(post_bid(maker.clone(), 50_000, 1, 1), 2);
    let posted = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(posted.status, OrderStatus::Open);
    let bal = e.balances.get(&(maker, usdc())).unwrap();
    assert_eq!(bal.locked, 50_000 * PRICE_SCALE);
}

#[test]
fn test_full_match_bid_against_ask() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 50_100 * PRICE_SCALE), 2);

    let ask_responses = e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);
    let posted = ask_responses
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(posted.status, OrderStatus::Open);
    assert!(e.order_books.get(&btc_usdc()).unwrap().best_ask().is_some());

    let bid_responses = e.process(post_bid(taker.clone(), 50_000, 1, 1), 4);
    let fill = bid_responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(fill.quantity, QUANTITY_SCALE);
    assert_eq!(fill.price, 50_000 * PRICE_SCALE);
    assert_eq!(fill.maker, maker);
    assert_eq!(fill.taker, taker);

    assert!(e.order_books.get(&btc_usdc()).unwrap().best_ask().is_none());

    let taker_btc = e.balances.get(&(taker.clone(), btc())).unwrap();
    assert_eq!(taker_btc.available, QUANTITY_SCALE);

    let maker_usdc = e.balances.get(&(maker.clone(), usdc())).unwrap();
    assert!(maker_usdc.available > 0);
}

#[test]
fn test_partial_fill() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 2 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    e.process(post_ask(maker.clone(), 50_000, 2, 1), 3);
    let bid_responses = e.process(post_bid(taker.clone(), 50_000, 1, 1), 4);

    let fill = bid_responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(fill.quantity, QUANTITY_SCALE);

    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_ask().is_some());
    let ask = book.depth_asks(1);
    assert_eq!(ask[0].1, QUANTITY_SCALE);
}

#[test]
fn test_ioc_no_fill_canceled() {
    let mut e = engine();
    let taker = user(1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    let responses = e.process(post_ioc_bid(taker.clone(), 50_000, 1, 1), 2);
    let posted = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(posted.status, OrderStatus::Canceled);
    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_bid().is_none());
}

#[test]
fn test_fok_not_fully_fillable_rejected() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    let responses = e.process(post_fok_bid(taker.clone(), 50_000, 2, 1), 4);
    let err = responses
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::FokNotFilled);

    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_ask().is_some());
}

#[test]
fn test_cancel_resting_order() {
    let mut e = engine();
    let maker = user(1);
    e.process(deposit(maker.clone(), usdc(), 50_100 * PRICE_SCALE), 1);
    let responses = e.process(post_bid(maker.clone(), 50_000, 1, 1), 2);
    let order_id = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();

    let cancel = Request::CancelOrder(CancelOrderRequest {
        user: maker.clone(),
        order_id: Some(order_id),
        client_order_id: None,
        nonce: 2,
        signature: vec![0u8; 65],
    });
    let cancel_responses = e.process(cancel, 3);
    assert!(matches!(cancel_responses[0], Response::OrderCanceled(_)));

    let bal = e.balances.get(&(maker, usdc())).unwrap();
    assert_eq!(bal.locked, 0);
    assert_eq!(bal.available, 50_100 * PRICE_SCALE);
}

#[test]
fn test_invalid_nonce_replay() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(post_bid(u.clone(), 50_000, 1, 5), 2);
    let responses = e.process(post_bid(u.clone(), 50_000, 1, 5), 3);
    let err = responses
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::InvalidNonce);
}

#[test]
fn test_no_self_match() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u.clone(), btc(), 1 * QUANTITY_SCALE), 2);
    e.process(post_ask(u.clone(), 50_000, 1, 1), 3);
    let responses = e.process(post_bid(u.clone(), 50_000, 1, 2), 4);
    let fill = responses
        .iter()
        .find(|r| matches!(r, Response::OrderFilled(_)));
    assert!(fill.is_none(), "self-match should not produce a fill");
}

#[test]
fn test_price_time_priority() {
    let mut e = engine();
    let maker1 = user(1);
    let maker2 = user(2);
    let taker = user(3);

    e.process(deposit(maker1.clone(), btc(), 2 * QUANTITY_SCALE), 1);
    e.process(deposit(maker2.clone(), btc(), 2 * QUANTITY_SCALE), 2);
    e.process(deposit(taker.clone(), usdc(), 200_000 * PRICE_SCALE), 3);

    e.process(post_ask(maker1.clone(), 49_000, 1, 1), 4);
    e.process(post_ask(maker2.clone(), 50_000, 1, 1), 5);

    let responses = e.process(post_bid(taker.clone(), 50_000, 1, 1), 6);
    let fill = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();

    assert_eq!(fill.maker, maker1, "lower ask price should fill first");
    assert_eq!(fill.price, 49_000 * PRICE_SCALE);
}

#[test]
fn test_insufficient_balance_rejected() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100 * PRICE_SCALE), 1);
    let responses = e.process(post_bid(u.clone(), 50_000, 1, 1), 2);
    let err = responses
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::InsufficientBalance);
}

#[test]
fn test_multi_level_fill() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 3 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 200_000 * PRICE_SCALE), 2);

    e.process(post_ask(maker.clone(), 49_000, 1, 1), 3);
    e.process(post_ask(maker.clone(), 50_000, 1, 2), 4);

    let responses = e.process(post_bid(taker.clone(), 50_000, 2, 1), 5);
    let fills: Vec<_> = responses
        .iter()
        .filter_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0].price, 49_000 * PRICE_SCALE);
    assert_eq!(fills[1].price, 50_000 * PRICE_SCALE);

    let taker_btc = e.balances.get(&(taker, btc())).unwrap();
    assert_eq!(taker_btc.available, 2 * QUANTITY_SCALE);
}

// ─── VEL-14: Credit system tests ──────────────────────────────────────────────

fn credit_engine(default_ratio: f64) -> MatchingEngine {
    let mut e = MatchingEngine::new(FeeConfig::default(), default_ratio);
    e.add_market(default_market());
    e
}

/// A maker with ratio 3.0 and 100 USDC can cumulatively quote up to 300 USDC notional,
/// even when individual orders exhaust available balance (credit fills the gap).
/// Note: per-order floor still applies — each order's notional must fit within total deposited.
#[test]
fn test_credit_allows_quoting_beyond_available() {
    let mut e = credit_engine(3.0);
    let maker = user(1);
    // Deposit 100 USDC
    e.process(deposit(maker.clone(), usdc(), 100 * PRICE_SCALE), 1);

    // First bid: 1 BTC @ 60 USDC → notional 60. available=100≥60, no credit needed.
    // Locks 60. available=40, locked=60.
    let r1 = e.process(post_bid(maker.clone(), 60, 1, 1), 2);
    let p1 = r1
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(p1.status, OrderStatus::Open, "first bid should rest");

    // Second bid: 1 BTC @ 60 USDC → notional 60. available=40 < 60 → CREDIT used.
    // total_quoted would be 60+60=120 ≤ 100*3=300. Credit allows it.
    let r2 = e.process(post_bid(maker.clone(), 60, 1, 2), 3);
    let p2 = r2
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        p2.status,
        OrderStatus::Open,
        "second bid using credit should rest"
    );

    // Third bid: 1 BTC @ 60 USDC → notional 60. available=0 < 60 → CREDIT used.
    // total_quoted=180 ≤ 300. Still within credit limit.
    let r3 = e.process(post_bid(maker.clone(), 60, 1, 3), 4);
    let p3 = r3
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        p3.status,
        OrderStatus::Open,
        "third bid using credit should rest"
    );

    let meta = e.metadata.get(&maker).unwrap();
    assert_eq!(meta.order_id_count(), 3);
    let expected_notional = CreditSystem::compute_notional(60 * PRICE_SCALE, QUANTITY_SCALE) * 3;
    assert_eq!(
        meta.total_quoted_notional, expected_notional,
        "total_quoted_notional should track all three bids"
    );
    // Maker quoted 3×60=180 USDC with only 100 deposited — credit enabled the extra 80.
    assert!(
        180 * PRICE_SCALE <= 100 * PRICE_SCALE * 3,
        "sanity: 180 ≤ 300 (ratio 3.0)"
    );
}

/// A single order whose notional exceeds the maker's total deposited is rejected
/// even with a high credit ratio — the 1:1 per-order floor still applies.
#[test]
fn test_single_order_bounded_by_deposited() {
    let mut e = credit_engine(10.0);
    let maker = user(1);
    e.process(deposit(maker.clone(), usdc(), 100 * PRICE_SCALE), 1);

    // Try to post a bid for 1 BTC @ 200 USDC — notional 200 > deposited 100
    let responses = e.process(post_bid(maker.clone(), 200, 1, 1), 2);
    let err = responses
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        err.code,
        ErrorCode::InsufficientBalance,
        "order notional > total deposited must be rejected regardless of credit ratio"
    );
}

/// After a fill reduces the maker's actual collateral, if total_quoted_notional exceeds
/// actual_collateral * ratio the engine must atomically cancel the maker's open orders.
///
/// Setup: ratio=1.5, maker deposits 100 USDC → max_quoted = 150.
///   Bid X: 1 BTC @ 70 USDC → notional 70. available=100≥70, rests. available=30.
///   Bid Y: 1 BTC @ 70 USDC → notional 70. available=30<70 → credit (70+70=140≤150). Rests.
///   actual_collateral = 100 USDC.
///
/// Taker IOC ask at 70, qty=1 → fills X (time priority).
///   actual_collateral = 100 - 70 = 30. total_quoted = 70. max = 30*1.5 = 45. 70 > 45 → BREACH.
///   Engine auto-cancels Y to restore ratio.
#[test]
fn test_fill_triggers_auto_cancel_when_ratio_breached() {
    let mut e = credit_engine(1.5);
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), usdc(), 100 * PRICE_SCALE), 1);
    e.process(deposit(taker.clone(), btc(), 1 * QUANTITY_SCALE), 2);

    // Bid X at 70: within deposited (100), uses available. Rests.
    let r_x = e.process(post_bid(maker.clone(), 70, 1, 1), 3);
    let oid_x = r_x
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();
    assert!(r_x
        .iter()
        .any(|r| matches!(r, Response::OrderPosted(p) if p.status == OrderStatus::Open)));

    // Bid Y at 70: available=30 < 70 → credit used (140 ≤ 150). Rests.
    let r_y = e.process(post_bid(maker.clone(), 70, 1, 2), 4);
    let oid_y = r_y
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();
    assert!(r_y
        .iter()
        .any(|r| matches!(r, Response::OrderPosted(p) if p.status == OrderStatus::Open)));

    {
        let meta = e.metadata.get(&maker).unwrap();
        assert_eq!(meta.order_id_count(), 2);
        assert_eq!(
            meta.actual_collateral,
            100 * PRICE_SCALE,
            "actual_collateral must equal deposit"
        );
    }

    // Taker IOC ask @ 70, qty=1: hits best bid (70), fills X first (FIFO).
    let taker_resp = e.process(
        Request::PostOrder(PostOrderRequest {
            user: taker.clone(),
            market: btc_usdc(),
            side: OrderSide::Ask,
            order_type: OrderType::ImmediateOrCancel,
            price: 70 * PRICE_SCALE,
            quantity: 1 * QUANTITY_SCALE,
            nonce: 1,
            client_order_id: None,
            signature: vec![0u8; 65],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        5,
    );

    // Fill must have occurred (X filled)
    let fills: Vec<_> = taker_resp
        .iter()
        .filter(|r| matches!(r, Response::OrderFilled(_)))
        .collect();
    assert!(!fills.is_empty(), "taker ask should fill bid X");

    // Auto-cancel must have fired for Y (breach: 70 > 30*1.5=45)
    let canceled_ids: Vec<_> = taker_resp
        .iter()
        .filter_map(|r| {
            if let Response::OrderCanceled(c) = r {
                Some(c.order_id)
            } else {
                None
            }
        })
        .collect();
    assert!(
        !canceled_ids.is_empty(),
        "credit breach must trigger auto-cancel"
    );
    assert!(
        canceled_ids.contains(&oid_y),
        "bid Y must be auto-canceled to restore ratio"
    );
    assert!(
        !canceled_ids.contains(&oid_x),
        "bid X was filled, not auto-canceled"
    );

    // Book should be empty: X filled, Y auto-canceled.
    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_bid().is_none(), "all maker bids should be gone");

    // Maker received 1 BTC from filling X.
    let maker_btc = e
        .balances
        .get(&(maker.clone(), btc()))
        .map(|b| b.total())
        .unwrap_or(0);
    assert_eq!(
        maker_btc,
        1 * QUANTITY_SCALE,
        "maker should have received 1 BTC"
    );

    // actual_collateral should have dropped by fill_notional.
    let meta = e.metadata.get(&maker).unwrap();
    assert_eq!(
        meta.actual_collateral,
        30 * PRICE_SCALE,
        "actual_collateral = 100 - 70 (fill) = 30 USDC"
    );
}

/// 1:1 asset backing: after fills, the total of each asset across all users
/// must not exceed the total deposited (exchange never creates value from nothing).
/// Uses non-credit orders to avoid ghost balance effects (credit is tested separately).
#[test]
fn test_asset_backing_invariant_holds() {
    let mut e = credit_engine(2.0);
    let maker = user(1);
    let taker = user(2);

    // Maker deposits 100 USDC. Taker deposits 1 BTC.
    e.process(deposit(maker.clone(), usdc(), 100 * PRICE_SCALE), 1);
    e.process(deposit(taker.clone(), btc(), 1 * QUANTITY_SCALE), 2);

    // Maker places a single non-credit bid: 1 BTC @ 50 USDC.
    // Notional = 50 ≤ deposited = 100. available=100≥50. No credit.
    e.process(post_bid(maker.clone(), 50, 1, 1), 3);

    // Also place a second resting bid that won't be filled (for locked USDC coverage).
    e.process(post_bid(maker.clone(), 40, 1, 2), 4);

    // Taker sells 1 BTC at 50 → fills maker's 50 bid.
    e.process(
        Request::PostOrder(PostOrderRequest {
            user: taker.clone(),
            market: btc_usdc(),
            side: OrderSide::Ask,
            order_type: OrderType::ImmediateOrCancel,
            price: 50 * PRICE_SCALE,
            quantity: 1 * QUANTITY_SCALE,
            nonce: 1,
            client_order_id: None,
            signature: vec![0u8; 65],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        5,
    );

    let maker_usdc = e
        .balances
        .get(&(maker.clone(), usdc()))
        .map(|b| b.total())
        .unwrap_or(0);
    let maker_btc = e
        .balances
        .get(&(maker.clone(), btc()))
        .map(|b| b.total())
        .unwrap_or(0);
    let taker_usdc = e
        .balances
        .get(&(taker.clone(), usdc()))
        .map(|b| b.total())
        .unwrap_or(0);

    // Taker received USDC from fill.
    assert!(taker_usdc > 0, "taker should have received USDC from fill");

    // Maker received 1 BTC and still holds USDC for remaining resting bid (40 locked).
    assert_eq!(maker_btc, 1 * QUANTITY_SCALE, "maker received 1 BTC");
    assert!(
        maker_usdc > 0,
        "maker should have remaining USDC for resting bid"
    );

    // 1:1 USDC backing invariant: total USDC in system ≤ 100 USDC deposited by maker.
    // USDC flows from maker's locked balance to taker; fees are net-consumed by exchange.
    // (taker pays 5bps, maker gets 1bps rebate → exchange earns 4bps net → USDC decreases)
    assert!(
        maker_usdc + taker_usdc <= 100 * PRICE_SCALE,
        "total USDC must not exceed deposit: {} + {} = {} > {}",
        maker_usdc,
        taker_usdc,
        maker_usdc + taker_usdc,
        100 * PRICE_SCALE
    );
}

/// set_credit_ratio persists: with ratio=1.0 the engine auto-cancels to make room; upgrading
/// the ratio to 2.0 gives enough headroom that no auto-cancel is needed for the same order.
#[test]
fn test_set_ratio_per_user() {
    let mut e = credit_engine(1.0); // default: ratio 1.0
    let maker = user(1);

    e.process(deposit(maker.clone(), usdc(), 100 * PRICE_SCALE), 1);

    // First bid: 1 BTC @ 50 USDC. Locks 50. available=50, total_quoted=50.
    e.process(post_bid(maker.clone(), 50, 1, 1), 2);

    // Second bid at 80: available=50 < 80. total_quoted+80=130 > 100*1.0=100.
    // With ratio=1.0, engine auto-cancels the first order to make room.
    let second = e.process(post_bid(maker.clone(), 80, 1, 2), 3);
    assert!(
        second
            .iter()
            .any(|r| matches!(r, Response::OrderPosted(p) if p.status == OrderStatus::Open)),
        "second bid must succeed via auto-cancel"
    );
    assert!(
        second
            .iter()
            .any(|r| matches!(r, Response::OrderCanceled(_))),
        "ratio=1.0 must trigger auto-cancel to make room"
    );

    // Upgrade maker's ratio to 2.0 → max = 200. State: total_quoted=80, available≥0.
    e.set_credit_ratio(maker.clone(), 2.0);

    // Third bid at 80: total_quoted+80=160 ≤ 200 → no auto-cancel needed.
    let third = e.process(post_bid(maker.clone(), 80, 1, 3), 4);
    assert!(
        third
            .iter()
            .any(|r| matches!(r, Response::OrderPosted(p) if p.status == OrderStatus::Open)),
        "third bid must succeed with ratio=2.0"
    );
    assert!(
        !third
            .iter()
            .any(|r| matches!(r, Response::OrderCanceled(_))),
        "ratio=2.0 must allow quoting without auto-cancel"
    );
}

// ─── VEL-P2-04: CoW delta buffer correctness tests ────────────────────────────

#[test]
fn test_cow_fok_full_fill_commits() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    let responses = e.process(post_fok_bid(taker.clone(), 50_000, 1, 1), 4);
    let fill = responses.iter().find_map(|r| {
        if let Response::OrderFilled(f) = r {
            Some(f)
        } else {
            None
        }
    });
    assert!(
        fill.is_some(),
        "FOK should produce a fill when fully fillable"
    );

    let taker_btc = e.balances.get(&(taker, btc())).unwrap();
    assert_eq!(
        taker_btc.available, QUANTITY_SCALE,
        "taker should have received BTC"
    );

    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_ask().is_none(), "maker ask consumed by FOK fill");
}

#[test]
fn test_cow_fok_partial_fail_no_state_change() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 200_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    let taker_usdc_before = e.balances.get(&(taker.clone(), usdc())).unwrap().available;

    let responses = e.process(post_fok_bid(taker.clone(), 50_000, 2, 1), 4);
    let err = responses
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::FokNotFilled);

    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(
        book.best_ask().is_some(),
        "maker ask must still exist after FOK rollback"
    );

    let taker_usdc_after = e.balances.get(&(taker.clone(), usdc())).unwrap().available;
    assert_eq!(
        taker_usdc_before, taker_usdc_after,
        "taker balance unchanged after FOK failure"
    );

    let taker_btc = e
        .balances
        .get(&(taker, btc()))
        .map(|b| b.available)
        .unwrap_or(0);
    assert_eq!(taker_btc, 0, "taker receives no BTC after failed FOK");
}

#[test]
fn test_cow_ioc_partial_fill_commits_filled_portion() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 200_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    let responses = e.process(post_ioc_bid(taker.clone(), 50_000, 2, 1), 4);
    let fill = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        fill.quantity, QUANTITY_SCALE,
        "only available 1 BTC should be filled"
    );

    let taker_btc = e.balances.get(&(taker, btc())).unwrap().available;
    assert_eq!(
        taker_btc, QUANTITY_SCALE,
        "taker receives exactly the filled quantity"
    );

    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_ask().is_none(), "maker ask fully consumed");
    assert!(
        book.best_bid().is_none(),
        "IOC remainder does not rest on book"
    );
}

#[test]
fn test_cow_ioc_no_fill_leaves_state_clean() {
    let mut e = engine();
    let taker = user(1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 1);

    let available_before = e.balances.get(&(taker.clone(), usdc())).unwrap().available;

    let responses = e.process(post_ioc_bid(taker.clone(), 50_000, 1, 1), 2);
    let posted = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(posted.status, OrderStatus::Canceled);

    let available_after = e.balances.get(&(taker.clone(), usdc())).unwrap().available;
    assert_eq!(
        available_before, available_after,
        "no balance change when IOC finds nothing"
    );

    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_bid().is_none(), "IOC must not rest on book");
}

#[test]
fn test_cow_validation_failure_leaves_state_clean() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100 * PRICE_SCALE), 1);

    e.process(post_bid(u.clone(), 50, 1, 1), 2);
    let locked_after_first = e.balances.get(&(u.clone(), usdc())).unwrap().locked;

    let responses = e.process(post_bid(u.clone(), 50_000, 1, 2), 3);
    let err = responses
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::InsufficientBalance);

    let locked_after_fail = e.balances.get(&(u.clone(), usdc())).unwrap().locked;
    assert_eq!(
        locked_after_first, locked_after_fail,
        "failed order must not mutate locked balance"
    );
}

#[test]
fn test_cow_failed_order_does_not_corrupt_subsequent_order() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 200_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    let fail_resp = e.process(post_fok_bid(taker.clone(), 50_000, 2, 1), 4);
    assert!(
        fail_resp.iter().any(|r| matches!(r, Response::Error(_))),
        "first FOK should fail"
    );

    let ok_resp = e.process(post_bid(taker.clone(), 50_000, 1, 2), 5);
    let fill = ok_resp.iter().find_map(|r| {
        if let Response::OrderFilled(f) = r {
            Some(f)
        } else {
            None
        }
    });
    assert!(
        fill.is_some(),
        "subsequent GTC bid should still fill against maker"
    );

    let taker_btc = e.balances.get(&(taker, btc())).unwrap().available;
    assert_eq!(
        taker_btc, QUANTITY_SCALE,
        "taker receives BTC from the successful GTC fill"
    );
}

// ─── VEL-P2-02: Rolling 20-window nonce tests ─────────────────────────────────

/// 20 concurrent bids with non-sequential nonces all accepted — no strict ordering required.
#[test]
fn test_nonce_window_20_concurrent_non_sequential() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 10_000_000 * PRICE_SCALE), 1);

    let nonces: [u64; 20] = [
        100, 5, 999, 3, 42, 77, 200, 1, 50, 88, 300, 6, 400, 17, 501, 23, 600, 11, 700, 99,
    ];
    for &n in &nonces {
        let resp = e.process(post_bid(u.clone(), 1, 1, n), 2);
        assert!(
            resp.iter().any(|r| matches!(r, Response::OrderPosted(_))),
            "nonce {} should be accepted",
            n
        );
    }
    let meta = e.metadata.get(&u).unwrap();
    assert_eq!(meta.order_id_count(), 20, "all 20 orders must be resting");
}

/// 21st order with nonce at or below the window minimum is rejected.
#[test]
fn test_nonce_window_21st_below_minimum_rejected() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 10_000_000 * PRICE_SCALE), 1);

    for n in 1u64..=20 {
        e.process(post_bid(u.clone(), 1, 1, n), 2);
    }
    // min=1, window full — nonce == min is rejected
    let resp = e.process(post_bid(u.clone(), 1, 1, 1), 3);
    let err = resp
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        err.code,
        ErrorCode::InvalidNonce,
        "nonce equal to min must be rejected"
    );

    // nonce 0 is below min — also rejected
    let resp2 = e.process(post_bid(u.clone(), 1, 1, 0), 3);
    let err2 = resp2
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        err2.code,
        ErrorCode::InvalidNonce,
        "nonce below min must be rejected"
    );
}

/// Duplicate nonce is rejected even when the window is full (replay protection).
#[test]
fn test_nonce_window_duplicate_rejected_when_full() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 10_000_000 * PRICE_SCALE), 1);

    for n in 1u64..=20 {
        e.process(post_bid(u.clone(), 1, 1, n), 2);
    }
    // nonce 15 is still in the window (> min=1) but already used
    let resp = e.process(post_bid(u.clone(), 1, 1, 15), 3);
    let err = resp
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        err.code,
        ErrorCode::InvalidNonce,
        "duplicate nonce must be rejected"
    );

    // The rejected replay must not have evicted the minimum — window still has 20 entries
    let meta = e.metadata.get(&u).unwrap();
    assert_eq!(
        meta.order_id_count(),
        20,
        "window size must not shrink on replay attempt"
    );
}

/// After the window slides forward, nonces that fell below the new minimum are permanently rejected.
#[test]
fn test_nonce_window_evicted_nonce_below_new_min_rejected() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 10_000_000 * PRICE_SCALE), 1);

    // Fill window with nonces 1..=20 (min=1)
    for n in 1u64..=20 {
        e.process(post_bid(u.clone(), 1, 1, n), 2);
    }
    // Submit nonce 21 — evicts min=1, window becomes {2..=21}, min=2
    let resp = e.process(post_bid(u.clone(), 1, 1, 21), 3);
    assert!(
        resp.iter().any(|r| matches!(r, Response::OrderPosted(_))),
        "nonce 21 must be accepted"
    );

    // Nonce 1 is now below the new min=2 — must be rejected even though it's no longer in the set
    let resp2 = e.process(post_bid(u.clone(), 1, 1, 1), 3);
    let err = resp2
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        err.code,
        ErrorCode::InvalidNonce,
        "evicted nonce below new minimum must be rejected"
    );
}

/// NonceWindow serializes and deserializes correctly — nonce state survives a snapshot/restore cycle.
#[test]
fn test_nonce_window_snapshot_restore() {
    let mut window = NonceWindow::new();
    for &n in &[1u64, 5, 10, 99, 200] {
        assert!(window.accept(n));
    }

    let meta = UserMetadata {
        user: user(1),
        nonce_window: window,
        open_order_ids: [0u64; 64],
        credit_ratio: 1.0,
        total_quoted_notional: 0,
        actual_collateral: 0,
        ref_by: None,
        ref_earnings: 0,
        referred_users: vec![],
        fee_tier: 0,
    };

    let json = serde_json::to_string(&meta).unwrap();
    let restored: UserMetadata = serde_json::from_str(&json).unwrap();
    let mut w = restored.nonce_window;

    // Previously accepted nonces are still in the restored window — rejected
    assert!(!w.accept(1), "nonce 1 must be rejected after restore");
    assert!(!w.accept(5), "nonce 5 must be rejected after restore");
    assert!(!w.accept(200), "nonce 200 must be rejected after restore");
    // A fresh nonce above the window contents is accepted
    assert!(
        w.accept(300),
        "new nonce 300 must be accepted after restore"
    );
}

// ─── VEL-P2-03: Client order ID tests ─────────────────────────────────────────

fn post_bid_with_coid(
    user: UserId,
    price: u64,
    qty: u64,
    nonce: u64,
    coid: Option<&str>,
) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::GoodTillCanceled,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: coid.map(|s| s.to_string()),
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: None,
    })
}

fn cancel_by_coid(user: UserId, coid: &str, nonce: u64) -> Request {
    Request::CancelOrder(CancelOrderRequest {
        user,
        order_id: None,
        client_order_id: Some(coid.to_string()),
        nonce,
        signature: vec![0u8; 65],
    })
}

/// Submit order with client_order_id → accepted, mapping stored in book index.
#[test]
fn test_coid_submit_accepted_and_stored() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);

    let resp = e.process(
        post_bid_with_coid(u.clone(), 50_000, 1, 1, Some("my-order-1")),
        2,
    );
    let posted = resp
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(posted.status, OrderStatus::Open);
    assert_eq!(posted.client_order_id.as_deref(), Some("my-order-1"));

    // The book should be able to find the order by client_order_id
    let book = e.order_books.get(&btc_usdc()).unwrap();
    let found = book.find_by_client_order_id(&u, "my-order-1");
    assert!(
        found.is_some(),
        "client_order_id mapping must be stored in order book"
    );
    assert_eq!(found.unwrap(), posted.order_id);
}

/// Submit duplicate client_order_id for same user → rejected.
#[test]
fn test_coid_duplicate_same_user_rejected() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 200_000 * PRICE_SCALE), 1);

    e.process(
        post_bid_with_coid(u.clone(), 50_000, 1, 1, Some("dup-id")),
        2,
    );
    let resp = e.process(
        post_bid_with_coid(u.clone(), 50_000, 1, 2, Some("dup-id")),
        3,
    );
    let err = resp
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::DuplicateClientOrderId);
}

/// Same client_order_id for different users → both accepted (not global).
#[test]
fn test_coid_same_id_different_users_both_accepted() {
    let mut e = engine();
    let u1 = user(1);
    let u2 = user(2);
    e.process(deposit(u1.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u2.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    let r1 = e.process(
        post_bid_with_coid(u1.clone(), 50_000, 1, 1, Some("shared-id")),
        3,
    );
    let r2 = e.process(
        post_bid_with_coid(u2.clone(), 50_000, 1, 1, Some("shared-id")),
        4,
    );

    assert!(
        r1.iter().any(|r| matches!(r, Response::OrderPosted(_))),
        "user 1 order should be accepted"
    );
    assert!(
        r2.iter().any(|r| matches!(r, Response::OrderPosted(_))),
        "user 2 order should be accepted"
    );
}

/// Cancel by client_order_id → order cancelled, mapping removed.
#[test]
fn test_coid_cancel_by_coid() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);

    let resp = e.process(
        post_bid_with_coid(u.clone(), 50_000, 1, 1, Some("cancel-me")),
        2,
    );
    let order_id = resp
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();

    let cancel_resp = e.process(cancel_by_coid(u.clone(), "cancel-me", 2), 3);
    let canceled = cancel_resp
        .iter()
        .find_map(|r| {
            if let Response::OrderCanceled(c) = r {
                Some(c)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(canceled.order_id, order_id);
    assert_eq!(canceled.client_order_id.as_deref(), Some("cancel-me"));

    // Mapping must be gone
    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(
        book.find_by_client_order_id(&u, "cancel-me").is_none(),
        "mapping must be removed after cancel"
    );
}

/// Cancel by client_order_id that doesn't exist → clear error.
#[test]
fn test_coid_cancel_nonexistent_returns_error() {
    let mut e = engine();
    let u = user(1);

    let resp = e.process(cancel_by_coid(u.clone(), "ghost-id", 1), 1);
    let err = resp
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::OrderNotFound);
}

/// Order fill removes client_order_id mapping from book index.
#[test]
fn test_coid_fill_removes_mapping() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    e.process(
        Request::PostOrder(PostOrderRequest {
            user: maker.clone(),
            market: btc_usdc(),
            side: OrderSide::Ask,
            order_type: OrderType::GoodTillCanceled,
            price: 50_000 * PRICE_SCALE,
            quantity: 1 * QUANTITY_SCALE,
            nonce: 1,
            client_order_id: Some("maker-ask-1".to_string()),
            signature: vec![0u8; 65],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        3,
    );

    // Verify mapping exists before fill
    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book
        .find_by_client_order_id(&maker, "maker-ask-1")
        .is_some());

    // Taker fills the maker's order completely
    e.process(post_bid(taker.clone(), 50_000, 1, 1), 4);

    // Mapping must be gone after fill
    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(
        book.find_by_client_order_id(&maker, "maker-ask-1")
            .is_none(),
        "mapping removed after fill"
    );
}

/// client_order_id with invalid characters → rejected.
#[test]
fn test_coid_invalid_characters_rejected() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);

    let resp = e.process(
        post_bid_with_coid(u.clone(), 50_000, 1, 1, Some("bad id!")),
        2,
    );
    let err = resp
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::InvalidClientOrderId);
}

/// client_order_id > 64 chars → rejected.
#[test]
fn test_coid_too_long_rejected() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);

    let long_id = "a".repeat(65);
    let resp = e.process(
        post_bid_with_coid(u.clone(), 50_000, 1, 1, Some(&long_id)),
        2,
    );
    let err = resp
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::InvalidClientOrderId);
}

/// client_order_id exactly 64 chars with valid chars → accepted.
#[test]
fn test_coid_exactly_64_chars_accepted() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);

    let id_64 = "a".repeat(64);
    let resp = e.process(post_bid_with_coid(u.clone(), 50_000, 1, 1, Some(&id_64)), 2);
    assert!(resp
        .iter()
        .any(|r| matches!(r, Response::OrderPosted(p) if p.status == OrderStatus::Open)));
}

// ─── VEL-P2-06: Auto-cancel on credit ratio breach ────────────────────────────

/// MM submits an order that would breach credit → oldest open order auto-cancelled
/// to make room, new order accepted. Cancel response must be emitted.
#[test]
fn test_credit_breach_auto_cancel_oldest_first() {
    let mut e = credit_engine(1.0);
    let maker = user(1);

    e.process(deposit(maker.clone(), usdc(), 100 * PRICE_SCALE), 1);

    // Bid at 70: notional=70. Locks 70. available=30, total_quoted=70. (oldest)
    let r_old = e.process(post_bid(maker.clone(), 70, 1, 1), 2);
    let oid_old = r_old
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();

    // Bid at 20: notional=20. Locks 20. available=10, total_quoted=90.
    let r_mid = e.process(post_bid(maker.clone(), 20, 1, 2), 3);
    let oid_mid = r_mid
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();

    // New bid at 50: notional=50. available=10 < 50. 90+50=140 > 100.
    // Auto-cancel oid_old (notional=70, oldest): total_quoted=20. 20+50=70 ≤ 100. Done.
    let r_in = e.process(post_bid(maker.clone(), 50, 1, 3), 4);

    let canceled_ids: Vec<_> = r_in
        .iter()
        .filter_map(|r| {
            if let Response::OrderCanceled(c) = r {
                Some(c.order_id)
            } else {
                None
            }
        })
        .collect();
    assert!(
        canceled_ids.contains(&oid_old),
        "oldest order must be auto-cancelled"
    );
    assert!(
        !canceled_ids.contains(&oid_mid),
        "newer order must NOT be auto-cancelled"
    );

    let posted = r_in
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        posted.status,
        OrderStatus::Open,
        "incoming order must be accepted after auto-cancel"
    );
}

/// Multiple orders auto-cancelled (oldest first) until ratio satisfied.
#[test]
fn test_credit_breach_multiple_cancels_until_ratio_satisfied() {
    let mut e = credit_engine(1.0);
    let maker = user(1);

    e.process(deposit(maker.clone(), usdc(), 90 * PRICE_SCALE), 1);

    // Three bids of 30 each. total_quoted=90, available=0.
    let r1 = e.process(post_bid(maker.clone(), 30, 1, 1), 2);
    let oid1 = r1
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();
    let r2 = e.process(post_bid(maker.clone(), 30, 1, 2), 3);
    let oid2 = r2
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();
    let r3 = e.process(post_bid(maker.clone(), 30, 1, 3), 4);
    let oid3 = r3
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();

    // New bid at 50: 90+50=140 > 90.
    // Cancel oid1 (30): total=60, 60+50=110 > 90. Still over.
    // Cancel oid2 (30): total=30, 30+50=80 ≤ 90. Done.
    let r_in = e.process(post_bid(maker.clone(), 50, 1, 4), 5);

    let canceled_ids: Vec<_> = r_in
        .iter()
        .filter_map(|r| {
            if let Response::OrderCanceled(c) = r {
                Some(c.order_id)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(canceled_ids.len(), 2, "two orders must be auto-cancelled");
    assert!(canceled_ids.contains(&oid1));
    assert!(canceled_ids.contains(&oid2));
    assert!(
        !canceled_ids.contains(&oid3),
        "third (newest) order must survive"
    );

    let posted = r_in
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(posted.status, OrderStatus::Open);

    let meta = e.metadata.get(&maker).unwrap();
    assert_eq!(
        meta.order_id_count(),
        2,
        "two orders remain (oid3 + new order)"
    );
}

/// If the incoming order fails after auto-cancels, the delta rollback undoes the auto-cancels.
#[test]
fn test_credit_breach_auto_cancel_rolled_back_on_order_failure() {
    let mut e = credit_engine(1.0);
    let maker = user(1);

    e.process(deposit(maker.clone(), usdc(), 100 * PRICE_SCALE), 1);

    // Resting bid at 80: locks 80. available=20. total_quoted=80.
    let r_rest = e.process(post_bid(maker.clone(), 80, 1, 1), 2);
    let oid_rest = r_rest
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();

    // FillOrKill bid at 30: notional=30. available=20 < 30.
    // 80+30=110 > 100 → auto-cancel oid_rest. total_quoted=0, 0+30=30 ≤ 100.
    // FillOrKill: no counterparty → FokNotFilled → delta rollback (undoes auto-cancel).
    let r_fok = e.process(
        Request::PostOrder(PostOrderRequest {
            user: maker.clone(),
            market: btc_usdc(),
            side: OrderSide::Bid,
            order_type: OrderType::FillOrKill,
            price: 30 * PRICE_SCALE,
            quantity: 1 * QUANTITY_SCALE,
            nonce: 2,
            client_order_id: None,
            signature: vec![0u8; 65],
            stp: Default::default(),
            min_quantity: None,
            display_quantity: None,
        }),
        3,
    );

    let err = r_fok
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::FokNotFilled);

    // Original order must still exist (auto-cancel was rolled back with delta)
    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(
        book.get_order(oid_rest).is_some(),
        "auto-cancelled order must be restored on rollback"
    );

    let meta = e.metadata.get(&maker).unwrap();
    assert!(meta.contains_order_id(oid_rest));
    let expected_notional = CreditSystem::compute_notional(80 * PRICE_SCALE, QUANTITY_SCALE);
    assert_eq!(
        meta.total_quoted_notional, expected_notional,
        "total_quoted_notional must be unchanged after rollback"
    );
}

/// MM with no open orders but notional > deposited → InsufficientBalance (hard floor, not auto-cancel).
#[test]
fn test_credit_breach_no_open_orders_insufficient_balance() {
    let mut e = credit_engine(1.0);
    let maker = user(1);

    e.process(deposit(maker.clone(), usdc(), 50 * PRICE_SCALE), 1);

    // notional=80 > deposited=50 → hard floor rejection before credit path
    let r = e.process(post_bid(maker.clone(), 80, 1, 1), 2);
    let err = r
        .iter()
        .find_map(|r| {
            if let Response::Error(e) = r {
                Some(e)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(err.code, ErrorCode::InsufficientBalance);

    let book = e.order_books.get(&btc_usdc()).unwrap();
    assert!(book.best_bid().is_none());
}

/// client_order_ids survive a JSON snapshot/restore cycle.
#[test]
fn test_coid_survives_snapshot_restore() {
    use engine::MatchingEngine;
    use types::{FeeConfig, Market, MarketId};

    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    let resp = e.process(
        post_bid_with_coid(u.clone(), 50_000, 1, 1, Some("persist-me")),
        2,
    );
    let original_id = resp
        .iter()
        .find_map(|r| {
            if let Response::OrderPosted(p) = r {
                Some(p.order_id)
            } else {
                None
            }
        })
        .unwrap();

    // Serialize the order book state (orders contain client_order_id)
    let orders: Vec<types::Order> = e
        .order_books
        .values()
        .flat_map(|b| b.all_orders())
        .collect();
    let json = serde_json::to_string(&orders).unwrap();
    let restored_orders: Vec<types::Order> = serde_json::from_str(&json).unwrap();

    // Build a fresh engine and restore
    let mut e2 = MatchingEngine::new(FeeConfig::default(), 5.0);
    e2.add_market(Market {
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
    let book2 = e2.order_books.get_mut(&btc_usdc()).unwrap();
    for o in restored_orders {
        book2.insert_resting(o).unwrap();
    }

    // The client_order_id index must be rebuilt
    let book2 = e2.order_books.get(&btc_usdc()).unwrap();
    let found = book2.find_by_client_order_id(&u, "persist-me");
    assert!(
        found.is_some(),
        "client_order_id must survive snapshot/restore"
    );
    assert_eq!(found.unwrap(), original_id);
}

// ─── VEL-P2-07: Maker/taker fee tests ─────────────────────────────────────────

fn zero_fee_market() -> Market {
    Market {
        id: btc_usdc(),
        base: btc(),
        quote: usdc(),
        max_orders: 10_000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: 0,
        taker_fee_bps: 0,
    }
}

fn fee_engine() -> MatchingEngine {
    let mut e = MatchingEngine::new(FeeConfig::default(), 5.0);
    e.add_market(default_market());
    e
}

/// On an ask fill (taker sells BTC against resting bid), taker's USDC proceeds are
/// reduced by taker_fee: taker receives fill_notional - taker_fee net.
#[test]
fn test_fee_taker_deducted_from_taker_balance_bid() {
    let mut e = fee_engine();
    let maker = user(1);
    let taker = user(2);

    // Maker places resting bid (locks USDC), taker is the incoming ask (seller).
    e.process(deposit(maker.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(taker.clone(), btc(), 1 * QUANTITY_SCALE), 2);

    e.process(post_bid(maker.clone(), 50_000, 1, 1), 3);

    let taker_usdc_before = e
        .balances
        .get(&(taker.clone(), usdc()))
        .map(|b| b.available)
        .unwrap_or(0);
    let responses = e.process(post_ask(taker.clone(), 50_000, 1, 1), 4);

    let fill = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();
    let taker_usdc_after = e.balances.get(&(taker.clone(), usdc())).unwrap().available;

    let fill_notional = CreditSystem::compute_notional(fill.price, fill.quantity);
    let expected_taker_fee = (fill_notional as i64 * 5) / 10_000;
    assert!(expected_taker_fee > 0, "taker fee must be positive");
    assert_eq!(
        fill.taker_fee, expected_taker_fee,
        "fill.taker_fee must match computed fee"
    );
    // Ask taker receives fill_notional credited then taker_fee debited from available
    assert_eq!(
        taker_usdc_after - taker_usdc_before,
        (fill_notional as i64 - expected_taker_fee) as u64,
        "ask taker net USDC = fill_notional - taker_fee"
    );
}

/// On a bid fill, maker receives a rebate in addition to the fill notional.
#[test]
fn test_fee_maker_rebate_credited_bid() {
    let mut e = fee_engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    let responses = e.process(post_bid(taker.clone(), 50_000, 1, 1), 4);
    let fill = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();

    let fill_notional = fill.price / PRICE_SCALE * fill.quantity / QUANTITY_SCALE * PRICE_SCALE;
    let expected_maker_fee = (fill_notional as i64 * -1) / 10_000; // negative = rebate
    assert!(
        expected_maker_fee < 0,
        "maker fee must be negative (rebate)"
    );

    let maker_usdc = e.balances.get(&(maker.clone(), usdc())).unwrap().available;
    let expected_maker_usdc = fill_notional + expected_maker_fee.unsigned_abs();
    assert_eq!(
        maker_usdc, expected_maker_usdc,
        "maker USDC must equal fill_notional + rebate"
    );
    assert_eq!(
        fill.maker_fee, expected_maker_fee,
        "fill.maker_fee must match computed rebate"
    );
}

/// Exchange fee_balances["USDC"] accumulates net fees (taker_fee - abs(maker_rebate)) after fills.
#[test]
fn test_exchange_fee_balance_increases() {
    let mut e = fee_engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    let responses = e.process(post_bid(taker.clone(), 50_000, 1, 1), 4);
    let fill = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();

    let net_fee = fill.taker_fee + fill.maker_fee;
    assert!(
        net_fee > 0,
        "net exchange fee must be positive with default rates"
    );

    let fee_balance = e.fee_balances.get("USDC").copied().unwrap_or(0);
    assert_eq!(
        fee_balance, net_fee as u64,
        "fee_balances[USDC] must equal net exchange fee"
    );
}

/// Fill response includes correct maker_fee and taker_fee amounts.
#[test]
fn test_fill_response_includes_fee_amounts() {
    let mut e = fee_engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 2 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 200_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);
    e.process(post_ask(maker.clone(), 51_000, 1, 2), 4);

    let responses = e.process(post_bid(taker.clone(), 51_000, 2, 1), 5);
    let fills: Vec<_> = responses
        .iter()
        .filter_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(fills.len(), 2);
    for fill in fills {
        let notional = fill.price / PRICE_SCALE * fill.quantity / QUANTITY_SCALE * PRICE_SCALE;
        let expected_taker_fee = (notional as i64 * 5) / 10_000;
        let expected_maker_fee = (notional as i64 * -1) / 10_000;
        assert_eq!(fill.taker_fee, expected_taker_fee);
        assert_eq!(fill.maker_fee, expected_maker_fee);
    }
}

/// With zero fee config, no fees are deducted from balances and fee_balances stays empty.
#[test]
fn test_zero_fee_config_no_fees() {
    let mut e = MatchingEngine::new(FeeConfig::default(), 5.0);
    e.add_market(zero_fee_market());

    let maker = user(1);
    let taker = user(2);

    // Maker places resting bid (locks USDC), taker is ask (seller).
    e.process(deposit(maker.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(taker.clone(), btc(), 1 * QUANTITY_SCALE), 2);
    e.process(post_bid(maker.clone(), 50_000, 1, 1), 3);

    let taker_usdc_before = e
        .balances
        .get(&(taker.clone(), usdc()))
        .map(|b| b.available)
        .unwrap_or(0);
    let responses = e.process(post_ask(taker.clone(), 50_000, 1, 1), 4);
    let fill = responses
        .iter()
        .find_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f)
            } else {
                None
            }
        })
        .unwrap();

    assert_eq!(
        fill.taker_fee, 0,
        "taker fee must be 0 with zero fee config"
    );
    assert_eq!(
        fill.maker_fee, 0,
        "maker fee must be 0 with zero fee config"
    );

    let taker_usdc_after = e.balances.get(&(taker.clone(), usdc())).unwrap().available;
    let fill_notional = CreditSystem::compute_notional(fill.price, fill.quantity);
    // Ask taker receives full fill_notional with no fee deduction
    assert_eq!(
        taker_usdc_after - taker_usdc_before,
        fill_notional,
        "no fee deducted with zero config"
    );

    assert_eq!(
        e.fee_balances.get("USDC").copied().unwrap_or(0),
        0,
        "fee_balances stays zero"
    );
}

/// Fee balances survive serialization and deserialization (snapshot/restore simulation).
#[test]
fn test_fee_accumulation_snapshot_restore() {
    let mut e = fee_engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);
    e.process(post_bid(taker.clone(), 50_000, 1, 1), 4);

    let fee_before = e.fee_balances.get("USDC").copied().unwrap_or(0);
    assert!(fee_before > 0, "fee_balances must be non-zero after fill");

    let serialized = serde_json::to_string(&e.fee_balances).unwrap();
    let restored: std::collections::HashMap<String, u64> =
        serde_json::from_str(&serialized).unwrap();

    assert_eq!(
        restored.get("USDC").copied().unwrap_or(0),
        fee_before,
        "fee_balances survive serde round-trip"
    );
}

/// Multiple fills accumulate fees correctly in fee_balances.
#[test]
fn test_fee_multiple_fills_accumulate() {
    let mut e = fee_engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 3 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 500_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);
    e.process(post_ask(maker.clone(), 50_000, 1, 2), 4);
    e.process(post_ask(maker.clone(), 50_000, 1, 3), 5);

    e.process(post_bid(taker.clone(), 50_000, 1, 1), 6);
    let after_first = e.fee_balances.get("USDC").copied().unwrap_or(0);

    e.process(post_bid(taker.clone(), 50_000, 1, 2), 7);
    let after_second = e.fee_balances.get("USDC").copied().unwrap_or(0);

    e.process(post_bid(taker.clone(), 50_000, 1, 3), 8);
    let after_third = e.fee_balances.get("USDC").copied().unwrap_or(0);

    assert!(
        after_second > after_first,
        "fee_balances must increase after second fill"
    );
    assert!(
        after_third > after_second,
        "fee_balances must increase after third fill"
    );
    assert_eq!(
        after_third,
        after_first * 3,
        "fee_balances must be 3x single fill (identical fills)"
    );
}

/// Negative net fee (rebate > taker fee) does not panic — handled gracefully.
#[test]
fn test_negative_net_fee_handled_gracefully() {
    let mut e = MatchingEngine::new(FeeConfig::default(), 5.0);
    e.add_market(Market {
        id: btc_usdc(),
        base: btc(),
        quote: usdc(),
        max_orders: 10_000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: -10, // large rebate: 10bps
        taker_fee_bps: 1,   // small fee: 1bps
    });

    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 1 * QUANTITY_SCALE), 1);
    // Give taker plenty of USDC to cover fee
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);
    e.process(post_ask(maker.clone(), 50_000, 1, 1), 3);

    // Should not panic — net exchange fee is negative (exchange subsidizes)
    let responses = e.process(post_bid(taker.clone(), 50_000, 1, 1), 4);
    let fill = responses.iter().find_map(|r| {
        if let Response::OrderFilled(f) = r {
            Some(f)
        } else {
            None
        }
    });
    assert!(
        fill.is_some(),
        "fill must complete even with negative net fee"
    );

    // fee_balances saturates at 0 if net is negative
    let fee_balance = e.fee_balances.get("USDC").copied().unwrap_or(0);
    assert_eq!(
        fee_balance, 0,
        "fee_balances saturates at 0 on negative net fee"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Self-trade prevention (STP) + min_quantity tests.
// Added when STP + min_quantity shipped (Tier 1 buildplan).
// ────────────────────────────────────────────────────────────────────────────

use types::SelfTradePreventionMode;

fn post_bid_stp(
    user: UserId,
    price: u64,
    qty: u64,
    nonce: u64,
    stp: SelfTradePreventionMode,
) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::GoodTillCanceled,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp,
        min_quantity: None,
        display_quantity: None,
    })
}

fn post_ask_stp(
    user: UserId,
    price: u64,
    qty: u64,
    nonce: u64,
    stp: SelfTradePreventionMode,
) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Ask,
        order_type: OrderType::GoodTillCanceled,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp,
        min_quantity: None,
        display_quantity: None,
    })
}

fn count_fills(responses: &[Response]) -> usize {
    responses
        .iter()
        .filter(|r| matches!(r, Response::OrderFilled(_)))
        .count()
}

fn count_cancels(responses: &[Response]) -> usize {
    responses
        .iter()
        .filter(|r| matches!(r, Response::OrderCanceled(_)))
        .count()
}

fn posted_status(responses: &[Response]) -> Option<OrderStatus> {
    responses.iter().find_map(|r| {
        if let Response::OrderPosted(p) = r {
            Some(p.status)
        } else {
            None
        }
    })
}

/// Baseline: without STP (None), a self-match is silently skipped
/// (historical behavior). The taker doesn't fill against its own resting
/// order but the resting order stays on the book.
#[test]
fn test_stp_none_skips_self_match_silently() {
    let mut e = engine();
    let u = user(1);
    // Deposit both sides.
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u.clone(), btc(), 5 * QUANTITY_SCALE), 2);

    // Post own ask at 100.
    e.process(
        post_ask_stp(u.clone(), 100, 1, 1, SelfTradePreventionMode::None),
        3,
    );
    // Try to lift own ask with a bid at 100. STP=None should silently skip.
    let responses = e.process(
        post_bid_stp(u.clone(), 100, 1, 2, SelfTradePreventionMode::None),
        4,
    );

    assert_eq!(count_fills(&responses), 0, "no fills against self");
    // The bid should rest since nothing else was on the book.
    assert_eq!(posted_status(&responses), Some(OrderStatus::Open));
}

/// CancelTaker: the incoming order is canceled with zero fills; the resting
/// order stays untouched.
#[test]
fn test_stp_cancel_taker_zeros_incoming() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u.clone(), btc(), 5 * QUANTITY_SCALE), 2);

    e.process(
        post_ask_stp(u.clone(), 100, 1, 1, SelfTradePreventionMode::None),
        3,
    );
    let responses = e.process(
        post_bid_stp(u.clone(), 100, 1, 2, SelfTradePreventionMode::CancelTaker),
        4,
    );

    assert_eq!(count_fills(&responses), 0);
    assert_eq!(count_cancels(&responses), 0, "resting maker is untouched");
    assert_eq!(posted_status(&responses), Some(OrderStatus::Canceled));
}

/// CancelMaker: the resting order gets canceled; the taker continues to the
/// next order. Here there's no next order so the taker rests.
#[test]
fn test_stp_cancel_maker_removes_resting() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u.clone(), btc(), 5 * QUANTITY_SCALE), 2);

    e.process(
        post_ask_stp(u.clone(), 100, 1, 1, SelfTradePreventionMode::None),
        3,
    );
    let responses = e.process(
        post_bid_stp(u.clone(), 100, 1, 2, SelfTradePreventionMode::CancelMaker),
        4,
    );

    assert_eq!(count_fills(&responses), 0);
    assert_eq!(count_cancels(&responses), 1, "resting maker canceled");
    // Taker rests since there's nothing else to match against.
    assert_eq!(posted_status(&responses), Some(OrderStatus::Open));
}

/// CancelBoth: resting canceled, taker canceled.
#[test]
fn test_stp_cancel_both() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u.clone(), btc(), 5 * QUANTITY_SCALE), 2);

    e.process(
        post_ask_stp(u.clone(), 100, 1, 1, SelfTradePreventionMode::None),
        3,
    );
    let responses = e.process(
        post_bid_stp(u.clone(), 100, 1, 2, SelfTradePreventionMode::CancelBoth),
        4,
    );

    assert_eq!(count_fills(&responses), 0);
    assert_eq!(count_cancels(&responses), 1, "resting maker canceled");
    assert_eq!(posted_status(&responses), Some(OrderStatus::Canceled));
}

/// DecrementAndCancel where taker >= maker: cancel maker in full, taker
/// continues with the leftover.
#[test]
fn test_stp_decrement_and_cancel_taker_larger() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u.clone(), btc(), 5 * QUANTITY_SCALE), 2);

    // Maker asks 1 BTC @ 100.
    e.process(
        post_ask_stp(u.clone(), 100, 1, 1, SelfTradePreventionMode::None),
        3,
    );
    // Taker bids 3 BTC @ 100. Should cancel maker, taker rests with 2 BTC
    // remaining (no other counterparty).
    let responses = e.process(
        post_bid_stp(
            u.clone(),
            100,
            3,
            2,
            SelfTradePreventionMode::DecrementAndCancel,
        ),
        4,
    );

    assert_eq!(count_fills(&responses), 0, "no fill happens under DAC");
    assert_eq!(count_cancels(&responses), 1, "maker canceled");
    // Taker rests because remaining quantity > 0 and order type is GTC.
    assert_eq!(posted_status(&responses), Some(OrderStatus::Open));
}

/// DecrementAndCancel where taker < maker (v1: cancel both, per doc).
#[test]
fn test_stp_decrement_and_cancel_taker_smaller_cancels_both() {
    let mut e = engine();
    let u = user(1);
    e.process(deposit(u.clone(), usdc(), 100_000 * PRICE_SCALE), 1);
    e.process(deposit(u.clone(), btc(), 5 * QUANTITY_SCALE), 2);

    // Maker asks 3 BTC @ 100.
    e.process(
        post_ask_stp(u.clone(), 100, 3, 1, SelfTradePreventionMode::None),
        3,
    );
    // Taker bids 1 BTC. v1 policy: cancel both.
    let responses = e.process(
        post_bid_stp(
            u.clone(),
            100,
            1,
            2,
            SelfTradePreventionMode::DecrementAndCancel,
        ),
        4,
    );

    assert_eq!(count_fills(&responses), 0);
    assert_eq!(count_cancels(&responses), 1);
    assert_eq!(posted_status(&responses), Some(OrderStatus::Canceled));
}

/// STP does not block matches against OTHER users.
#[test]
fn test_stp_does_not_affect_other_users() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);
    e.process(deposit(maker.clone(), btc(), 5 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    e.process(
        post_ask_stp(maker, 100, 1, 1, SelfTradePreventionMode::None),
        3,
    );
    let responses = e.process(
        post_bid_stp(taker, 100, 1, 1, SelfTradePreventionMode::CancelTaker),
        4,
    );

    assert_eq!(
        count_fills(&responses),
        1,
        "matches against other user still fill"
    );
    assert_eq!(posted_status(&responses), Some(OrderStatus::Filled));
}

/// min_quantity: order rejects atomically if total filled falls short.
#[test]
fn test_min_quantity_rejects_under_threshold() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 5 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 1_000_000 * PRICE_SCALE), 2);

    // Only 1 BTC available at 100.
    e.process(
        post_ask_stp(maker.clone(), 100, 1, 1, SelfTradePreventionMode::None),
        3,
    );

    // Taker wants 5 BTC with min_quantity=3. Only 1 available → reject.
    let req = Request::PostOrder(PostOrderRequest {
        user: taker.clone(),
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::ImmediateOrCancel,
        price: 100 * PRICE_SCALE,
        quantity: 5 * QUANTITY_SCALE,
        nonce: 1,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: SelfTradePreventionMode::None,
        min_quantity: Some(3 * QUANTITY_SCALE),
        display_quantity: None,
    });

    let responses = e.process(req, 4);

    // The error path returns a single Error response.
    let err = responses.iter().find_map(|r| {
        if let Response::Error(e) = r {
            Some(e.code)
        } else {
            None
        }
    });
    assert_eq!(err, Some(ErrorCode::MinQuantityNotMet));

    // Atomicity: the maker's resting ask is still there (nothing was consumed).
    e.process(
        post_ask_stp(maker.clone(), 101, 1, 2, SelfTradePreventionMode::None),
        5,
    );
    // If the previous rejected order had partially filled, this cheap sanity check
    // (posting a second ask) would still work regardless. The stronger check is that
    // taker's USDC balance was not debited — checked via a second small IOC below.
    let req_small = Request::PostOrder(PostOrderRequest {
        user: taker,
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::ImmediateOrCancel,
        price: 100 * PRICE_SCALE,
        quantity: 1 * QUANTITY_SCALE,
        nonce: 2,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: SelfTradePreventionMode::None,
        min_quantity: None,
        display_quantity: None,
    });
    let r2 = e.process(req_small, 6);
    assert_eq!(
        count_fills(&r2),
        1,
        "taker balance was not debited by rejected order"
    );
}

/// min_quantity: order fills when threshold is met.
#[test]
fn test_min_quantity_accepts_when_met() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 5 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 1_000_000 * PRICE_SCALE), 2);

    e.process(
        post_ask_stp(maker.clone(), 100, 3, 1, SelfTradePreventionMode::None),
        3,
    );

    // Taker wants 5 BTC with min_quantity=2. 3 available → accept, fills 3.
    let req = Request::PostOrder(PostOrderRequest {
        user: taker,
        market: btc_usdc(),
        side: OrderSide::Bid,
        order_type: OrderType::ImmediateOrCancel,
        price: 100 * PRICE_SCALE,
        quantity: 5 * QUANTITY_SCALE,
        nonce: 1,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: SelfTradePreventionMode::None,
        min_quantity: Some(2 * QUANTITY_SCALE),
        display_quantity: None,
    });
    let responses = e.process(req, 4);

    assert_eq!(count_fills(&responses), 1);
    // IOC + partial fill → status Canceled with 3/5 filled.
    let filled_qty: u64 = responses
        .iter()
        .filter_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f.quantity)
            } else {
                None
            }
        })
        .sum();
    assert_eq!(filled_qty, 3 * QUANTITY_SCALE);
}

// ────────────────────────────────────────────────────────────────────────────
// Iceberg order tests (Tier 1 buildplan).
// ────────────────────────────────────────────────────────────────────────────

fn post_iceberg_ask(user: UserId, price: u64, qty: u64, display: u64, nonce: u64) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market: btc_usdc(),
        side: OrderSide::Ask,
        order_type: OrderType::GoodTillCanceled,
        price: price * PRICE_SCALE,
        quantity: qty * QUANTITY_SCALE,
        nonce,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: Some(display * QUANTITY_SCALE),
    })
}

/// Iceberg: public depth shows only the display slice, not the full quantity.
#[test]
fn test_iceberg_hides_reserve_from_public_depth() {
    let mut e = engine();
    let maker = user(1);
    e.process(deposit(maker.clone(), btc(), 100 * QUANTITY_SCALE), 1);

    // Ask 10 BTC @ 100 but only display 2 BTC.
    e.process(post_iceberg_ask(maker, 100, 10, 2, 1), 2);

    let book = e.order_books.get(&btc_usdc()).unwrap();
    let asks = book.depth_asks(1);
    assert_eq!(asks.len(), 1);
    let (_price, visible) = asks[0];
    assert_eq!(
        visible,
        2 * QUANTITY_SCALE,
        "public depth shows display_qty"
    );

    // top_ask_quantity uses the full remaining for internal use.
    assert_eq!(
        book.top_ask_quantity(),
        10 * QUANTITY_SCALE,
        "internal callers see full remaining"
    );
}

/// Iceberg: the hidden reserve is still matchable. A large taker consumes
/// through it in one sweep (v1 behavior).
#[test]
fn test_iceberg_hidden_reserve_is_matchable() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 100 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    // Maker posts iceberg ask: 10 BTC total, display 2.
    e.process(post_iceberg_ask(maker.clone(), 100, 10, 2, 1), 3);

    // Taker sweeps 10 BTC — should fill the whole iceberg.
    let responses = e.process(post_bid(taker, 100, 10, 1), 4);

    let filled: u64 = responses
        .iter()
        .filter_map(|r| {
            if let Response::OrderFilled(f) = r {
                Some(f.quantity)
            } else {
                None
            }
        })
        .sum();
    assert_eq!(
        filled,
        10 * QUANTITY_SCALE,
        "taker matched through the hidden reserve"
    );
}

/// After a partial fill of an iceberg order, the visible depth remains
/// at display_quantity (until the remaining reserve drops below).
#[test]
fn test_iceberg_visible_refills_after_partial_fill() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 100 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    // Iceberg: 10 total, display 3.
    e.process(post_iceberg_ask(maker.clone(), 100, 10, 3, 1), 3);
    // Small taker: fills 1 BTC out of 10. Iceberg remaining now 9.
    e.process(post_bid(taker, 100, 1, 1), 4);

    let book = e.order_books.get(&btc_usdc()).unwrap();
    let (_price, visible) = book.depth_asks(1)[0];
    assert_eq!(
        visible,
        3 * QUANTITY_SCALE,
        "visible refills to display size after partial fill"
    );
    // Full remaining is 9.
    assert_eq!(book.top_ask_quantity(), 9 * QUANTITY_SCALE);
}

/// When remaining drops below display, visible = remaining.
#[test]
fn test_iceberg_visible_tail_below_display() {
    let mut e = engine();
    let maker = user(1);
    let taker = user(2);

    e.process(deposit(maker.clone(), btc(), 100 * QUANTITY_SCALE), 1);
    e.process(deposit(taker.clone(), usdc(), 100_000 * PRICE_SCALE), 2);

    // Iceberg: 5 total, display 3.
    e.process(post_iceberg_ask(maker.clone(), 100, 5, 3, 1), 3);
    // Taker fills 3.
    e.process(post_bid(taker, 100, 3, 1), 4);

    let book = e.order_books.get(&btc_usdc()).unwrap();
    let (_price, visible) = book.depth_asks(1)[0];
    // Remaining is 2, display is 3, so visible = min(3, 2) = 2.
    assert_eq!(visible, 2 * QUANTITY_SCALE);
}

/// Validation: display_quantity = 0 is rejected.
#[test]
fn test_iceberg_zero_display_rejected() {
    let mut e = engine();
    let maker = user(1);
    e.process(deposit(maker.clone(), btc(), 10 * QUANTITY_SCALE), 1);

    let req = Request::PostOrder(PostOrderRequest {
        user: maker,
        market: btc_usdc(),
        side: OrderSide::Ask,
        order_type: OrderType::GoodTillCanceled,
        price: 100 * PRICE_SCALE,
        quantity: 5 * QUANTITY_SCALE,
        nonce: 1,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: Some(0),
    });
    let responses = e.process(req, 2);
    let err = responses.iter().find_map(|r| {
        if let Response::Error(e) = r {
            Some(e.code)
        } else {
            None
        }
    });
    assert_eq!(err, Some(ErrorCode::InvalidDisplayQuantity));
}

/// Validation: display > quantity is rejected.
#[test]
fn test_iceberg_display_larger_than_quantity_rejected() {
    let mut e = engine();
    let maker = user(1);
    e.process(deposit(maker.clone(), btc(), 100 * QUANTITY_SCALE), 1);

    let req = Request::PostOrder(PostOrderRequest {
        user: maker,
        market: btc_usdc(),
        side: OrderSide::Ask,
        order_type: OrderType::GoodTillCanceled,
        price: 100 * PRICE_SCALE,
        quantity: 5 * QUANTITY_SCALE,
        nonce: 1,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: Some(10 * QUANTITY_SCALE),
    });
    let responses = e.process(req, 2);
    let err = responses.iter().find_map(|r| {
        if let Response::Error(e) = r {
            Some(e.code)
        } else {
            None
        }
    });
    assert_eq!(err, Some(ErrorCode::InvalidDisplayQuantity));
}

/// Validation: iceberg on IOC is rejected (would-not-rest is meaningless).
#[test]
fn test_iceberg_on_ioc_rejected() {
    let mut e = engine();
    let maker = user(1);
    e.process(deposit(maker.clone(), btc(), 100 * QUANTITY_SCALE), 1);

    let req = Request::PostOrder(PostOrderRequest {
        user: maker,
        market: btc_usdc(),
        side: OrderSide::Ask,
        order_type: OrderType::ImmediateOrCancel,
        price: 100 * PRICE_SCALE,
        quantity: 5 * QUANTITY_SCALE,
        nonce: 1,
        client_order_id: None,
        signature: vec![0u8; 65],
        stp: Default::default(),
        min_quantity: None,
        display_quantity: Some(2 * QUANTITY_SCALE),
    });
    let responses = e.process(req, 2);
    let err = responses.iter().find_map(|r| {
        if let Response::Error(e) = r {
            Some(e.code)
        } else {
            None
        }
    });
    assert_eq!(err, Some(ErrorCode::InvalidDisplayQuantity));
}
