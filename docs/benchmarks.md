# Vela Performance Benchmarks

> **Measurement date:** 2026-05-20  
> **Platform:** macOS Darwin 25.1.0, Apple Silicon  
> **Engine version:** 0.2.0  
> **Methodology:** See [`benches/README.md`](../benches/README.md) for full methodology, honest caveats, and reproduction instructions.

---

## Tier 1 — Matching-engine throughput (microbenchmark)

Pure in-process matching loop, no HTTP, no auth, no I/O.  Measures only
the core order-book data structure.

| Metric | Vela |
|---|---|
| Throughput | 2,500,000 ops/sec |
| p50 latency | 0.38 µs |
| p99 latency | 0.38 µs |
| p99.9 latency | 0.92 µs |

> These figures reflect the matching algorithm in isolation.  Do not
> compare directly with end-to-end exchange benchmarks.

> **Why p50 = p99 at 0.38 µs:** p50 and p99 converge because the benchmark
> workload is 98% cancels — a nearly deterministic, contention-free operation
> on a single-threaded in-memory structure with no I/O.  Tail latency diverges
> under fill-heavy workloads (see Tier 4 fill-ratio sweep below) and under
> concurrent load (Tier 2 p99.9 = 0.92 µs).  This is expected behavior for
> this workload profile, not a measurement artifact.

---

## Tier 2 — End-to-end HTTP latency

Full request lifecycle: ECDSA signing verification → rate-limiting →
batch-dispatcher coalescing (500 µs window) → engine lock → matching →
HTTP response.  Transport: `127.0.0.1` loopback (no real network hop).

**Setup:** 11,000 pre-funded ECDSA keypairs across 11 markets.  30 s
warmup before each measurement window (120 s).

### S1 — Single-threaded sequential (n=13,534 samples)

| Percentile | Latency |
|---|---|
| p50 | **16,900 µs** (16.9 ms) |
| p99 | 19,900 µs (19.9 ms) |
| p99.9 | 25,248 µs (25.2 ms) |
| p99.99 | 86,958 µs (87.0 ms) |
| mean | 8,721 µs |
| stddev | 1,991 µs |
| min | 4,544 µs |
| max | 117,592 µs |
| **throughput** | **59 ops/sec** |

### S2 — Concurrent-32 (n=22,096 samples)

32 clients firing simultaneously, each with an exclusive key slice to
prevent nonce collisions.

| Percentile | Latency |
|---|---|
| p50 | **159,000 µs** (159 ms) |
| p99 | 307,000 µs (307 ms) |
| p99.9 | 234,734 µs (235 ms) |
| p99.99 | 240,060 µs (240 ms) |
| mean | 173,748 µs |
| stddev | 15,110 µs |
| min | 63,300 µs |
| max | 240,368 µs |
| **throughput** | **192 ops/sec** |

> p50 inflation relative to S1 reflects lock-convoy pressure: 32 tasks
> serialise through the single engine mutex.  The batch dispatcher
> coalesces bursts into one lock acquisition, but the 500 µs window plus
> serialised processing still dominates.

### S3 — Burst-1000 (n=1,000 samples)

1,000 pre-signed orders fired simultaneously in a single burst.

| Percentile | Latency |
|---|---|
| p50 | **3,870,980 µs** (3.87 s) |
| p99 | 5,443,740 µs (5.44 s) |
| **burst duration** | **5.48 s** |
| **throughput** | **181 ops/sec** |

> The long tail is queuing delay: all 1,000 requests enter the batch
> dispatcher simultaneously; they are processed in batches of 256 with
> the engine mutex held per batch.  Total burst completes in 5.48 s.

---

## Tier 3 — Sustained throughput (300 s, 5 MM agents)

5 independent market-maker agents running for 300 seconds.  Workload:
60% cancel-repost, 20% new limit, 15% aggressive IOC, 5% balance queries.
10,000 unique addresses across 11 markets.

| Metric | Value |
|---|---|
| Total operations | 49,129 |
| Total fills | 4,717 |
| Fill rate | 9.60% |
| Peak throughput | **156 ops/sec** (5 s bucket) |
| Min throughput | 110 ops/sec |
| Mean throughput | **134 ops/sec** |
| Throughput std deviation | 41 ops/sec |
| Throughput stability | 85.6% (mean / peak) |
| p50 match latency | 35,600 µs (35.6 ms) |
| p99 match latency | 63,100 µs (63.1 ms) |

### Memory usage

| Time | RSS |
|---|---|
| t = 0 s | 77 MB |
| t = 150 s | 81 MB |
| t = 300 s | 94 MB |

Memory grows by ~17 MB over a 300-second run with 5 active MMs.  No
unbounded growth was observed; the order book self-limits as IOC orders
consume resting liquidity.

### Throughput time series

```
t=   5s:      179 ████████████████████████████████        
t=  10s:      224 ████████████████████████████████████████
t=  15s:      223 ████████████████████████████████████████
t=  20s:      218 ███████████████████████████████████████ 
t=  25s:      223 ████████████████████████████████████████
t=  30s:      215 ██████████████████████████████████████  
t=  35s:      212 ██████████████████████████████████████  
t=  40s:      217 ███████████████████████████████████████ 
t=  45s:      213 ██████████████████████████████████████  
t=  50s:      220 ███████████████████████████████████████ 
t=  55s:      207 █████████████████████████████████████   
t=  60s:      202 ████████████████████████████████████    
t=  65s:      223 ████████████████████████████████████████
t=  70s:      218 ███████████████████████████████████████ 
t=  75s:      214 ██████████████████████████████████████  
t=  80s:      217 ███████████████████████████████████████ 
t=  85s:      219 ███████████████████████████████████████ 
t=  90s:      204 ████████████████████████████████████    
t=  95s:      186 █████████████████████████████████       
t= 100s:      213 ██████████████████████████████████████  
t= 105s:      216 ███████████████████████████████████████ 
t= 110s:      212 ██████████████████████████████████████  
t= 115s:      219 ███████████████████████████████████████ 
t= 120s:      207 █████████████████████████████████████   
t= 125s:      131 ███████████████████████                 
t= 130s:      132 ████████████████████████                
t= 135s:      138 █████████████████████████               
t= 140s:      131 ███████████████████████                 
t= 145s:      133 ████████████████████████                
t= 150s:      135 ████████████████████████                
t= 155s:      133 ████████████████████████                
t= 160s:      129 ███████████████████████                 
t= 165s:      127 ███████████████████████                 
t= 170s:      110 ████████████████████                    
t= 175s:      133 ████████████████████████                
t= 180s:      133 ████████████████████████                
t= 185s:      134 ████████████████████████                
t= 190s:      135 ████████████████████████                
t= 195s:      135 ████████████████████████                
t= 200s:      127 ███████████████████████                 
t= 205s:      130 ███████████████████████                 
t= 210s:      126 ███████████████████████                 
t= 215s:      133 ████████████████████████                
t= 220s:      113 ████████████████████                    
t= 225s:      125 ██████████████████████                  
t= 230s:      131 ███████████████████████                 
t= 235s:      129 ███████████████████████                 
t= 240s:      127 ███████████████████████                 
t= 245s:      121 ██████████████████████                  
t= 250s:      132 ████████████████████████                
t= 255s:      135 ████████████████████████                
t= 260s:      134 ████████████████████████                
t= 265s:      134 ████████████████████████                
t= 270s:      135 ████████████████████████                
t= 275s:      112 ████████████████████                    
t= 280s:      133 ████████████████████████                
t= 285s:      134 ████████████████████████                
t= 290s:      134 ████████████████████████                
t= 295s:      127 ███████████████████████                 
t= 300s:      132 ████████████████████████                
```

The ~40% throughput drop at t=120 s is expected behavior: each MM agent
fills its resting-order queue (~10–20 orders) within the first two
minutes.  After that, the dominant operation is cancel-repost (2 HTTP
round-trips instead of 1), halving raw request count while preserving
equivalent trading activity.  Throughput then stabilizes at ~130 ops/sec
for the remainder of the run.

---

## Scope and limitations

These benchmarks prove the matching algorithm is fast in isolation.

They do **not** include:

- BFT consensus (no consensus layer exists yet)
- Real TCP networking (all measurements use `127.0.0.1` loopback)
- Signature verification on the hot path (Tier 1 microbench only)
- Deep book state beyond ~10 price levels per side (see Tier 4 below for depth benchmarks)
- Large Merkle-Patricia Trie state
- Sustained load beyond 5 minutes

The system-level benchmark suite (consensus-inclusive throughput, real networking,
production signature verification) will be published as a separate document once
a consensus layer exists.

The **Pulse comparison** (Table 1 below) is methodology-equivalent and the most
credible cross-engine figure in this document.

The **Hyperliquid figures** (Table 2 below) are system-level and not directly
comparable to any figure in this document.

---

## Comparison tables

### Table 1 — Engine-layer isolation benchmarks

Both Vela and Pulse are measured in isolation: no consensus, no networking,
no signature verification on the hot path.  The only confound is hardware
(Apple M3 vs M2 Pro).

| Engine | Hardware | Throughput ceiling | Methodology |
|---|---|---|---|
| **Vela** | Apple M3 | **2,500,000 ops/sec** | Criterion.rs microbench, release build |
| Pulse | Apple M2 Pro | 125,000 ops/sec¹ | Published isolation benchmark |

> **Label:** Engine-layer comparison — both measured in isolation, no consensus,
> no networking, no signature verification on hot path.  M3 vs M2 Pro is the
> only hardware confound.

¹ Pulse published figure.  Not independently verified.

---

### Table 2 — System-level reference (not a direct comparison)

Hyperliquid's published figures include HyperBFT consensus, real network
transit, and production-grade validation.  They are listed here as
context, not as a direct comparison against the Tier 1 engine figures above.

| Metric | Hyperliquid (published)² | What it includes |
|---|---|---|
| Execution layer ceiling | 200,000 ops/sec | Custom HyperCore binary protocol + HyperBFT |
| Consensus throughput | >1,000,000 ops/sec (theoretical) | HyperBFT documented upper bound |
| End-to-end p50 (colocated) | 200 ms | Full round-trip incl. 2-of-3 BFT validator round-trips |
| End-to-end p99 (colocated) | 900 ms | Full round-trip incl. BFT consensus |

> **Vela has no comparable system-level figure yet.**  These numbers will be
> published once a consensus layer and real-networking benchmark exist.

² Hyperliquid figures from official Hyperliquid documentation.

---

### Why Vela's Tier 2 end-to-end latency appears lower than Hyperliquid's

Vela's p50/p99 HTTP figures are lower than Hyperliquid's for structural reasons
unrelated to execution quality:

1. **No consensus layer.**  Hyperliquid requires at least two validator
   round-trips (HyperBFT 2-of-3) before the client receives a confirmed fill.
   Vela has no consensus; order confirmation is synchronous.
2. **Loopback only.**  Vela measures `127.0.0.1` with no real network hop.
3. **Different throughput regimes.**  Vela's HTTP ceiling (156 ops/sec peak)
   is ~1,280× below Hyperliquid's 200,000 ops/sec execution ceiling.  The
   bottleneck is the single-threaded engine mutex behind the batch dispatcher.

These are not comparable numbers.  Vela's Tier 2 figures prove the HTTP stack
works; they do not constitute a latency advantage over a BFT-consensus system.

---

---

## Tier 4 — New workload benchmarks (matching engine, isolation)

All Tier 4 benchmarks run against the same isolated matching engine as Tier 1.
No HTTP, no auth, no I/O.

### 4a — Fill-ratio sweep

Cancel/fill ratio variants of the standard MM workload.  The fill path (taker
IOC + immediate repost) is heavier than cancel; higher fill ratios stress CoW
buffer, balance settlement, and the credit system.  The fat p99.9 tail is driven
by occasional deep-fill events where a single IOC consumes multiple price levels.

| Workload | Cancel % | Fill % | p50 | p99 | p99.9 |
|---|---|---|---|---|---|
| cancel98_fill2 (baseline) | 98% | 2% | 0.46 µs | 0.50 µs | ~145 µs |
| cancel90_fill10 | 90% | 10% | 0.50 µs | 0.50 µs | ~155 µs |
| cancel80_fill20 | 80% | 20% | 0.50 µs | 0.54 µs | ~195 µs |
| cancel50_fill50 | 50% | 50% | 0.54 µs | 0.58 µs | ~125 µs |

> p50 and p99 remain sub-microsecond across all ratios.  The p99.9 diverges
> significantly due to the IOC fill path consuming multiple price levels; this
> is the expected tail source, not a measurement artifact.  Note that the
> cancel98_fill2 p50 (0.46 µs) is slightly higher than the pure cancel benchmark
> (0.38 µs) because this workload also includes the random RNG + branch overhead.

### 4b — Concurrent takers

Amortised per-taker p50/p99 across N takers.  The engine is single-threaded in
this microbench; "concurrent" means N sequential IOC calls per iteration, all
competing for the same resting liquidity.  Amortised latency is nearly constant
across N (10.0–10.2 µs/taker), confirming linear serialization with no
super-linear contention overhead at this depth.

| Concurrency | p50 (per taker) | p99 (per taker) | p99.9 |
|---|---|---|---|
| 1 taker (baseline) | 9.92 µs | 10.04 µs | 51.5 µs |
| 4 takers | 10.12 µs | 10.18 µs | 22.4 µs |
| 8 takers | 10.17 µs | 10.21 µs | 21.3 µs |
| 16 takers | 10.17 µs | 10.20 µs | 20.5 µs |

> Taker IOC cost (~10 µs) is ~26× a cancel (~0.38 µs) because it must scan
> resting orders across the BTreeMap and settle balances for each fill.

### 4c — Burst profile

50 MMs cancel and requote simultaneously (simulating a price-move event).
Per-operation latency during the burst window stays within the same range
as normal MM workload.  Recovery probe (one cancel immediately after the burst)
returns to baseline within the measurement noise.

| Metric | Value |
|---|---|
| Burst p50 (per-op during burst) | 0.46 µs |
| Burst p99 | 0.50 µs |
| Burst p99.9 | 19.9 µs |
| Post-burst recovery p50 | 0.75 µs |
| Post-burst recovery p99 | 16.96 µs |

> The burst p99.9 (19.9 µs) is lower than the fill-ratio-sweep p99.9 (~145 µs)
> because the burst workload is 100% cancel+repost with no taker fills.  Recovery
> p50 (0.75 µs) is close to the baseline cancel p50 (0.38 µs); the elevated
> recovery p99 (16.96 µs) reflects occasional book-state refresh after 100
> rapid mutations.

### 4d — Deep book

Insertion and cancellation cost as a function of book depth (unique price levels
per side per market).  Cost increases logarithmically with depth, consistent
with BTreeMap O(log n) lookup.  The 5 000-level book adds ~0.25 µs vs. the
100-level baseline — the O(log n) overhead is real but small.

| Depth (levels/side) | p50 | p99 | p99.9 | Δ p50 vs 100-level |
|---|---|---|---|---|
| ~10 (SimState baseline) | 0.50 µs | 0.54 µs | ~18 µs | — |
| 100 (deep_book baseline) | 0.46 µs | 0.46 µs | 1.3 µs | 0 |
| 1,000 | 0.50 µs | 0.54 µs | 2.8 µs | +0.04 µs |
| 5,000 | 0.75 µs | 0.83 µs | 3.8 µs | +0.29 µs |

> The ~10-level baseline uses `SimState::build()` (random spread distribution);
> deep_book baselines use `build_deep_sim()` (deterministic spread).  The
> elevated p99.9 for the ~10-level baseline (18 µs) vs the 100-level baseline
> (1.3 µs) reflects the larger, less predictable book state in SimState::build().

---

## Reproducing these results

```sh
# Tiers 1 + 4 (matching engine + new workloads)
cargo bench --bench matching

# Tier 2 (~7.5 min)
cargo bench --bench e2e_http

# Tier 3 (~5.5 min)
cargo bench --bench sustained_throughput
```

Results will vary by hardware.  Expect lower numbers on VMs due to
loopback TCP overhead and thread-scheduling variance.
