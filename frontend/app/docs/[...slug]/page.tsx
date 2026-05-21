import { notFound } from 'next/navigation'
import type { ReactNode } from 'react'

function H1({ children }: { children: ReactNode }) {
  return (
    <h1
      style={{
        fontFamily: 'var(--font-inter)',
        fontWeight: 700,
        fontSize: '2rem',
        color: '#1A0608',
        marginBottom: '8px',
        lineHeight: 1.2,
      }}
    >
      {children}
    </h1>
  )
}

function H2({ children }: { children: ReactNode }) {
  return (
    <div style={{ marginTop: '48px', marginBottom: '16px' }}>
      <h2
        style={{
          fontFamily: 'var(--font-inter)',
          fontWeight: 600,
          fontSize: '1.3rem',
          color: '#1A0608',
          marginBottom: '8px',
        }}
      >
        {children}
      </h2>
      <div style={{ height: '1px', backgroundColor: 'rgba(26,18,8,0.1)' }} />
    </div>
  )
}

function H3({ children }: { children: ReactNode }) {
  return (
    <h3
      style={{
        fontFamily: 'var(--font-inter)',
        fontWeight: 600,
        fontSize: '1rem',
        color: '#1A0608',
        marginTop: '32px',
        marginBottom: '12px',
      }}
    >
      {children}
    </h3>
  )
}

function P({ children }: { children: ReactNode }) {
  return (
    <p
      style={{
        fontFamily: 'var(--font-inter)',
        fontWeight: 400,
        fontSize: '1rem',
        lineHeight: 1.8,
        color: '#3A2A1A',
        marginBottom: '16px',
      }}
    >
      {children}
    </p>
  )
}

function C({ children }: { children: ReactNode }) {
  return (
    <code
      style={{
        fontFamily: 'var(--font-mono)',
        fontSize: '0.85rem',
        backgroundColor: 'rgba(196,30,58,0.06)',
        color: '#8B0F22',
        padding: '2px 6px',
        borderRadius: '2px',
      }}
    >
      {children}
    </code>
  )
}

function Pre({ children }: { children: ReactNode }) {
  return (
    <pre
      style={{
        backgroundColor: '#F7F5F0',
        border: '1px solid rgba(26,18,8,0.1)',
        padding: '20px 24px',
        fontFamily: 'var(--font-mono)',
        fontSize: '0.85rem',
        color: '#1A0608',
        borderRadius: 0,
        overflowX: 'auto',
        margin: '24px 0',
        lineHeight: 1.7,
        whiteSpace: 'pre',
      }}
    >
      {children}
    </pre>
  )
}

function Note({ children }: { children: ReactNode }) {
  return (
    <div
      style={{
        borderLeft: '3px solid #4A6D9C',
        backgroundColor: 'rgba(74,109,156,0.05)',
        padding: '16px 20px',
        margin: '24px 0',
        fontFamily: 'var(--font-inter)',
        fontSize: '0.9375rem',
        color: '#3A2A1A',
        lineHeight: 1.7,
      }}
    >
      {children}
    </div>
  )
}

function Warning({ children }: { children: ReactNode }) {
  return (
    <div
      style={{
        borderLeft: '3px solid #C41E3A',
        backgroundColor: 'rgba(196,30,58,0.05)',
        padding: '16px 20px',
        margin: '24px 0',
        fontFamily: 'var(--font-inter)',
        fontSize: '0.9375rem',
        color: '#3A2A1A',
        lineHeight: 1.7,
      }}
    >
      {children}
    </div>
  )
}

function Tip({ children }: { children: ReactNode }) {
  return (
    <div
      style={{
        borderLeft: '3px solid #6B8C52',
        backgroundColor: 'rgba(107,140,82,0.05)',
        padding: '16px 20px',
        margin: '24px 0',
        fontFamily: 'var(--font-inter)',
        fontSize: '0.9375rem',
        color: '#3A2A1A',
        lineHeight: 1.7,
      }}
    >
      {children}
    </div>
  )
}

function Ul({ children }: { children: ReactNode }) {
  return (
    <ul
      style={{
        margin: '16px 0',
        paddingLeft: '24px',
        fontFamily: 'var(--font-inter)',
        fontSize: '1rem',
        color: '#3A2A1A',
        lineHeight: 1.8,
      }}
    >
      {children}
    </ul>
  )
}

function Ol({ children }: { children: ReactNode }) {
  return (
    <ol
      style={{
        margin: '16px 0',
        paddingLeft: '24px',
        fontFamily: 'var(--font-inter)',
        fontSize: '1rem',
        color: '#3A2A1A',
        lineHeight: 1.8,
      }}
    >
      {children}
    </ol>
  )
}

function Li({ children }: { children: ReactNode }) {
  return <li style={{ marginBottom: '8px' }}>{children}</li>
}

function Table({ headers, rows }: { headers: string[]; rows: string[][] }) {
  return (
    <div style={{ margin: '24px 0', overflowX: 'auto' }}>
      <table style={{ width: '100%', borderCollapse: 'collapse' }}>
        <thead>
          <tr>
            {headers.map((h) => (
              <th
                key={h}
                style={{
                  backgroundColor: '#F7F5F0',
                  fontFamily: 'var(--font-inter)',
                  fontSize: '0.7rem',
                  fontWeight: 600,
                  textTransform: 'uppercase' as const,
                  letterSpacing: '0.1em',
                  color: '#6B4F2E',
                  padding: '12px 16px',
                  textAlign: 'left' as const,
                  borderBottom: '1px solid rgba(26,18,8,0.1)',
                }}
              >
                {h}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, ri) => (
            <tr key={ri}>
              {row.map((cell, ci) => (
                <td
                  key={ci}
                  style={{
                    fontFamily: 'var(--font-inter)',
                    fontSize: '0.9375rem',
                    color: '#3A2A1A',
                    padding: '12px 16px',
                    borderBottom: '1px solid rgba(26,18,8,0.08)',
                  }}
                >
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

function Subtitle({ children }: { children: ReactNode }) {
  return (
    <p
      style={{
        fontFamily: 'var(--font-inter)',
        fontSize: '1.1rem',
        color: '#6B4F2E',
        lineHeight: 1.6,
        marginBottom: '40px',
        marginTop: '4px',
      }}
    >
      {children}
    </p>
  )
}

function WhatIsVela() {
  return (
    <>
      <H1>What is Vela?</H1>
      <Subtitle>A high-performance verifiable spot DEX built by Monolith Systematic LLC.</Subtitle>

      <H2>The Problem</H2>
      <P>
        Cryptocurrency trading infrastructure exists on a spectrum between two poles. Centralized
        exchanges like Binance and Coinbase deliver the speed and order book depth that professional
        traders need — sub-millisecond latency, tight spreads, reliable fill execution. But they are
        opaque by design. Users must trust that the exchange is matching orders fairly, that their
        balances are accurate, and that the operator is not front-running their flow. There is no
        independent way to verify any of this.
      </P>
      <P>
        Decentralized exchanges, by contrast, are transparent and non-custodial. Every swap is
        verifiable on-chain. But the dominant DEX model — automated market makers (AMMs) — imposes
        serious costs: capital inefficiency, impermanent loss for liquidity providers, and price
        impact that scales poorly with order size. More fundamentally, AMMs are designed around
        passive liquidity rather than active quoting, making them ill-suited for institutional market
        makers who want to express precise two-sided quotes.
      </P>

      <H2>Vela's Approach</H2>
      <P>
        Vela combines the execution model of a centralized exchange with the verifiability guarantees
        of a blockchain. At its core is a central limit order book (CLOB) — the same model used by
        every professional exchange in traditional and crypto markets. Orders are matched by a
        deterministic price-time priority algorithm, the same algorithm that governs the NYSE, Nasdaq,
        and every major derivatives venue.
      </P>
      <P>
        The key difference from a traditional CEX is that every batch of matches is cryptographically
        verifiable. The matching engine publishes state roots and transaction logs to a data
        availability layer, and any party can run a fraud proof to verify that the operator computed
        the correct result. If the operator cheats, the fraud proof detects it.
      </P>

      <H2>Key Performance Numbers</H2>
      <Table
        headers={['Metric', 'Value', 'Context']}
        rows={[
          ['p50 match latency', '1.066 μs', 'Per-operation on realistic MM workload'],
          ['Throughput', '85,400 ops/sec', 'Sustained on Apple M3'],
          ['vs Pulse (baseline)', '7.4× faster', 'Per-operation comparison'],
          ['Markets (beta)', '11', 'Live on public beta'],
        ]}
      />

      <H2>Who Vela Is For</H2>
      <H3>Traders</H3>
      <P>
        Traders who want the UX of a centralized exchange — limit orders, market orders, a live order
        book, immediate fills — without having to trust a centralized operator. Vela provides familiar
        trading mechanics with cryptographic guarantees underneath.
      </P>

      <H3>Market Makers</H3>
      <P>
        Professional market makers who need low-latency execution and capital-efficient quoting. The
        credit system allows MMs to quote up to N× their deposited collateral across all markets
        simultaneously, with atomic enforcement. Private L3 WebSocket feeds give MMs real-time
        visibility into their own fills and balance changes without exposing their positions publicly.
      </P>

      <H3>Researchers and Builders</H3>
      <P>
        Developers building on top of Vela via the HTTP and WebSocket APIs. Researchers studying
        order book dynamics, CLOB matching algorithms, or optimistic-ZK proof systems. The full source
        code is MIT licensed and available on GitHub.
      </P>

      <H2>Current Status</H2>
      <Warning>
        Vela is in public beta. Deposits are trust-based and not settled on-chain. Do not deposit
        significant funds. On-chain settlement and fraud proof verification are on the M6 roadmap.
      </Warning>
      <P>
        The public beta includes 11 markets, full order book matching, live WebSocket feeds, and the
        MM credit system. The zkVM fraud proof crate is implemented and verifiable locally. On-chain
        verification contracts are in development.
      </P>
    </>
  )
}

function ArchitectureOverview() {
  return (
    <>
      <H1>Architecture Overview</H1>
      <Subtitle>A six-crate Rust workspace with a single-threaded matching engine at its core.</Subtitle>

      <H2>System Diagram</H2>
      <Pre>{`┌──────────────────────────────────────────────────────────────────────┐
│                         Vela Architecture                             │
├──────────────────────────────────────────────────────────────────────┤
│                                                                       │
│   Clients (HTTP / WebSocket)                                          │
│        │                                                              │
│        ▼                                                              │
│   ┌──────────┐  ECDSA verify  ┌────────────────────────────────┐    │
│   │   api    │ ─────────────▶ │       engine                   │    │
│   │  (axum)  │                │  (single-threaded event loop)  │    │
│   └──────────┘                └──────────────┬─────────────────┘    │
│                                              │ CommitBatch           │
│                                              ▼                       │
│                               ┌─────────────────────────────────┐   │
│                               │          committer               │   │
│                               │      (async channel)             │   │
│                               └────────┬──────────┬─────────────┘   │
│                                        │          │                  │
│                          ┌─────────────┘          └───────────┐     │
│                          ▼                                     ▼     │
│               ┌────────────────────┐           ┌─────────────────┐  │
│               │       state        │           │    DA Layer     │  │
│               │  (MPT in-memory    │           │ (Celestia /     │  │
│               │   + trie root)     │           │  local file)    │  │
│               └────────────────────┘           └────────┬────────┘  │
│                                                         │            │
│                                                         ▼            │
│                                              ┌──────────────────┐   │
│                                              │      zkvm        │   │
│                                              │  (fraud proofs)  │   │
│                                              └──────────────────┘   │
└──────────────────────────────────────────────────────────────────────┘`}</Pre>

      <H2>Crates</H2>

      <H3>types</H3>
      <P>
        Shared type definitions used across the entire workspace. All prices and quantities are stored
        as 64-bit unsigned integers scaled by 1,000,000 — a fixed-point representation that eliminates
        floating-point rounding errors entirely. A price of <C>3200.50</C> is stored as{' '}
        <C>3200500000</C>. This crate also defines the wire protocol: the request and response
        structures that flow between the API layer and the matching engine, and from the committer to
        downstream consumers.
      </P>

      <H3>engine</H3>
      <P>
        The matching engine. A single-threaded event loop that processes requests in strict arrival
        order. The engine maintains a price-time priority order book for each market, a copy-on-write
        (CoW) cache for atomic execution, and a credit ledger for market maker accounts. Supported
        order types are GTC, IOC, FOK, and Post-Only limit orders, plus market orders. The engine
        never performs I/O — all state mutations are in-memory and deterministic.
      </P>

      <H3>state</H3>
      <P>
        The MPT (Merkle Patricia Trie) state layer. Account balances and order states are stored in a
        trie that produces a cryptographic root after each committed batch. To eliminate trie traversal
        from the hot path, all reads and writes during matching go through an in-memory HashMap cache.
        The trie root is computed only at batch commit time, after the engine has finished processing
        a batch of orders. This design means the trie is never on the critical path for latency.
      </P>

      <H3>api</H3>
      <P>
        The HTTP and WebSocket server. Built on Axum, Rust's composable async web framework. Every
        mutating request — place order, cancel order, withdrawal — requires an ECDSA signature from
        the account owner. The api crate verifies signatures using the k256 crate (secp256k1 curve),
        recovers the signer address, and forwards valid requests to the engine. WebSocket connections
        support both public channels (order book, trades) and authenticated private channels (fills,
        order updates, balance changes).
      </P>

      <H3>committer</H3>
      <P>
        The async batch committer, fully decoupled from the matching engine hot path via a Tokio
        channel. When the engine completes a batch of operations, it sends a <C>CommitBatch</C>{' '}
        message through the channel and immediately continues processing the next request. The
        committer thread receives the batch, computes the MPT root over the new state, and publishes
        the root and transaction log to the configured data availability backend. This decoupling
        ensures that DA publishing latency never affects match latency.
      </P>

      <H3>zkvm</H3>
      <P>
        The optimistic-ZK fraud proof component. Given a pre-batch state snapshot, the zkvm crate
        seeds a fresh matching engine instance, re-executes all requests in the batch, and compares
        the resulting state root and balance deltas to what the committer published. If they match,
        execution was correct. If they diverge, a fraud proof is generated. The on-chain verification
        contract (M6 roadmap) will allow any party to submit this fraud proof and slash the operator.
      </P>

      <H2>Data Flow</H2>
      <P>
        A typical order lifecycle: the client signs an order with their Ethereum wallet and sends it
        via HTTP POST to <C>/orders</C>. The api crate verifies the ECDSA signature and forwards the
        order to the engine. The engine executes the order against the book, produces fills, updates
        balances in the CoW cache, and commits the delta. The delta is sent to the committer, which
        appends it to the MPT and publishes to DA. Concurrently, the api crate pushes fills and order
        updates to subscribed WebSocket clients.
      </P>
    </>
  )
}

function WhitePaper() {
  return (
    <>
      <H1>White Paper</H1>
      <Subtitle>Monolith Research Vol. 2 — published on SSRN under Monolith Systematic LLC.</Subtitle>

      <H2>Overview</H2>
      <P>
        The Vela technical white paper, published as Monolith Research Vol. 2, provides a formal
        treatment of the protocol design, performance benchmarks, and the theoretical foundations of
        the optimistic-ZK verification approach. It is available on SSRN at{' '}
        <strong>ssrn.com/abstract=6579199</strong>.
      </P>

      <H2>Key Contributions</H2>
      <H3>Fixed-Point CLOB Semantics</H3>
      <P>
        The paper formally defines the matching semantics of Vela's CLOB, including the fixed-point
        arithmetic representation, price-time priority ordering, and the conditions under which each
        order type (GTC, IOC, FOK, Post-Only) produces fills or rests in the book. The formal
        semantics are the foundation for the fraud proof system: a fraud proof is valid if and only
        if it demonstrates a deviation from these semantics.
      </P>

      <H3>CoW Cache and Delta Replay</H3>
      <P>
        The copy-on-write execution model allows the engine to attempt order execution speculatively
        and commit or roll back atomically. The paper analyzes the correctness properties of delta
        replay and proves that the resulting state is equivalent to a naive re-execution of all
        orders from genesis, modulo determinism of the ordering.
      </P>

      <H3>MM Credit System</H3>
      <P>
        The credit system formalism defines the invariant that must hold at all times: the total
        notional value of open orders for a market maker, across all markets, must not exceed the
        product of deposited collateral and credit ratio. The paper proves that the engine enforces
        this invariant atomically for every state transition, including the edge cases of partial
        fills and concurrent cancellations.
      </P>

      <H3>Performance Analysis</H3>
      <P>
        Benchmarks are conducted on a realistic market maker workload — continuous two-sided quoting
        across multiple markets with frequent cancel-and-replace. Results are compared against Pulse,
        a reference CLOB implementation, at both the per-operation and throughput levels. Vela
        achieves 1.066 μs p50 match latency and 85,400 ops/sec sustained, approximately 7.4× faster
        than Pulse on a per-operation basis.
      </P>

      <H3>Optimistic-ZK Trust Model</H3>
      <P>
        The paper characterizes the trust assumptions of the current beta and the target trust model
        after on-chain fraud proof verification is deployed. In the current beta, users must trust
        the operator for liveness and correctness. After M6, the trust assumption reduces to: the
        operator can delay withdrawals (liveness failure) but cannot produce incorrect balance states
        without being detectably slashed.
      </P>

      <H2>Citation</H2>
      <Pre>{`Monolith Systematic LLC. (2025). Vela: A High-Performance Verifiable
Spot CLOB DEX. Monolith Research Vol. 2. SSRN.
Available at: https://ssrn.com/abstract=6579199`}</Pre>

      <Note>
        The white paper is updated as the protocol evolves. Check SSRN for the most recent version.
        The version number is printed on the cover page.
      </Note>
    </>
  )
}

function ConnectWallet() {
  return (
    <>
      <H1>Connect Your Wallet</H1>
      <Subtitle>Vela uses Ethereum-compatible wallets. No private keys are ever shared.</Subtitle>

      <Warning>
        Vela is currently in public beta. Deposits are trust-based and not settled on-chain. Do not
        deposit real funds you cannot afford to lose.
      </Warning>

      <H2>Supported Wallets</H2>
      <P>
        Any Ethereum-compatible wallet that supports the <C>personal_sign</C> method (EIP-191) works
        with Vela. MetaMask is the primary tested wallet. WalletConnect-compatible wallets should
        also work but are not yet officially supported.
      </P>

      <H2>Connection Flow</H2>
      <Ol>
        <Li>
          Click <strong>Connect Wallet</strong> in the top navigation bar.
        </Li>
        <Li>
          Your browser wallet (e.g. MetaMask) will prompt you to approve the connection to the Vela
          origin. Approve the connection.
        </Li>
        <Li>
          Vela requests a <em>challenge nonce</em> from the server — a short random hex string unique
          to your session.
        </Li>
        <Li>
          Your wallet is asked to sign the nonce using <C>personal_sign</C>. The signed message is:
          <Pre>{`"vela:auth:{nonce}"`}</Pre>
          The <C>personal_sign</C> method automatically prepends the Ethereum message prefix
          (<C>\x19Ethereum Signed Message:\n{'{len}'}</C>), making it distinct from a transaction
          signature.
        </Li>
        <Li>
          Vela's server receives your signature, recovers the signer address using secp256k1 ECDSA
          recovery, and verifies it matches the address you claimed.
        </Li>
        <Li>
          A session is established. Your wallet address is displayed in the navigation bar, truncated
          for readability (e.g. <C>0x1234…abcd</C>).
        </Li>
      </Ol>

      <H2>What Gets Signed</H2>
      <P>
        The connection flow only asks you to sign a short challenge string — not a transaction. No ETH
        or tokens leave your wallet during connection. The signature proves that you control the
        private key for the claimed address, nothing more.
      </P>
      <P>
        Every subsequent order you place will also require a signature. Order signatures commit to the
        specific market, side, price, quantity, order type, and a nonce that prevents replay attacks.
        See <strong>ECDSA Authentication</strong> for full details on the signing scheme.
      </P>

      <H2>Sessions</H2>
      <P>
        Sessions are per-browser and are not persisted to a server-side database in the current beta.
        If you close the tab and return, you will need to reconnect your wallet. The reconnection
        flow is identical to the initial connection and takes only a few seconds.
      </P>

      <Tip>
        If MetaMask shows a pending signature request but the Vela UI seems stuck, try refreshing
        the page and connecting again. Pending signature requests from prior sessions can occasionally
        block new ones.
      </Tip>

      <H2>Disconnecting</H2>
      <P>
        Click <strong>Disconnect</strong> in the navigation bar to end your session. This clears your
        session state from the browser. Your wallet extension itself remains connected to the Vela
        origin — to fully revoke access, use your wallet's "Connected sites" settings.
      </P>
    </>
  )
}

function DepositFunds() {
  return (
    <>
      <H1>Deposit Funds</H1>
      <Subtitle>
        In the current beta, deposits are trust-based and administered off-chain.
      </Subtitle>

      <Warning>
        Deposits in the Vela public beta are not settled on-chain. Your balance is maintained by the
        Vela operator. Do not deposit funds you cannot afford to lose. On-chain settlement with
        non-custodial guarantees is the M6 roadmap milestone.
      </Warning>

      <H2>How Deposits Work in Beta</H2>
      <P>
        During the public beta, the deposit mechanism is intentionally simple: you send funds to a
        designated address and the operator credits your account balance in the exchange. Your balance
        is tracked in the Vela state layer (MPT) and is visible in your Dashboard. All operations
        against your balance — fills, fees, withdrawals — are reflected there in real time.
      </P>
      <P>
        This trust-based model lets the team focus on matching engine performance and protocol
        correctness before deploying the on-chain settlement contract. The fraud proof system is
        already implemented in the <C>zkvm</C> crate; on-chain enforcement is what remains.
      </P>

      <H2>Supported Assets</H2>
      <P>
        The beta supports USDC as the primary quote asset. Base assets (ETH, BTC, SOL, etc.) are
        represented as synthetic positions rather than direct custody. This will change with on-chain
        settlement, where real assets will be locked in a settlement contract.
      </P>

      <H2>Viewing Your Balance</H2>
      <P>
        After connecting your wallet, navigate to <strong>Dashboard</strong>. The balances panel
        shows:
      </P>
      <Ul>
        <Li>
          <strong>Available</strong> — funds free to use for new orders.
        </Li>
        <Li>
          <strong>Locked</strong> — funds reserved by open orders. Released when orders are cancelled
          or filled.
        </Li>
        <Li>
          <strong>Total</strong> — available + locked.
        </Li>
      </Ul>

      <H2>Withdrawals</H2>
      <P>
        Withdrawals are initiated via the Dashboard or directly via the <C>POST /withdrawals</C> API
        endpoint. Each withdrawal request requires an ECDSA signature from your wallet. In the beta,
        withdrawals are processed manually by the operator and are not guaranteed to be instant.
      </P>

      <Note>
        To request a deposit or withdrawal during the beta, contact the Vela team through the
        official channels listed in the exchange footer. The deposit flow will be self-service after
        the on-chain settlement contract is deployed.
      </Note>
    </>
  )
}

function PlaceFirstOrder() {
  return (
    <>
      <H1>Place Your First Order</H1>
      <Subtitle>A step-by-step walkthrough of placing a limit order on Vela.</Subtitle>

      <H2>Prerequisites</H2>
      <Ul>
        <Li>A connected Ethereum-compatible wallet (see Connect Your Wallet)</Li>
        <Li>A funded account balance (see Deposit Funds)</Li>
      </Ul>

      <H2>Step-by-Step</H2>
      <Ol>
        <Li>
          <strong>Navigate to a market.</strong> From the Markets page, click on any market, for
          example <C>ETH-USDC</C>. You will see the live order book, recent trades, and the order
          entry panel.
        </Li>
        <Li>
          <strong>Connect your wallet.</strong> If not already connected, click{' '}
          <strong>Connect Wallet</strong> in the navigation bar and complete the connection flow.
        </Li>
        <Li>
          <strong>Select Buy or Sell.</strong> The order entry panel has two tabs — Buy (bid side)
          and Sell (ask side). Select the direction you want to trade.
        </Li>
        <Li>
          <strong>Select Limit or Market.</strong> For a limit order, you set the price. For a market
          order, the engine fills at the best available price immediately. Most traders start with a
          limit order.
        </Li>
        <Li>
          <strong>Enter the price.</strong> For a limit order, enter the price you are willing to
          pay (buy) or accept (sell). Prices are in the quote asset (e.g. USDC for ETH-USDC).
        </Li>
        <Li>
          <strong>Enter the size.</strong> Enter the quantity of the base asset you want to buy or
          sell (e.g. 1.5 ETH).
        </Li>
        <Li>
          <strong>Select Time in Force.</strong> Choose GTC (rests until filled or cancelled), IOC
          (fill what you can immediately, cancel the rest), or FOK (fill all or cancel). For most
          traders, GTC is the right choice.
        </Li>
        <Li>
          <strong>Click Submit.</strong> Your wallet will prompt you to sign the order. The signature
          commits to all order parameters: market, side, price, quantity, order type, time in force,
          your address, and a nonce.
        </Li>
        <Li>
          <strong>Order is accepted.</strong> After signing, the order is sent to the engine. If
          accepted, it appears in the order book (for limit GTC orders that do not immediately match)
          or produces an immediate fill.
        </Li>
        <Li>
          <strong>Monitor your order.</strong> Open orders appear in the My Orders panel on the
          market page. When a match occurs, a fill appears in the Trade History panel and your balance
          updates.
        </Li>
      </Ol>

      <H2>Order Signing</H2>
      <P>
        Every order placed on Vela is signed by your wallet. This is not optional — the matching
        engine verifies every signature before accepting any order. You cannot place an order on
        behalf of another account, and the engine will reject any order whose signature does not
        recover to the <C>user</C> address in the request body.
      </P>
      <P>
        The order parameters are serialized to a canonical byte string (see the API Reference for the
        exact format) and passed to <C>personal_sign</C>. The resulting 65-byte signature is included
        in the request body. Latency-sensitive traders using the API directly should pre-serialize the
        signing payload to minimize round-trip time.
      </P>

      <Tip>
        Use GTC limit orders when you are not in a hurry and want price certainty. Use market orders
        only when you need immediate execution and are comfortable accepting the current spread.
      </Tip>

      <H2>Cancelling an Order</H2>
      <P>
        Open orders can be cancelled from the My Orders panel by clicking the cancel icon. Cancellation
        also requires a wallet signature. Once cancelled, the reserved balance is released back to your
        available balance immediately.
      </P>
    </>
  )
}

function UnderstandingOrderStatus() {
  return (
    <>
      <H1>Understanding Order Status</H1>
      <Subtitle>
        The lifecycle of an order — from submission through fill or cancellation.
      </Subtitle>

      <H2>Order States</H2>
      <Table
        headers={['Status', 'Meaning']}
        rows={[
          ['Pending', 'The order has been submitted but not yet processed by the engine.'],
          ['Open', 'The order is resting in the order book, awaiting a counterparty match.'],
          ['Partially Filled', 'Some quantity has been matched. The remainder is still open.'],
          ['Filled', 'The full quantity has been matched. The order is complete.'],
          ['Cancelled', 'The order was cancelled before being fully filled.'],
          ['Rejected', 'The engine refused the order (insufficient balance, invalid signature, etc.).'],
        ]}
      />

      <H2>State Transitions</H2>
      <P>
        When you submit an order, it enters the <strong>Pending</strong> state. The engine processes
        requests in strict arrival order, so under high load a brief pending period is normal.
      </P>
      <P>
        Once the engine processes the order, one of three things happens:
      </P>
      <Ul>
        <Li>
          <strong>Immediate match:</strong> The order crosses the spread and matches against resting
          orders. The matched quantity is filled. For GTC orders, any remaining quantity becomes Open.
          For IOC/FOK, the remainder is cancelled.
        </Li>
        <Li>
          <strong>Rests in book:</strong> The order does not cross the spread. For GTC and Post-Only
          orders, the order enters the Open state and waits for a matching order to arrive.
        </Li>
        <Li>
          <strong>Rejected:</strong> The engine determines the order is invalid — insufficient
          balance, nonce already used, signature mismatch, credit limit exceeded, or Post-Only order
          that would have crossed.
        </Li>
      </Ul>

      <H2>Partial Fills</H2>
      <P>
        When only part of your order can be matched — because there is insufficient opposing liquidity
        at your price — the order becomes <strong>Partially Filled</strong>. The filled portion is
        complete; the unfilled portion remains in the book as Open (for GTC orders). Each partial fill
        generates a separate fill record with its own price and size.
      </P>

      <H2>Viewing Fill History</H2>
      <P>
        Completed fills are visible in the Trade History panel on each market page and in the
        Dashboard. Each fill record shows the market, side, fill price, fill quantity, fee charged,
        and timestamp. Private WebSocket subscribers receive fill events in real time.
      </P>

      <Note>
        Order status updates are pushed via WebSocket in real time. If you are monitoring orders
        programmatically, subscribe to the <C>orders</C> and <C>fills</C> private channels after
        authenticating your WebSocket connection.
      </Note>
    </>
  )
}

function OrderTypes() {
  return (
    <>
      <H1>Order Types</H1>
      <Subtitle>Vela supports limit and market orders, each with distinct execution semantics.</Subtitle>

      <H2>Limit Order</H2>
      <P>
        A limit order instructs the engine to execute at your specified price or better. If no
        matching orders exist at that price when the order arrives, the order rests in the book until
        a counterparty arrives, you cancel the order, or the session ends. Limit orders give you full
        control over the price you pay or receive, at the cost of uncertain fill timing.
      </P>
      <P>
        For buy (bid) limit orders, "better" means a lower price — you may be filled at your limit
        price or below. For sell (ask) limit orders, "better" means a higher price — you may be
        filled at your limit price or above. In practice, fills usually occur exactly at the limit
        price.
      </P>
      <P>
        Limit orders support all four time-in-force options: GTC, IOC, FOK, and Post-Only. See Time
        in Force for details.
      </P>
      <P>
        Limit orders are best for market makers who need precise price control, and for traders who
        want to avoid paying the spread.
      </P>

      <H2>Market Order</H2>
      <P>
        A market order instructs the engine to execute immediately at the best available price,
        walking through the order book as needed to fill the requested quantity. Market orders
        guarantee execution timing but not execution price.
      </P>
      <P>
        The engine fills a market order by matching against resting limit orders on the opposing side,
        starting at the best price and working outward. If there is insufficient liquidity to fill the
        entire quantity, the order is partially filled and the remainder is cancelled (market orders
        never rest in the book).
      </P>
      <P>
        Market orders are best for traders who need immediate execution and are willing to accept the
        current market price, including the spread and any slippage from walking the book.
      </P>

      <H2>Comparison</H2>
      <Table
        headers={['Feature', 'Limit', 'Market']}
        rows={[
          ['Price control', 'Yes — you set the price', 'No — best available price'],
          ['Guaranteed fill', 'No', 'Yes (if liquidity exists)'],
          ['Rests in book', 'Yes (GTC / Post-Only)', 'Never'],
          ['Supports Post-Only', 'Yes', 'No'],
          ['Maker rebate eligible', 'Yes (if resting)', 'No — always taker'],
          ['Slippage risk', 'None', 'Yes — walks the book'],
        ]}
      />

      <Tip>
        For large orders, a market order can produce significant slippage on a thin book. Use a limit
        order instead and monitor the depth chart to understand available liquidity at your price.
      </Tip>
    </>
  )
}

function TimeInForce() {
  return (
    <>
      <H1>Time in Force</H1>
      <Subtitle>
        Time in force controls how long an order stays active and how it handles partial fills.
      </Subtitle>

      <H2>GTC — Good Till Cancelled</H2>
      <P>
        A GTC order rests in the book until it is fully filled, manually cancelled, or the exchange
        session ends. GTC is the default time in force for limit orders and is appropriate for the
        vast majority of trading use cases.
      </P>
      <P>
        When a GTC order is partially matched — for example, only 0.5 ETH of a 1 ETH order is filled
        — the remaining 0.5 ETH stays in the book at the original price, waiting for additional
        counterparty flow.
      </P>

      <H2>IOC — Immediate or Cancel</H2>
      <P>
        An IOC order attempts to fill as much as possible immediately. Any quantity that cannot be
        matched immediately is cancelled — it never rests in the book. IOC is useful when you want to
        take available liquidity at a price but are not willing to wait for the remainder.
      </P>
      <P>
        Example: you submit a limit IOC buy for 2 ETH at $3,200. The best ask has 1.5 ETH available
        at $3,199. The order fills 1.5 ETH immediately. The remaining 0.5 ETH is cancelled. Your
        resulting fill is 1.5 ETH at $3,199.
      </P>

      <H2>FOK — Fill or Kill</H2>
      <P>
        A FOK order must be fully filled immediately or it is cancelled entirely. No partial fills
        are permitted. If the book cannot accommodate the full quantity at the limit price at the
        moment the order arrives, the entire order is rejected.
      </P>
      <P>
        FOK is used when a partial fill would be operationally problematic — for example, a hedging
        strategy that requires exact position sizes. It is the most restrictive time in force.
      </P>

      <H2>Post-Only</H2>
      <P>
        A Post-Only order is accepted only if it would rest in the book as a maker order — that is, if
        it would not immediately match any existing resting order. If the order would cross the spread
        and take liquidity, it is rejected outright.
      </P>
      <P>
        Post-Only is used by market makers who want to guarantee they receive the maker rebate rather
        than paying the taker fee. If market conditions shift and your quote would cross the spread
        before you can cancel it, Post-Only prevents you from accidentally taking liquidity.
      </P>

      <H2>Comparison</H2>
      <Table
        headers={['TIF', 'Rests in book', 'Partial fills', 'Use case']}
        rows={[
          ['GTC', 'Yes', 'Yes', 'Standard resting orders'],
          ['IOC', 'No', 'Yes (immediate only)', 'Take available liquidity now'],
          ['FOK', 'No', 'No', 'All-or-nothing execution'],
          ['Post-Only', 'Yes (or rejected)', 'Yes', 'Maker rebate guarantee'],
        ]}
      />
    </>
  )
}

function FeesAndRebates() {
  return (
    <>
      <H1>Fees and Rebates</H1>
      <Subtitle>Vela uses a maker-rebate / taker-fee model. Specific rates will be published before mainnet.</Subtitle>

      <H2>Maker vs Taker</H2>
      <P>
        Every fill on Vela involves two parties: the <strong>maker</strong>, who had a resting limit
        order in the book, and the <strong>taker</strong>, whose incoming order matched against it.
        These roles determine how fees are assessed.
      </P>
      <Ul>
        <Li>
          <strong>Maker:</strong> Provides liquidity to the book. Earns a rebate — a negative fee
          that increases your balance. Resting GTC and Post-Only limit orders are maker orders when
          they fill.
        </Li>
        <Li>
          <strong>Taker:</strong> Removes liquidity from the book. Pays a fee. Market orders are
          always taker. IOC and FOK orders are taker for the quantity that fills immediately.
        </Li>
      </Ul>

      <H2>Fee Structure</H2>
      <P>
        Specific fee rates will be published in the official fee schedule before mainnet launch. The
        beta operates with placeholder rates. The structure follows the standard exchange model:
      </P>
      <Table
        headers={['Role', 'Fee direction', 'Rate (beta)']}
        rows={[
          ['Maker', 'Rebate (added to balance)', 'TBD — published at mainnet'],
          ['Taker', 'Fee (deducted from balance)', 'TBD — published at mainnet'],
        ]}
      />

      <H2>Fee Calculation</H2>
      <P>
        Fees are calculated on the notional value of each fill: <C>fee = price × size × rate</C>.
        For a taker fill of 1 ETH at $3,200 with a 0.05% taker fee, the fee is $1.60 USDC. This is
        deducted from the fill proceeds (for sells) or added to the cost (for buys).
      </P>
      <P>
        Maker rebates work the same way: for a maker fill of 1 ETH at $3,200 with a 0.02% maker
        rebate, the maker receives $0.64 USDC in addition to the fill proceeds.
      </P>

      <H2>Fee Tiers</H2>
      <P>
        Volume-based fee tiers are planned for mainnet. Market makers with consistently high volume
        may negotiate custom rates. Contact the Vela team through official channels for information
        on institutional fee arrangements.
      </P>

      <Note>
        All fee and rebate amounts are visible in fill records, both in the Dashboard and in private
        WebSocket fill messages. The <C>fee</C> field in a fill message is signed positive for
        taker fees and signed negative for maker rebates.
      </Note>
    </>
  )
}

function ReadingOrderBook() {
  return (
    <>
      <H1>Reading the Order Book</H1>
      <Subtitle>
        The order book is the real-time record of all resting buy and sell orders.
      </Subtitle>

      <H2>Structure</H2>
      <P>
        The order book has two sides:
      </P>
      <Ul>
        <Li>
          <strong>Asks (sell side, top):</strong> Prices at which sellers are willing to sell. Sorted
          ascending — the lowest ask price is at the top of the ask side, closest to the spread.
        </Li>
        <Li>
          <strong>Bids (buy side, bottom):</strong> Prices at which buyers are willing to buy. Sorted
          descending — the highest bid price is at the bottom of the bid side, closest to the spread.
        </Li>
      </Ul>
      <P>
        At each price level, the book shows the <em>aggregate quantity</em> of all resting orders at
        that price. Individual order identity is not exposed in the public feed.
      </P>

      <H2>The Spread</H2>
      <P>
        The spread is the gap between the best ask (lowest sell price) and the best bid (highest buy
        price). It is displayed in both absolute terms (e.g. $1.60) and in basis points (bps), where
        1 bps = 0.01%.
      </P>
      <P>
        The spread represents the minimum round-trip cost of entering and immediately exiting a
        position. Tighter spreads mean better market conditions. Active market makers compete to
        quote tight spreads in exchange for fill flow.
      </P>

      <H2>Depth Bars</H2>
      <P>
        Each price level in the order book displays a horizontal depth bar. The bar width is
        proportional to the quantity at that level relative to the largest level visible. This gives
        a quick visual sense of where order flow is concentrated and where the book is thin.
      </P>

      <H2>Depth Chart</H2>
      <P>
        The depth chart (accessible via the chart panel) shows cumulative liquidity on each side as
        a step function. The X axis is price, the Y axis is cumulative quantity. The bid curve starts
        at the best bid and expands left; the ask curve starts at the best ask and expands right. The
        gap between the two curves at the center is the spread.
      </P>
      <P>
        Steep slopes on the depth chart indicate thin liquidity — a large order at that price range
        would produce significant slippage. Flat sections indicate thick liquidity where large orders
        can be absorbed with minimal price impact.
      </P>

      <H2>Best Bid and Ask</H2>
      <P>
        The best bid is the highest price a buyer is currently willing to pay. The best ask is the
        lowest price a seller is currently willing to accept. These are displayed prominently in the
        order book UI and are used as the reference price for market orders. The mid price —{' '}
        <C>(best_bid + best_ask) / 2</C> — is the fair value estimate commonly used for mark-to-market
        calculations.
      </P>

      <Tip>
        Before placing a large market order, check the depth chart to estimate slippage. A large buy
        order will walk up the ask side; the depth chart shows how many ETH are available at each
        price increment.
      </Tip>
    </>
  )
}

function DepthChart() {
  return (
    <>
      <H1>Depth Chart</H1>
      <Subtitle>Cumulative order book liquidity visualized as a step function by price.</Subtitle>

      <H2>What the Depth Chart Shows</H2>
      <P>
        The depth chart renders the cumulative liquidity on both sides of the order book as a
        continuous step function. Unlike the raw order book table — which shows quantity at each
        discrete price level — the depth chart accumulates quantities as you move away from the
        mid-price. This makes it easy to answer questions like: how much ETH can I buy before the
        price moves by $50?
      </P>

      <H2>Reading the Chart</H2>
      <Ul>
        <Li>
          <strong>X axis:</strong> Price. The center is the mid-price (midpoint of best bid and best
          ask). The left side extends into the bid prices; the right side extends into the ask prices.
        </Li>
        <Li>
          <strong>Y axis:</strong> Cumulative quantity. For bids, this is the total quantity you
          could sell at that price or better. For asks, the total quantity you could buy at that
          price or better.
        </Li>
        <Li>
          <strong>Bid curve (left, green):</strong> Steps down from the best bid outward. Each step
          represents a price level in the book.
        </Li>
        <Li>
          <strong>Ask curve (right, red):</strong> Steps up from the best ask outward. Each step
          represents a price level in the book.
        </Li>
      </Ul>

      <H2>Estimating Slippage</H2>
      <P>
        To estimate the slippage of a buy order of size Q, find Q on the Y axis of the ask curve
        and read the corresponding X value. That X value is the price at which the Q-th unit of your
        order would fill. The difference between that price and the current best ask is the slippage
        for the marginal unit.
      </P>
      <P>
        For the average fill price across the entire order, integrate across the step function from
        the best ask to the price at quantity Q. The depth chart tooltip (hover over the chart) shows
        this estimate as you move your mouse.
      </P>

      <H2>Interaction with the Depth Chart</H2>
      <P>
        Hovering over either side of the depth chart shows:
      </P>
      <Ul>
        <Li>The cumulative quantity available up to that price</Li>
        <Li>The price at that point</Li>
        <Li>The estimated average fill price for an order of that size</Li>
      </Ul>

      <Note>
        The depth chart updates in real time from the WebSocket book snapshot feed. Rapid changes —
        for example, a large order being placed or cancelled — will visibly shift the curve.
      </Note>
    </>
  )
}

function CreditSystem() {
  return (
    <>
      <H1>MM Credit System</H1>
      <Subtitle>
        Capital-efficient quoting — quote up to N× your deposited collateral across all markets.
      </Subtitle>

      <H2>The Problem with Naive Collateral Requirements</H2>
      <P>
        In a naive DEX design, every open order must be fully collateralized. A market maker quoting
        $10,000 on the bid and $10,000 on the ask across five markets would need $100,000 in
        deposited collateral — even though only some fraction of those orders will actually fill at
        any given time, and the fills on each side partially offset each other.
      </P>
      <P>
        This capital inefficiency is a major barrier to professional market making on DEXes. It forces
        MMs to choose between fewer markets or thinner quotes, degrading liquidity for all traders.
      </P>

      <H2>How Vela's Credit System Works</H2>
      <P>
        The Vela credit system allows each market maker to quote up to <C>deposited_collateral ×
        credit_ratio</C> in total notional value across all markets simultaneously. The credit ratio
        is configurable per user, with a default of 1× (no leverage) and a maximum set by the
        operator.
      </P>
      <P>
        The engine tracks <em>total quoted notional</em>: the sum of <C>price × size</C> across all
        open orders for the account. This total must not exceed <C>deposited × ratio</C> at any point.
        When a new order would breach this limit, it is rejected.
      </P>

      <H2>Example</H2>
      <Pre>{`Deposited collateral:    10,000 USDC
Credit ratio:                5×
Maximum quoted notional: 50,000 USDC

Market maker quotes:
  ETH-USDC bids:  25,000 USDC notional
  BTC-USDC bids:  15,000 USDC notional
  SOL-USDC asks:   8,000 USDC notional
  ─────────────────────────────────────
  Total quoted:   48,000 USDC  ✓ (under 50,000 limit)

  Remaining credit: 2,000 USDC`}</Pre>

      <H2>Atomic Enforcement</H2>
      <P>
        The credit check is performed atomically within the matching engine's CoW cache. When a fill
        occurs that reduces a resting order's size, the credit consumed by that order decreases
        accordingly. If a fill would momentarily push a different MM's total over their limit — an
        edge case in high-frequency scenarios — the engine atomically cancels lower-priority orders
        from that account to restore compliance before committing the batch.
      </P>

      <H2>Credit Utilization Display</H2>
      <P>
        The MM Dashboard displays credit utilization as an arc gauge. The gauge has three zones:
      </P>
      <Table
        headers={['Utilization', 'Zone', 'Behavior']}
        rows={[
          ['0–79%', 'Normal', 'All new orders accepted'],
          ['80–94%', 'Warning', 'Dashboard alerts; new large orders may be rejected'],
          ['95–100%', 'Critical', 'New orders very likely to be rejected; consider cancelling'],
        ]}
      />

      <Note>
        The credit ratio system is designed for active market makers who maintain continuous two-sided
        quotes. Traders who place occasional large directional orders should use a lower credit ratio
        or a ratio of 1× to avoid unexpected rejections.
      </Note>
    </>
  )
}

function ManagingCreditRatio() {
  return (
    <>
      <H1>Managing Credit Ratio</H1>
      <Subtitle>How to set, monitor, and adjust your credit ratio effectively.</Subtitle>

      <H2>Choosing a Credit Ratio</H2>
      <P>
        The credit ratio determines how much total notional you can quote relative to your deposited
        collateral. Choosing the right ratio requires balancing capital efficiency against risk.
      </P>
      <Ul>
        <Li>
          <strong>Ratio 1× (default):</strong> No leverage. Every quoted dollar requires a deposited
          dollar. Safest — you cannot over-quote even if all orders fill simultaneously.
        </Li>
        <Li>
          <strong>Ratio 2–3×:</strong> Suitable for MMs with consistent two-sided flow where bid and
          ask fills offset each other. A fill on both sides simultaneously reduces net exposure.
        </Li>
        <Li>
          <strong>Ratio 4–5×:</strong> Appropriate for highly active MMs with sophisticated risk
          management. At this ratio, a directional market move that fills all your bids could require
          more collateral than you have deposited. Ensure your delta management can handle it.
        </Li>
      </Ul>

      <H2>Setting Your Ratio</H2>
      <P>
        Credit ratio is configured in the MM Dashboard under Account Settings. Changes take effect
        immediately and apply to all subsequent order checks. Lowering your ratio may cause some
        existing open orders to exceed the new limit — those orders will be flagged but not
        automatically cancelled. You should review and cancel them manually to bring your utilization
        under the new limit.
      </P>

      <H2>Monitoring in Real Time</H2>
      <P>
        The MM Dashboard displays live credit utilization. The private WebSocket feed also publishes
        a <C>BalanceUpdate</C> message after every fill, which includes the current credit utilization
        as a percentage. Automated MM strategies should subscribe to this feed and implement their own
        utilization-based order management logic.
      </P>

      <H2>Risk Management Best Practices</H2>
      <Ul>
        <Li>
          Never approach 100% utilization in a volatile market. Volatility increases the probability
          of consecutive directional fills, which can spike utilization rapidly.
        </Li>
        <Li>
          Use Post-Only orders when possible. A Post-Only rejection is preferable to an accidental
          taker fill that moves your utilization unexpectedly.
        </Li>
        <Li>
          Implement automated circuit breakers in your quoting strategy that pause new order
          placement when utilization exceeds a threshold (e.g. 85%).
        </Li>
        <Li>
          Periodically reconcile your open orders against your utilization. Stale resting orders that
          are far from the market consume credit without generating fills.
        </Li>
      </Ul>

      <Warning>
        The credit system does not protect you from losses. It only controls how much you can quote.
        If all your bid orders fill in a falling market, you will hold the resulting positions
        regardless of your credit ratio.
      </Warning>
    </>
  )
}

function PrivateL3Feeds() {
  return (
    <>
      <H1>Private L3 Feeds</H1>
      <Subtitle>
        Real-time fills, order updates, and balance changes — visible only to your authenticated
        session.
      </Subtitle>

      <H2>What Are Private Feeds?</H2>
      <P>
        Public WebSocket channels broadcast anonymized aggregate data: order book snapshots and
        anonymized trade events. Private channels give authenticated market makers and traders access
        to their own order-level data — every fill, every order status change, and every balance
        update — in real time, as it happens.
      </P>
      <P>
        Private data is never included in public feeds. Other users cannot see your fills, your order
        sizes, or your balance. The L3 designation refers to order-level granularity: while public
        L2 data shows aggregated quantities at each price level, L3 data includes individual order
        events.
      </P>

      <H2>Authentication Flow</H2>
      <Ol>
        <Li>
          Connect to the WebSocket endpoint: <C>wss://vela-engine.fly.dev/ws</C>
        </Li>
        <Li>
          Send a challenge request:
          <Pre>{`{ "type": "request_challenge" }`}</Pre>
        </Li>
        <Li>
          The server responds with a nonce:
          <Pre>{`{ "type": "challenge", "nonce": "a3f8c1d2e4b5..." }`}</Pre>
        </Li>
        <Li>
          Sign the nonce using your wallet: <C>personal_sign("vela:auth:{'{nonce}'}")</C>
        </Li>
        <Li>
          Send the auth message:
          <Pre>{`{
  "type": "auth",
  "address": "0xYourAddress",
  "signature": "0x..."
}`}</Pre>
        </Li>
        <Li>
          The server recovers your address from the signature and grants private stream access for
          this WebSocket connection.
        </Li>
      </Ol>

      <H2>Nonce Security</H2>
      <P>
        The challenge nonce is single-use and expires after 60 seconds. You cannot reuse a nonce
        from a previous challenge — the server will reject it. Replay attacks are prevented at the
        nonce level: even if someone intercepts your authentication message, they cannot use it again.
      </P>

      <H2>Private Message Types</H2>
      <H3>Fill</H3>
      <P>
        Sent when one of your resting orders is matched. Contains the fill price, size, side, fee,
        and the order ID.
      </P>
      <Pre>{`{
  "type": "fill",
  "order_id": "ord_abc123",
  "market": "ETH-USDC",
  "side": "bid",
  "price": "3200.00",
  "size": "1.000000",
  "fee": "-0.64",
  "timestamp": 1734567890
}`}</Pre>

      <H3>OrderUpdate</H3>
      <P>
        Sent when one of your orders changes status — accepted, partially filled, cancelled, or
        rejected.
      </P>

      <H3>BalanceUpdate</H3>
      <P>
        Sent after any event that changes your account balance, including fills, fees, rebates, and
        deposits. Contains per-asset available and locked amounts.
      </P>

      <Note>
        Private channels require re-authentication if the WebSocket connection drops and reconnects.
        Your strategy should include reconnection logic that repeats the challenge-response flow
        before resuming order placement.
      </Note>
    </>
  )
}

function MMDashboard() {
  return (
    <>
      <H1>MM Dashboard</H1>
      <Subtitle>Real-time credit utilization, positions, fills, and account state.</Subtitle>

      <H2>Overview</H2>
      <P>
        The MM Dashboard is the primary interface for market makers operating on Vela. It aggregates
        account-level information — credit utilization, open positions, recent fills, and balance
        history — in a single view, updated in real time via the private WebSocket feed.
      </P>

      <H2>Credit Utilization Gauge</H2>
      <P>
        The prominent arc gauge at the top of the dashboard shows your current credit utilization as
        a percentage of your maximum allowed notional. The gauge updates immediately with every fill
        or new order placement. Color coding:
      </P>
      <Ul>
        <Li><strong>Green (0–79%):</strong> Normal operating range.</Li>
        <Li><strong>Amber (80–94%):</strong> Warning — approaching limit.</Li>
        <Li><strong>Red (95–100%):</strong> Critical — new orders likely to be rejected.</Li>
      </Ul>

      <H2>Open Orders Panel</H2>
      <P>
        A real-time table of all open orders across all markets. For each order: market, side, price,
        original quantity, filled quantity, remaining quantity, order type, time in force, and a
        cancel button. Orders can be cancelled individually or all cancelled at once with the "Cancel
        All" button.
      </P>

      <H2>Recent Fills</H2>
      <P>
        A chronological log of your fills, scrollable and filterable by market, side, and time range.
        Each fill entry shows: market, side, fill price, fill size, fee or rebate amount, and the
        order ID that generated the fill.
      </P>

      <H2>Balance Summary</H2>
      <P>
        Per-asset breakdown of available and locked balances. Available balance is immediately usable
        for new orders. Locked balance is reserved by open orders and released when those orders fill
        or are cancelled.
      </P>

      <H2>Quote Spread Monitor</H2>
      <P>
        For each market where you have open orders on both sides, the dashboard shows your current
        quoted spread in basis points. This helps you quickly identify markets where your quotes have
        drifted off mid-market or where your spread has widened due to partial fills.
      </P>

      <Note>
        The dashboard is built on top of the private WebSocket feed. All displayed data is live — there
        is no polling. If you notice a discrepancy between the dashboard and your own order management
        system, check that your WebSocket connection is still authenticated and receiving events.
      </Note>
    </>
  )
}

function ApiAuthentication() {
  return (
    <>
      <H1>Authentication</H1>
      <Subtitle>
        Every mutating API call requires an ECDSA signature. Read-only endpoints are unauthenticated.
      </Subtitle>

      <H2>Signature Scheme</H2>
      <P>
        Vela uses secp256k1 ECDSA — the same elliptic curve used by Ethereum. Signatures are
        produced using the <C>personal_sign</C> method (EIP-191), which prepends the Ethereum
        message prefix before hashing, ensuring signatures cannot be confused with raw transaction
        signatures.
      </P>

      <H2>Signing an Order</H2>
      <P>
        The order parameters are serialized to a canonical byte string in the following format:
      </P>
      <Pre>{`{market_id}:{side}:{price}:{quantity}:{order_type}:{time_in_force}:{user}:{nonce}`}</Pre>
      <P>
        For example:
      </P>
      <Pre>{`"ETH-USDC:bid:3200000000:1000000:limit:gtc:0xAbCd...1234:42"`}</Pre>
      <P>
        This string is passed to <C>personal_sign</C>. The resulting 65-byte signature (r, s, v)
        is hex-encoded and included in the request body as the <C>signature</C> field.
      </P>

      <H2>Nonce</H2>
      <P>
        Each order includes a <C>nonce</C> — a monotonically increasing integer unique to your
        account. The engine rejects any order whose nonce has already been used. Start at 1 and
        increment by 1 for each order. There is no server-side nonce endpoint; you manage your own
        nonce counter.
      </P>

      <H2>Address Recovery</H2>
      <P>
        When the engine receives a signed order, it recovers the signer address from the signature
        and verifies it matches the <C>user</C> field. If they do not match, the order is rejected
        with error code <C>E_SIG_MISMATCH</C>.
      </P>

      <H2>WebSocket Authentication</H2>
      <P>
        WebSocket connections for private channels use a separate challenge-response flow. See
        Private L3 Feeds for the full authentication sequence.
      </P>

      <H2>Example (JavaScript / ethers.js)</H2>
      <Pre>{`import { ethers } from 'ethers'

const provider = new ethers.BrowserProvider(window.ethereum)
const signer = await provider.getSigner()

const message = [
  'ETH-USDC', 'bid', '3200000000', '1000000',
  'limit', 'gtc', await signer.getAddress(), '42'
].join(':')

const signature = await signer.signMessage(message)

const response = await fetch('https://vela-engine.fly.dev/orders', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({
    market_id: 'ETH-USDC',
    side: 'bid',
    price: '3200000000',
    quantity: '1000000',
    order_type: 'limit',
    time_in_force: 'gtc',
    user: await signer.getAddress(),
    nonce: 42,
    signature,
  }),
})`}</Pre>
    </>
  )
}

function HttpEndpoints() {
  return (
    <>
      <H1>HTTP Endpoints</H1>
      <Subtitle>Base URL: https://vela-engine.fly.dev</Subtitle>

      <H2>Health</H2>
      <H3>GET /health</H3>
      <P>Returns engine status. No authentication required.</P>
      <Pre>{`Response 200:
{ "ok": true }`}</Pre>

      <H2>Markets</H2>
      <H3>GET /markets</H3>
      <P>
        Returns all active markets with best bid, best ask, and spread. No authentication required.
      </P>
      <Pre>{`Response 200:
{
  "ok": true,
  "data": [
    {
      "id": "ETH-USDC",
      "base": "ETH",
      "quote": "USDC",
      "best_bid": "3200.000000",
      "best_ask": "3201.600000",
      "spread": "1.600000",
      "spread_bps": "4.99"
    }
  ]
}`}</Pre>

      <H3>GET /markets/:market/book</H3>
      <P>
        Returns the full order book for a market (up to 50 levels per side). No authentication
        required.
      </P>
      <Pre>{`Params: market = "ETH-USDC"

Response 200:
{
  "ok": true,
  "data": {
    "market": "ETH-USDC",
    "bids": [["3200.000000", "1.500000"], ["3199.000000", "3.200000"]],
    "asks": [["3201.600000", "2.000000"], ["3202.000000", "0.800000"]]
  }
}`}</Pre>

      <H2>Account</H2>
      <H3>GET /account/:address/balances</H3>
      <P>Returns balances for a wallet address.</P>
      <Pre>{`Response 200:
{
  "ok": true,
  "data": [
    { "asset": "USDC", "available": "10000.000000", "locked": "500.000000" }
  ]
}`}</Pre>

      <H3>GET /account/:address/orders</H3>
      <P>Returns all open orders for a wallet address.</P>
      <Pre>{`Response 200:
{
  "ok": true,
  "data": [
    {
      "id": "ord_abc123",
      "market": "ETH-USDC",
      "side": "bid",
      "price": "3200.000000",
      "quantity": "1.000000",
      "filled": "0.000000",
      "status": "open",
      "order_type": "limit",
      "time_in_force": "gtc",
      "created_at": 1734567890
    }
  ]
}`}</Pre>

      <H2>Orders</H2>
      <H3>POST /orders</H3>
      <P>Place a new order. Requires a valid ECDSA signature.</P>
      <Pre>{`Request body:
{
  "market_id": "ETH-USDC",
  "side": "bid",
  "price": "3200000000",
  "quantity": "1000000",
  "order_type": "limit",
  "time_in_force": "gtc",
  "user": "0xAbCd...1234",
  "nonce": 42,
  "signature": "0x..."
}

Response 200:
{
  "ok": true,
  "data": { "order_id": "ord_abc123", "status": "open" }
}`}</Pre>

      <H3>POST /orders/cancel</H3>
      <P>Cancel an open order. Requires an ECDSA signature.</P>
      <Pre>{`Request body:
{
  "order_id": "ord_abc123",
  "user": "0xAbCd...1234",
  "nonce": 43,
  "signature": "0x..."
}

Response 200:
{ "ok": true }`}</Pre>

      <H2>Withdrawals</H2>
      <H3>POST /withdrawals</H3>
      <P>Initiate a withdrawal. Requires an ECDSA signature.</P>
      <Pre>{`Request body:
{
  "asset": "USDC",
  "amount": "1000000000",
  "user": "0xAbCd...1234",
  "nonce": 44,
  "signature": "0x..."
}

Response 200:
{ "ok": true, "data": { "withdrawal_id": "wdl_xyz789" } }`}</Pre>

      <Note>
        All prices and quantities in request bodies are integers scaled by 1,000,000. A price of
        $3,200.00 is encoded as <C>3200000000</C>. A quantity of 1.5 ETH is encoded as{' '}
        <C>1500000</C>.
      </Note>
    </>
  )
}

function WebSocketProtocol() {
  return (
    <>
      <H1>WebSocket Protocol</H1>
      <Subtitle>
        Connect to wss://vela-engine.fly.dev/ws for real-time order book, trades, and private data.
      </Subtitle>

      <H2>Connection</H2>
      <Pre>{`const ws = new WebSocket('wss://vela-engine.fly.dev/ws')`}</Pre>

      <P>
        All messages are JSON-encoded objects with a <C>type</C> field in snake_case. The connection
        does not require authentication for public channels. Subscribe to public channels immediately
        after connecting.
      </P>

      <H2>Client → Server Messages</H2>

      <H3>Subscribe</H3>
      <P>Subscribe to one or more channels.</P>
      <Pre>{`{
  "type": "subscribe",
  "channels": ["book:ETH-USDC", "trades:ETH-USDC", "book:BTC-USDC"]
}`}</Pre>

      <H3>Unsubscribe</H3>
      <Pre>{`{ "type": "unsubscribe", "channels": ["book:ETH-USDC"] }`}</Pre>

      <H3>Request Challenge</H3>
      <P>Begin the private channel authentication flow.</P>
      <Pre>{`{ "type": "request_challenge" }`}</Pre>

      <H3>Authenticate</H3>
      <Pre>{`{
  "type": "auth",
  "address": "0xAbCd...1234",
  "signature": "0x..."
}`}</Pre>

      <H3>Ping</H3>
      <Pre>{`{ "type": "ping" }`}</Pre>

      <H2>Server → Client Messages</H2>

      <H3>Book Snapshot</H3>
      <P>Full order book snapshot, sent on subscription and after significant changes.</P>
      <Pre>{`{
  "type": "book_snapshot",
  "market": "ETH-USDC",
  "bids": [["3200.000000", "1.500000"], ["3199.000000", "3.200000"]],
  "asks": [["3201.600000", "2.000000"], ["3202.000000", "0.800000"]],
  "timestamp": 1734567890
}`}</Pre>

      <H3>Trade</H3>
      <P>Anonymized public trade event.</P>
      <Pre>{`{
  "type": "trade",
  "market": "ETH-USDC",
  "price": "3200.000000",
  "size": "0.500000",
  "side": "buy",
  "timestamp": 1734567890
}`}</Pre>

      <H3>Challenge</H3>
      <P>Response to <C>request_challenge</C>. Sign the nonce to authenticate.</P>
      <Pre>{`{ "type": "challenge", "nonce": "a3f8c1d2e4b59f..." }`}</Pre>

      <H3>Fill (private)</H3>
      <P>Sent when one of your orders is matched. Requires authentication.</P>
      <Pre>{`{
  "type": "fill",
  "order_id": "ord_abc123",
  "market": "ETH-USDC",
  "side": "bid",
  "price": "3200.000000",
  "size": "1.000000",
  "fee": "-0.640000",
  "timestamp": 1734567890
}`}</Pre>

      <H3>Order Update (private)</H3>
      <Pre>{`{
  "type": "order_update",
  "order_id": "ord_abc123",
  "status": "partially_filled",
  "filled": "0.500000",
  "remaining": "0.500000",
  "timestamp": 1734567890
}`}</Pre>

      <H3>Balance Update (private)</H3>
      <Pre>{`{
  "type": "balance_update",
  "asset": "USDC",
  "available": "9500.000000",
  "locked": "500.000000",
  "timestamp": 1734567890
}`}</Pre>

      <H3>Pong</H3>
      <Pre>{`{ "type": "pong" }`}</Pre>

      <H2>Available Channels</H2>
      <Table
        headers={['Channel', 'Auth required', 'Description']}
        rows={[
          ['book:{market}', 'No', 'Order book snapshots for the specified market'],
          ['trades:{market}', 'No', 'Anonymized public trade events'],
          ['fills', 'Yes', 'Your fill events across all markets'],
          ['orders', 'Yes', 'Your order status updates'],
          ['balances', 'Yes', 'Your balance changes'],
        ]}
      />
    </>
  )
}

function ErrorCodes() {
  return (
    <>
      <H1>Error Codes</H1>
      <Subtitle>HTTP and WebSocket error responses include a machine-readable error code.</Subtitle>

      <H2>Error Response Format</H2>
      <Pre>{`{
  "ok": false,
  "error": {
    "code": "E_INSUFFICIENT_BALANCE",
    "message": "Insufficient available balance for this order"
  }
}`}</Pre>

      <H2>Order Errors</H2>
      <Table
        headers={['Code', 'Description']}
        rows={[
          ['E_SIG_MISMATCH', 'Recovered signer address does not match the user field'],
          ['E_NONCE_USED', 'This nonce has already been used by this account'],
          ['E_NONCE_INVALID', 'Nonce is not a valid positive integer'],
          ['E_INSUFFICIENT_BALANCE', 'Available balance is too low for this order'],
          ['E_CREDIT_LIMIT', 'Order would exceed the account credit limit'],
          ['E_MARKET_NOT_FOUND', 'The specified market ID does not exist'],
          ['E_INVALID_PRICE', 'Price is zero, negative, or not a valid integer'],
          ['E_INVALID_QUANTITY', 'Quantity is zero, negative, or not a valid integer'],
          ['E_POST_ONLY_REJECTED', 'Post-Only order would have crossed the spread'],
          ['E_FOK_REJECTED', 'FOK order could not be fully filled immediately'],
          ['E_ORDER_NOT_FOUND', 'Order ID does not exist or does not belong to this account'],
          ['E_MARKET_CLOSED', 'The market is currently not accepting orders'],
        ]}
      />

      <H2>Authentication Errors</H2>
      <Table
        headers={['Code', 'Description']}
        rows={[
          ['E_CHALLENGE_EXPIRED', 'The nonce from request_challenge has expired (60s TTL)'],
          ['E_CHALLENGE_USED', 'This challenge nonce has already been used'],
          ['E_AUTH_REQUIRED', 'This channel requires authentication'],
          ['E_AUTH_FAILED', 'Signature recovery failed or address mismatch'],
        ]}
      />

      <H2>System Errors</H2>
      <Table
        headers={['Code', 'Description']}
        rows={[
          ['E_INTERNAL', 'Unexpected internal engine error'],
          ['E_RATE_LIMIT', 'Too many requests from this address'],
          ['E_INVALID_REQUEST', 'Request body is malformed or missing required fields'],
        ]}
      />

      <Tip>
        In automated strategies, handle <C>E_NONCE_USED</C> by incrementing your nonce counter and
        retrying. Never retry on <C>E_SIG_MISMATCH</C> — this indicates a signing error that needs
        investigation.
      </Tip>
    </>
  )
}

function MatchingEngine() {
  return (
    <>
      <H1>Matching Engine</H1>
      <Subtitle>
        A single-threaded event loop with price-time priority matching and copy-on-write execution.
      </Subtitle>

      <H2>Design Principles</H2>
      <P>
        The Vela matching engine is designed around three non-negotiable properties: determinism,
        low latency, and verifiability. Every design decision flows from these constraints.
      </P>
      <Ul>
        <Li>
          <strong>Determinism:</strong> Given the same sequence of requests in the same order, the
          engine must always produce the same output. This is required for the fraud proof system to
          work — a verifier seeding from a snapshot must be able to reproduce the exact same state
          root as the original execution.
        </Li>
        <Li>
          <strong>Low latency:</strong> The engine is single-threaded to avoid synchronization
          overhead. There are no locks, no mutexes, no async I/O on the critical path. All state
          mutations are in-memory.
        </Li>
        <Li>
          <strong>Verifiability:</strong> Every state transition is expressed as a delta that can be
          re-applied deterministically. The MPT root after each batch is a cryptographic commitment
          to the full state.
        </Li>
      </Ul>

      <H2>Order Matching Algorithm</H2>
      <P>
        Vela uses <strong>price-time priority</strong> (FIFO within a price level), the standard
        algorithm used by all professional exchanges:
      </P>
      <Ol>
        <Li>
          The incoming order is validated: balance check, nonce check, signature verification.
        </Li>
        <Li>
          Execution begins in a copy-on-write (CoW) cache — a snapshot of the current book state.
          No mutations to live state occur during execution.
        </Li>
        <Li>
          The engine scans the opposing side of the book for matchable orders, starting at the best
          price and working outward.
        </Li>
        <Li>
          At each price level, orders are filled in FIFO order — the oldest resting order at a given
          price is filled first.
        </Li>
        <Li>
          For each match, balances are updated in the CoW cache: the taker's quote balance decreases,
          the taker's base balance increases, the maker's positions flip. Fees and rebates are applied.
        </Li>
        <Li>
          If the incoming order is GTC and quantity remains after exhausting crossable price levels,
          the remainder rests in the book.
        </Li>
        <Li>
          If IOC: remainder is cancelled. If FOK and full fill is impossible: the entire CoW cache
          is discarded and no changes are committed.
        </Li>
        <Li>
          The CoW cache delta is committed to engine memory. A <C>CommitBatch</C> message is sent to
          the committer channel.
        </Li>
      </Ol>

      <H2>Fixed-Point Arithmetic</H2>
      <P>
        All prices and quantities are stored as 64-bit unsigned integers scaled by 1,000,000. This
        eliminates floating-point rounding errors entirely. A price of $3,200.50 is stored as{' '}
        <C>3200500000</C>. Arithmetic on these values is exact integer arithmetic with no rounding.
      </P>
      <P>
        Fee calculations use the same fixed-point representation. The only rounding that occurs is
        in the final fee amount, which rounds down (in favor of the exchange) to the nearest
        microsatoshi equivalent.
      </P>

      <H2>Performance</H2>
      <P>
        On a realistic market maker workload (continuous two-sided quoting, frequent
        cancel-and-replace), the engine achieves 1.066 μs p50 match latency and 85,400 ops/sec
        sustained on Apple M3. The dominant cost is memory access — the hot path touches only a
        small working set of in-memory structures.
      </P>
    </>
  )
}

function MptStateLayer() {
  return (
    <>
      <H1>MPT State Layer</H1>
      <Subtitle>
        A Merkle Patricia Trie that provides cryptographic state commitments without impacting match
        latency.
      </Subtitle>

      <H2>What Is an MPT?</H2>
      <P>
        A Merkle Patricia Trie (MPT) is a data structure that combines a prefix trie (for efficient
        key lookups) with a Merkle tree (for cryptographic hashing). Every node in the trie is
        identified by the hash of its contents, and the root hash is a compact cryptographic
        commitment to the entire state — all account balances, all open orders, all ledger entries.
        Changing any value anywhere in the trie produces a completely different root hash.
      </P>
      <P>
        Vela uses the MPT to produce state roots that are published to the data availability layer
        after each committed batch. These roots allow any verifier to confirm that the operator
        computed the correct state, without needing to store the full state themselves.
      </P>

      <H2>In-Memory Cache</H2>
      <P>
        Trie traversal is expensive — a naive MPT read or write involves hashing multiple trie nodes
        and potentially disk I/O. Performing these operations on the critical path of order matching
        would add unacceptable latency.
      </P>
      <P>
        To avoid this, all reads and writes during order matching go through an in-memory HashMap
        cache that mirrors the relevant portions of the trie. The matching engine reads and writes
        the HashMap directly, with zero trie traversal. The trie itself is updated only at batch
        commit time, after the engine has finished processing the batch.
      </P>

      <H2>Batch Commit Process</H2>
      <Ol>
        <Li>The matching engine sends a <C>CommitBatch</C> to the committer channel.</Li>
        <Li>
          The committer receives the batch delta — a list of key-value changes (account balances,
          order states).
        </Li>
        <Li>The committer applies the delta to the MPT.</Li>
        <Li>The new MPT root is computed.</Li>
        <Li>The root and batch data are published to the DA backend.</Li>
      </Ol>
      <P>
        Steps 3–5 happen asynchronously, entirely decoupled from the engine's main loop. The engine
        continues processing new requests immediately after sending the <C>CommitBatch</C> message.
      </P>

      <H2>State Keys</H2>
      <P>The MPT stores the following key types:</P>
      <Table
        headers={['Key prefix', 'Value']}
        rows={[
          ['balance:{address}:{asset}', 'Available and locked balance (u128, u128)'],
          ['order:{order_id}', 'Full order state (price, quantity, status, etc.)'],
          ['nonce:{address}', 'Last used nonce for replay protection'],
          ['credit:{address}', 'Credit ratio and current utilization'],
        ]}
      />
    </>
  )
}

function CommitterThread() {
  return (
    <>
      <H1>Committer Thread</H1>
      <Subtitle>
        An async batch committer that decouples DA publishing from the matching engine hot path.
      </Subtitle>

      <H2>Architecture</H2>
      <P>
        The committer is a separate async task (Tokio task) that communicates with the matching
        engine via a bounded channel. The engine sends <C>CommitBatch</C> messages; the committer
        receives them and performs the slow work of MPT root computation and DA publishing.
      </P>
      <P>
        This decoupling is critical. DA publishing — especially to a remote backend like Celestia —
        can take tens or hundreds of milliseconds. If the engine had to wait for DA confirmation
        before processing the next request, match latency would be dominated by network round trips.
        The channel allows the engine to continue at memory-bandwidth speeds while the committer
        works in parallel.
      </P>

      <H2>CommitBatch Message</H2>
      <Pre>{`struct CommitBatch {
    batch_id:    u64,
    deltas:      Vec<(StateKey, StateValue)>,
    fills:       Vec<Fill>,
    timestamp:   u64,
}`}</Pre>

      <H2>Batch Accumulation</H2>
      <P>
        The committer does not necessarily publish one DA blob per engine batch. Under high load, it
        may accumulate multiple engine batches into a single DA submission. This reduces DA costs and
        improves throughput at the cost of slightly higher latency to the published state root.
        The accumulation window is configurable.
      </P>

      <H2>DA Backend Selection</H2>
      <P>
        The committer supports multiple DA backends, selected by configuration:
      </P>
      <Table
        headers={['Backend', 'Description', 'Status']}
        rows={[
          ['Local file', 'Appends batches to a local log file', 'Default in dev'],
          ['Celestia', 'Publishes to the Celestia DA network', 'Supported'],
          ['Ethereum calldata', 'Embeds batch data in Ethereum transactions', 'Roadmap'],
        ]}
      />

      <H2>Fault Handling</H2>
      <P>
        If the DA backend is unavailable, the committer retries with exponential backoff. The matching
        engine is not informed of DA failures — it continues matching. The batch is buffered in the
        committer's internal queue until publication succeeds. A configurable maximum queue depth
        prevents unbounded memory growth; if the queue is full, the committer blocks and the engine's
        channel send will block, applying back-pressure to order processing.
      </P>
    </>
  )
}

function ZkVm() {
  return (
    <>
      <H1>zkVM and Fraud Proofs</H1>
      <Subtitle>
        Optimistic-ZK execution verification — any party can challenge incorrect state transitions.
      </Subtitle>

      <H2>Design</H2>
      <P>
        Vela uses an <strong>optimistic-ZK</strong> design. Batches are published optimistically —
        the operator does not need to produce a validity proof for every batch, which would be
        computationally expensive. Instead, batches are assumed correct unless challenged.
      </P>
      <P>
        Any party with access to the DA layer data can challenge any batch by running the fraud proof
        verification locally. If the operator published an incorrect state root, the verification
        will detect the discrepancy and produce a fraud proof. In the target post-M6 system, this
        fraud proof can be submitted on-chain to slash the operator.
      </P>

      <H2>How Fraud Proof Generation Works</H2>
      <Ol>
        <Li>
          The challenger fetches the pre-batch state snapshot and the batch transaction log from the
          DA layer.
        </Li>
        <Li>
          A fresh matching engine instance is seeded from the pre-batch state snapshot. This recreates
          the exact engine state at the beginning of the challenged batch.
        </Li>
        <Li>
          The challenger re-executes all requests in the batch, in order, using the same deterministic
          matching algorithm.
        </Li>
        <Li>
          The resulting state is hashed to produce a local state root.
        </Li>
        <Li>
          The local state root is compared to the state root published by the operator in the DA
          layer.
        </Li>
        <Li>
          If the roots match: the batch is correct. If they diverge: a fraud proof is generated,
          containing the pre-batch snapshot, the batch data, and the diverging outputs.
        </Li>
      </Ol>

      <H2>Determinism Requirement</H2>
      <P>
        The entire system depends on the matching engine being perfectly deterministic. Given the
        same pre-state and the same ordered sequence of requests, it must produce byte-for-byte
        identical output every time, on every machine. This is why the engine uses fixed-point
        arithmetic, a single thread with no concurrency, and no system calls on the hot path.
      </P>

      <H2>Current Status</H2>
      <P>
        The <C>zkvm</C> crate implements the fraud proof logic and can be run locally against any
        published batch. On-chain verification — where a smart contract validates the fraud proof
        and enforces slashing — is the M6 roadmap milestone. Until then, fraud proofs are
        informational: they prove operator misbehavior but cannot enforce consequences.
      </P>

      <Warning>
        In the current beta, users must trust the operator for correctness. The fraud proof system
        provides transparency but not enforcement. Significant funds should not be deposited until
        on-chain verification is live.
      </Warning>
    </>
  )
}

function DaLayer() {
  return (
    <>
      <H1>DA Layer</H1>
      <Subtitle>
        Data availability — the foundation for verifiability and fraud proofs.
      </Subtitle>

      <H2>Why Data Availability Matters</H2>
      <P>
        Verifiability requires that anyone can access the data needed to verify execution. If the
        operator publishes a state root but withholds the transaction data, no one can verify whether
        the root is correct. The data availability (DA) layer guarantees that transaction data is
        publicly accessible, making fraud proof challenges possible.
      </P>
      <P>
        DA is distinct from execution and settlement. DA guarantees that data exists and is retrievable
        by anyone; it does not guarantee that the data is correct. Correctness is the job of the
        fraud proof system.
      </P>

      <H2>What Gets Published</H2>
      <P>For each committed batch, the committer publishes:</P>
      <Ul>
        <Li>
          <strong>State root:</strong> The MPT root after applying all transactions in the batch.
        </Li>
        <Li>
          <strong>Transaction log:</strong> The ordered list of all requests processed in the batch,
          including order placements, cancellations, and fills.
        </Li>
        <Li>
          <strong>Balance deltas:</strong> The net change in each account balance resulting from the
          batch.
        </Li>
        <Li>
          <strong>Batch metadata:</strong> Batch ID, timestamp, and a hash linking to the previous
          batch (chain structure).
        </Li>
      </Ul>

      <H2>Celestia Integration</H2>
      <P>
        Celestia is the primary DA backend for the Vela mainnet. Celestia is a modular blockchain
        purpose-built for data availability — it uses data availability sampling (DAS) to allow light
        nodes to verify data availability without downloading full blocks. Batches are submitted as
        Celestia blobs in the Vela namespace.
      </P>

      <H2>Local File Backend</H2>
      <P>
        In development and testing, the local file backend appends batches to a structured log file.
        This allows the fraud proof verifier to run locally without a live DA network connection. The
        local backend is also useful for debugging: the log file can be inspected directly to trace
        the state of any account across any batch.
      </P>

      <Note>
        The DA layer stores compressed transaction logs. The decompressed batch data needed for fraud
        proof verification can be significantly larger — budget for this when running the zkvm verifier.
      </Note>
    </>
  )
}

function ForcedInclusion() {
  return (
    <>
      <H1>Forced Inclusion</H1>
      <Subtitle>
        A mechanism for users to bypass the operator and submit transactions directly to the
        settlement layer.
      </Subtitle>

      <H2>The Problem: Operator Censorship</H2>
      <P>
        In the optimistic rollup model, the operator controls which transactions are included in each
        batch. A malicious or negligent operator could choose to ignore certain users — refusing to
        process their orders or withdrawals. This is the censorship problem: even with a fully correct
        and verifiable matching engine, a user can be prevented from accessing their funds if the
        operator simply stops including their transactions.
      </P>

      <H2>Forced Inclusion Design</H2>
      <P>
        Forced inclusion provides an escape hatch. Users can submit transactions directly to the
        settlement contract on Ethereum, bypassing the operator entirely. The operator is required
        to include forced transactions within a defined time window. If the operator fails to include
        a forced transaction within this window, the settlement contract enters a halt mode and the
        operator loses their bond.
      </P>
      <P>
        This mechanism is the primary user protection against operator censorship. Combined with
        fraud proofs (which protect against incorrect execution), forced inclusion closes the
        remaining trust gap: with both mechanisms, a Vela user's funds are safe as long as at least
        one honest party is monitoring the system.
      </P>

      <H2>Current Status</H2>
      <P>
        Forced inclusion is a roadmap item for the M6 settlement milestone. In the current beta,
        withdrawals are processed by the operator manually and there is no forced inclusion mechanism.
        This is one of the primary trust assumptions of the current system.
      </P>

      <H2>Implementation Plan</H2>
      <Ul>
        <Li>
          Users will submit forced transactions to a queue in the settlement contract, with a small
          ETH bond to prevent spam.
        </Li>
        <Li>
          The operator has a fixed window (e.g. 24 hours) to include the forced transaction in a
          batch.
        </Li>
        <Li>
          If the operator fails, the settlement contract enters halt mode: no new batches can be
          submitted until the forced transaction is processed.
        </Li>
        <Li>
          In halt mode, users can initiate emergency withdrawals directly from the settlement
          contract using the last published state root.
        </Li>
      </Ul>

      <Note>
        Until forced inclusion is live, do not rely on Vela for funds you need immediate access to.
        The operator is committed to processing withdrawals promptly, but this commitment is
        operational, not cryptographic.
      </Note>
    </>
  )
}

function EcdsaAuthentication() {
  return (
    <>
      <H1>ECDSA Authentication</H1>
      <Subtitle>
        Every mutating operation requires a secp256k1 ECDSA signature from the account owner.
      </Subtitle>

      <H2>Signature Scheme</H2>
      <P>
        Vela uses the secp256k1 elliptic curve — the same curve used by Ethereum for transaction
        signing. Signatures are produced using the EIP-191 personal sign standard, which prepends
        the Ethereum message prefix before hashing. This prevents signatures from being confused with
        or repurposed as raw transaction signatures.
      </P>
      <Table
        headers={['Property', 'Value']}
        rows={[
          ['Curve', 'secp256k1'],
          ['Hash function', 'keccak256 (after EIP-191 prefix)'],
          ['Signature format', '65 bytes: r (32) + s (32) + v (1)'],
          ['Library (Rust)', 'k256 crate'],
          ['Address derivation', 'keccak256(public_key_bytes)[12..] → 20 bytes'],
        ]}
      />

      <H2>EIP-191 Personal Sign</H2>
      <P>
        The personal sign standard prepends the following prefix to the message before hashing:
      </P>
      <Pre>{`"\x19Ethereum Signed Message:\n" + len(message)`}</Pre>
      <P>
        For example, signing the string <C>"vela:auth:abc123"</C> (16 characters) produces a hash
        of:
      </P>
      <Pre>{`keccak256("\x19Ethereum Signed Message:\n16vela:auth:abc123")`}</Pre>

      <H2>Order Signing</H2>
      <P>
        Order parameters are serialized to a deterministic colon-delimited string and signed. The
        format is:
      </P>
      <Pre>{`"{market}:{side}:{price}:{quantity}:{order_type}:{tif}:{user}:{nonce}"`}</Pre>
      <P>
        The engine deserializes the request, reconstructs this canonical string from the request
        parameters, and verifies that the provided signature recovers to the <C>user</C> address.
        If the signature is invalid or the recovered address does not match, the order is rejected
        with <C>E_SIG_MISMATCH</C>.
      </P>

      <H2>Address Recovery</H2>
      <P>
        ECDSA signatures on secp256k1 support public key recovery: given a message and a valid
        signature, you can deterministically recover the public key that produced the signature.
        Vela uses this property to avoid requiring users to separately register their public keys —
        the address is recovered from every signed request, on the fly.
      </P>

      <H2>Session Authentication</H2>
      <P>
        WebSocket connections for private channels use a separate challenge-response authentication
        that also relies on personal sign. See Private L3 Feeds for the full flow. HTTP requests
        for private endpoints include authentication in the request body via the order signature
        mechanism described above.
      </P>
    </>
  )
}

function ReplayProtection() {
  return (
    <>
      <H1>Replay Protection</H1>
      <Subtitle>
        Nonce-based replay prevention ensures each signed message can only be used once.
      </Subtitle>

      <H2>The Replay Attack Problem</H2>
      <P>
        Without replay protection, an attacker who intercepts a valid signed order could re-submit
        it to the engine repeatedly. Since the signature is valid, the engine would accept each
        submission and create multiple fills — draining the account balance far beyond the trader's
        intent. Replay protection makes each signed message single-use.
      </P>

      <H2>Order Nonces</H2>
      <P>
        Every order includes a <C>nonce</C> field — a strictly positive integer chosen by the
        sender. The engine tracks the set of used nonces per account. When an order arrives, the
        engine checks whether the nonce has already been used. If it has, the order is rejected with{' '}
        <C>E_NONCE_USED</C>.
      </P>
      <P>
        Unlike some systems that require a strictly monotonic nonce, Vela allows any positive integer
        as a nonce, as long as it has not been used before by the same account. The simplest correct
        strategy is to start at 1 and increment by 1 for each order. Nonces do not need to be
        sequential — gaps are allowed.
      </P>

      <H2>WebSocket Challenge Nonces</H2>
      <P>
        Challenge nonces issued by the server for WebSocket authentication are different from order
        nonces. They are server-generated, 16-byte random hex strings, and they expire after 60
        seconds. A challenge nonce can only be used once — the server marks it as consumed
        immediately upon receiving a valid auth message using it.
      </P>

      <H2>Nonce Storage</H2>
      <P>
        Used order nonces are stored in the MPT state layer under the key <C>nonce:{'{address}'}</C>.
        This ensures they are persisted across engine restarts and included in the published state
        root, making them verifiable by the fraud proof system.
      </P>

      <Tip>
        If your strategy runs across multiple processes or machines, ensure only one process manages
        the nonce counter for each account. Concurrent processes using the same account and overlapping
        nonces will produce <C>E_NONCE_USED</C> rejections.
      </Tip>
    </>
  )
}

function FraudProofs() {
  return (
    <>
      <H1>Fraud Proofs</H1>
      <Subtitle>
        Cryptographic evidence of operator misbehavior — computable by any party from public data.
      </Subtitle>

      <H2>What a Fraud Proof Is</H2>
      <P>
        A fraud proof is a compact artifact that demonstrates that the operator published an incorrect
        state root for a specific batch. It contains:
      </P>
      <Ul>
        <Li>The pre-batch state snapshot (the state root before the batch)</Li>
        <Li>The batch transaction log (all requests processed, in order)</Li>
        <Li>The operator-published post-batch state root</Li>
        <Li>The locally-computed post-batch state root</Li>
        <Li>Evidence of divergence (specific accounts or keys where the states differ)</Li>
      </Ul>

      <H2>What Can Be Proven</H2>
      <P>Fraud proofs can detect any of the following operator misbehaviors:</P>
      <Ul>
        <Li>
          <strong>Incorrect fill execution:</strong> The operator matched orders incorrectly — wrong
          price, wrong size, wrong counterparty.
        </Li>
        <Li>
          <strong>Fee theft:</strong> The operator charged a higher fee than the configured rate.
        </Li>
        <Li>
          <strong>Balance inflation:</strong> The operator credited balances without a corresponding
          deposit transaction.
        </Li>
        <Li>
          <strong>Nonce manipulation:</strong> The operator accepted orders with duplicate nonces.
        </Li>
        <Li>
          <strong>Order book manipulation:</strong> The operator applied a different matching
          algorithm (e.g. not price-time priority).
        </Li>
      </Ul>

      <H2>Running the Verifier</H2>
      <P>
        The fraud proof verifier is included in the Vela repository as the <C>zkvm</C> crate. To
        verify a specific batch:
      </P>
      <Pre>{`# Fetch batch data from DA layer and run verifier
cargo run --bin zkvm-verify -- \\
  --batch-id 12345 \\
  --da-backend local \\
  --da-path ./data/batches.log`}</Pre>

      <H2>Current Limitations</H2>
      <P>
        In the public beta, fraud proofs are purely informational. The verifier can detect and prove
        operator misbehavior, but there is no on-chain mechanism to act on the proof. On-chain fraud
        proof verification, combined with operator slashing, is the M6 milestone.
      </P>

      <Note>
        Even without on-chain enforcement, running the verifier locally provides meaningful
        assurance. If the verifier consistently agrees with the operator's published roots, execution
        is correct. Public verification by multiple independent parties builds confidence in the
        operator's honesty.
      </Note>
    </>
  )
}

function TrustModel() {
  return (
    <>
      <H1>Trust Model</H1>
      <Subtitle>
        What you trust today, what you won't need to trust after M6.
      </Subtitle>

      <H2>Current Beta Trust Assumptions</H2>
      <P>
        The Vela public beta requires users to trust the operator for three properties:
      </P>

      <H3>1. Correctness</H3>
      <P>
        You must trust that the operator is running the correct matching engine and applying the
        correct algorithm to all orders. The fraud proof system allows you to verify this, but
        verification is local and informational — there is no on-chain enforcement. If the operator
        publishes an incorrect state root, you can prove it but not enforce consequences.
      </P>

      <H3>2. Liveness</H3>
      <P>
        You must trust that the operator will process your orders and withdrawals in a timely manner.
        There is no forced inclusion mechanism in the beta. If the operator chooses to ignore your
        transactions, you have no cryptographic recourse.
      </P>

      <H3>3. Custody</H3>
      <P>
        Deposits are trust-based. Your balance exists in the Vela state layer, but there is no
        on-chain settlement contract holding real assets in escrow. The operator could, in principle,
        refuse to honor withdrawals.
      </P>

      <H2>Post-M6 Trust Model</H2>
      <P>
        After the on-chain settlement milestone (M6), the trust model changes significantly:
      </P>
      <Table
        headers={['Property', 'Beta (today)', 'Post-M6']}
        rows={[
          ['Correctness', 'Trust operator; verify locally', 'On-chain fraud proofs; operator slashed if wrong'],
          ['Liveness', 'Trust operator to process transactions', 'Forced inclusion; users can bypass operator'],
          ['Custody', 'Trust operator with funds', 'Funds locked in settlement contract; always withdrawable'],
        ]}
      />

      <H2>What You Never Need to Trust</H2>
      <P>
        Even in the current beta, some properties are trust-free:
      </P>
      <Ul>
        <Li>
          <strong>Order authenticity:</strong> The engine cryptographically verifies every order
          signature. The operator cannot place orders on your behalf.
        </Li>
        <Li>
          <strong>Matching algorithm:</strong> The algorithm is deterministic and verifiable. Anyone
          can check that the published state roots are consistent with the transaction logs.
        </Li>
        <Li>
          <strong>Privacy:</strong> Your private fills and balance data are only accessible to
          authenticated WebSocket connections using your wallet. The operator cannot reveal your
          order identity to other users.
        </Li>
      </Ul>

      <Warning>
        Do not deposit significant funds into Vela during the public beta. The trust assumptions
        above represent real risks. The team is committed to minimizing them — but commitment is not
        cryptographic guarantee.
      </Warning>
    </>
  )
}

function Faq() {
  return (
    <>
      <H1>Frequently Asked Questions</H1>

      <H3>Is Vela live?</H3>
      <P>
        Yes. Vela public beta is live. Eleven markets are available. Deposits are currently
        trust-based.
      </P>

      <H3>Are deposits safe?</H3>
      <P>
        Vela is in public beta. Deposits are not settled on-chain and are trust-based. Do not deposit
        significant funds. On-chain settlement is the M6 roadmap milestone.
      </P>

      <H3>What wallets are supported?</H3>
      <P>
        Any Ethereum-compatible wallet with <C>personal_sign</C> support. MetaMask is the primary
        tested wallet. WalletConnect-compatible wallets should work but are not yet officially
        supported.
      </P>

      <H3>What is the credit system?</H3>
      <P>
        Market makers can quote up to N× their deposited collateral across all markets simultaneously.
        The default credit ratio is 1× (no leverage). See the Market Making section for full details.
      </P>

      <H3>Is the source code available?</H3>
      <P>
        Yes. Vela is MIT licensed and available at <strong>github.com/arpjw/vela</strong>.
      </P>

      <H3>What is the white paper?</H3>
      <P>
        The Vela technical white paper is available on SSRN at <strong>ssrn.com/abstract=6579199</strong>{' '}
        under Monolith Research Vol. 2.
      </P>

      <H3>How fast is the matching engine?</H3>
      <P>
        1.066 μs p50 match latency and 85,400 ops/sec on a realistic market maker workload. 7.4×
        faster than Pulse on a per-operation basis.
      </P>

      <H3>What order types are supported?</H3>
      <P>
        Limit orders (GTC, IOC, FOK, Post-Only) and Market orders.
      </P>

      <H3>How are fees structured?</H3>
      <P>
        Maker-rebate, taker-fee model. Specific rates will be published before mainnet launch. See
        Fees and Rebates for details.
      </P>

      <H3>What programming languages can I use with the API?</H3>
      <P>
        Any language with an HTTP client and an Ethereum signing library. The API is language-agnostic.
        JavaScript/TypeScript with ethers.js or viem is the most straightforward. Python with
        web3.py also works.
      </P>

      <H3>How do I report a bug?</H3>
      <P>
        Open an issue on the Vela GitHub repository at <strong>github.com/arpjw/vela</strong>. For
        security vulnerabilities, contact the team through private channels before public disclosure.
      </P>

      <H3>What markets are available?</H3>
      <P>
        Eleven markets are live in the beta. The current market list is available at the{' '}
        <C>GET /markets</C> endpoint and on the Markets page of the exchange.
      </P>

      <H3>Can I use Vela programmatically without the UI?</H3>
      <P>
        Yes. The HTTP and WebSocket APIs are fully documented and designed for programmatic access.
        Market makers in particular are expected to use the API directly rather than the UI. See the
        API Reference section for full documentation.
      </P>
    </>
  )
}

function renderContent(key: string): React.ReactNode {
  switch (key) {
    case 'introduction/what-is-vela':
      return <WhatIsVela />
    case 'introduction/architecture-overview':
      return <ArchitectureOverview />
    case 'introduction/white-paper':
      return <WhitePaper />
    case 'getting-started/connect-wallet':
      return <ConnectWallet />
    case 'getting-started/deposit-funds':
      return <DepositFunds />
    case 'getting-started/place-first-order':
      return <PlaceFirstOrder />
    case 'getting-started/understanding-order-status':
      return <UnderstandingOrderStatus />
    case 'trading/order-types':
      return <OrderTypes />
    case 'trading/time-in-force':
      return <TimeInForce />
    case 'trading/fees-and-rebates':
      return <FeesAndRebates />
    case 'trading/reading-order-book':
      return <ReadingOrderBook />
    case 'trading/depth-chart':
      return <DepthChart />
    case 'market-making/credit-system':
      return <CreditSystem />
    case 'market-making/managing-credit-ratio':
      return <ManagingCreditRatio />
    case 'market-making/private-l3-feeds':
      return <PrivateL3Feeds />
    case 'market-making/mm-dashboard':
      return <MMDashboard />
    case 'api-reference/authentication':
      return <ApiAuthentication />
    case 'api-reference/http-endpoints':
      return <HttpEndpoints />
    case 'api-reference/websocket-protocol':
      return <WebSocketProtocol />
    case 'api-reference/error-codes':
      return <ErrorCodes />
    case 'architecture/matching-engine':
      return <MatchingEngine />
    case 'architecture/mpt-state-layer':
      return <MptStateLayer />
    case 'architecture/committer-thread':
      return <CommitterThread />
    case 'architecture/zk-vm':
      return <ZkVm />
    case 'architecture/da-layer':
      return <DaLayer />
    case 'architecture/forced-inclusion':
      return <ForcedInclusion />
    case 'security/ecdsa-authentication':
      return <EcdsaAuthentication />
    case 'security/replay-protection':
      return <ReplayProtection />
    case 'security/fraud-proofs':
      return <FraudProofs />
    case 'security/trust-model':
      return <TrustModel />
    case 'faq':
      return <Faq />
    default:
      return null
  }
}

export default function DocsPage({ params }: { params: { slug: string[] } }) {
  const key = params.slug.join('/')
  const content = renderContent(key)
  if (content === null) notFound()
  return <>{content}</>
}
