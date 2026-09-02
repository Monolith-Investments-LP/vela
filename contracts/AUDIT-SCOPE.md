# Vela contracts — external audit scope

**Version:** v1.1 (post-hardening pass — commit `e49fd9b`, 2026-09-02)
**Contact:** engineering@monolithsystematic.com
**Repository:** https://github.com/Monolith-Investments-LP/vela

This document is the artefact an external audit firm (Trail of Bits,
Zellic, Spearbit, ChainSecurity) needs before quoting. Everything in
here is subject to change up to engagement kickoff; the intent is to
give a first-pass estimate rather than a fixed statement of work.

---

## Scope

Five in-scope contracts. All Solidity `0.8.28`, Cancun EVM,
`optimizer_runs = 200`.

| Contract | LoC | Purpose | Deployment target |
|---|---|---|---|
| [`VelaSettlement.sol`](src/VelaSettlement.sol) | ~340 | User custody (ETH + ERC20), operator-signed withdrawals, emergency exit, state-root anchoring | mainnet + L2s |
| [`InsuranceFund.sol`](src/InsuranceFund.sol) | ~90 | One-way USDC sink; only `PerpEngine` can drain to cover bad debt | mainnet |
| [`VLPVault.sol`](src/VLPVault.sol) | ~150 | ERC-4626 LP vault with a single strategist; capped strategist pull | mainnet |
| [`SequencerRegistry.sol`](src/SequencerRegistry.sol) | ~145 | Sequencer bond + slashing register for the rotating-sequencer program | mainnet |
| [`PerpEngine.sol`](src/PerpEngine.sol) | ~230 | Operator-signed perp settlement ledger; pulls from `InsuranceFund` on bad debt | mainnet |

**Out of scope for this engagement:**
- The Rust matching engine (audited separately — internal review).
- The `docs/api-reference` OpenAPI spec.
- The frontend (`frontend/`).
- Contracts in `contracts/lib/` (OZ v5.6.1 and forge-std — upstream).

---

## Threat model

Below is the shortlist of adversaries and expected outcomes. Anything
not on this list is out of scope but flagging in the report is welcome.

### 1. Malicious operator (compromise of operator key)

- **What they can do:** sign an arbitrary `withdraw(...)` payload for
  any user, sign an arbitrary `anchorStateRoot(...)`, propose an
  operator rotation, sign perp `settlePosition` deltas.
- **What they cannot do:** pause the contract, unpause it, sign a
  withdrawal against a different chain / different contract (domain
  separation), skip the emergency-exit 7-day timelock, complete
  operator rotation without the 48-hour timelock.

### 2. Malicious owner (compromise of owner key)

- **What they can do:** propose a new operator (48h delayed), rotate
  the guardian, set `sequencerUptimeFeed`, unpause, set the
  `InsuranceFund` address on `PerpEngine`.
- **What they cannot do:** sign withdrawals (only operator can),
  bypass the operator-rotation timelock, drain user balances
  directly.

### 3. Malicious guardian

- **What they can do:** pause the contract (`whenNotPaused` gates
  block deposit + withdraw + anchor).
- **What they cannot do:** unpause (owner-only), rotate operator,
  drain funds. Emergency exit stays available while paused — a
  pause-and-abandon cannot trap user funds.

### 4. Malicious L2 sequencer (Arbitrum, Base, Optimism)

- **What they can do:** try to submit a stale-anchor or withdrawal
  when the sequencer is officially down.
- **Mitigation:** `sequencerUp` modifier fails closed when the
  Chainlink L2 sequencer-uptime feed says `answer != 0` or the feed
  came back up within the last hour (`SEQUENCER_GRACE_PERIOD`). L1
  deployments leave `sequencerUptimeFeed = address(0)` and skip the
  check.

### 5. Public liquidator gaming perp positions

- **What they can do:** trigger `settlePosition` via the operator's
  signature (out of scope for on-chain surface).
- **On-chain surface:** none — perp liquidations are operator-signed.
  The maintenance-margin check happens off-chain; the on-chain
  contract only applies deltas.

### 6. Malicious ERC20 (fee-on-transfer, reverting, malicious return)

- **VelaSettlement:** `depositToken` credits the *requested* amount,
  not the amount actually received. Fee-on-transfer tokens leave the
  contract short by the fee — deposits over-credit. See
  `test_depositTokenOverCreditsFeeOnTransfer`. **Operationally
  mitigated by asset allowlist; no on-chain guard.** Flag if the
  auditor wants a `balanceOf` diff check.
- **All contracts:** SafeERC20 is used for every ERC20 interaction, so
  a token that returns `false` from `transfer` still reverts the tx.

### 7. Reentrancy

- Every fund-moving path is guarded by `nonReentrant`.
- `strategistPull` in VLPVault does not re-enter user paths (calls
  only `IERC20.safeTransfer`), but the pattern is guarded regardless.
- `coverLoss` in InsuranceFund transfers to `PerpEngine`, which does
  not call back — but is still guarded.

---

## Invariants

Enforced by the Foundry test suite (`contracts/test/`, 31 tests
including 2 invariant runs of 256 × 32 = 8_192 calls each). Auditors
should verify these and propose additions.

### VelaSettlement

1. `sum(balances[user][ETH].amount) == address(this).balance` — ETH
   fund conservation. Verified by `invariant_ethConservation`.
2. `sum(balances[user][token].amount) == token.balanceOf(address(this))`
   — ERC20 fund conservation. Verified by `invariant_erc20Conservation`.
3. `usedWithdrawNonces[user][nonce]` is monotonic (never cleared).
   Verified by `test_replayAfterRedepositIsRejected`.
4. A signature valid for deployment A never verifies against
   deployment B (domain separation). Verified by
   `test_crossContractReplayIsRejected`.
5. `initiateEmergencyExit` is idempotent within a 7-day window; a
   second call extends the timelock but does not remove funds.
6. `executeEmergencyExit` is callable while paused (deliberate — pause
   must never trap user funds).

### InsuranceFund

1. Only the address stored in `perpEngine` may call `coverLoss`.
2. `coverLoss` cannot exceed `asset.balanceOf(address(this))`.
3. `governanceDrain` is owner-only; every drain emits an event with
   the destination.

### VLPVault

1. `strategistPull(amount)` reverts when `amount > strategistPullCap`.
2. ERC-4626 accounting: `convertToAssets(convertToShares(x)) == x`
   (subject to rounding-down). Not explicitly tested — OZ's ERC4626
   is the reference implementation; the reviewer should confirm.
3. All entry/exit ERC-4626 methods respect `whenNotPaused`.

### SequencerRegistry

1. A sequencer that has not initiated unbonding cannot withdraw.
2. `slash` is capped by the sequencer's current `bond` (over-slash is
   silently capped, not reverted).
3. Slashed funds always land in `beneficiary` (non-zero required).

### PerpEngine

1. `settlePosition` cannot be replayed for the same `(user, nonce)`
   pair. Verified by `test_settleReplayRejected`.
2. Losses beyond a user's collateral pull from `InsuranceFund`; the
   settled position is set to zero, not underflowed.
3. `withdrawCollateral` requires an operator signature — the contract
   itself does not compute margin.
4. Operator rotation goes through the 48h timelock. Verified by
   `test_operatorRotationTimelock`.

---

## Prior fuzz + invariant coverage

- Foundry fuzz runs: **10 000** per property test (bumped from 256).
- Invariant runs: **256** with **depth 32** = 8192 calls per invariant
  per property test.
- Coverage:
  - `test/VelaSettlement.t.sol` — 10 tests (including
    `testFuzz_nonceIndependence`).
  - `test/VelaSettlementHardening.t.sol` — 7 tests + 2 invariants +
    fee-on-transfer + reverting-ERC20 mocks.
  - `test/Scaffolds.t.sol` — 12 tests across all four scaffolds.
- Result on `main` at commit `e49fd9b`: **31 passing, 0 failing**.

---

## Known limitations & non-goals

1. **No upgrade path.** All five contracts are immutable. If a bug is
   found post-deployment, the mitigation is to pause + drain via
   governance + redeploy. Users escape via `initiateEmergencyExit`
   (7-day timelock).
2. **VLPVault is single-strategist.** Multi-strategist / operator
   allowlist is v2.
3. **PerpEngine settlement is operator-signed.** Zk-verified
   settlement (design sketched in `docs/perp-zk-settlement.md`) is
   the target for a subsequent audit.
4. **InsuranceFund has no per-depositor accounting.** Depositors
   cannot exit; the fund is one-way. If governance drains, LPs get
   nothing on that transaction — this is intentional (an insurance
   pool that lets contributors exit at any time isn't insurance).
5. **No slashing conditions enforced on-chain.** SequencerRegistry
   trusts governance to compute the slashing offence off-chain; the
   contract only applies the debit.
6. **Malicious ERC20 (fee-on-transfer)**: VelaSettlement's
   `depositToken` over-credits FoT tokens. Operationally mitigated by
   asset allowlist; not fixed in-contract.

---

## Suggested engagement structure

1. **Kickoff meeting** — walk-through of the five contracts + this
   scope doc + threat model.
2. **Static + manual review** — ~2 weeks. Auditors have full access
   to the codebase, prior test runs, and this doc.
3. **Findings report** — categorised by severity (Critical / High /
   Medium / Low / Informational). Each finding pairs with a
   reproduction test where possible.
4. **Fix pass** — engineering addresses findings; auditors verify
   fixes. Any fix that touches the surface should re-run the invariant
   suite.
5. **Publication** — final report published on the auditor's site and
   linked from `README.md` + `docs/security/`.

Expected total: **3–5 weeks** wall-clock, **$25k–$60k USD** depending
on firm and severity of prep. Timeline is compatible with the
BUILDPLAN Q3 mainnet-deploy target.

---

## Deliverables the audit firm should return

1. Full findings report (Markdown + PDF), each finding with:
   - Category, severity, location (file + line).
   - Impact + likelihood.
   - Recommended fix, with sample diff where trivial.
2. A machine-readable JSON version of the findings (for the on-chain
   reputation attester).
3. A signed final SHA of the audited commit.
4. Any invariants added during review, in the form of new Foundry
   tests.

---

*Last updated 2026-09-02 by the engineering team, alongside the
post-audit hardening pass tracked in the 2026-09-02 gap report.*
