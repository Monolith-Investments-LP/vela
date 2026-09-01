use std::sync::atomic::{AtomicU64, Ordering};
/// Tier 2 — End-to-end HTTP latency benchmark
///
/// Measures round-trip latency from `reqwest::Client::post().send()` to full
/// response body received.  Covers: loopback TCP, Axum routing, ECDSA
/// verification (spawn_blocking), batch-dispatcher window (500 µs default),
/// matching engine, and response serialization.
///
/// Client-side ECDSA signing is performed OUTSIDE the timed window.
///
/// Compare with Hyperliquid colocated figures (official docs):
///   p50: 200,000 µs (200 ms)   p99: 900,000 µs (900 ms)
///
/// Vela's numbers are in-process (loopback, zero network transit).
/// Hyperliquid's 200 ms floor is driven by HyperBFT consensus; Vela has no
/// consensus layer.  The correct comparison is execution-layer vs.
/// execution-layer latency.
use std::sync::Arc;
use std::time::{Duration, Instant};

use k256::ecdsa::SigningKey;
use rand::rngs::StdRng;
use rand::SeedableRng;
use sha3::{Digest, Keccak256};
use types::{
    AssetId, DepositRequest, FeeConfig, Market, MarketId, Request, PRICE_SCALE, QUANTITY_SCALE,
};

// ---------------------------------------------------------------------------
// Markets — the 11 Pyth production feeds
// ---------------------------------------------------------------------------

struct MarketSpec {
    id: &'static str,
    base: &'static str,
    mid: u64,
    qty: u64,
}

const MARKETS: &[MarketSpec] = &[
    MarketSpec {
        id: "BTC-USDC",
        base: "BTC",
        mid: 50_000 * PRICE_SCALE,
        qty: QUANTITY_SCALE / 100,
    },
    MarketSpec {
        id: "ETH-USDC",
        base: "ETH",
        mid: 3_000 * PRICE_SCALE,
        qty: QUANTITY_SCALE / 10,
    },
    MarketSpec {
        id: "SOL-USDC",
        base: "SOL",
        mid: 150 * PRICE_SCALE,
        qty: QUANTITY_SCALE,
    },
    MarketSpec {
        id: "AVAX-USDC",
        base: "AVAX",
        mid: 35 * PRICE_SCALE,
        qty: QUANTITY_SCALE,
    },
    MarketSpec {
        id: "MATIC-USDC",
        base: "MATIC",
        mid: PRICE_SCALE,
        qty: 10 * QUANTITY_SCALE,
    },
    MarketSpec {
        id: "LINK-USDC",
        base: "LINK",
        mid: 15 * PRICE_SCALE,
        qty: QUANTITY_SCALE,
    },
    MarketSpec {
        id: "UNI-USDC",
        base: "UNI",
        mid: 10 * PRICE_SCALE,
        qty: QUANTITY_SCALE,
    },
    MarketSpec {
        id: "ARB-USDC",
        base: "ARB",
        mid: PRICE_SCALE,
        qty: 10 * QUANTITY_SCALE,
    },
    MarketSpec {
        id: "OP-USDC",
        base: "OP",
        mid: 2 * PRICE_SCALE,
        qty: 5 * QUANTITY_SCALE,
    },
    MarketSpec {
        id: "AAVE-USDC",
        base: "AAVE",
        mid: 100 * PRICE_SCALE,
        qty: QUANTITY_SCALE / 10,
    },
    MarketSpec {
        id: "DOGE-USDC",
        base: "DOGE",
        mid: PRICE_SCALE / 10,
        qty: 100 * QUANTITY_SCALE,
    },
];
const NUM_MARKETS: usize = 11;

// ---------------------------------------------------------------------------
// Key pool layout
//
// Keys are allocated to non-overlapping scenarios so nonce windows
// never interfere between scenarios.
//
//   [ 0 ..  S1_KEYS )              →  Scenario 1 (single-threaded)
//   [ S1_KEYS .. S1_KEYS+S2_KEYS ) →  Scenario 2 (concurrent-32)
//   [ S1_KEYS+S2_KEYS .. TOTAL )   →  Scenario 3 (burst, nonce=1)
// ---------------------------------------------------------------------------
const S1_KEYS: usize = 5_000;
const S2_KEYS: usize = 5_000; // 32 tasks × ~156 keys each
const S3_KEYS: usize = 1_000; // burst — each used exactly once (nonce=1)
const TOTAL_KEYS: usize = S1_KEYS + S2_KEYS + S3_KEYS;

const WARMUP_SECS: u64 = 30;
const MEASURE_SECS: u64 = 120;
const CONCURRENT_CLIENTS: usize = 32;
const BURST_COUNT: usize = S3_KEYS;

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
// Signing / body preparation
// ---------------------------------------------------------------------------

fn eth_msg_hash(message: &[u8]) -> [u8; 32] {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let mut h = Keccak256::new();
    h.update(prefix.as_bytes());
    h.update(message);
    h.finalize().into()
}

/// Build and sign a POST /orders JSON body.  Called BEFORE the timed section.
fn prepare_order(kp: &Keypair, slot: usize, nonce: u64) -> Vec<u8> {
    let mi = slot % NUM_MARKETS;
    let ms = &MARKETS[mi];
    let is_bid = slot % 2 == 0;
    let side_sign = if is_bid { "bid" } else { "ask" };
    let spread = ms.mid / 100;
    let price = if is_bid {
        ms.mid.saturating_sub(spread)
    } else {
        ms.mid + spread
    };

    let msg = format!(
        "vela:order:{}:{}:{}:{}:{}",
        ms.id, side_sign, price, ms.qty, nonce
    );
    let hash = eth_msg_hash(msg.as_bytes());
    let (sig, recid) = kp.key.sign_prehash_recoverable(&hash).expect("sign");
    let mut sb = Vec::with_capacity(65);
    sb.extend_from_slice(sig.to_bytes().as_ref());
    sb.push(recid.to_byte() + 27);

    let side_json = if is_bid { "Bid" } else { "Ask" };
    serde_json::to_vec(&serde_json::json!({
        "market": ms.id,
        "side": side_json,
        "order_type": "GoodTillCanceled",
        "price": price,
        "quantity": ms.qty,
        "nonce": nonce,
        "address": kp.address,
        "signature": format!("0x{}", hex::encode(&sb)),
    }))
    .expect("serialize")
}

// ---------------------------------------------------------------------------
// Engine setup
// ---------------------------------------------------------------------------

fn build_funded_engine(keypairs: &[Keypair]) -> engine::MatchingEngine {
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
    let mut ts: u64 = 1;

    for (ui, kp) in keypairs.iter().enumerate() {
        let user = types::UserId::from_hex(&kp.address).unwrap();
        let mut h = [0u8; 32];
        h[..8].copy_from_slice(&(ui as u64 * 100).to_le_bytes());
        eng.process(
            Request::Deposit(DepositRequest {
                user: user.clone(),
                asset: usdc.clone(),
                amount: 1_000_000_000 * PRICE_SCALE,
                l1_tx_hash: h,
            }),
            ts,
        );
        ts += 1;
        for (mi, ms) in MARKETS.iter().enumerate() {
            let mut h2 = [0u8; 32];
            h2[..8].copy_from_slice(&(ui as u64 * 100 + mi as u64 + 1).to_le_bytes());
            eng.process(
                Request::Deposit(DepositRequest {
                    user: user.clone(),
                    asset: AssetId::from_str(ms.base),
                    amount: 1_000_000_000 * QUANTITY_SCALE,
                    l1_tx_hash: h2,
                }),
                ts,
            );
            ts += 1;
        }
    }
    eng
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

async fn start_server(keypairs: &[Keypair]) -> u16 {
    let tmp = std::env::temp_dir().join(format!(
        "vela_e2e_bench_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(tmp.join("da")).unwrap();
    std::fs::create_dir_all(tmp.join("wal")).unwrap();
    std::env::set_var("DA_DIR", tmp.join("da").to_str().unwrap());

    let wal = Arc::new(api::wal::Wal::new(&tmp.join("wal")).unwrap());
    let eng = build_funded_engine(keypairs);
    let state = api::AppState::new(eng, wal);
    let router = api::build_router(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    port
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

struct Stats {
    p50: u64,
    p99: u64,
    p999: u64,
    p9999: u64,
    min: u64,
    max: u64,
    mean_ns: u64,
    stddev_ns: u64,
    count: usize,
}

fn compute_stats(timings: &mut Vec<u64>) -> Stats {
    timings.sort_unstable();
    let n = timings.len();
    assert!(n > 0, "no timing samples");
    let sum: u64 = timings.iter().sum();
    let mean = sum as f64 / n as f64;
    let variance = timings
        .iter()
        .map(|&t| {
            let d = t as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / n as f64;
    let pct = |p: usize| -> u64 {
        let idx = (n * p).saturating_sub(1) / 1000;
        timings[idx.min(n - 1)]
    };
    Stats {
        p50: pct(500),
        p99: pct(990),
        p999: pct(999),
        p9999: timings[((n as f64 * 0.9999) as usize).min(n - 1)],
        min: timings[0],
        max: timings[n - 1],
        mean_ns: mean as u64,
        stddev_ns: variance.sqrt() as u64,
        count: n,
    }
}

// ---------------------------------------------------------------------------
// Core timed send
// ---------------------------------------------------------------------------

#[inline]
async fn timed_send(client: &reqwest::Client, url: &str, body: Vec<u8>) -> u64 {
    let t0 = Instant::now();
    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .expect("send");
    let _ = resp.bytes().await.expect("body");
    t0.elapsed().as_nanos() as u64
}

// ---------------------------------------------------------------------------
// Scenario 1: single-threaded sequential (keys 0..S1_KEYS)
// ---------------------------------------------------------------------------

async fn scenario_single_threaded(
    client: &reqwest::Client,
    url: &str,
    keypairs: &[Keypair], // slice of length S1_KEYS
) -> Stats {
    let n = keypairs.len();
    let mut nonces = vec![0u64; n];
    let mut ki = 0usize;

    // Warmup
    let warmup_end = Instant::now() + Duration::from_secs(WARMUP_SECS);
    while Instant::now() < warmup_end {
        nonces[ki] += 1;
        let body = prepare_order(&keypairs[ki], ki, nonces[ki]);
        let _ = timed_send(client, url, body).await;
        ki = (ki + 1) % n;
    }

    // Measurement
    let mut timings: Vec<u64> = Vec::with_capacity(200_000);
    let measure_end = Instant::now() + Duration::from_secs(MEASURE_SECS);
    while Instant::now() < measure_end {
        nonces[ki] += 1;
        let body = prepare_order(&keypairs[ki], ki, nonces[ki]);
        timings.push(timed_send(client, url, body).await);
        ki = (ki + 1) % n;
    }
    compute_stats(&mut timings)
}

// ---------------------------------------------------------------------------
// Scenario 2: concurrent-32 (keys S1_KEYS..S1_KEYS+S2_KEYS)
// ---------------------------------------------------------------------------

async fn scenario_concurrent(
    client: Arc<reqwest::Client>,
    url: Arc<String>,
    keypairs: Arc<Vec<Keypair>>, // S2_KEYS keys, indexed 0..S2_KEYS
) -> Stats {
    let total = keypairs.len();
    let keys_per_task = total / CONCURRENT_CLIENTS;
    let nonces: Arc<Vec<AtomicU64>> = Arc::new((0..total).map(|_| AtomicU64::new(0)).collect());

    // Warmup
    {
        let warmup_end = Instant::now() + Duration::from_secs(WARMUP_SECS);
        let mut handles = Vec::with_capacity(CONCURRENT_CLIENTS);
        for task_id in 0..CONCURRENT_CLIENTS {
            let c = Arc::clone(&client);
            let u = Arc::clone(&url);
            let kp = Arc::clone(&keypairs);
            let ns = Arc::clone(&nonces);
            let ks = task_id * keys_per_task;
            let ke = ks + keys_per_task;
            handles.push(tokio::spawn(async move {
                let mut ki = ks;
                while Instant::now() < warmup_end {
                    let nonce = ns[ki].fetch_add(1, Ordering::Relaxed) + 1;
                    let body = prepare_order(&kp[ki], ki, nonce);
                    let _ = timed_send(&c, &u, body).await;
                    ki = ks + (ki - ks + 1) % (ke - ks);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
    }

    // Measurement
    let timings_acc: Arc<std::sync::Mutex<Vec<u64>>> =
        Arc::new(std::sync::Mutex::new(Vec::with_capacity(500_000)));
    let measure_end = Instant::now() + Duration::from_secs(MEASURE_SECS);
    let mut handles = Vec::with_capacity(CONCURRENT_CLIENTS);
    for task_id in 0..CONCURRENT_CLIENTS {
        let c = Arc::clone(&client);
        let u = Arc::clone(&url);
        let kp = Arc::clone(&keypairs);
        let ns = Arc::clone(&nonces);
        let ts = Arc::clone(&timings_acc);
        let ks = task_id * keys_per_task;
        let ke = ks + keys_per_task;
        handles.push(tokio::spawn(async move {
            let mut local: Vec<u64> = Vec::with_capacity(20_000);
            let mut ki = ks;
            while Instant::now() < measure_end {
                let nonce = ns[ki].fetch_add(1, Ordering::Relaxed) + 1;
                let body = prepare_order(&kp[ki], ki, nonce);
                local.push(timed_send(&c, &u, body).await);
                ki = ks + (ki - ks + 1) % (ke - ks);
            }
            ts.lock().unwrap().extend_from_slice(&local);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    let mut timings = Arc::try_unwrap(timings_acc).unwrap().into_inner().unwrap();
    compute_stats(&mut timings)
}

// ---------------------------------------------------------------------------
// Scenario 3: burst-1000 (dedicated keys, nonce=1 for each)
// ---------------------------------------------------------------------------

async fn scenario_burst(
    client: &reqwest::Client,
    url: &str,
    keypairs: &[Keypair], // length BURST_COUNT, each used exactly once
) -> Stats {
    let bodies: Vec<Vec<u8>> = (0..BURST_COUNT)
        .map(|i| prepare_order(&keypairs[i], i, 1))
        .collect();

    let t_start = Instant::now();
    let client = Arc::new(client.clone());
    let url = Arc::new(url.to_string());
    let timings_acc: Arc<std::sync::Mutex<Vec<u64>>> =
        Arc::new(std::sync::Mutex::new(Vec::with_capacity(BURST_COUNT)));

    let mut handles = Vec::with_capacity(BURST_COUNT);
    for body in bodies {
        let c = Arc::clone(&client);
        let u = Arc::clone(&url);
        let ts = Arc::clone(&timings_acc);
        handles.push(tokio::spawn(async move {
            let elapsed = timed_send(&c, &u, body).await;
            ts.lock().unwrap().push(elapsed);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    eprintln!(
        "[e2e_http] Burst finished in {:.2}s",
        t_start.elapsed().as_secs_f64()
    );

    let mut timings = Arc::try_unwrap(timings_acc).unwrap().into_inner().unwrap();
    compute_stats(&mut timings)
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
    // Key generation
    // Each scenario uses a non-overlapping slice so nonce windows never
    // interfere between scenarios.
    // -----------------------------------------------------------------------
    eprintln!("[e2e_http] Generating {TOTAL_KEYS} ECDSA keypairs ...");
    let mut rng = StdRng::seed_from_u64(0xDEAD_CAFE_1234_5678);
    let all_keypairs: Vec<Keypair> = (0..TOTAL_KEYS).map(|_| gen_keypair(&mut rng)).collect();

    eprintln!("[e2e_http] Building engine + funding {TOTAL_KEYS} users ...");
    let t0 = Instant::now();
    let port = start_server(&all_keypairs).await;
    let url = format!("http://127.0.0.1:{port}/orders");
    eprintln!(
        "[e2e_http] Server on :{port}  setup={:.1}s",
        t0.elapsed().as_secs_f64()
    );

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(128)
        .tcp_nodelay(true)
        .build()
        .unwrap();

    // Partition the pool
    // S1: indices 0..S1_KEYS
    // S2: indices S1_KEYS..S1_KEYS+S2_KEYS
    // S3: indices S1_KEYS+S2_KEYS..TOTAL_KEYS
    let s1_end = S1_KEYS;
    let s2_start = s1_end;
    let s2_end = s2_start + S2_KEYS;
    let s3_start = s2_end;

    // -----------------------------------------------------------------------
    // Scenario 1: single-threaded sequential
    // -----------------------------------------------------------------------
    eprintln!(
        "[e2e_http] S1 single-threaded — {WARMUP_SECS}s warmup + {MEASURE_SECS}s measure ..."
    );
    let t_s1 = Instant::now();
    let s1 = scenario_single_threaded(&client, &url, &all_keypairs[0..s1_end]).await;
    let elapsed_s1 = (t_s1.elapsed().as_secs_f64() - WARMUP_SECS as f64).max(1.0);

    // -----------------------------------------------------------------------
    // Scenario 2: concurrent-32
    // -----------------------------------------------------------------------
    eprintln!("[e2e_http] S2 concurrent-{CONCURRENT_CLIENTS} — {WARMUP_SECS}s warmup + {MEASURE_SECS}s measure ...");
    // Rebuild an Arc<Vec<Keypair>> for the S2 slice
    // (We regenerate from the same deterministic seed to get the same addresses)
    let mut rng2 = StdRng::seed_from_u64(0xDEAD_CAFE_1234_5678);
    // Skip S1_KEYS keypairs
    for _ in 0..S1_KEYS {
        let _ = gen_keypair(&mut rng2);
    }
    let s2_keypairs: Arc<Vec<Keypair>> =
        Arc::new((0..S2_KEYS).map(|_| gen_keypair(&mut rng2)).collect());
    let _ = all_keypairs[s2_start..s2_end].len(); // silence lint

    let t_s2 = Instant::now();
    let s2 =
        scenario_concurrent(Arc::new(client.clone()), Arc::new(url.clone()), s2_keypairs).await;
    let elapsed_s2 = (t_s2.elapsed().as_secs_f64() - WARMUP_SECS as f64).max(1.0);

    // -----------------------------------------------------------------------
    // Scenario 3: burst-1000
    // -----------------------------------------------------------------------
    eprintln!("[e2e_http] S3 burst-{BURST_COUNT} ...");
    // Regenerate S3 keys from the same seed (matching funded addresses)
    let mut rng3 = StdRng::seed_from_u64(0xDEAD_CAFE_1234_5678);
    for _ in 0..(S1_KEYS + S2_KEYS) {
        let _ = gen_keypair(&mut rng3);
    }
    let burst_keys: Vec<Keypair> = (0..S3_KEYS).map(|_| gen_keypair(&mut rng3)).collect();
    let _ = s3_start; // silence lint

    let t_s3 = Instant::now();
    let s3 = scenario_burst(&client, &url, &burst_keys).await;
    let elapsed_s3 = t_s3.elapsed().as_secs_f64().max(0.001);

    // -----------------------------------------------------------------------
    // Results
    // -----------------------------------------------------------------------
    let tput1 = s1.count as f64 / elapsed_s1;
    let tput2 = s2.count as f64 / elapsed_s2;
    let tput3 = BURST_COUNT as f64 / elapsed_s3;

    println!();
    println!("=== VELA END-TO-END HTTP LATENCY (Tier 2) ===");
    println!("Warmup: {WARMUP_SECS}s/scenario   Measurement: {MEASURE_SECS}s/scenario");
    println!(
        "Samples: single={} concurrent={} burst={}",
        s1.count, s2.count, s3.count
    );
    println!();
    println!(
        "{:<22} {:>10} {:>10} {:>10} {:>10} {:>14}",
        "Scenario", "p50", "p99", "p99.9", "p99.99", "throughput"
    );
    println!("{}", "-".repeat(82));
    for (label, s, tput) in [
        ("single-threaded", &s1, tput1),
        ("concurrent-32", &s2, tput2),
        ("burst-1000", &s3, tput3),
    ] {
        println!(
            "{:<22} {:>8} µs {:>8} µs {:>8} µs {:>8} µs {:>10.0} ops/s",
            label,
            s.p50 / 1_000,
            s.p99 / 1_000,
            s.p999 / 1_000,
            s.p9999 / 1_000,
            tput
        );
    }
    println!();
    println!("HYPERLIQUID BASELINE (official docs, colocated):");
    println!("  p50: 200,000 µs (200 ms)   p99: 900,000 µs (900 ms)");
    println!();
    println!("NOTE: Vela latency is in-process (loopback only, zero network transit).");
    println!("Hyperliquid's 200 ms floor is driven by HyperBFT consensus.  Vela has no");
    println!("consensus layer yet; these numbers are execution-layer only.");
    println!();
    println!("--- Additional statistics ---");
    for (label, s) in [
        ("single-threaded", &s1),
        ("concurrent-32", &s2),
        ("burst-1000", &s3),
    ] {
        println!(
            "  {:<22} min={:.0} µs  mean={:.0} µs  stddev={:.0} µs  max={:.0} µs",
            label,
            s.min as f64 / 1_000.0,
            s.mean_ns as f64 / 1_000.0,
            s.stddev_ns as f64 / 1_000.0,
            s.max as f64 / 1_000.0
        );
    }
}
