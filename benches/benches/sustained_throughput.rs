/// Tier 3 — Sustained throughput under realistic MM load
///
/// Runs 5 independent market-maker agents simultaneously for 300 seconds.
/// Samples throughput every 5 seconds to produce a 60-point time series that
/// shows whether throughput is stable, degrading, or a short burst artifact.
///
/// Workload composition (per task, randomized each iteration):
///   60%  cancel-repost cycle (cancel a resting order, repost at new price)
///   20%  new limit GTC post (both sides, spread around mid)
///   15%  aggressive IOC fill (crosses the spread — generates actual fills)
///    5%  balance query (GET /account/:addr/balances — monitoring traffic)
///
/// All MM agents use the real HTTP stack.  No engine or dispatcher access is
/// bypassed.  Signature verification, rate limiting (set very high for this
/// bench), batch-dispatcher window, and matching engine all run normally.
///
/// Memory usage is sampled at t=0, t=150s, t=300s using process inspection.
///
/// Compare with Hyperliquid's published ceiling:
///   200,000 ops/sec (execution-bottlenecked; consensus theoretically >1M ops/sec
///   per Hyperliquid docs)
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use k256::ecdsa::SigningKey;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use sha3::{Digest, Keccak256};
use types::{AssetId, DepositRequest, FeeConfig, Market, MarketId, Request, PRICE_SCALE, QUANTITY_SCALE};

// ---------------------------------------------------------------------------
// Market definitions
// ---------------------------------------------------------------------------

struct MarketSpec {
    id: &'static str,
    base: &'static str,
    mid: u64,
    qty: u64,
}

const MARKETS: &[MarketSpec] = &[
    MarketSpec { id: "BTC-USDC",   base: "BTC",   mid: 50_000 * PRICE_SCALE, qty: QUANTITY_SCALE / 100 },
    MarketSpec { id: "ETH-USDC",   base: "ETH",   mid:  3_000 * PRICE_SCALE, qty: QUANTITY_SCALE / 10  },
    MarketSpec { id: "SOL-USDC",   base: "SOL",   mid:    150 * PRICE_SCALE, qty: QUANTITY_SCALE       },
    MarketSpec { id: "AVAX-USDC",  base: "AVAX",  mid:     35 * PRICE_SCALE, qty: QUANTITY_SCALE       },
    MarketSpec { id: "MATIC-USDC", base: "MATIC", mid:          PRICE_SCALE, qty: 10 * QUANTITY_SCALE  },
    MarketSpec { id: "LINK-USDC",  base: "LINK",  mid:     15 * PRICE_SCALE, qty: QUANTITY_SCALE       },
    MarketSpec { id: "UNI-USDC",   base: "UNI",   mid:     10 * PRICE_SCALE, qty: QUANTITY_SCALE       },
    MarketSpec { id: "ARB-USDC",   base: "ARB",   mid:          PRICE_SCALE, qty: 10 * QUANTITY_SCALE  },
    MarketSpec { id: "OP-USDC",    base: "OP",    mid:      2 * PRICE_SCALE, qty: 5 * QUANTITY_SCALE   },
    MarketSpec { id: "AAVE-USDC",  base: "AAVE",  mid:    100 * PRICE_SCALE, qty: QUANTITY_SCALE / 10  },
    MarketSpec { id: "DOGE-USDC",  base: "DOGE",  mid: PRICE_SCALE / 10,     qty: 100 * QUANTITY_SCALE },
];
const NUM_MARKETS: usize = 11;

// ---------------------------------------------------------------------------
// Bench parameters
// ---------------------------------------------------------------------------

const NUM_MMS: usize = 5;
const KEYS_PER_MM: usize = 2_000;
const TOTAL_KEYS: usize = NUM_MMS * KEYS_PER_MM;
const RUN_SECS: u64 = 300;
const BUCKET_SECS: u64 = 5;
const NUM_BUCKETS: usize = (RUN_SECS / BUCKET_SECS) as usize;

// ---------------------------------------------------------------------------
// Keypair
// ---------------------------------------------------------------------------

struct Keypair {
    key: SigningKey,
    address: String,
}

fn gen_keypair(rng: &mut StdRng) -> Keypair {
    let key = SigningKey::random(rng);
    let point = key.verifying_key().to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    let address = format!("0x{}", hex::encode(&hash[12..]));
    Keypair { key, address }
}

// ---------------------------------------------------------------------------
// Signing helpers
// ---------------------------------------------------------------------------

fn eth_msg_hash(message: &[u8]) -> [u8; 32] {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let mut h = Keccak256::new();
    h.update(prefix.as_bytes());
    h.update(message);
    h.finalize().into()
}

fn sign_order(kp: &Keypair, market: &str, side: &str, price: u64, qty: u64, nonce: u64) -> Vec<u8> {
    let msg = format!("vela:order:{}:{}:{}:{}:{}", market, side, price, qty, nonce);
    let hash = eth_msg_hash(msg.as_bytes());
    let (sig, recid) = kp.key.sign_prehash_recoverable(&hash).expect("sign");
    let mut sb = Vec::with_capacity(65);
    sb.extend_from_slice(sig.to_bytes().as_ref());
    sb.push(recid.to_byte() + 27);
    let side_json = if side == "bid" { "Bid" } else { "Ask" };
    serde_json::to_vec(&serde_json::json!({
        "market": market,
        "side": side_json,
        "order_type": "GoodTillCanceled",
        "price": price,
        "quantity": qty,
        "nonce": nonce,
        "address": kp.address,
        "signature": format!("0x{}", hex::encode(&sb)),
    })).unwrap()
}

fn sign_ioc_order(kp: &Keypair, market: &str, side: &str, price: u64, qty: u64, nonce: u64) -> Vec<u8> {
    let msg = format!("vela:order:{}:{}:{}:{}:{}", market, side, price, qty, nonce);
    let hash = eth_msg_hash(msg.as_bytes());
    let (sig, recid) = kp.key.sign_prehash_recoverable(&hash).expect("sign");
    let mut sb = Vec::with_capacity(65);
    sb.extend_from_slice(sig.to_bytes().as_ref());
    sb.push(recid.to_byte() + 27);
    let side_json = if side == "bid" { "Bid" } else { "Ask" };
    serde_json::to_vec(&serde_json::json!({
        "market": market,
        "side": side_json,
        "order_type": "ImmediateOrCancel",
        "price": price,
        "quantity": qty,
        "nonce": nonce,
        "address": kp.address,
        "signature": format!("0x{}", hex::encode(&sb)),
    })).unwrap()
}

fn sign_cancel(kp: &Keypair, order_id: u64, nonce: u64) -> Vec<u8> {
    let msg = format!("vela:cancel:{}::{}", order_id, nonce);
    let hash = eth_msg_hash(msg.as_bytes());
    let (sig, recid) = kp.key.sign_prehash_recoverable(&hash).expect("sign cancel");
    let mut sb = Vec::with_capacity(65);
    sb.extend_from_slice(sig.to_bytes().as_ref());
    sb.push(recid.to_byte() + 27);
    serde_json::to_vec(&serde_json::json!({
        "order_id": order_id,
        "nonce": nonce,
        "address": kp.address,
        "signature": format!("0x{}", hex::encode(&sb)),
    })).unwrap()
}

// ---------------------------------------------------------------------------
// Engine setup
// ---------------------------------------------------------------------------

fn build_engine(keypairs: &[Keypair]) -> engine::MatchingEngine {
    let mut eng = engine::MatchingEngine::new(FeeConfig::default(), 10.0);

    for ms in MARKETS {
        eng.add_market(Market {
            id: MarketId(ms.id.to_string()),
            base: AssetId::from_str(ms.base),
            quote: AssetId::from_str("USDC"),
            max_orders: 100_000,
            min_order_size: 1,
            price_tick: 1,
            quantity_tick: 1,
            maker_fee_bps: -1,
            taker_fee_bps: 5,
        });
    }

    let usdc = AssetId::from_str("USDC");
    let mut hash_ctr: u64 = 0;
    let mut seq: u64 = 0;

    for kp in keypairs {
        let user = types::UserId::from_hex(&kp.address).unwrap();
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&hash_ctr.to_le_bytes());
        hash_ctr += 1;
        seq += 1;
        eng.process(Request::Deposit(DepositRequest {
            user: user.clone(),
            asset: usdc.clone(),
            amount: 1_000_000_000 * PRICE_SCALE,
            l1_tx_hash: h,
        }), seq);
        for ms in MARKETS {
            let mut h = [0u8; 32];
            h[..8].copy_from_slice(&hash_ctr.to_le_bytes());
            hash_ctr += 1;
            seq += 1;
            eng.process(Request::Deposit(DepositRequest {
                user: user.clone(),
                asset: AssetId::from_str(ms.base),
                amount: 1_000_000_000 * QUANTITY_SCALE,
                l1_tx_hash: h,
            }), seq);
        }
    }
    eng
}

// ---------------------------------------------------------------------------
// Memory reporting
// ---------------------------------------------------------------------------

fn rss_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if line.starts_with("VmRSS:") {
                    if let Some(kb) = line.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()) {
                        return kb / 1024;
                    }
                }
            }
        }
        0
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(out) = Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
        {
            if let Ok(kb) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                return kb / 1024;
            }
        }
        0
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    0
}

// ---------------------------------------------------------------------------
// Shared atomic counters
// ---------------------------------------------------------------------------

struct Counters {
    total_ops: AtomicU64,
    total_fills: AtomicU64,
    match_lat_ns: std::sync::Mutex<Vec<u64>>,
}

impl Counters {
    fn new() -> Arc<Self> {
        Arc::new(Counters {
            total_ops: AtomicU64::new(0),
            total_fills: AtomicU64::new(0),
            match_lat_ns: std::sync::Mutex::new(Vec::with_capacity(100_000)),
        })
    }
}

// ---------------------------------------------------------------------------
// MM task
// ---------------------------------------------------------------------------

async fn mm_task(
    mm_id: usize,
    keypairs: Arc<Vec<Keypair>>,
    base_url: Arc<String>,
    client: Arc<reqwest::Client>,
    counters: Arc<Counters>,
    run_end: Instant,
    bucket_ops: Arc<Vec<AtomicU64>>,
) {
    let key_start = mm_id * KEYS_PER_MM;

    let mut rng = StdRng::seed_from_u64(0xABCD_1234 + mm_id as u64);

    // Each MM operates on 3–5 randomly selected markets
    let n_markets = rng.gen_range(3..=5usize).min(NUM_MARKETS);
    let mm_markets: Vec<usize> = {
        let mut idx: Vec<usize> = (0..NUM_MARKETS).collect();
        idx.sort_by_key(|_| rng.gen::<u64>());
        idx[..n_markets].to_vec()
    };

    let mut nonces = vec![0u64; KEYS_PER_MM];
    let mut ki = 0usize; // current key index within this MM's slice
    // Track resting orders: (order_id, key_index, market_idx, side)
    let mut resting: VecDeque<(u64, usize, usize, &'static str)> = VecDeque::new();

    let orders_url = format!("{}/orders", base_url);
    let cancel_url = format!("{}/orders/cancel", base_url);

    while Instant::now() < run_end {
        let roll: u8 = rng.gen_range(0..100);

        let bucket_idx = {
            let elapsed = run_end.duration_since(Instant::now()).as_secs();
            let elapsed_from_start = RUN_SECS.saturating_sub(elapsed);
            (elapsed_from_start / BUCKET_SECS) as usize
        };

        // ── 60%: cancel-repost ──────────────────────────────────────────────
        if roll < 60 {
            if let Some((oid, cancel_ki, mi, _side)) = resting.pop_front() {
                let kp = &keypairs[key_start + cancel_ki];
                nonces[cancel_ki] += 1;
                let cancel_body = sign_cancel(kp, oid, nonces[cancel_ki]);

                let t0 = Instant::now();
                let resp = client.post(&cancel_url)
                    .header("content-type", "application/json")
                    .body(cancel_body)
                    .send().await;
                let lat_ns = t0.elapsed().as_nanos() as u64;

                if resp.map(|r| r.status().is_success()).unwrap_or(false) {
                    counters.total_ops.fetch_add(1, Ordering::Relaxed);
                    counters.match_lat_ns.lock().unwrap().push(lat_ns);
                    if bucket_idx < NUM_BUCKETS {
                        bucket_ops[bucket_idx].fetch_add(1, Ordering::Relaxed);
                    }
                }

                // Repost at a new price
                let ms = &MARKETS[mi];
                let is_bid = ki % 2 == 0;
                let side_sign = if is_bid { "bid" } else { "ask" };
                let spread_ticks = rng.gen_range(1u64..=20);
                let spread = ms.mid / 100 * spread_ticks;
                let price = if is_bid { ms.mid.saturating_sub(spread) } else { ms.mid + spread };

                nonces[ki] += 1;
                let body = sign_order(&keypairs[key_start + ki], ms.id, side_sign, price, ms.qty, nonces[ki]);

                let t0 = Instant::now();
                if let Ok(resp) = client.post(&orders_url)
                    .header("content-type", "application/json")
                    .body(body)
                    .send().await
                {
                    let lat_ns = t0.elapsed().as_nanos() as u64;
                    if resp.status().is_success() {
                        if let Ok(bytes) = resp.bytes().await {
                            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                                if let Some(oid) = extract_order_id(&json) {
                                    resting.push_back((oid, ki, mi, if is_bid { "bid" } else { "ask" }));
                                }
                            }
                        }
                        counters.total_ops.fetch_add(1, Ordering::Relaxed);
                        counters.match_lat_ns.lock().unwrap().push(lat_ns);
                        if bucket_idx < NUM_BUCKETS {
                            bucket_ops[bucket_idx].fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                ki = (ki + 1) % KEYS_PER_MM;
            } else {
                // No resting orders to cancel — fall through to a new post
                let mi = mm_markets[rng.gen_range(0..mm_markets.len())];
                let ms = &MARKETS[mi];
                let is_bid = ki % 2 == 0;
                let side_sign = if is_bid { "bid" } else { "ask" };
                let spread = ms.mid / 100;
                let price = if is_bid { ms.mid.saturating_sub(spread) } else { ms.mid + spread };
                nonces[ki] += 1;
                let body = sign_order(&keypairs[key_start + ki], ms.id, side_sign, price, ms.qty, nonces[ki]);
                let t0 = Instant::now();
                if let Ok(resp) = client.post(&orders_url)
                    .header("content-type", "application/json")
                    .body(body)
                    .send().await
                {
                    let lat_ns = t0.elapsed().as_nanos() as u64;
                    if resp.status().is_success() {
                        if let Ok(bytes) = resp.bytes().await {
                            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                                if let Some(oid) = extract_order_id(&json) {
                                    resting.push_back((oid, ki, mi, if is_bid { "bid" } else { "ask" }));
                                }
                            }
                        }
                        counters.total_ops.fetch_add(1, Ordering::Relaxed);
                        counters.match_lat_ns.lock().unwrap().push(lat_ns);
                        if bucket_idx < NUM_BUCKETS {
                            bucket_ops[bucket_idx].fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                ki = (ki + 1) % KEYS_PER_MM;
            }
        }
        // ── 20%: new limit GTC post ─────────────────────────────────────────
        else if roll < 80 {
            let mi = mm_markets[rng.gen_range(0..mm_markets.len())];
            let ms = &MARKETS[mi];
            let is_bid = rng.gen_bool(0.5);
            let side_sign = if is_bid { "bid" } else { "ask" };
            let spread_ticks = rng.gen_range(1u64..=50);
            let spread = ms.mid / 100 * spread_ticks;
            let price = if is_bid { ms.mid.saturating_sub(spread) } else { ms.mid + spread };
            nonces[ki] += 1;
            let body = sign_order(&keypairs[key_start + ki], ms.id, side_sign, price, ms.qty, nonces[ki]);
            let t0 = Instant::now();
            if let Ok(resp) = client.post(&orders_url)
                .header("content-type", "application/json")
                .body(body)
                .send().await
            {
                let lat_ns = t0.elapsed().as_nanos() as u64;
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes().await {
                        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            if let Some(oid) = extract_order_id(&json) {
                                resting.push_back((oid, ki, mi, if is_bid { "bid" } else { "ask" }));
                            }
                        }
                    }
                    counters.total_ops.fetch_add(1, Ordering::Relaxed);
                    counters.match_lat_ns.lock().unwrap().push(lat_ns);
                    if bucket_idx < NUM_BUCKETS {
                        bucket_ops[bucket_idx].fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            ki = (ki + 1) % KEYS_PER_MM;
        }
        // ── 15%: aggressive IOC fill ────────────────────────────────────────
        else if roll < 95 {
            let mi = mm_markets[rng.gen_range(0..mm_markets.len())];
            let ms = &MARKETS[mi];
            // Cross the spread aggressively (price 5% above/below mid)
            let is_bid = rng.gen_bool(0.5);
            let side_sign = if is_bid { "bid" } else { "ask" };
            let price = if is_bid { ms.mid * 105 / 100 } else { ms.mid * 95 / 100 };
            nonces[ki] += 1;
            let body = sign_ioc_order(&keypairs[key_start + ki], ms.id, side_sign, price, ms.qty, nonces[ki]);
            let t0 = Instant::now();
            if let Ok(resp) = client.post(&orders_url)
                .header("content-type", "application/json")
                .body(body)
                .send().await
            {
                let lat_ns = t0.elapsed().as_nanos() as u64;
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes().await {
                        if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                            if has_fill(&json) {
                                counters.total_fills.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    counters.total_ops.fetch_add(1, Ordering::Relaxed);
                    counters.match_lat_ns.lock().unwrap().push(lat_ns);
                    if bucket_idx < NUM_BUCKETS {
                        bucket_ops[bucket_idx].fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            ki = (ki + 1) % KEYS_PER_MM;
        }
        // ── 5%: balance query ───────────────────────────────────────────────
        else {
            let kp = &keypairs[key_start + ki];
            let balance_url = format!("{}/account/{}/balances", base_url, kp.address);
            let t0 = Instant::now();
            if let Ok(resp) = client.get(&balance_url).send().await {
                let lat_ns = t0.elapsed().as_nanos() as u64;
                if resp.status().is_success() {
                    let _ = resp.bytes().await;
                    counters.total_ops.fetch_add(1, Ordering::Relaxed);
                    counters.match_lat_ns.lock().unwrap().push(lat_ns);
                    if bucket_idx < NUM_BUCKETS {
                        bucket_ops[bucket_idx].fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            ki = (ki + 1) % KEYS_PER_MM;
        }
    }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn extract_order_id(json: &serde_json::Value) -> Option<u64> {
    json["data"]
        .as_array()?
        .iter()
        .find_map(|item| item["OrderPosted"]["order_id"].as_u64())
}

fn has_fill(json: &serde_json::Value) -> bool {
    json["data"]
        .as_array()
        .map(|arr| arr.iter().any(|item| item.get("OrderFilled").is_some()))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// ASCII bar chart
// ---------------------------------------------------------------------------

fn bar(value: u64, max_value: u64, width: usize) -> String {
    if max_value == 0 {
        return " ".repeat(width);
    }
    let filled = ((value as f64 / max_value as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), " ".repeat(width - filled))
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    std::env::set_var("VELA_ORDER_RATE_LIMIT", "10000000");
    std::env::set_var("VELA_RATE_WINDOW_SECS", "1");
    std::env::set_var("VELA_PYTH_ENABLED", "false");
    std::env::set_var("VELA_BATCH_WINDOW_US", "500");

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run())
}

async fn run() {
    // -----------------------------------------------------------------------
    // Setup
    // -----------------------------------------------------------------------
    eprintln!("[sustained] Generating {} keypairs ...", TOTAL_KEYS);
    let mut rng = StdRng::seed_from_u64(0x1234_ABCD_5678_EF00);
    let keypairs: Vec<Keypair> = (0..TOTAL_KEYS).map(|_| gen_keypair(&mut rng)).collect();
    let keypairs = Arc::new(keypairs);

    eprintln!("[sustained] Building engine + funding {} users ...", TOTAL_KEYS);
    let t_setup = Instant::now();

    let tmp = std::env::temp_dir().join(format!(
        "vela_sustained_bench_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(tmp.join("da")).unwrap();
    std::fs::create_dir_all(tmp.join("wal")).unwrap();
    std::env::set_var("DA_DIR", tmp.join("da").to_str().unwrap());

    let wal = Arc::new(api::wal::Wal::new(&tmp.join("wal")).unwrap());
    let eng = build_engine(&keypairs);
    let state = api::AppState::new(eng, wal);
    let router = api::build_router(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
    tokio::time::sleep(Duration::from_millis(100)).await;

    eprintln!("[sustained] Server on :{port}  setup={:.1}s", t_setup.elapsed().as_secs_f64());

    let base_url = Arc::new(format!("http://127.0.0.1:{port}"));

    // -----------------------------------------------------------------------
    // Shared state
    // -----------------------------------------------------------------------
    let counters = Counters::new();
    let bucket_ops: Arc<Vec<AtomicU64>> = Arc::new(
        (0..NUM_BUCKETS).map(|_| AtomicU64::new(0)).collect()
    );

    // -----------------------------------------------------------------------
    // Run
    // -----------------------------------------------------------------------
    let mem_t0 = rss_mb();
    let run_end = Instant::now() + Duration::from_secs(RUN_SECS);

    eprintln!("[sustained] Starting {NUM_MMS} MM agents for {RUN_SECS}s ...");

    let client = Arc::new(
        reqwest::Client::builder()
            .pool_max_idle_per_host(32)
            .tcp_nodelay(true)
            .build()
            .unwrap()
    );

    let mut handles = Vec::with_capacity(NUM_MMS);
    for mm_id in 0..NUM_MMS {
        let kp = Arc::clone(&keypairs);
        let url = Arc::clone(&base_url);
        let cli = Arc::clone(&client);
        let ctr = Arc::clone(&counters);
        let bkt = Arc::clone(&bucket_ops);
        handles.push(tokio::spawn(async move {
            mm_task(mm_id, kp, url, cli, ctr, run_end, bkt).await;
        }));
    }

    // Snapshot memory at t=150s
    let mem_t150 = Arc::new(std::sync::Mutex::new(0u64));
    {
        let m = Arc::clone(&mem_t150);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(150)).await;
            *m.lock().unwrap() = rss_mb();
        });
    }

    for h in handles { h.await.unwrap(); }

    let mem_t300 = rss_mb();
    let mem_t150_val = *mem_t150.lock().unwrap();

    // -----------------------------------------------------------------------
    // Compute statistics
    // -----------------------------------------------------------------------
    let total_ops = counters.total_ops.load(Ordering::Relaxed);
    let total_fills = counters.total_fills.load(Ordering::Relaxed);
    let fill_rate = if total_ops > 0 {
        100.0 * total_fills as f64 / total_ops as f64
    } else {
        0.0
    };

    let bucket_vals: Vec<u64> = bucket_ops.iter().map(|b| b.load(Ordering::Relaxed)).collect();
    let peak = bucket_vals.iter().copied().max().unwrap_or(0);
    let min_tput = bucket_vals.iter().copied().min().unwrap_or(0);
    let mean_tput = if !bucket_vals.is_empty() {
        bucket_vals.iter().sum::<u64>() as f64 / bucket_vals.len() as f64
    } else {
        0.0
    };
    let variance_tput = bucket_vals.iter()
        .map(|&v| { let d = v as f64 - mean_tput; d * d })
        .sum::<f64>() / bucket_vals.len() as f64;
    let stddev_tput = variance_tput.sqrt();
    let stability = if peak > 0 { 100.0 * mean_tput / peak as f64 } else { 0.0 };

    // Scale to ops/sec (bucket = BUCKET_SECS seconds)
    let bucket_ops_per_sec: Vec<u64> = bucket_vals.iter().map(|&v| v / BUCKET_SECS).collect();
    let peak_ops_s = peak / BUCKET_SECS;
    let min_ops_s = min_tput / BUCKET_SECS;
    let mean_ops_s = mean_tput / BUCKET_SECS as f64;
    let stddev_ops_s = stddev_tput / BUCKET_SECS as f64;

    let mut match_lat: Vec<u64> = counters.match_lat_ns.lock().unwrap().clone();
    match_lat.sort_unstable();
    let n_lat = match_lat.len();
    let (p50_lat, p99_lat) = if n_lat > 0 {
        let p50 = match_lat[(n_lat * 500).saturating_sub(1) / 1000];
        let p99 = match_lat[(n_lat * 990).saturating_sub(1) / 1000];
        (p50, p99)
    } else {
        (0, 0)
    };

    // -----------------------------------------------------------------------
    // Print
    // -----------------------------------------------------------------------
    println!();
    println!("=== VELA SUSTAINED THROUGHPUT (Tier 3, {}s) ===", RUN_SECS);
    println!("Total orders processed:      {}", total_ops);
    println!("Total fills:                 {}", total_fills);
    println!("Fill rate:                   {:.2}%", fill_rate);
    println!("Peak throughput ({}s bucket): {} ops/sec", BUCKET_SECS, peak_ops_s);
    println!("Min  throughput ({}s bucket): {} ops/sec", BUCKET_SECS, min_ops_s);
    println!("Mean throughput:             {:.0} ops/sec", mean_ops_s);
    println!("Throughput std deviation:    {:.0} ops/sec", stddev_ops_s);
    println!("Throughput stability:        {:.1}% (mean/peak)", stability);
    println!("p50 match latency:           {} µs", p50_lat / 1_000);
    println!("p99 match latency:           {} µs", p99_lat / 1_000);
    println!("Memory at t=0:               {} MB", mem_t0);
    println!("Memory at t=150s:            {} MB", mem_t150_val);
    println!("Memory at t=300s:            {} MB", mem_t300);
    println!();

    // ASCII bar chart — scale bars to peak ops/sec
    let max_bar = peak_ops_s.max(1);
    println!("THROUGHPUT TIME SERIES (ops/sec every {}s):", BUCKET_SECS);
    for (i, &ops_s) in bucket_ops_per_sec.iter().enumerate() {
        let t = (i as u64 + 1) * BUCKET_SECS;
        println!(
            "t={:>4}s: {:>8} {}",
            t,
            ops_s,
            bar(ops_s, max_bar, 40)
        );
    }

    println!();
    println!("HYPERLIQUID BASELINE: 200,000 ops/sec ceiling (documented design figure,");
    println!("execution-bottlenecked per Hyperliquid docs; consensus theoretically");
    println!(">1M ops/sec per Hyperliquid technical documentation).");
    println!();
    println!("NOTE: Both Vela and Hyperliquid figures are in-process / single-datacenter.");
    println!("Vela has no BFT consensus; Hyperliquid's 200k is with HyperBFT enabled.");
}
