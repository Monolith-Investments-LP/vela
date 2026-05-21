# Vela Benchmark Suite — Methodology

This document describes how the three benchmark tiers are constructed,
what they measure, and what the numbers mean.  Read this before citing
any figure from `docs/benchmarks.md`.

---

## Tier 1 — Matching-engine microbenchmark (`benches/matching.rs`)

**Scope:** Pure in-process matching-engine throughput with no HTTP, no
authentication, and no I/O.  Uses Criterion for statistical rigor.

**What it measures:** The core order-matching loop: insert-limit, cancel,
market-order matching against a pre-populated book.

**Honest caveats:**
- No ECDSA verification, no rate-limiting, no batch-dispatcher latency.
- No disk, no network.  Numbers reflect the matching algorithm only.
- Not comparable to end-to-end exchange performance.

**How to run:**

```sh
cargo bench --bench matching
```

---

## Tier 2 — End-to-end HTTP latency (`benches/e2e_http.rs`)

**Scope:** Full request lifecycle: ECDSA signing → HTTP POST → API auth →
rate-limit → batch-dispatcher window → engine lock → matching → response.

**What it does NOT mock:**
- Signature verification (real k256/secp256k1 ECDSA)
- Rate limiter (DashMap per-address sliding-window, set to 10M/s for the bench)
- Batch dispatcher (500 µs coalescing window, max 256 per batch)
- Matching engine mutex

**What IS simplified:**
- Transport: loopback only (`127.0.0.1`).  No real network hop.
- No BFT consensus.  Vela's execution layer is measured in isolation.
- All keys pre-funded to 1B USDC + 1B base asset; no margin calls.

### Scenarios

| Scenario | Clients | Duration | Key pool |
|---|---|---|---|
| S1 — single-threaded | 1 | 30 s warmup + 120 s measure | keys 0–4999 |
| S2 — concurrent-32 | 32 | 30 s warmup + 120 s measure | keys 5000–9999 |
| S3 — burst-1000 | 1000 | one-shot, all-concurrent | keys 10000–10999 |

Key pools are non-overlapping to prevent nonce conflicts.  Each key
maintains a monotonically increasing per-address nonce.  Within S2 each
task gets an exclusive slice of keys (~156 per task) to eliminate
inter-task nonce collisions.

Timing wraps `client.send().await` + `response.bytes().await` only.
Request construction and signing happen before the timer starts.

### Warmup rationale

30 seconds is long enough for the batch-dispatcher queue, rate-limiter
DashMap, and Tokio's thread pool to reach thermal equilibrium.  Without
warmup, the first few thousand requests inflate p99 significantly.

**How to run:**

```sh
cargo bench --bench e2e_http
```

Expected runtime: ~7.5 minutes (30 + 120 + 30 + 120 + burst ≈ 305 s
plus setup).

---

## Tier 3 — Sustained throughput (`benches/sustained_throughput.rs`)

**Scope:** 5 independent market-maker agents operating simultaneously for
300 seconds.  Tests whether performance holds over time (no GC cliff, no
lock-convoy, no memory runaway).

**Workload mix (per MM agent, random each iteration):**

| Weight | Operation |
|---|---|
| 60% | Cancel-repost: cancel a resting order, post a new limit at updated price |
| 20% | New GTC limit: post both sides around mid-price |
| 15% | Aggressive IOC: cross the spread by 5% to generate actual fills |
|  5% | Balance query: `GET /account/:addr/balances` |

10,000 unique addresses (2,000 per MM), all pre-funded.  Each MM agent
operates on 3–5 of the 11 markets (randomly selected at startup).

5-second sampling buckets produce a 60-point time series for visual
inspection of throughput stability.

**Memory tracking:** RSS sampled at t=0, t=150 s, t=300 s via
`/proc/self/status` (Linux) or `ps -o rss=` (macOS).

**Honest caveats:**
- The throughput drop observed around t=120 s in steady state is
  expected: once each MM's resting-order queue fills, cancel-repost
  dominates (2 HTTP round-trips instead of 1 for a new post), halving
  raw request count while preserving the same trading activity.
- `127.0.0.1` loopback.  No real network latency.
- No consensus.

**How to run:**

```sh
cargo bench --bench sustained_throughput
```

Expected runtime: ~5.5 minutes (setup + 300 s run).

---

## Comparison with Hyperliquid

Hyperliquid publishes two figures:
- **200,000 ops/sec execution ceiling** (documented as execution-bottlenecked;
  consensus can theoretically sustain >1M ops/sec per their technical docs).
- **p50 ≈ 200 ms, p99 ≈ 900 ms** end-to-end latency from a colocated client.

Vela is **not comparable on throughput** today — Vela's execution layer
processes ~134–156 ops/sec in the HTTP benchmarks, five orders of magnitude
below Hyperliquid's execution ceiling.  The gap is expected:

1. Hyperliquid's 200k figure is for their native binary protocol with a custom
   HyperCore execution engine.  Vela uses a standard Axum HTTP stack.
2. Hyperliquid's architecture is purpose-built for throughput.  Vela's current
   bottleneck is the single-threaded engine mutex behind the batch dispatcher.
3. No attempt has been made to parallelize Vela's matching engine yet.

Vela's latency numbers **appear lower** than Hyperliquid's published p50 only
because Hyperliquid's 200 ms floor is driven by HyperBFT consensus round-trips
(2-of-3 validators in a datacenter).  Vela has no consensus layer; these are
pure execution-layer figures.

---

## Environment notes

Benchmarks set the following environment variables before starting:

```
VELA_ORDER_RATE_LIMIT=10000000   # effectively unlimited for the bench
VELA_RATE_WINDOW_SECS=1
VELA_PYTH_ENABLED=false          # suppress live price feed during bench
VELA_BATCH_WINDOW_US=500         # 500 µs batch-coalescing window (default)
DA_DIR=<tmpdir>                  # ephemeral DA storage
```

WAL and DA directories are created under `$TMPDIR` and deleted on process
exit.  Do not run benchmarks against a production data directory.
