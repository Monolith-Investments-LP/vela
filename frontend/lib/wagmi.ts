// ---------------------------------------------------------------------------
// wagmi + viem configuration.
//
// Uses the injected connector so any EIP-1193 wallet (MetaMask, Rabby,
// Coinbase Wallet, Brave, Frame) works out of the box. WalletConnect
// can be added by dropping in the walletConnect connector — deferred
// until we have a projectId and a decision on relay hosting.
//
// Chain set matches the API's `VELA_CHAIN_ID`:
//   - sepolia (11155111) — public beta today.
//   - mainnet  (1)        — mainnet deploy target.
//
// The transport falls back to public RPC when NEXT_PUBLIC_ALCHEMY_API_URL
// isn't set, but production deploys should point it at an Alchemy /
// QuickNode endpoint.
// ---------------------------------------------------------------------------

import { createConfig, http, injected } from 'wagmi'
import { mainnet, sepolia } from 'wagmi/chains'
// `injected` is re-exported from wagmi's top-level module (originating
// in @wagmi/core), which avoids the Safe / Coinbase / MetaMask-SDK
// barrel in `wagmi/connectors` and its optional peer deps.

const alchemyUrl = process.env.NEXT_PUBLIC_ALCHEMY_API_URL

export const wagmiConfig = createConfig({
  chains: [sepolia, mainnet],
  connectors: [injected()],
  transports: {
    [sepolia.id]: alchemyUrl ? http(alchemyUrl) : http(),
    [mainnet.id]: http(),
  },
  ssr: true,
})

// Re-export so consumers don't need to depend on wagmi/chains directly.
export { mainnet, sepolia }
