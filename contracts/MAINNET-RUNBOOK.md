# Vela mainnet deploy — runbook

**Target:** Ethereum mainnet, then Base + Arbitrum (L2 sequencer feed
required).
**Prerequisites:** external audit complete + findings addressed, all
five contracts frozen at a single commit SHA, operator + guardian +
owner keys generated on hardware wallets.

This document is the checklist someone can execute on deploy day.
Nothing here is meant to be discovered under time pressure.

---

## Pre-flight (T-7 days)

- [ ] External audit report published + linked from `README.md` and
      `docs/security/`.
- [ ] All Critical / High findings fixed at a single commit SHA
      (annotate the fix commits with `AUDIT-<n>`).
- [ ] Foundry test suite: `forge test` passes at 31/31.
- [ ] Foundry coverage: `forge coverage --report summary` snapshot
      committed to `contracts/coverage.txt`.
- [ ] Operator key generated on a hardware wallet (Ledger). No copies.
- [ ] Guardian key generated on a **separate** hardware wallet.
      Must be a different device from the operator; distinct on-call
      rota (so a stolen ops laptop cannot pause and un-pause).
- [ ] Owner key held by 2-of-3 Safe multisig with three separate
      hardware signers.
- [ ] Bug bounty published on Immunefi. Minimum reward tier:
      $25k Critical.

## Pre-flight (T-24 hours)

- [ ] `contracts/deployments.json` template pre-filled (chain, name,
      commit SHA, deployer address) but not signed.
- [ ] `.env.deploy` staged with `PRIVATE_KEY`, `ETHERSCAN_API_KEY`,
      `RPC_URL` (from Alchemy / QuickNode — no public RPC).
- [ ] `flyctl status --app vela-engine` — engine is on the same
      commit SHA as the contracts.
- [ ] Frontend `NEXT_PUBLIC_CHAIN_ID` in `.env.production` is set
      to the target chain, not Sepolia.
- [ ] Comms plan: Twitter announcement queued, Discord announcement
      queued, dashboard banner ready.

## Deploy — VelaSettlement

```bash
cd contracts
export PRIVATE_KEY=<hardware-signer>
export ETHERSCAN_API_KEY=<...>
export RPC_URL=<alchemy mainnet>

forge script script/Deploy.s.sol \
  --rpc-url $RPC_URL \
  --broadcast \
  --verify \
  --slow \
  -vvv
```

- [ ] Record the deployed address in `deployments.json`.
- [ ] Etherscan verification succeeded (source verified, ABI visible).
- [ ] Deployer immediately calls `transferOwnership(gnosisSafe)` and
      the safe calls `acceptOwnership()` (Ownable2Step).
- [ ] Deployer calls `setGuardian(guardianAddress)` from the safe.

## Deploy — InsuranceFund + PerpEngine

```bash
forge script script/DeployPerp.s.sol \
  --rpc-url $RPC_URL --broadcast --verify --slow -vvv
```

- [ ] `InsuranceFund` deployed; owner = safe.
- [ ] `PerpEngine` deployed; operator = hardware key, owner = safe.
- [ ] `PerpEngine.setInsuranceFund(fund)` called from the safe.
- [ ] `InsuranceFund.setPerpEngine(engine)` called from the safe.
- [ ] Etherscan verification succeeded on both.

## Deploy — VLPVault

```bash
forge script script/DeployVault.s.sol \
  --rpc-url $RPC_URL --broadcast --verify --slow -vvv
```

- [ ] Vault deployed; owner = safe, asset = mainnet USDC.
- [ ] `setStrategist(strategist)` called.
- [ ] `setStrategistPullCap(500_000e6)` called (adjust per policy).
- [ ] Vault is deliberately un-funded on day one; seed happens via a
      controlled LP round.

## Deploy — SequencerRegistry

Skip on day one unless the rotating-sequencer program has already
been socialised. Standalone deploy:

```bash
forge script script/DeploySequencerRegistry.s.sol --broadcast --verify -vvv
```

- [ ] Owner = safe, bond asset = mainnet USDC, minBond = 100_000e6.
- [ ] No sequencer registers on day one — this is infra for the
      rotation program.

## Wiring (T+0)

- [ ] `VelaSettlement.setSequencerFeed(0x…)` **on L2 deploys only**
      (Chainlink L2 sequencer uptime feed address).
- [ ] Fly.io machine env updated:
  - `VELA_CHAIN_ID=1` (or 8453 for Base, 42161 for Arbitrum).
  - `VELA_SETTLEMENT_ADDRESS=<deployed>`.
  - `VELA_CONTRACT_ADDRESS=<deployed>` (legacy alias).
  - `OPERATOR_PRIVATE_KEY=<hardware key>` (secret).
  - `ADMIN_TOKEN=<rotated>`.
  - `ENVIRONMENT=production` — this enables the fail-closed guards
    on ZKVM and TEE. Only flip this after `ZKVM_PROVIDER=sp1` +
    `VELA_SP1_PROVER_URL` are also set; otherwise the api binary
    refuses to boot (by design).
  - `TEE_PLATFORM=amd-sev-snp` (only on attested hardware — fails
    closed otherwise).
- [ ] Fly deploy: `flyctl deploy --remote-only`.
- [ ] Health check green: `curl https://vela-engine.fly.dev/health`.
- [ ] Metrics green: `curl https://vela-engine.fly.dev/metrics | grep
      vela_verifiability_provider` shows `provider="sp1"` and
      `platform="amd-sev-snp"`.
- [ ] Frontend deployed to Vercel prod.

## Announcement

- [ ] Twitter thread published (contract addresses, block explorer
      links, audit report link).
- [ ] Discord `#announcements` post.
- [ ] Dashboard banner switched from "Sepolia beta" to "Mainnet".
- [ ] `README.md` and `docs/` updated with mainnet addresses.
- [ ] Etherscan tag: request an "Official Vela Exchange" tag via
      https://etherscan.io/contactus.

---

## Post-launch (T+24h)

- [ ] Monitor `/metrics` for anomalies: `vela_oracle_stale_reads_total`
      creeping, `vela_perp_liquidations_total` unexpectedly firing,
      `vela_verifiability_provider{provider="placeholder"}` ever
      showing 1 (indicates a silent env flip).
- [ ] No withdrawals attempted with the emergency-exit path (that
      would indicate operator is silent).
- [ ] Ops on-call rota confirmed for the first 30 days.

---

## Rollback / incident response

### Suspected compromise

1. Guardian pauses immediately: `pause()` on `VelaSettlement`,
   `VLPVault`, and `PerpEngine`.
2. Announce on Twitter + Discord + dashboard banner within 15 min.
3. Users can escape via `initiateEmergencyExit` — the pause does not
   block this path (by design).
4. Owner (safe) coordinates fix + verification with the auditors.
5. Owner `unpause()` once the fix has been externally reviewed. Do
   NOT unpause silently.

### Operator key rotation

1. `proposeOperator(newKey)` from the safe.
2. Wait 48 hours (mandatory timelock).
3. `acceptProposedOperator()` from the safe.
4. Update fly.io secret `OPERATOR_PRIVATE_KEY`, redeploy.
5. Confirm new operator signs a test anchor before the old key is
   revoked from the hardware wallet.

### Sequencer down (L2 only)

- No manual action required. `sequencerUp` modifier fails
  `withdraw` / `anchorStateRoot` / `executeEmergencyExit` while the
  Chainlink feed reports `answer != 0` OR the feed came back up in
  the last hour (`SEQUENCER_GRACE_PERIOD`).

---

## Contact tree

| Role | Contact |
|---|---|
| On-call ops | rotation: engineering@monolithsystematic.com |
| Legal | legal@monolithsystematic.com |
| Auditor point-of-contact | (filled at engagement kickoff) |
| Insurance / bug bounty | Immunefi ticketing |

*Last updated 2026-09-02.*
