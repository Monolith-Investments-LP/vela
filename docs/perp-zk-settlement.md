# Zk-verified perp settlement — design sketch

**Status:** design only. Not implemented. Rough LoE **10–14 weeks** for
one engineer plus the SP1 circuit toolchain + audit.

**Goal.** Remove `operator` as the trust anchor of the on-chain
`PerpEngine.settlePosition(...)` call. Today the operator signs a
`(user, market, pnlDelta, nonce)` payload; the contract trusts that
signature. This document describes replacing the signature with a
zero-knowledge proof that the off-chain matcher computed `pnlDelta`
correctly from a public batch of fills + a public mark-price feed.

Follow-up E from the 2026-09-02 gap audit.

---

## Trust model — before vs after

| | Today (operator-signed) | After (zk-verified) |
|---|---|---|
| Who computes `pnlDelta` | Off-chain matcher | Off-chain matcher |
| Who verifies it on-chain | `ecrecover == operator` | SP1 verifier contract |
| Blast radius of stolen operator key | Full perp book | Cannot forge PnL; still needs to slash sequencer |
| Latency to on-chain finality | 1 tx | 1 tx **+ proving time** (30–60 min) |
| On-chain cost per settle | ~55k gas | ~250–400k gas (SP1 verifier) |

The zk approach trades ~4× more gas + a proving delay for eliminating
the operator-key single point of failure. It composes with the batch
prover for the spot matcher (see `zkvm/src/prover.rs`).

---

## Circuit — public inputs / outputs

The proven statement is:

> Given `(prevRoot, batch, markPriceFeed, fundingIndexFeed)`, the
> matcher's state-transition function produces `(nextRoot, settles)`
> where every entry in `settles` is a legitimate
> `(user, market, pnlDelta, nonce)`.

### Public inputs

- `prevRoot: bytes32` — perp state root at the start of the batch.
- `batchDigest: bytes32` — keccak256 of the ordered `Fill[]` in this
  batch. Same digest the spot matcher already publishes.
- `markPriceFeed: bytes32` — commitment to the mark-price feed for
  every market touched (Merkle root over `(market, price, ts)`).
- `fundingIndexFeed: bytes32` — commitment to the funding-index feed
  (Merkle root over `(market, idx, ts)`).
- `settlesRoot: bytes32` — Merkle root over the `Settle[]` output.
- `nextRoot: bytes32` — perp state root after applying the batch.

### Public outputs (via `settlesRoot`)

Each `Settle` is `(user, market, pnlDelta, nonce)`. On-chain the
verifier accepts a leaf against `settlesRoot` — so a client submits:

```solidity
struct SettleProof {
    address user;
    bytes32 market;
    int256 pnlDelta;
    uint256 nonce;
    bytes32[] merkleProof; // against settlesRoot
}
```

`PerpEngine.applyProvenSettle(SettleProof, zkProof)` verifies:

1. `zkProof` is a valid SP1 proof against the `(prevRoot, batchDigest,
   markPriceFeed, fundingIndexFeed, settlesRoot, nextRoot)` public
   inputs.
2. `SettleProof.merkleProof` proves the `Settle` leaf is in
   `settlesRoot`.
3. `usedSettleNonces[user][nonce]` is not set (replay guard).
4. `prevRoot == currentRoot`, then advances `currentRoot = nextRoot`.

Steps (3) and (4) are the only on-chain state changes beyond the
existing `PerpEngine.settlePosition` semantics.

---

## SP1 program shape

The zk-VM program is a `no_std` Rust binary that:

1. Reads `(prevRoot, batch, markPriceFeed, fundingIndexFeed)` from
   `sp1_zkvm::io::read`.
2. Rebuilds the perp state Merkle root from `prevRoot` via inclusion
   proofs supplied as private inputs.
3. Runs the SAME `apply_fill` / `settle_funding` / `margin_report`
   functions from `perp/src/lib.rs` — the crate is deliberately
   `no_std`-friendly today so the guest can re-use them.
4. Emits `(nextRoot, settlesRoot)` via `sp1_zkvm::io::commit`.

The critical property: the SP1 program is a linear composition of
functions already covered by the perp crate's unit tests. Any
divergence between prover and verifier is a bug in `perp` itself,
which the existing integration tests catch.

---

## Rough size

- Fills per batch: 256 (matches the matcher's `VELA_BATCH_MAX_SIZE`).
- Merkle tree height for `settlesRoot`: 8 (256 leaves).
- Public inputs total: 6 × 32 bytes = 192 bytes.
- Proof size (SP1 STARK, no PLONK wrap): ~2 MB.
- Proof size (with Groth16 wrap for on-chain verify): ~256 bytes.
- Groth16 verifier gas: ~250k gas per verify.

Wall-clock proving latency on Succinct's Prover Network with a batch
of 256 settles: **30–60 min** end-to-end (proving + Groth16 wrap +
tx submit). Compatible with an hourly settlement cadence but not a
per-fill cadence.

---

## Migration path

Ship in phases so the operator-signed path stays live while the
zk-verified path bakes.

### Phase 0 — no_std hardening (2–3 weeks)

- Add `#![no_std]` to `perp/`. Wire up `alloc` where `Vec` etc. show
  up. Verify `cargo test --no-default-features --target
  riscv32im-unknown-none-elf` compiles.
- Fixture: dump 100 test-batches to `perp/tests/fixtures/batch-*.bin`
  so the SP1 program can be tested against known inputs offline.

### Phase 1 — SP1 program skeleton (3–4 weeks)

- New workspace member `perp-zkvm/`. `perp-zkvm/program/src/main.rs`
  is the SP1 guest binary. `perp-zkvm/host/src/lib.rs` is the host
  runner + `ProverClient` wrapper.
- Reuse the `sp1-prover` feature flag pattern from `zkvm/` — the
  host runner uses the deterministic mock path by default and a
  real SP1 Prover Network URL when the feature is on.
- Golden-file tests: given a batch fixture, the guest emits the same
  `(nextRoot, settlesRoot)` as the host running `perp` directly.

### Phase 2 — on-chain verifier (2–3 weeks)

- Add `contracts/src/PerpVerifier.sol` — Groth16 verifier, generated
  from SP1's `sp1-recursion-gnark-ffi` output.
- Add `PerpEngine.applyProvenSettle(...)` that calls the verifier
  and, on success, updates `usedSettleNonces` + `currentRoot`.
- **Keep the operator-signed `settlePosition(...)` alive during the
  migration.** Every call is instrumented so we can measure the
  ratio of signed vs zk settles.
- New env: `PERP_VERIFIER_ADDRESS` on the frontend.

### Phase 3 — cutover (2 weeks + audit)

- External audit of `PerpVerifier.sol` + the guest binary.
- Turn on `ENFORCE_ZK_SETTLE=true` — after a two-week soak the
  operator-signed path emits an event but the contract also requires
  a matching zk proof within 15 minutes (hybrid mode).
- After a further two weeks, remove the operator-signed path entirely
  and delete the operator key from `PerpEngine`.

---

## Non-goals

- Per-fill settlement. Wall-clock proving latency makes this
  infeasible in 2026 (matches BUILDPLAN's Tier 4 deferral note).
- On-chain funding accrual. Funding stays a snapshot input to the
  proof, computed off-chain from mark and index feeds.
- Zk-verified spot settlement — spot uses optimistic-ZK today via
  `zkvm::optimistic`; that's a separate migration.
- Full zkML of the toxicity scorer. Proving cost is 3–4 OOM too high
  in 2026.

---

## Open questions

1. **Do we need a separate committee for the mark-price feed?** If a
   sequencer can inject a fake mark price, the proof verifies a
   correct computation over a corrupted input. Two options:
   - Commit to the Pyth wormhole VAA on-chain and require the guest
     to verify the VAA signature.
   - Use TEOB to hold the mark-price feed accountable.
2. **How do we handle nonce collisions between the two paths during
   Phase 3 hybrid mode?** Simplest: nonces are per-path (`signed`
   vs `zk` prefix). Cleaner but adds a byte to the leaf.
3. **What happens to already-signed but not-yet-applied settles when
   Phase 3 flips?** Grandfather clause: any settle with a signature
   older than the cutover block is still accepted for 30 days.

---

## References

- SP1 docs: https://docs.succinct.xyz/
- SP1 Rust template: https://github.com/succinctlabs/sp1-project-template
- Groth16 on-chain verifier gas: measured 245k for BN254 pairing on
  Cancun EVM (2025 Q4 numbers).
- Existing Vela zk work: `zkvm/src/prover.rs`,
  `zkvm/src/optimistic.rs`.
- BUILDPLAN Tier 4 "Real ZK proving via SP1" — this document is the
  perp-specific slice of that plan.

---

*Last updated 2026-09-02. Author: engineering@monolithsystematic.com.*
