# Vela Exchange — Benchmark Methodology

## Hardware

Apple M3 (arm64)

## Methodology

Benchmarks use [Criterion.rs](https://github.com/bheisler/criterion.rs) with 100 samples per benchmark and HTML reports.

The `realistic_mm_workload` benchmark mirrors Pulse's published design:
- 10 order book markets, each filled to 100 orders per side (50 MMs × 2 orders per side)
- 50 market maker accounts, 1 taker account
- Pre-generated 10,000 engine requests:
  - 9,800 MM operations (4,900 cancel/re-quote pairs at new prices — 98%)
  - 200 taker IOC orders crossing the spread (2%)
- All requests processed sequentially against a single engine instance
- Latency = total elapsed time / 10,000 requests

Isolated benchmarks (post, cancel, fill, FOK, nonce, credit, fee) each measure a single `engine.process()` call using `iter_batched(PerIteration)` to create fresh engine state before each measurement without including setup in the timed window.

## Phase 2 improvements measured

| Feature | Measured impact |
|---------|----------------|
| CoW delta buffer | ~0.3 μs overhead vs zero-fee fill; instant rollback (FOK rollback: 841 ns) |
| HFT nonce window | 20 concurrent non-sequential nonces accepted; 1.39 μs/order avg |
| Credit auto-cancel | Full oldest-order eviction + new order post: 4.33 μs |
| Fee calculation | ~0.2 μs overhead vs zero-fee fill (3.47 μs vs 3.30 μs) |

## Results

Measured 2026-04-19 on Apple M3.

| Benchmark | Time (p50) | Throughput |
|-----------|-----------|------------|
| `realistic_mm_workload` | 0.71 μs / request | 725k ops/sec |
| `post_order_gtc` | 10.24 μs | — |
| `cancel_order` | 9.97 μs | — |
| `fill_order` | 3.46 μs | — |
| `fok_rollback` | 841 ns | — |
| `hft_nonce_window` (20 orders) | 27.72 μs | 721k orders/sec |
| `credit_auto_cancel` | 4.33 μs | — |
| `fee_calculation_overhead/with_fees` | 3.47 μs | — |
| `fee_calculation_overhead/zero_fees` | 3.30 μs | — |

### vs. Pulse (reference open-source DEX engine, measured on Apple M2 Pro)

| Metric | Vela Phase 2 (M3) | Pulse (M2 Pro) |
|--------|-------------------|----------------|
| Full loop latency (p50) | 0.71 μs | 7.92 μs |
| Full loop latency (p99) | 0.75 μs | N/A |
| Full loop latency (p99.9) | 1.83 μs | N/A |
| Throughput | 725k ops/sec | 125k ops/sec |
| Relative speedup | **11.2×** | baseline |

### Running benchmarks

```bash
bash scripts/run_benchmarks.sh
# HTML report: engine/target/criterion/report/index.html
```

Or directly:

```bash
cd engine
cargo bench --bench matching_engine_bench
```
