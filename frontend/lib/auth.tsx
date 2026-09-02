'use client'

// ---------------------------------------------------------------------------
// Vela wallet auth context — thin wrapper over wagmi.
//
// wagmi/viem handles account state, connection, and signing. This module
// keeps the historical AuthContext surface (`address`, `isConnected`,
// `isAuthenticated`, `connect`, `signOut`, `signIn`) so pages don't
// need to change, but delegates every wallet interaction to wagmi hooks.
//
// Persistence
// -----------
// wagmi persists the connected account itself (localStorage under
// `wagmi.store`) and auto-reconnects on mount. We only persist the
// address explicitly to preserve the legacy key for any external tool
// that reads it — but wagmi is the source of truth.
//
// isAuthenticated is deliberately NOT persisted. It tracks the per-WS
// challenge, and a stale bearer that survives a page reload would
// silently diverge from the server view. On refresh, the user's account
// is reconnected by wagmi and `isAuthenticated` starts false.
// ---------------------------------------------------------------------------

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import {
  useAccount,
  useConnect,
  useConnectors,
  useDisconnect,
  useSignMessage,
} from 'wagmi'
import type { VelaWsClient } from './ws'

interface AuthState {
  address: string | null
  isConnected: boolean
  isAuthenticated: boolean
}

export interface AuthContextValue extends AuthState {
  /** Trigger the wagmi connect flow (uses the first available connector,
   * typically the injected wallet). Returns the connected address. */
  connect: () => Promise<string>
  /** Disconnect via wagmi + clear local state. */
  signOut: () => void
  /** Run the WS challenge-response flow. Signs via wagmi's
   * useSignMessage; no direct window.ethereum access. */
  signIn: (wsClient: VelaWsClient) => Promise<void>
}

const PERSISTED_ADDRESS_KEY = 'vela.auth.address'
const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const { address: wagmiAddress, isConnected: wagmiConnected } = useAccount()
  const { connectAsync } = useConnect()
  const connectors = useConnectors()
  const { disconnect } = useDisconnect()
  const { signMessageAsync } = useSignMessage()

  const [isAuthenticated, setIsAuthenticated] = useState(false)

  const address = useMemo(
    () => (wagmiAddress ? wagmiAddress.toLowerCase() : null),
    [wagmiAddress],
  )

  // Mirror the connected address into the legacy localStorage key so
  // external tools that used to read it (e.g. curl helpers, older
  // frontend versions) keep working. wagmi already persists its own
  // state — this is best-effort compat only.
  useEffect(() => {
    if (typeof window === 'undefined') return
    try {
      if (address) {
        window.localStorage.setItem(PERSISTED_ADDRESS_KEY, address)
      } else {
        window.localStorage.removeItem(PERSISTED_ADDRESS_KEY)
      }
    } catch {
      /* private mode / quota — best effort */
    }
  }, [address])

  // If the wallet disconnects mid-session, the WS-authenticated flag has
  // to drop too — the server-side session is tied to the address.
  useEffect(() => {
    if (!wagmiConnected) {
      setIsAuthenticated(false)
    }
  }, [wagmiConnected])

  const connect = useCallback(async (): Promise<string> => {
    const connector = connectors[0]
    if (!connector) {
      throw new Error(
        'No browser wallet detected. Install MetaMask, Rabby, Coinbase Wallet, Brave, or Frame.',
      )
    }
    const result = await connectAsync({ connector })
    const next = result.accounts[0]
    if (!next) throw new Error('Wallet returned no accounts.')
    return next.toLowerCase()
  }, [connectAsync, connectors])

  const signOut = useCallback(() => {
    disconnect()
    setIsAuthenticated(false)
  }, [disconnect])

  const signIn = useCallback(
    (wsClient: VelaWsClient): Promise<void> => {
      if (!address) {
        return Promise.reject(new Error('Wallet not connected.'))
      }
      return new Promise<void>((resolve, reject) => {
        let settled = false
        const unsubscribe = wsClient.onMessage(async (msg) => {
          if (settled) return
          if (msg.type === 'challenge') {
            try {
              const message = `vela:auth:${msg.nonce}`
              const signature = await signMessageAsync({ message })
              wsClient.authChallenge(address, signature, msg.nonce)
            } catch (err) {
              settled = true
              unsubscribe()
              reject(err)
            }
            return
          }
          if (msg.type === 'authenticated') {
            settled = true
            unsubscribe()
            setIsAuthenticated(true)
            resolve()
            return
          }
          if (msg.type === 'error') {
            settled = true
            unsubscribe()
            reject(new Error(msg.message))
          }
        })
        wsClient.requestChallenge()
      })
    },
    [address, signMessageAsync],
  )

  const value: AuthContextValue = {
    address,
    isConnected: wagmiConnected,
    isAuthenticated,
    connect,
    signOut,
    signIn,
  }

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext)
  if (!ctx) throw new Error('useAuth must be used within <AuthProvider>.')
  return ctx
}
