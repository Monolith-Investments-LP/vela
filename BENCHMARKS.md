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
| CoW delta buffer | ~0.3 μs overhead vs zero-fee fill; instant rollback (FOK rollback: 802 ns) |
| HFT nonce window | 20 concurrent non-sequential nonces accepted; 1.40 μs/order avg |
| Credit auto-cancel | Full oldest-order eviction + new order post: 4.10 μs |
| Fee calculation | ~0.4 μs overhead vs zero-fee fill (3.81 μs vs 3.43 μs) |

## Phase 3 hot-path optimizations (2026-05-20)

Three targeted changes to reduce metadata-clone overhead on the 98% non-fill path:

| Optimization | Component impact |
|---|---|
| `NonceWindow`: `BTreeSet<Nonce>` → `[Nonce; 20]` sorted array | `user_metadata_clone`: 200 ns → 58.2 ns (−71%) |
| `top_depth` computed lazily (only when fills occur) | eliminates `Vec` allocation on every non-fill `match_order` call |
| `matchable_asks_ref` / `matchable_bids_ref`: iterate by reference | eliminates `Order` clones for matching price levels |

## Results

Measured 2026-05-20 on Apple M3.

| Benchmark | Time (p50) | Throughput |
|-----------|-----------|------------|
| `realistic_mm_workload` | 1.066 μs / request | 85.4k ops/sec |
| `post_order_gtc` | 9.13 μs | — |
| `cancel_order` | 10.16 μs | — |
| `fill_order` | 3.80 μs | — |
| `fok_rollback` | 802 ns | — |
| `hft_nonce_window` (20 orders) | 27.98 μs | 715k orders/sec |
| `credit_auto_cancel` | 4.10 μs | — |
| `fee_calculation_overhead/with_fees` | 3.81 μs | — |
| `fee_calculation_overhead/zero_fees` | 3.43 μs | — |
| `latency_percentiles/post_order_raw` (p50) | 1.019 μs | — |
| `component_breakdown/user_metadata_clone` | 58.2 ns | — |
| `component_breakdown/nonce_window_accept` | 19.5 ns | — |
| `component_breakdown/engine_process_post_order` | 1.95 μs | — |

### vs. Pulse (reference open-source DEX engine, measured on Apple M2 Pro)

| Metric | Vela Phase 3 (M3) | Pulse (M2 Pro) |
|--------|-------------------|----------------|
| Full loop latency (p50) | 1.066 μs | 7.92 μs |
| Throughput | 85.4k ops/sec | 125k ops/sec |
| Relative speedup | **7.4×** | baseline |

### Remaining hot-path costs (for future work)

The largest remaining costs on the cancel/re-quote path are:
- `AssetId(String)` clone in `DeltaBuffer::get_balance` — String allocation per balance lookup (fix: `Arc<str>`)
- `open_order_ids: Vec<OrderId>` clone in `UserMetadata` — 40 entries × 8 bytes per MM metadata read (fix: `Arc<[OrderId]>` or in-place delta)

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
