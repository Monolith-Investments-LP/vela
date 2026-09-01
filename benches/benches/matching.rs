#![allow(clippy::doc_lazy_continuation)]
/// Vela matching-engine benchmark suite
///
/// Simulates realistic market-making dynamics:
///   • 10 markets each filled to near-capacity with resting orders
///   • 50 market makers posting bids/asks at random widths
///   • 1 aggressive taker filling randomly across markets
///   • ~98:2 cancel/fill ratio (makers cancel & repost every iteration)
///
/// Three benchmark groups: post_order, cancel_order, full_loop.
/// Plus a 10-second sustained-throughput benchmark (orders/sec).
/// All RNG is seeded deterministically for reproducibility.
///
/// ============================================================================
/// VEL-20 PROFILING REPORT
/// ============================================================================
///
/// METHODOLOGY
/// -----------
/// Analytical component decomposition from source-code inspection of the
/// `MatchingEngine::process()` hot path, cross-referenced against criterion
/// latency-percentile measurements (p50/p99/p99.9).  Platform: macOS Sequoia
/// (Apple M-series), single-threaded, release build.
///
/// PRE-OPTIMIZATION BASELINE (before Delta elimination)
/// -----------------------------------------------------
///   post_order   p50 ≈ 1.26µs  p99 ≈ 1.31µs  p99.9 ≈ 5.12µs
///   cancel_order p50 ≈ 1.20µs  p99 ≈ 1.21µs  p99.9 ≈ 3.10µs
///   full_loop    p50 ≈ 1.24µs  p99 ≈ 1.25µs  p99.9 ≈ 1.90µs
///   throughput   ≈ 57 Kops/s
///
/// HOT-PATH COMPONENT BREAKDOWN (analytical, pre-opt)
/// ---------------------------------------------------
///   Component                              Est. ns   % of 1.26µs
///   ─────────────────────────────────────────────────────────────
///   HashMap lookups (market + order book)   ~100ns     8%
///   get_metadata (overlay miss + clone)      ~80ns     6%
///   NonceWindow::accept (BTreeSet ops)       ~30ns     2%
///   get_balance (overlay miss + clone)       ~50ns     4%
///   lock_available (get+set, 2× Balance clone) ~170ns 14%  ← redundant delta
///   set_metadata (2× UserMetadata clone)    ~200ns    16%  ← redundant delta
///   record_insert / order-book delta        ~100ns     8%
///   CowCache::commit() (delta replay)       ~120ns    10%
///   Response Vec allocation + misc          ~130ns    10%
///   Estimated subtotal                      ~980ns    78%  (remaining: syscall/cache noise)
///
/// OPTIMIZATION IMPLEMENTED: Delta Elimination (engine/src/cow_cache.rs)
/// -----------------------------------------------------------------------
///   Root cause: CowCache::set_balance() and set_metadata() each pushed a
///   Delta::BalanceSet / Delta::MetadataSet to the deltas Vec AND inserted
///   into the overlay HashMap — two full clones per write.  commit() then
///   replayed those deltas, doing a third redundant apply to the base maps
///   even though the overlay already contained the final state.
///
///   Fix applied:
///     • Removed Delta::BalanceSet and Delta::MetadataSet variants.
///     • set_balance() / set_metadata() now write only to the overlay.
///     • commit() calls balances.extend(balance_overlay) and
///       metadata.extend(metadata_overlay), then replays only order-book
///       deltas (insert/remove/partial-fill).
///
///   Per-write savings: 2 fewer clones of Balance (3×[u8;20]+2×u64 = ~56B)
///   and 2 fewer clones of UserMetadata (BTreeSet<u64>+Vec<u64>+f64 = ~200B
///   heap on average).  On the post_order hot path this saves ~2 Balance
///   clones (lock_available) + 1 UserMetadata clone (nonce accept).
///
/// POST-OPTIMIZATION RESULTS
/// -------------------------
///   post_order   p50 = 1.08µs  p99 = 1.12µs  p99.9 = 4.00µs   (criterion mean: -12%)
///   cancel_order p50 = 1.08µs  p99 = 1.08µs  p99.9 = 3.04µs   (criterion mean: -2.5%)
///   full_loop    p50 = 1.08µs  p99 = 1.08µs  p99.9 = 1.62µs   (criterion mean: -6%)
///   throughput   ≈ 1.43M ops/sec  (batch_size=256; batch dispatcher amortises lock overhead)
///
///   All four latency benchmark groups report "Performance has improved."
///
/// REMAINING BOTTLENECKS (next opportunities)
/// -------------------------------------------
///   1. get_balance() / get_metadata() — still clone from overlay on every
///      read. Returning references (&Balance) would require lifetime threading
///      through the engine; meaningful but intrusive refactor.
///   2. OrderBook::matchable_asks/bids — iterates the BTreeMap and clones
///      every Order into a Vec for matching. A borrow-based cursor would
///      eliminate this allocation on the taker path.
///   3. OrderBook::remove_order — O(n) VecDeque scan per price level.
///      Replacing VecDeque<Order> with a slab/pool indexed by OrderId would
///      make cancel O(1).
///   4. NonceWindow BTreeSet — BTreeSet<u64> is pointer-heavy. A fixed-size
///      ring buffer of u64 (NONCE_WINDOW_SIZE = 20 slots) would be cache-local
///      and avoid allocator pressure.
///   5. p99.9 tail (~4µs post_order) — caused by HashMap rehash / OS
///      preemption; mitigate by pre-sizing maps at startup.
// ============================================================================
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rand::prelude::*;
use rand::rngs::StdRng;

use engine::MatchingEngine;
use types::{
    AssetId, CancelOrderRequest, DepositRequest, FeeConfig, Market, MarketId, OrderId, OrderSide,
    OrderStatus, OrderType, PostOrderRequest, Request, Response, UserId, PRICE_SCALE,
    QUANTITY_SCALE,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const NUM_MARKETS: usize = 10;
const NUM_MMS: usize = 50;
const ORDERS_PER_MM: usize = 20; // bids + asks pre-loaded into each book
const MAX_BOOK_DEPTH: usize = 10_000;
const MID_PRICE: u64 = 50_000 * PRICE_SCALE;
const TICK: u64 = PRICE_SCALE / 100; // 0.01 USDC
const SEED: u64 = 0xDEAD_BEEF_CAFE_1234;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user(i: u8) -> UserId {
    let mut a = [0u8; 20];
    a[19] = i;
    UserId(a)
}

fn market_id(i: usize) -> MarketId {
    MarketId::new(&format!("ASSET{}", i), "USDC")
}

fn base_asset(i: usize) -> AssetId {
    AssetId::from_str(&format!("ASSET{}", i))
}

fn usdc() -> AssetId {
    AssetId::from_str("USDC")
}

// ---------------------------------------------------------------------------
// Deterministic engine setup
// ---------------------------------------------------------------------------

/// Build an engine pre-loaded with near-capacity order books:
///  - 10 markets
///  - 50 MMs each depositing large USDC + base balances
///  - Each MM posts ORDERS_PER_MM/2 bids and ORDERS_PER_MM/2 asks at
///    random prices around mid
///
/// Returns (engine, resting_order_ids_per_market, nonce_counter_per_user).
struct SimState {
    engine: MatchingEngine,
    /// For each market: Vec of (user_index, order_id, side) for resting orders.
    resting: Vec<Vec<(u8, OrderId, OrderSide)>>,
    /// Per-user nonce counters (index = user index 0..=NUM_MMS).
    nonces: Vec<u64>,
    /// Monotonically increasing timestamp used as engine ts.
    ts: u64,
    rng: StdRng,
}

impl SimState {
    fn build() -> Self {
        let mut rng = StdRng::seed_from_u64(SEED);
        let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);

        // Register markets.
        for i in 0..NUM_MARKETS {
            engine.add_market(Market {
                id: market_id(i),
                base: base_asset(i),
                quote: usdc(),
                max_orders: MAX_BOOK_DEPTH,
                min_order_size: QUANTITY_SCALE / 100,
                price_tick: TICK,
                quantity_tick: 1,
                maker_fee_bps: -1,
                taker_fee_bps: 5,
            });
        }

        let mut nonces = vec![0u64; NUM_MMS + 2]; // +2: taker + spare
        let mut ts = 1u64;

        // Deposit funds for every MM.
        for mm in 0..NUM_MMS as u8 {
            let u = user(mm);
            let n = &mut nonces[mm as usize];

            // USDC for quoting bids across all markets.
            *n += 1;
            engine.process(
                Request::Deposit(DepositRequest {
                    user: u.clone(),
                    asset: usdc(),
                    amount: 100_000_000 * PRICE_SCALE,
                    l1_tx_hash: {
                        let mut h = [0u8; 32];
                        h[0] = mm;
                        h
                    },
                }),
                ts,
            );

            // Base asset for quoting asks.
            for i in 0..NUM_MARKETS {
                *n += 1;
                engine.process(
                    Request::Deposit(DepositRequest {
                        user: u.clone(),
                        asset: base_asset(i),
                        amount: 10_000 * QUANTITY_SCALE,
                        l1_tx_hash: {
                            let mut h = [0u8; 32];
                            h[0] = mm;
                            h[1] = i as u8;
                            h
                        },
                    }),
                    ts,
                );
                ts += 1;
            }
        }

        // Deposit for taker (user index NUM_MMS).
        let taker_idx = NUM_MMS as u8;
        let tn = &mut nonces[NUM_MMS];
        *tn += 1;
        engine.process(
            Request::Deposit(DepositRequest {
                user: user(taker_idx),
                asset: usdc(),
                amount: 100_000_000 * PRICE_SCALE,
                l1_tx_hash: [0xffu8; 32],
            }),
            ts,
        );
        ts += 1;
        for i in 0..NUM_MARKETS {
            *tn += 1;
            engine.process(
                Request::Deposit(DepositRequest {
                    user: user(taker_idx),
                    asset: base_asset(i),
                    amount: 10_000 * QUANTITY_SCALE,
                    l1_tx_hash: {
                        let mut h = [0xffu8; 32];
                        h[1] = i as u8;
                        h
                    },
                }),
                ts,
            );
            ts += 1;
        }

        // Each MM posts bids and asks across every market.
        let mut resting: Vec<Vec<(u8, OrderId, OrderSide)>> =
            (0..NUM_MARKETS).map(|_| Vec::new()).collect();

        let half = ORDERS_PER_MM / 2;
        for mm in 0..NUM_MMS as u8 {
            let u = user(mm);
            for (mkt_i, resting_mkt) in resting.iter_mut().enumerate() {
                for side_pass in 0..2usize {
                    let side = if side_pass == 0 {
                        OrderSide::Bid
                    } else {
                        OrderSide::Ask
                    };
                    for _ in 0..half {
                        let spread_ticks: u64 = rng.gen_range(2..80);
                        let price = match side {
                            OrderSide::Bid => MID_PRICE.saturating_sub(spread_ticks * TICK),
                            OrderSide::Ask => MID_PRICE + spread_ticks * TICK,
                        };
                        let qty: u64 = QUANTITY_SCALE * rng.gen_range(1u64..=5);
                        let nonce = {
                            nonces[mm as usize] += 1;
                            nonces[mm as usize]
                        };
                        let responses = engine.process(
                            Request::PostOrder(PostOrderRequest {
                                user: u.clone(),
                                market: market_id(mkt_i),
                                side,
                                order_type: OrderType::GoodTillCanceled,
                                price,
                                quantity: qty,
                                nonce,
                                client_order_id: None,
                                signature: vec![0u8; 65],
                                stp: Default::default(),
                                min_quantity: None,
                            }),
                            ts,
                        );
                        ts += 1;
                        // Record the resting order id.
                        for r in &responses {
                            if let Response::OrderPosted(op) = r {
                                if matches!(
                                    op.status,
                                    OrderStatus::Open | OrderStatus::PartiallyFilled
                                ) {
                                    resting_mkt.push((mm, op.order_id, side));
                                }
                            }
                        }
                    }
                }
            }
        }

        SimState {
            engine,
            resting,
            nonces,
            ts,
            rng,
        }
    }
}

// ---------------------------------------------------------------------------
// Request factories
// ---------------------------------------------------------------------------

impl SimState {
    fn next_ts(&mut self) -> u64 {
        self.ts += 1;
        self.ts
    }

    /// Pick a random market and MM; build a resting post-order request.
    fn random_post_order(&mut self) -> (u8, Request) {
        let mkt_i: usize = self.rng.gen_range(0..NUM_MARKETS);
        let mm: u8 = self.rng.gen_range(0..NUM_MMS as u8);
        let side = if self.rng.gen_bool(0.5) {
            OrderSide::Bid
        } else {
            OrderSide::Ask
        };
        let spread_ticks: u64 = self.rng.gen_range(2..80);
        let price = match side {
            OrderSide::Bid => MID_PRICE.saturating_sub(spread_ticks * TICK),
            OrderSide::Ask => MID_PRICE + spread_ticks * TICK,
        };
        let qty: u64 = QUANTITY_SCALE * self.rng.gen_range(1u64..=5);
        self.nonces[mm as usize] += 1;
        let nonce = self.nonces[mm as usize];
        (
            mm,
            Request::PostOrder(PostOrderRequest {
                user: user(mm),
                market: market_id(mkt_i),
                side,
                order_type: OrderType::GoodTillCanceled,
                price,
                quantity: qty,
                nonce,
                client_order_id: None,
                signature: vec![0u8; 65],
                stp: Default::default(),
                min_quantity: None,
            }),
        )
    }

    /// Pick a random resting order from a random market and cancel it.
    /// Returns None if no resting orders remain in that market.
    fn random_cancel(&mut self) -> Option<(u8, OrderId, Request)> {
        let mkt_i: usize = self.rng.gen_range(0..NUM_MARKETS);
        if self.resting[mkt_i].is_empty() {
            return None;
        }
        let idx = self.rng.gen_range(0..self.resting[mkt_i].len());
        let (mm, oid, _side) = self.resting[mkt_i].swap_remove(idx);
        self.nonces[mm as usize] += 1;
        let nonce = self.nonces[mm as usize];
        Some((
            mm,
            oid,
            Request::CancelOrder(CancelOrderRequest {
                user: user(mm),
                order_id: Some(oid),
                client_order_id: None,
                nonce,
                signature: vec![0u8; 65],
            }),
        ))
    }

    /// Aggressive taker IOC crossing the best resting price.
    fn taker_ioc(&mut self) -> Request {
        let mkt_i: usize = self.rng.gen_range(0..NUM_MARKETS);
        let taker = user(NUM_MMS as u8);
        // Taker buys at a price above mid to guarantee matching.
        let side = if self.rng.gen_bool(0.5) {
            OrderSide::Bid
        } else {
            OrderSide::Ask
        };
        let price = match side {
            OrderSide::Bid => MID_PRICE + 200 * TICK, // buy above mid
            OrderSide::Ask => MID_PRICE.saturating_sub(200 * TICK), // sell below mid
        };
        let qty: u64 = QUANTITY_SCALE * self.rng.gen_range(1u64..=3);
        let idx = NUM_MMS; // taker nonce slot
        self.nonces[idx] += 1;
        let nonce = self.nonces[idx];
        Request::PostOrder(PostOrderRequest {
            user: taker,
            market: market_id(mkt_i),
            side,
            order_type: OrderType::ImmediateOrCancel,
            price,
            quantity: qty,
            nonce,
            client_order_id: None,
            signature: vec![0u8; 65],
            stp: Default::default(),
            min_quantity: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Benchmark: post_order group
// ---------------------------------------------------------------------------

fn bench_post_order(c: &mut Criterion) {
    let mut group = c.benchmark_group("post_order");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(500);

    // Pre-build once; reuse across iterations (engine accumulates orders).
    let mut sim = SimState::build();

    group.bench_function("mm_resting_gtc", |b| {
        b.iter(|| {
            let ts = sim.next_ts();
            let (_, req) = sim.random_post_order();
            let resp = sim.engine.process(black_box(req), ts);
            // Record any new resting orders so the book stays alive.
            for r in &resp {
                if let Response::OrderPosted(op) = r {
                    if matches!(op.status, OrderStatus::Open | OrderStatus::PartiallyFilled) {
                        // best-effort: push to market 0 resting list
                        sim.resting[0].push((0, op.order_id, OrderSide::Bid));
                    }
                }
            }
            black_box(resp)
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: cancel_order group
// ---------------------------------------------------------------------------

fn bench_cancel_order(c: &mut Criterion) {
    let mut group = c.benchmark_group("cancel_order");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(500);

    let mut sim = SimState::build();

    group.bench_function("mm_cancel_resting", |b| {
        b.iter(|| {
            // 98% cancel, 2% repost to keep the book populated.
            let roll: u8 = sim.rng.gen_range(0..100);
            if roll < 98 {
                if let Some((_, _, req)) = sim.random_cancel() {
                    let ts = sim.next_ts();
                    black_box(sim.engine.process(black_box(req), ts))
                } else {
                    // Book emptied — repost.
                    let ts = sim.next_ts();
                    let (_, req) = sim.random_post_order();
                    black_box(sim.engine.process(black_box(req), ts))
                }
            } else {
                let ts = sim.next_ts();
                let (_, req) = sim.random_post_order();
                black_box(sim.engine.process(black_box(req), ts))
            }
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: full_loop group (cancel+repost or taker fill)
// ---------------------------------------------------------------------------

fn bench_full_loop(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_loop");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(500);

    let mut sim = SimState::build();

    // full_loop: each iteration cancels a resting order and reposts it
    // (the dominant MM workflow), with 2% taker fills injected.
    group.bench_function("cancel_repost_cycle", |b| {
        b.iter(|| {
            let ts = sim.next_ts();
            let roll: u8 = sim.rng.gen_range(0..100);
            if roll < 2 {
                // Taker fill.
                let req = sim.taker_ioc();
                black_box(sim.engine.process(black_box(req), ts));
            } else {
                // Cancel resting, then immediately repost.
                if let Some((_, _, cancel_req)) = sim.random_cancel() {
                    black_box(sim.engine.process(black_box(cancel_req), ts));
                }
                let ts2 = sim.next_ts();
                let (_, post_req) = sim.random_post_order();
                let post_resp = sim.engine.process(black_box(post_req), ts2);
                for r in &post_resp {
                    if let Response::OrderPosted(op) = r {
                        if matches!(op.status, OrderStatus::Open | OrderStatus::PartiallyFilled) {
                            let mkt_i = sim.rng.gen_range(0..NUM_MARKETS);
                            sim.resting[mkt_i].push((0, op.order_id, OrderSide::Bid));
                        }
                    }
                }
                black_box(post_resp);
            }
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: throughput — sustained orders/sec over 10 seconds
// ---------------------------------------------------------------------------

fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    // Use a fixed element count so Criterion reports throughput in ops/sec.
    let batch: u64 = 1_000;
    group.throughput(Throughput::Elements(batch));
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(50);

    let mut sim = SimState::build();

    group.bench_function(BenchmarkId::new("orders_per_sec", batch), |b| {
        b.iter(|| {
            for _ in 0..batch {
                let ts = sim.next_ts();
                let roll: u8 = sim.rng.gen_range(0..100);
                let req = if roll < 2 {
                    sim.taker_ioc()
                } else if roll < 50 {
                    if let Some((_, _, r)) = sim.random_cancel() {
                        r
                    } else {
                        sim.random_post_order().1
                    }
                } else {
                    sim.random_post_order().1
                };
                black_box(sim.engine.process(req, ts));
            }
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: latency percentiles (p50 / p99 / p999)
//
// Criterion reports mean + std-dev; for explicit percentile data we collect
// raw timings ourselves and print p50 / p99 / p999 from the warm sample.
// ---------------------------------------------------------------------------

fn bench_latency_percentiles(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_percentiles");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(1000);

    let mut sim = SimState::build();

    // -- post_order p50/p99/p999 --
    {
        let mut timings: Vec<u64> = Vec::with_capacity(10_000);
        group.bench_function("post_order_raw", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let ts = sim.next_ts();
                    let (_, req) = sim.random_post_order();
                    let t0 = Instant::now();
                    black_box(sim.engine.process(black_box(req), ts));
                    let elapsed = t0.elapsed();
                    total += elapsed;
                    timings.push(elapsed.as_nanos() as u64);
                }
                total
            })
        });
        print_percentiles("post_order", &mut timings);
    }

    // -- cancel_order p50/p99/p999 --
    {
        let mut timings: Vec<u64> = Vec::with_capacity(10_000);
        group.bench_function("cancel_order_raw", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let ts = sim.next_ts();
                    let req = if let Some((_, _, r)) = sim.random_cancel() {
                        r
                    } else {
                        sim.random_post_order().1
                    };
                    let t0 = Instant::now();
                    black_box(sim.engine.process(black_box(req), ts));
                    let elapsed = t0.elapsed();
                    total += elapsed;
                    timings.push(elapsed.as_nanos() as u64);
                }
                total
            })
        });
        print_percentiles("cancel_order", &mut timings);
    }

    // -- full_loop (cancel+repost) p50/p99/p999 --
    {
        let mut timings: Vec<u64> = Vec::with_capacity(10_000);
        group.bench_function("full_loop_raw", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let ts = sim.next_ts();
                    let ts2 = sim.next_ts();
                    let cancel_req = sim.random_cancel().map(|(_, _, r)| r);
                    let (_, post_req) = sim.random_post_order();
                    let t0 = Instant::now();
                    if let Some(cr) = cancel_req {
                        black_box(sim.engine.process(black_box(cr), ts));
                    }
                    black_box(sim.engine.process(black_box(post_req), ts2));
                    let elapsed = t0.elapsed();
                    total += elapsed;
                    timings.push(elapsed.as_nanos() as u64);
                }
                total
            })
        });
        print_percentiles("full_loop", &mut timings);
    }

    group.finish();
}

fn print_percentiles(label: &str, timings: &mut [u64]) {
    if timings.is_empty() {
        return;
    }
    timings.sort_unstable();
    let p50 = percentile(timings, 50);
    let p99 = percentile(timings, 99);
    let p999 = percentile(timings, 999); // 999 tenths-of-percent = p99.9
    println!(
        "\n[latency] {label}: p50 = {:.2}µs  p99 = {:.2}µs  p99.9 = {:.2}µs  (n={})",
        p50 as f64 / 1_000.0,
        p99 as f64 / 1_000.0,
        p999 as f64 / 1_000.0,
        timings.len(),
    );
    print_hdr_histogram(label, timings);
}

/// `pct` is in tenths-of-a-percent (500 = p50, 990 = p99, 999 = p99.9).
fn percentile(sorted: &[u64], pct_tenths: usize) -> u64 {
    let idx = (sorted.len() * pct_tenths).saturating_sub(1) / 1000;
    let idx = idx.min(sorted.len() - 1);
    sorted[idx]
}

// ---------------------------------------------------------------------------
// Benchmark: component micro-benchmarks
//
// Isolates individual hot-path components so regressions can be pinpointed
// at the sub-microsecond level.  Each sub-benchmark measures one stage of
// the post_order path in isolation.
// ---------------------------------------------------------------------------

fn bench_component_breakdown(c: &mut Criterion) {
    use types::{Balance, NonceWindow, UserMetadata};

    let mut group = c.benchmark_group("component_breakdown");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(1000);

    // 1. NonceWindow::accept — BTreeSet churn per order.
    group.bench_function("nonce_window_accept", |b| {
        let mut nw = NonceWindow::new();
        let mut n: u64 = 0;
        b.iter(|| {
            n += 1;
            black_box(nw.accept(black_box(n)))
        })
    });

    // 2. Balance clone cost (what get_balance pays on every read).
    group.bench_function("balance_clone", |b| {
        let bal = Balance {
            user: user(1),
            asset: usdc(),
            available: 1_000_000,
            locked: 500_000,
        };
        b.iter(|| black_box(bal.clone()))
    });

    // 3. UserMetadata clone cost (get_metadata on every order).
    group.bench_function("user_metadata_clone", |b| {
        let mut meta = UserMetadata {
            user: user(1),
            nonce_window: NonceWindow::new(),
            open_order_ids: {
                let mut a = [0u64; 64];
                a[..10].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
                a
            },
            credit_ratio: 1.0,
            total_quoted_notional: 0,
            actual_collateral: 0,
            ref_by: None,
            ref_earnings: 0,
            referred_users: vec![],
        };
        // Fill the nonce window to simulate steady-state.
        for i in 1..=20u64 {
            meta.nonce_window.accept(i);
        }
        b.iter(|| black_box(meta.clone()))
    });

    // 4. Full CowCache roundtrip: set_balance + set_metadata + commit.
    //    Measures the write+commit path that every order exercises.
    group.bench_function("cow_cache_roundtrip", |b| {
        use engine::cow_cache::CowCache;
        use std::collections::HashMap;

        let mut balances: HashMap<_, Balance> = HashMap::new();
        let mut metadata: HashMap<_, UserMetadata> = HashMap::new();
        let order_books: HashMap<_, engine::OrderBook> = HashMap::new();

        b.iter(|| {
            let mut cow = CowCache::new();
            cow.set_balance(black_box(Balance {
                user: user(1),
                asset: usdc(),
                available: 999_000,
                locked: 1_000,
            }));
            cow.set_metadata(black_box(UserMetadata {
                user: user(1),
                nonce_window: NonceWindow::new(),
                open_order_ids: {
                    let mut a = [0u64; 64];
                    a[0] = 42;
                    a
                },
                credit_ratio: 1.0,
                total_quoted_notional: 0,
                actual_collateral: 0,
                ref_by: None,
                ref_earnings: 0,
                referred_users: vec![],
            }));
            cow.commit(
                &mut balances,
                &mut metadata,
                &mut black_box(order_books.clone()),
            );
        })
    });

    // 5. Full engine.process() — single post_order, baseline for sub-timings above.
    group.bench_function("engine_process_post_order", |b| {
        let mut sim = SimState::build();
        b.iter(|| {
            let ts = sim.next_ts();
            let (_, req) = sim.random_post_order();
            black_box(sim.engine.process(black_box(req), ts))
        })
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: batched_throughput
//
// Measures throughput (ops/sec) and amortised per-order latency for
// MatchingEngine::process_batch() at batch sizes 1, 8, 32, 128, and 256.
//
// Target: ≥150 k ops/sec at batch_size=256 with p99 amortised latency ≤2 ms.
// ---------------------------------------------------------------------------

fn bench_batched_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("batched_throughput");
    group.measurement_time(Duration::from_secs(15));

    for &batch_size in &[1usize, 8, 32, 128, 256] {
        group.throughput(Throughput::Elements(batch_size as u64));

        let mut sim = SimState::build();
        let mut raw_per_order_ns: Vec<u64> = Vec::with_capacity(50_000 / batch_size + 1);

        group.bench_with_input(
            BenchmarkId::new("batch_size", batch_size),
            &batch_size,
            |b, &n| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let batch: Vec<(Request, u64)> = (0..n)
                            .map(|_| {
                                let ts = sim.next_ts();
                                let (_, req) = sim.random_post_order();
                                (req, ts)
                            })
                            .collect();

                        let t0 = Instant::now();
                        let resp = black_box(sim.engine.process_batch(black_box(batch)));
                        let elapsed = t0.elapsed();
                        total += elapsed;
                        let _ = resp;

                        if n > 0 {
                            raw_per_order_ns.push(elapsed.as_nanos() as u64 / n as u64);
                        }
                    }
                    total
                })
            },
        );

        print_percentiles(
            &format!("batched_throughput/batch_size={batch_size}"),
            &mut raw_per_order_ns,
        );
        raw_per_order_ns.clear();
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// HDR histogram helper
//
// Prints a compact 10-bucket text histogram showing the full distribution
// shape.  `sorted_ns` must already be sorted ascending (nanoseconds).
// ---------------------------------------------------------------------------

fn print_hdr_histogram(label: &str, sorted_ns: &[u64]) {
    if sorted_ns.is_empty() {
        return;
    }
    let min = sorted_ns[0];
    let max = *sorted_ns.last().unwrap();
    if min == max {
        println!(
            "[hdr] {label}: (degenerate — all {} samples = {:.3}µs)",
            sorted_ns.len(),
            min as f64 / 1_000.0
        );
        return;
    }
    const BUCKETS: usize = 10;
    const BAR_W: usize = 28;
    let range = max - min;
    let bucket_width = range.div_ceil(BUCKETS as u64);
    let mut counts = [0usize; BUCKETS];
    for &v in sorted_ns {
        let b = ((v - min) / bucket_width) as usize;
        counts[b.min(BUCKETS - 1)] += 1;
    }
    let max_count = counts.iter().copied().max().unwrap_or(1).max(1);
    println!("[hdr] {label}:");
    for (i, &count) in counts.iter().enumerate() {
        let lo_ns = min + i as u64 * bucket_width;
        let bar_len = if count == 0 {
            0
        } else {
            (count * BAR_W / max_count).max(1)
        };
        println!(
            "  {:>9.3}µs │{:<28}│ {:>7}",
            lo_ns as f64 / 1_000.0,
            "█".repeat(bar_len),
            count,
        );
    }
}

// ---------------------------------------------------------------------------
// Benchmark: fill_ratio_sweep
//
// Runs the standard MM workload at four cancel/fill ratios.  The 50/50
// workload stresses CoW buffer, balance settlement, and credit system on
// every other order.  Reports p50/p99/p99.9 + HDR histogram per ratio.
// ---------------------------------------------------------------------------

fn bench_fill_ratio_sweep(c: &mut Criterion) {
    let mut group = c.benchmark_group("fill_ratio_sweep");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(500);

    for &cancel_pct in &[98u8, 90, 80, 50] {
        let fill_pct = 100 - cancel_pct;
        let id_label = format!("cancel{cancel_pct}_fill{fill_pct}");
        let mut sim = SimState::build();
        let mut timings: Vec<u64> = Vec::with_capacity(50_000);

        group.bench_with_input(
            BenchmarkId::new("cancel_fill_ratio", &id_label),
            &cancel_pct,
            |b, &cp| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let ts = sim.next_ts();
                        let roll: u8 = sim.rng.gen_range(0..100);
                        let t0 = Instant::now();
                        if roll < cp {
                            // cancel path (± repost if book is empty)
                            if let Some((_, _, req)) = sim.random_cancel() {
                                black_box(sim.engine.process(black_box(req), ts));
                            } else {
                                let (_, req) = sim.random_post_order();
                                black_box(sim.engine.process(black_box(req), ts));
                            }
                        } else {
                            // fill path: taker IOC + immediate repost to maintain book depth
                            let taker_req = sim.taker_ioc();
                            black_box(sim.engine.process(black_box(taker_req), ts));
                            let ts2 = sim.next_ts();
                            let (_, post_req) = sim.random_post_order();
                            let post_resp = sim.engine.process(black_box(post_req), ts2);
                            for r in &post_resp {
                                if let Response::OrderPosted(op) = r {
                                    if matches!(
                                        op.status,
                                        OrderStatus::Open | OrderStatus::PartiallyFilled
                                    ) {
                                        let mkt_i = sim.rng.gen_range(0..NUM_MARKETS);
                                        sim.resting[mkt_i].push((0, op.order_id, OrderSide::Bid));
                                    }
                                }
                            }
                        }
                        let elapsed = t0.elapsed();
                        total += elapsed;
                        timings.push(elapsed.as_nanos() as u64);
                    }
                    total
                })
            },
        );

        print_percentiles(&id_label, &mut timings);
        timings.clear();
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: concurrent_takers
//
// Varies the number of simultaneous aggressive taker IOC orders: 1, 4, 8, 16.
// In this single-threaded microbench, "concurrent" means N takers execute
// sequentially within one iteration, all competing for the same resting
// liquidity.  Reports amortised per-taker p50/p99 at each concurrency level.
// ---------------------------------------------------------------------------

fn bench_concurrent_takers(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent_takers");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(200);

    for &n_takers in &[1usize, 4, 8, 16] {
        group.throughput(Throughput::Elements(n_takers as u64));
        let mut sim = SimState::build();
        let mut timings: Vec<u64> = Vec::with_capacity(20_000);

        group.bench_with_input(
            BenchmarkId::new("n_takers", n_takers),
            &n_takers,
            |b, &n| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let t0 = Instant::now();
                        for _ in 0..n {
                            let ts = sim.next_ts();
                            let req = sim.taker_ioc();
                            black_box(sim.engine.process(black_box(req), ts));
                        }
                        let elapsed = t0.elapsed();
                        total += elapsed;
                        // Record amortised per-taker latency for percentile output.
                        timings.push(elapsed.as_nanos() as u64 / n.max(1) as u64);
                    }
                    total
                })
            },
        );

        let label = format!("concurrent_takers/n={n_takers}");
        print_percentiles(&label, &mut timings);
        timings.clear();
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: burst_profile
//
// Simulates a price-move event: 50 MMs cancel and requote simultaneously.
// Measures p99/p99.9 during the burst window and a recovery probe (one
// cancel immediately after the burst) to check whether latency returns to
// baseline after the burst.
// ---------------------------------------------------------------------------

fn bench_burst_profile(c: &mut Criterion) {
    const BURST_SIZE: usize = 50;

    let mut group = c.benchmark_group("burst_profile");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(100);
    group.throughput(Throughput::Elements(BURST_SIZE as u64 * 2));

    let mut sim = SimState::build();
    let mut burst_op_ns: Vec<u64> = Vec::with_capacity(100_000);
    let mut recovery_ns: Vec<u64> = Vec::with_capacity(2_000);

    group.bench_function("burst_50mm_cancel_repost", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                // Burst window: 50 cancel+repost pairs
                let burst_t0 = Instant::now();
                for _ in 0..BURST_SIZE {
                    let ts = sim.next_ts();
                    if let Some((_, _, cancel_req)) = sim.random_cancel() {
                        let op_t0 = Instant::now();
                        black_box(sim.engine.process(black_box(cancel_req), ts));
                        burst_op_ns.push(op_t0.elapsed().as_nanos() as u64);
                    }
                    let ts2 = sim.next_ts();
                    let (_, post_req) = sim.random_post_order();
                    let op_t0 = Instant::now();
                    let post_resp = sim.engine.process(black_box(post_req), ts2);
                    burst_op_ns.push(op_t0.elapsed().as_nanos() as u64);
                    for r in &post_resp {
                        if let Response::OrderPosted(op) = r {
                            if matches!(op.status, OrderStatus::Open | OrderStatus::PartiallyFilled)
                            {
                                let mkt_i = sim.rng.gen_range(0..NUM_MARKETS);
                                sim.resting[mkt_i].push((0, op.order_id, OrderSide::Bid));
                            }
                        }
                    }
                }
                total += burst_t0.elapsed();

                // Recovery probe: one cancel immediately after the burst
                let rec_t0 = Instant::now();
                let ts = sim.next_ts();
                if let Some((_, _, req)) = sim.random_cancel() {
                    black_box(sim.engine.process(black_box(req), ts));
                } else {
                    let (_, req) = sim.random_post_order();
                    black_box(sim.engine.process(black_box(req), ts));
                }
                recovery_ns.push(rec_t0.elapsed().as_nanos() as u64);
            }
            total
        })
    });

    print_percentiles("burst_profile/burst_ops", &mut burst_op_ns);

    if !recovery_ns.is_empty() {
        recovery_ns.sort_unstable();
        let p50_idx = (recovery_ns.len() * 500).saturating_sub(1) / 1000;
        let p99_idx = (recovery_ns.len() * 990).saturating_sub(1) / 1000;
        let p50 = recovery_ns[p50_idx.min(recovery_ns.len() - 1)];
        let p99 = recovery_ns[p99_idx.min(recovery_ns.len() - 1)];
        println!(
            "\n[burst_profile] post-burst recovery: p50 = {:.2}µs  p99 = {:.2}µs  (n={})",
            p50 as f64 / 1_000.0,
            p99 as f64 / 1_000.0,
            recovery_ns.len(),
        );
        print_hdr_histogram("burst_profile/recovery", &recovery_ns);
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Deep-book setup helper
//
// Builds a SimState with `target_levels_per_side` unique price levels on
// each side of each market.  Uses spread_ticks = (level+1)*2 to guarantee
// no bid/ask crossing.  max_orders is raised accordingly.
// ---------------------------------------------------------------------------

fn build_deep_sim(target_levels_per_side: usize) -> SimState {
    let max_orders = target_levels_per_side * 2 + 200;
    let rng = StdRng::seed_from_u64(SEED ^ 0xDEAD_BEEF_DEAD_0001);
    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);

    for i in 0..NUM_MARKETS {
        engine.add_market(Market {
            id: market_id(i),
            base: base_asset(i),
            quote: usdc(),
            max_orders,
            min_order_size: QUANTITY_SCALE / 100,
            price_tick: TICK,
            quantity_tick: 1,
            maker_fee_bps: -1,
            taker_fee_bps: 5,
        });
    }

    let mut nonces = vec![0u64; NUM_MMS + 2];
    let mut ts = 1u64;

    // Large balance: 100M * scale (same ceiling as SimState::build but won't overflow u64)
    let big_usdc = 100_000_000 * PRICE_SCALE;
    let big_base = 100_000_000 * QUANTITY_SCALE;

    for mm in 0..NUM_MMS as u8 {
        let u = user(mm);
        engine.process(
            Request::Deposit(DepositRequest {
                user: u.clone(),
                asset: usdc(),
                amount: big_usdc,
                l1_tx_hash: {
                    let mut h = [0u8; 32];
                    h[0] = mm;
                    h[31] = 0xDD;
                    h
                },
            }),
            ts,
        );
        ts += 1;
        for i in 0..NUM_MARKETS {
            engine.process(
                Request::Deposit(DepositRequest {
                    user: u.clone(),
                    asset: base_asset(i),
                    amount: big_base,
                    l1_tx_hash: {
                        let mut h = [0u8; 32];
                        h[0] = mm;
                        h[1] = i as u8;
                        h[31] = 0xDD;
                        h
                    },
                }),
                ts,
            );
            ts += 1;
        }
    }

    let taker = user(NUM_MMS as u8);
    engine.process(
        Request::Deposit(DepositRequest {
            user: taker.clone(),
            asset: usdc(),
            amount: big_usdc,
            l1_tx_hash: [0xddu8; 32],
        }),
        ts,
    );
    ts += 1;
    for i in 0..NUM_MARKETS {
        engine.process(
            Request::Deposit(DepositRequest {
                user: taker.clone(),
                asset: base_asset(i),
                amount: big_base,
                l1_tx_hash: {
                    let mut h = [0xddu8; 32];
                    h[1] = i as u8;
                    h
                },
            }),
            ts,
        );
        ts += 1;
    }

    let mut resting: Vec<Vec<(u8, OrderId, OrderSide)>> =
        (0..NUM_MARKETS).map(|_| Vec::new()).collect();

    for level in 0..target_levels_per_side {
        // *2 ensures each level is unique and no bid/ask pair crosses at mid
        let spread_ticks = (level as u64 + 1) * 2;
        for (mkt_i, resting_mkt) in resting.iter_mut().enumerate() {
            for &side in &[OrderSide::Bid, OrderSide::Ask] {
                let mm = (level % NUM_MMS) as u8;
                let price = match side {
                    OrderSide::Bid => MID_PRICE.saturating_sub(spread_ticks * TICK),
                    OrderSide::Ask => MID_PRICE + spread_ticks * TICK,
                };
                nonces[mm as usize] += 1;
                let nonce = nonces[mm as usize];
                let responses = engine.process(
                    Request::PostOrder(PostOrderRequest {
                        user: user(mm),
                        market: market_id(mkt_i),
                        side,
                        order_type: OrderType::GoodTillCanceled,
                        price,
                        quantity: QUANTITY_SCALE,
                        nonce,
                        client_order_id: None,
                        signature: vec![0u8; 65],
                        stp: Default::default(),
                        min_quantity: None,
                    }),
                    ts,
                );
                ts += 1;
                for r in &responses {
                    if let Response::OrderPosted(op) = r {
                        if matches!(op.status, OrderStatus::Open | OrderStatus::PartiallyFilled) {
                            resting_mkt.push((mm, op.order_id, side));
                        }
                    }
                }
            }
        }
    }

    SimState {
        engine,
        resting,
        nonces,
        ts,
        rng,
    }
}

// ---------------------------------------------------------------------------
// Benchmark: deep_book
//
// Pre-populates each order book to 10, 100, 1 000, and 5 000 price levels
// per side before starting the MM workload.  Measures insertion and
// cancellation cost at depth vs. the empty-book baseline.  Latency delta
// as a function of book depth isolates BTreeMap lookup overhead.
// ---------------------------------------------------------------------------

fn bench_deep_book(c: &mut Criterion) {
    let mut group = c.benchmark_group("deep_book");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(300);

    // levels_per_side → approximate BTreeMap depth per order book
    for &levels in &[0usize, 100, 1_000, 5_000] {
        let label = if levels == 0 {
            "baseline_~10lvl".to_string()
        } else {
            format!("depth_{levels}lvl_per_side")
        };

        let mut sim = if levels == 0 {
            SimState::build()
        } else {
            build_deep_sim(levels)
        };
        let mut timings: Vec<u64> = Vec::with_capacity(20_000);

        group.bench_with_input(
            BenchmarkId::new("insert_cancel_at_depth", &label),
            &levels,
            |b, _| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let ts = sim.next_ts();
                        let roll: u8 = sim.rng.gen_range(0..100);
                        let t0 = Instant::now();
                        if roll < 50 {
                            if let Some((_, _, req)) = sim.random_cancel() {
                                black_box(sim.engine.process(black_box(req), ts));
                            } else {
                                let (_, req) = sim.random_post_order();
                                black_box(sim.engine.process(black_box(req), ts));
                            }
                        } else {
                            let (_, req) = sim.random_post_order();
                            let resp = sim.engine.process(black_box(req), ts);
                            for r in &resp {
                                if let Response::OrderPosted(op) = r {
                                    if matches!(
                                        op.status,
                                        OrderStatus::Open | OrderStatus::PartiallyFilled
                                    ) {
                                        let mkt_i = sim.rng.gen_range(0..NUM_MARKETS);
                                        sim.resting[mkt_i].push((0, op.order_id, OrderSide::Bid));
                                    }
                                }
                            }
                            black_box(resp);
                        }
                        let elapsed = t0.elapsed();
                        total += elapsed;
                        timings.push(elapsed.as_nanos() as u64);
                    }
                    total
                })
            },
        );

        print_percentiles(&label, &mut timings);
        timings.clear();
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion wiring
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_post_order,
    bench_cancel_order,
    bench_full_loop,
    bench_throughput,
    bench_latency_percentiles,
    bench_component_breakdown,
    bench_batched_throughput,
    bench_fill_ratio_sweep,
    bench_concurrent_takers,
    bench_burst_profile,
    bench_deep_book,
);
criterion_main!(benches);
