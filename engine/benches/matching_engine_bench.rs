use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use engine::MatchingEngine;
use types::{
    AssetId, CancelOrderRequest, DepositRequest, FeeConfig, Market, MarketId, OrderSide, OrderType,
    PostOrderRequest, Request, Response, UserId, PRICE_SCALE, QUANTITY_SCALE,
};

const MARKET_BASES: &[&str] = &[
    "BTC", "ETH", "SOL", "AVAX", "LINK", "UNI", "ARB", "OP", "AAVE", "MATIC",
];

fn uid(i: u32) -> UserId {
    let mut a = [0u8; 20];
    a[16] = (i >> 24) as u8;
    a[17] = (i >> 16) as u8;
    a[18] = (i >> 8) as u8;
    a[19] = i as u8;
    UserId(a)
}

fn mkid(base: &str) -> MarketId {
    MarketId::new(base, "USDC")
}

fn make_market_fees(base: &str, maker_bps: i64, taker_bps: i64) -> Market {
    Market {
        id: mkid(base),
        base: AssetId::from_str(base),
        quote: AssetId::from_str("USDC"),
        max_orders: 10_000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: maker_bps,
        taker_fee_bps: taker_bps,
    }
}

fn make_market(base: &str) -> Market {
    make_market_fees(base, -1, 5)
}

fn dep_req(user: UserId, asset: AssetId, amount: u64, idx: u32) -> Request {
    let mut h = [0u8; 32];
    h[28] = (idx >> 24) as u8;
    h[29] = (idx >> 16) as u8;
    h[30] = (idx >> 8) as u8;
    h[31] = idx as u8;
    Request::Deposit(DepositRequest {
        user,
        asset,
        amount,
        l1_tx_hash: h,
    })
}

fn post_req(
    user: UserId,
    market: MarketId,
    side: OrderSide,
    otype: OrderType,
    price: u64,
    qty: u64,
    nonce: u64,
) -> Request {
    Request::PostOrder(PostOrderRequest {
        user,
        market,
        side,
        order_type: otype,
        price,
        quantity: qty,
        nonce,
        client_order_id: None,
        signature: vec![],
    })
}

fn cancel_req(user: UserId, order_id: u64, nonce: u64) -> Request {
    Request::CancelOrder(CancelOrderRequest {
        user,
        order_id: Some(order_id),
        client_order_id: None,
        nonce,
        signature: vec![],
    })
}

fn setup_realistic_mm() -> (MatchingEngine, Vec<Request>) {
    let taker = uid(0);
    let usdc = AssetId::from_str("USDC");
    let large: u64 = 1_000_000_000 * PRICE_SCALE;
    let num_mms: u32 = 50;

    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    for base in MARKET_BASES {
        engine.add_market(make_market(base));
    }

    let mut dep_idx: u32 = 0;

    engine.process(dep_req(taker.clone(), usdc.clone(), large, dep_idx), 0);
    dep_idx += 1;
    for base in MARKET_BASES {
        engine.process(
            dep_req(taker.clone(), AssetId::from_str(base), large, dep_idx),
            0,
        );
        dep_idx += 1;
    }

    for mm_i in 1..=num_mms {
        let mm = uid(mm_i);
        engine.process(dep_req(mm.clone(), usdc.clone(), large, dep_idx), 0);
        dep_idx += 1;
        for base in MARKET_BASES {
            engine.process(
                dep_req(mm.clone(), AssetId::from_str(base), large, dep_idx),
                0,
            );
            dep_idx += 1;
        }
    }

    let mut nonces = vec![0u64; (num_mms + 1) as usize];
    let mut initial_orders: Vec<(u32, usize, OrderSide, u64)> = Vec::with_capacity(2000);

    for mm_i in 1..=num_mms {
        let mm = uid(mm_i);
        for (mi, base) in MARKET_BASES.iter().enumerate() {
            let mid = mkid(base);
            for slot in 0u64..2 {
                nonces[mm_i as usize] += 1;
                let bid_price = (1_000 + mm_i as u64 * 2 + slot) * PRICE_SCALE;
                let resps = engine.process(
                    post_req(
                        mm.clone(),
                        mid.clone(),
                        OrderSide::Bid,
                        OrderType::GoodTillCanceled,
                        bid_price,
                        QUANTITY_SCALE,
                        nonces[mm_i as usize],
                    ),
                    1,
                );
                for r in &resps {
                    if let Response::OrderPosted(p) = r {
                        initial_orders.push((mm_i, mi, OrderSide::Bid, p.order_id));
                    }
                }

                nonces[mm_i as usize] += 1;
                let ask_price = (1_200 + mm_i as u64 * 2 + slot) * PRICE_SCALE;
                let resps = engine.process(
                    post_req(
                        mm.clone(),
                        mid.clone(),
                        OrderSide::Ask,
                        OrderType::GoodTillCanceled,
                        ask_price,
                        QUANTITY_SCALE,
                        nonces[mm_i as usize],
                    ),
                    1,
                );
                for r in &resps {
                    if let Response::OrderPosted(p) = r {
                        initial_orders.push((mm_i, mi, OrderSide::Ask, p.order_id));
                    }
                }
            }
        }
    }

    let num_initial = initial_orders.len();
    let mut requests: Vec<Request> = Vec::with_capacity(10_000);

    for i in 0usize..4_900 {
        let (mm_i, mi, side, order_id) = initial_orders[i % num_initial];
        let mm = uid(mm_i);
        let mid = mkid(MARKET_BASES[mi]);

        nonces[mm_i as usize] += 1;
        requests.push(cancel_req(mm.clone(), order_id, nonces[mm_i as usize]));

        let new_price = match side {
            OrderSide::Bid => (990 + (i as u64 % 20)) * PRICE_SCALE,
            OrderSide::Ask => (1_210 + (i as u64 % 20)) * PRICE_SCALE,
        };
        nonces[mm_i as usize] += 1;
        requests.push(post_req(
            mm,
            mid,
            side,
            OrderType::GoodTillCanceled,
            new_price,
            QUANTITY_SCALE,
            nonces[mm_i as usize],
        ));
    }

    for i in 0usize..200 {
        let mid = mkid(MARKET_BASES[i % MARKET_BASES.len()]);
        nonces[0] += 1;
        requests.push(post_req(
            taker.clone(),
            mid,
            OrderSide::Bid,
            OrderType::ImmediateOrCancel,
            2_000 * PRICE_SCALE,
            QUANTITY_SCALE,
            nonces[0],
        ));
    }

    (engine, requests)
}

fn setup_post_gtc() -> (MatchingEngine, Request) {
    let mid = mkid("BTC");
    let usdc = AssetId::from_str("USDC");
    let btc = AssetId::from_str("BTC");
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    engine.add_market(make_market("BTC"));

    for i in 1u32..=50 {
        let u = uid(i);
        engine.process(
            dep_req(u.clone(), usdc.clone(), 1_000_000 * PRICE_SCALE, i),
            0,
        );
        engine.process(
            dep_req(u.clone(), btc.clone(), 1_000_000 * QUANTITY_SCALE, 100 + i),
            0,
        );
        engine.process(
            post_req(
                u.clone(),
                mid.clone(),
                OrderSide::Bid,
                OrderType::GoodTillCanceled,
                (90_000 - i as u64 * 10) * PRICE_SCALE,
                QUANTITY_SCALE,
                1,
            ),
            1,
        );
        engine.process(
            post_req(
                u.clone(),
                mid.clone(),
                OrderSide::Ask,
                OrderType::GoodTillCanceled,
                (110_000 + i as u64 * 10) * PRICE_SCALE,
                QUANTITY_SCALE,
                2,
            ),
            1,
        );
    }

    let poster = uid(99);
    engine.process(
        dep_req(poster.clone(), usdc.clone(), 1_000_000 * PRICE_SCALE, 200),
        0,
    );
    let req = post_req(
        poster,
        mid,
        OrderSide::Bid,
        OrderType::GoodTillCanceled,
        80_000 * PRICE_SCALE,
        QUANTITY_SCALE,
        1,
    );
    (engine, req)
}

fn setup_cancel() -> (MatchingEngine, Request) {
    let mid = mkid("BTC");
    let usdc = AssetId::from_str("USDC");
    let btc = AssetId::from_str("BTC");
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    engine.add_market(make_market("BTC"));

    let u = uid(1);
    engine.process(
        dep_req(u.clone(), usdc.clone(), 1_000_000_000 * PRICE_SCALE, 0),
        0,
    );
    engine.process(
        dep_req(u.clone(), btc.clone(), 1_000_000_000 * QUANTITY_SCALE, 1),
        0,
    );

    let mut cancel_id = 0u64;
    for i in 1u64..=100 {
        let resps = engine.process(
            post_req(
                u.clone(),
                mid.clone(),
                OrderSide::Bid,
                OrderType::GoodTillCanceled,
                (50_000 + i * 10) * PRICE_SCALE,
                QUANTITY_SCALE,
                i,
            ),
            1,
        );
        if i == 50 {
            for r in &resps {
                if let Response::OrderPosted(p) = r {
                    cancel_id = p.order_id;
                }
            }
        }
    }
    for i in 101u64..=200 {
        engine.process(
            post_req(
                u.clone(),
                mid.clone(),
                OrderSide::Ask,
                OrderType::GoodTillCanceled,
                (150_000 + (i - 100) * 10) * PRICE_SCALE,
                QUANTITY_SCALE,
                i,
            ),
            1,
        );
    }

    let req = cancel_req(u, cancel_id, 201);
    (engine, req)
}

fn setup_fill() -> (MatchingEngine, Request) {
    let mid = mkid("BTC");
    let usdc = AssetId::from_str("USDC");
    let btc = AssetId::from_str("BTC");
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    engine.add_market(make_market("BTC"));

    let maker = uid(1);
    let taker = uid(2);
    engine.process(
        dep_req(maker.clone(), btc.clone(), 1_000_000 * QUANTITY_SCALE, 0),
        0,
    );
    engine.process(
        post_req(
            maker,
            mid.clone(),
            OrderSide::Ask,
            OrderType::GoodTillCanceled,
            100_000 * PRICE_SCALE,
            QUANTITY_SCALE,
            1,
        ),
        1,
    );
    engine.process(
        dep_req(taker.clone(), usdc.clone(), 1_000_000 * PRICE_SCALE, 1),
        0,
    );
    let req = post_req(
        taker,
        mid,
        OrderSide::Bid,
        OrderType::ImmediateOrCancel,
        110_000 * PRICE_SCALE,
        QUANTITY_SCALE,
        1,
    );
    (engine, req)
}

fn setup_fok_rollback() -> (MatchingEngine, Request) {
    let mid = mkid("BTC");
    let usdc = AssetId::from_str("USDC");
    let btc = AssetId::from_str("BTC");
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    engine.add_market(make_market("BTC"));

    let maker = uid(1);
    let taker = uid(2);
    engine.process(
        dep_req(maker.clone(), btc.clone(), 1_000_000 * QUANTITY_SCALE, 0),
        0,
    );
    engine.process(
        post_req(
            maker,
            mid.clone(),
            OrderSide::Ask,
            OrderType::GoodTillCanceled,
            200_000 * PRICE_SCALE,
            10 * QUANTITY_SCALE,
            1,
        ),
        1,
    );
    engine.process(
        dep_req(taker.clone(), usdc.clone(), 1_000_000 * PRICE_SCALE, 1),
        0,
    );
    let req = post_req(
        taker,
        mid,
        OrderSide::Bid,
        OrderType::FillOrKill,
        50_000 * PRICE_SCALE,
        QUANTITY_SCALE,
        1,
    );
    (engine, req)
}

fn setup_hft_nonce() -> (MatchingEngine, Vec<Request>) {
    let mid = mkid("BTC");
    let usdc = AssetId::from_str("USDC");
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    engine.add_market(make_market("BTC"));

    let u = uid(1);
    engine.process(
        dep_req(u.clone(), usdc.clone(), 1_000_000 * PRICE_SCALE, 0),
        0,
    );

    let nonces: [u64; 20] = [
        20, 1, 15, 3, 8, 11, 2, 18, 5, 14, 7, 17, 4, 12, 9, 19, 6, 16, 10, 13,
    ];
    let requests: Vec<Request> = nonces
        .iter()
        .enumerate()
        .map(|(i, &nonce)| {
            post_req(
                u.clone(),
                mid.clone(),
                OrderSide::Bid,
                OrderType::GoodTillCanceled,
                (80_000 + i as u64) * PRICE_SCALE,
                QUANTITY_SCALE,
                nonce,
            )
        })
        .collect();

    (engine, requests)
}

fn setup_credit_auto_cancel() -> (MatchingEngine, Request) {
    let mid = mkid("BTC");
    let usdc = AssetId::from_str("USDC");
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    engine.add_market(make_market("BTC"));

    let mm = uid(1);
    engine.set_credit_ratio(mm.clone(), 1.0);
    engine.process(
        dep_req(mm.clone(), usdc.clone(), 1_000_000 * PRICE_SCALE, 0),
        0,
    );

    for i in 1u64..=19 {
        engine.process(
            post_req(
                mm.clone(),
                mid.clone(),
                OrderSide::Bid,
                OrderType::GoodTillCanceled,
                50_000 * PRICE_SCALE,
                QUANTITY_SCALE,
                i,
            ),
            i,
        );
    }

    let req = post_req(
        mm,
        mid,
        OrderSide::Bid,
        OrderType::GoodTillCanceled,
        60_000 * PRICE_SCALE,
        QUANTITY_SCALE,
        20,
    );
    (engine, req)
}

fn setup_fill_with_fees(maker_bps: i64, taker_bps: i64) -> (MatchingEngine, Request) {
    let mid = mkid("BTC");
    let usdc = AssetId::from_str("USDC");
    let btc = AssetId::from_str("BTC");
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);
    engine.add_market(make_market_fees("BTC", maker_bps, taker_bps));

    let maker = uid(1);
    let taker = uid(2);
    engine.process(
        dep_req(maker.clone(), btc.clone(), 1_000_000 * QUANTITY_SCALE, 0),
        0,
    );
    engine.process(
        post_req(
            maker,
            mid.clone(),
            OrderSide::Ask,
            OrderType::GoodTillCanceled,
            100_000 * PRICE_SCALE,
            QUANTITY_SCALE,
            1,
        ),
        1,
    );
    engine.process(
        dep_req(taker.clone(), usdc.clone(), 1_000_000 * PRICE_SCALE, 1),
        0,
    );
    let req = post_req(
        taker,
        mid,
        OrderSide::Bid,
        OrderType::ImmediateOrCancel,
        110_000 * PRICE_SCALE,
        QUANTITY_SCALE,
        1,
    );
    (engine, req)
}

fn bench_realistic_mm_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_mm_workload");
    group.throughput(Throughput::Elements(10_000));
    group.bench_function("realistic_mm_workload", |b| {
        b.iter_batched(
            setup_realistic_mm,
            |(mut engine, requests)| {
                for req in requests {
                    black_box(engine.process(black_box(req), 1));
                }
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_post_order_gtc(c: &mut Criterion) {
    c.bench_function("post_order_gtc", |b| {
        b.iter_batched(
            setup_post_gtc,
            |(mut engine, req)| black_box(engine.process(black_box(req), 2)),
            BatchSize::PerIteration,
        );
    });
}

fn bench_cancel_order(c: &mut Criterion) {
    c.bench_function("cancel_order", |b| {
        b.iter_batched(
            setup_cancel,
            |(mut engine, req)| black_box(engine.process(black_box(req), 2)),
            BatchSize::PerIteration,
        );
    });
}

fn bench_fill_order(c: &mut Criterion) {
    c.bench_function("fill_order", |b| {
        b.iter_batched(
            setup_fill,
            |(mut engine, req)| black_box(engine.process(black_box(req), 2)),
            BatchSize::PerIteration,
        );
    });
}

fn bench_fok_rollback(c: &mut Criterion) {
    c.bench_function("fok_rollback", |b| {
        b.iter_batched(
            setup_fok_rollback,
            |(mut engine, req)| black_box(engine.process(black_box(req), 2)),
            BatchSize::PerIteration,
        );
    });
}

fn bench_hft_nonce_window(c: &mut Criterion) {
    let mut group = c.benchmark_group("hft_nonce_window");
    group.throughput(Throughput::Elements(20));
    group.bench_function("hft_nonce_window", |b| {
        b.iter_batched(
            setup_hft_nonce,
            |(mut engine, requests)| {
                for req in requests {
                    black_box(engine.process(black_box(req), 1));
                }
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_credit_auto_cancel(c: &mut Criterion) {
    c.bench_function("credit_auto_cancel", |b| {
        b.iter_batched(
            setup_credit_auto_cancel,
            |(mut engine, req)| black_box(engine.process(black_box(req), 20)),
            BatchSize::PerIteration,
        );
    });
}

fn bench_fee_calculation_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("fee_calculation_overhead");
    group.bench_function("with_fees", |b| {
        b.iter_batched(
            || setup_fill_with_fees(-1, 5),
            |(mut engine, req)| black_box(engine.process(black_box(req), 2)),
            BatchSize::PerIteration,
        );
    });
    group.bench_function("zero_fees", |b| {
        b.iter_batched(
            || setup_fill_with_fees(0, 0),
            |(mut engine, req)| black_box(engine.process(black_box(req), 2)),
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_realistic_mm_workload,
    bench_post_order_gtc,
    bench_cancel_order,
    bench_fill_order,
    bench_fok_rollback,
    bench_hft_nonce_window,
    bench_credit_auto_cancel,
    bench_fee_calculation_overhead,
);
criterion_main!(benches);
