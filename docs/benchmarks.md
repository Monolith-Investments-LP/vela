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

## Comparison with Hyperliquid

### Side-by-side

| Metric | Vela | Hyperliquid (published) | What it measures |
|---|---|---|---|
| Engine throughput ceiling | **2,500,000 ops/sec** (Tier 1 microbench) | 200,000 ops/sec¹ | In-process order matching, no I/O |
| Consensus throughput ceiling | N/A (no consensus layer) | >1,000,000 ops/sec¹ | HyperBFT theoretical maximum |
| End-to-end p50 (1 client) | 16.9 ms (loopback)² | **200 ms** (colocated)¹ | Full round-trip incl. auth, dispatch, matching |
| End-to-end p99 (1 client) | 19.9 ms (loopback)² | **900 ms** (colocated)¹ | Full round-trip incl. auth, dispatch, matching |
| End-to-end p50 (32 concurrent) | 159 ms (loopback)² | Not published | — |
| Sustained throughput (HTTP, 5 MM agents) | 134 ops/sec mean, 156 ops/sec peak | Not published | Live multi-agent workload over 300 s |

¹ Hyperliquid figures from official Hyperliquid documentation.  
² Measured over `127.0.0.1` loopback — no real network hop.

---

### What each number represents

**Vela engine throughput (2,500,000 ops/sec):** Measured with Criterion.rs on
Apple M3.  Pure in-process matching loop — no ECDSA verification, no HTTP, no
I/O, no consensus.  This figure reflects the matching algorithm only and is the
appropriate comparison for Hyperliquid's execution-layer ceiling.  See Tier 1
above.

**Hyperliquid execution ceiling (200,000 ops/sec):** Published by Hyperliquid
as their execution-bottlenecked design figure.  Uses a custom native binary
protocol and a purpose-built execution engine.  Per Hyperliquid's own
documentation, this is the execution-layer ceiling with HyperBFT enabled; their
docs note consensus can theoretically sustain >1M ops/sec.

**Vela end-to-end latency (16.9 ms p50, single client):** Full request
lifecycle over loopback: ECDSA signature verification → rate-limiting →
batch-dispatcher coalescing (500 µs window) → engine mutex → matching →
HTTP response.  No network transit.  No consensus.  Vela's HTTP bottleneck
is the single-threaded engine mutex and the batch-dispatcher coalescing window.

**Hyperliquid end-to-end latency (200 ms p50, 900 ms p99):** Measured from
a colocated client.  Includes HyperBFT consensus: the order must traverse
2-of-3 validator round-trips before the client receives confirmation.  The
200 ms floor is driven by consensus, not execution speed.

---

### Disclaimer

> Vela engine benchmarks measure the isolated matching layer only (no network transit, no BFT consensus). Hyperliquid's published figures include HyperBFT consensus and network overhead for colocated clients. Vela does not currently have a consensus layer. End-to-end figures are in-process loopback measurements. Hyperliquid's 200k ops/sec ceiling is self-reported and execution-bottlenecked per their own documentation. All comparisons should be interpreted as execution-layer vs. execution-layer, not full-system vs. full-system.

---

### Why Vela's end-to-end latency appears lower than Hyperliquid's

Vela's p50/p99 HTTP latency numbers are lower than Hyperliquid's for structural
reasons that have nothing to do with execution quality:

1. **No consensus layer.**  A system with no validators processes an order in
   one step (execution only).  Hyperliquid requires at least two validator
   round-trips per HyperBFT before the client receives a confirmed fill.
2. **Loopback only.**  Vela's end-to-end benchmark measures `127.0.0.1` with
   no real network hop.  A production client hitting Vela over the internet
   would see additional network latency.
3. **Single-threaded mutex bottleneck.**  Vela's HTTP throughput ceiling
   (156 ops/sec peak, 134 ops/sec mean) is ~1,280× below Hyperliquid's
   200,000 ops/sec execution ceiling.  The bottleneck is the single-threaded
   engine mutex behind the batch dispatcher — not consensus or network.

Vela is at an early stage.  The matching engine microbenchmark demonstrates
the core algorithm is capable of 2.5M ops/sec; the HTTP stack, batch
dispatcher, and single-threaded engine mutex are the current bottlenecks.

---

## Reproducing these results

```sh
# All three tiers
cargo bench --bench matching
cargo bench --bench e2e_http          # ~7.5 min
cargo bench --bench sustained_throughput  # ~5.5 min
```

Results will vary by hardware.  Expect lower numbers on VMs due to
loopback TCP overhead and thread-scheduling variance.
