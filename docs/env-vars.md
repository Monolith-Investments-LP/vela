# Environment Variables Reference

Every environment variable read by the Vela workspace, grouped by
subsystem. All variables are optional unless marked **REQUIRED**.

For a ready-to-copy template, see [`.env.example`](../.env.example) in
the repo root and [`frontend/.env.example`](../frontend/.env.example).

---

## Core ops

| Variable | Default | Notes |
|---|---|---|
| `PORT` | `3001` | HTTP server port. |
| `SNAPSHOT_DIR` | `/data` | Directory for engine snapshot JSON and WAL. |
| `DA_DIR` | `/data/da` | Directory for content-addressed DA blobs. |
| `VELA_EXPORT_DIR` | `/data/exports` | Historical export target (Parquet, daily trades, L2 snapshots). |
| `ENVIRONMENT` | `development` | `production` switches tracing to JSON output. |
| `RUST_LOG` | `info` | Standard `tracing` env filter. |

## Auth / admin

| Variable | Notes |
|---|---|
| `ADMIN_TOKEN` | **REQUIRED** to unlock admin endpoints and boot the api crate in tests. |

## Chain & contract

| Variable | Default | Notes |
|---|---|---|
| `VELA_CHAIN_ID` | `11155111` (Sepolia) | Numeric chain id used when composing on-chain payloads. |
| `VELA_SETTLEMENT_ADDRESS` | Sepolia address | `VelaSettlement.sol` address. |
| `VELA_CONTRACT_ADDRESS` | — | Legacy alias; kept for compat. Prefer `VELA_SETTLEMENT_ADDRESS`. |
| `VELA_BRIDGE_ALLOWLIST` | `[]` | JSON array of bridge contract addresses accepted for cross-chain deposits. |
| `ALCHEMY_API_URL` | Sepolia demo | RPC endpoint for on-chain reads and anchor tx submission. |
| `OPERATOR_PRIVATE_KEY` | empty | ECDSA key for withdrawal signatures + state-root anchoring. Anchor task disables itself when unset. |
| `OPERATOR_ADDRESS` / `OPERATOR_WALLET_ADDRESS` | empty | Derived public address (either name accepted). |

## Batch dispatcher

| Variable | Default | Notes |
|---|---|---|
| `VELA_BATCH_WINDOW_US` | `500` | Dispatch window in microseconds. |
| `VELA_BATCH_MAX_SIZE` | `256` | Max orders coalesced into a single batch. |
| `VELA_ORDER_CHANNEL_SIZE` | `8192` | Bounded order channel depth. |
| `VELA_FEED_CHANNEL_SIZE` | `4096` | Bounded feed channel depth. |
| `VELA_DISPATCH_TIMEOUT_MS` | `1000` | Max wait for a batch response before failing the request. |
| `VELA_SPEED_BUMP_US` | `0` | IEX-style delay on marketable orders. |

## Price feed

| Variable | Default | Notes |
|---|---|---|
| `VELA_PYTH_ENABLED` | `true` | Enable the Pyth Hermes v2 feed task. |

## Committee (TEOB)

| Variable | Default | Notes |
|---|---|---|
| `VELA_THRESHOLD_T` / `VELA_THRESHOLD_N` | `15` / `21` | BLS threshold decryption ratio. |
| `VELA_COMMITTEE_PUBKEY` | — | Aggregate committee public key (hex). |
| `VELA_COMMITTEE_KEY_{i}` | — | Per-node HMAC auth key (`i` from `0` to `N-1`). |

## Portfolio margin

| Variable | Default | Notes |
|---|---|---|
| `VELA_PM_MAINT_BPS` | `500` | Maintenance margin ratio (bps). |
| `VELA_PM_INITIAL_BPS` | `1000` | Initial margin ratio (bps). |
| `VELA_PM_CORR_ETH_BTC` | `80` | Correlation input for scenario sweep. |
| `VELA_PM_CORR_SOL_BTC` | `60` | Correlation input for scenario sweep. |

## Credit system

| Variable | Default | Notes |
|---|---|---|
| `VELA_CREDIT_WINDOW_MS` | `30000` | Rolling window for credit exposure. |
| `VELA_MAX_CREDIT_PER_BP_MICRO_USDC` | `1_000_000_000` | Credit ceiling per basis-point of MM depth. |

## Verifiability providers

Both default to fail-closed on `ENVIRONMENT=production` when the
required infrastructure vars are unset.

| Variable | Values | Notes |
|---|---|---|
| `ZKVM_PROVIDER` | `placeholder` \| `sp1` | Selects the prover backend. |
| `VELA_PROVER` | same as above | Legacy alias. |
| `VELA_SP1_PROVER_URL` | URL | Succinct Prover Network endpoint (when `sp1`). |
| `VELA_SP1_VERIFIER_URL` | URL | Verifier endpoint. |
| `VELA_SP1_ELF_ID` | hex | Program identifier for the deployed STF. |
| `TEE_PLATFORM` | `placeholder` \| `amd-sev-snp` | Attestation backend. |

## FIX gateway

| Variable | Default | Notes |
|---|---|---|
| `VELA_FIX_BIND` | `0.0.0.0:5001` | TCP bind for the FIX 4.4 acceptor. |
| `VELA_FIX_COMP_ID` | `VELA` | Server-side SenderCompID. |
| `VELA_FIX_HEARTBEAT_S` | `30` | Heartbeat interval. |

## RFQ

| Variable | Default | Notes |
|---|---|---|
| `VELA_RFQ_MAKERS` | empty | JSON array of allowlisted maker addresses. |
| `VELA_RFQ_MAKER_MIN_SCORE_BPS` | `7000` | Minimum reputation score to accept maker quotes. |
| `VELA_RFQ_MIN_NOTIONAL_MICRO` | `250_000_000_000` | Floor at $250k notional. |

## Toxicity gating

| Variable | Default | Notes |
|---|---|---|
| `VELA_TOX_AMBER_THRESHOLD` | `60` | Score above which amber tier engages. |
| `VELA_TOX_RED_THRESHOLD` | `85` | Score above which red tier engages. |
| `VELA_TOX_AMBER_EXTRA_BUMP_US` | `250` | Extra speed-bump µs applied to amber-tier orders. |

## Listings & reputation

| Variable | Default | Notes |
|---|---|---|
| `VELA_LISTING_BOND_MICRO` | `1_000_000_000` | Bond in micro-USDC to propose a market. |
| `VELA_LISTING_CHALLENGE_HOURS` | `48` | Challenge window before a proposed market lists. |
| `VELA_REPUTATION_TTL_MS` | `86_400_000` | Reputation entry decay window (ms). |

## Frontend (`NEXT_PUBLIC_*`)

| Variable | Notes |
|---|---|
| `NEXT_PUBLIC_API_URL` | Engine HTTP endpoint. |
| `NEXT_PUBLIC_WS_URL` | Engine WebSocket endpoint. |
| `NEXT_PUBLIC_CONTRACT_ADDRESS` | `VelaSettlement.sol` address for on-chain reads. |
| `NEXT_PUBLIC_ALCHEMY_API_URL` | Alchemy RPC for client-side reads. |
| `NEXT_PUBLIC_ADMIN_TOKEN` | Only set for internal tools; do not ship. |
