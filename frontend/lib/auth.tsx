'use client'

// ---------------------------------------------------------------------------
// Vela wallet auth context.
//
// Flow:
//   1. connect()           — request wallet accounts, store address, persist
//   2. signIn(wsClient)    — RequestChallenge → sign vela:auth:{nonce} → Auth
//   3. signOut()           — clear session AND persisted address
//
// Persistence
// -----------
// We persist only the connected address in localStorage. The WS-side
// `isAuthenticated` flag is deliberately NOT persisted — that flag tracks
// the freshness of a per-connection challenge/response, and a stale
// bearer that survives page reload would silently diverge from the
// server's view. On refresh the user sees "connected" and clicks "Sign In"
// to re-establish an authenticated WS session.
//
// Wallet detection
// ----------------
// Detection uses the EIP-1193 `window.ethereum` injection, which every
// major browser wallet (MetaMask, Rabby, Coinbase Wallet, Brave, Frame)
// exposes. Fuller wallet abstraction (WalletConnect, Ledger over WC) is
// tracked separately — a wagmi/viem migration is a larger refactor.
// ---------------------------------------------------------------------------

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from 'react'
import type { VelaWsClient } from './ws'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface AuthState {
  address: string | null
  isConnected: boolean
  isAuthenticated: boolean
}

export interface AuthContextValue extends AuthState {
  /** Prompt the wallet, store the returned address, and persist it. */
  connect: () => Promise<string>
  /** Clear all auth state (session + persisted address). */
  signOut: () => void
  /**
   * Run the WS challenge-response flow:
   *   RequestChallenge → receive nonce → personal_sign → Auth → Authenticated
   */
  signIn: (wsClient: VelaWsClient) => Promise<void>
}

const PERSISTED_ADDRESS_KEY = 'vela.auth.address'

function readPersistedAddress(): string | null {
  if (typeof window === 'undefined') return null
  try {
    const raw = window.localStorage.getItem(PERSISTED_ADDRESS_KEY)
    if (!raw) return null
    // Basic shape guard — an obviously-invalid stored value should be
    // ignored rather than surfacing as a "connected" state.
    if (!/^0x[0-9a-f]{40}$/.test(raw)) return null
    return raw
  } catch {
    return null
  }
}

function writePersistedAddress(address: string | null) {
  if (typeof window === 'undefined') return
  try {
    if (address) {
      window.localStorage.setItem(PERSISTED_ADDRESS_KEY, address)
    } else {
      window.localStorage.removeItem(PERSISTED_ADDRESS_KEY)
    }
  } catch {
    // Storage may be unavailable (private mode, quota, etc.) — best effort.
  }
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function AuthProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AuthState>({
    address: null,
    isConnected: false,
    isAuthenticated: false,
  })

  // Rehydrate the persisted address on mount so a refresh keeps the
  // header showing the connected wallet.
  useEffect(() => {
    const stored = readPersistedAddress()
    if (stored) {
      setState({
        address: stored,
        isConnected: true,
        isAuthenticated: false,
      })
    }
  }, [])

  // Listen for wallet-level account changes and drop our session if the
  // user switches accounts or disconnects the site.
  useEffect(() => {
    if (typeof window === 'undefined' || !window.ethereum?.on) return
    const handleAccountsChanged = (accounts: unknown) => {
      if (!Array.isArray(accounts) || accounts.length === 0) {
        writePersistedAddress(null)
        setState({ address: null, isConnected: false, isAuthenticated: false })
        return
      }
      const next = String(accounts[0]).toLowerCase()
      writePersistedAddress(next)
      setState({ address: next, isConnected: true, isAuthenticated: false })
    }
    // EIP-1193 event surface is optional-typed; guard the on/removeListener.
    const eth = window.ethereum as unknown as {
      on?: (event: string, handler: (accounts: unknown) => void) => void
      removeListener?: (event: string, handler: (accounts: unknown) => void) => void
    }
    eth.on?.('accountsChanged', handleAccountsChanged)
    return () => eth.removeListener?.('accountsChanged', handleAccountsChanged)
  }, [])

  const connect = useCallback(async (): Promise<string> => {
    if (typeof window === 'undefined' || !window.ethereum) {
      throw new Error(
        'No browser wallet detected. Install MetaMask, Rabby, or another EIP-1193 wallet.',
      )
    }
    const accounts = (await window.ethereum.request({
      method: 'eth_requestAccounts',
    })) as string[]

    if (!accounts[0]) throw new Error('No accounts returned from wallet.')
    const address = accounts[0].toLowerCase()
    writePersistedAddress(address)
    setState((s) => ({ ...s, address, isConnected: true }))
    return address
  }, [])

  const signOut = useCallback(() => {
    writePersistedAddress(null)
    setState({ address: null, isConnected: false, isAuthenticated: false })
  }, [])

  const signIn = useCallback(
    (wsClient: VelaWsClient): Promise<void> => {
      const { address } = state

      if (!address) {
        return Promise.reject(new Error('Wallet not connected.'))
      }
      if (typeof window === 'undefined' || !window.ethereum) {
        return Promise.reject(new Error('No browser wallet detected.'))
      }

      return new Promise<void>((resolve, reject) => {
        let settled = false

        const unsubscribe = wsClient.onMessage(async (msg) => {
          if (settled) return

          if (msg.type === 'challenge') {
            const { nonce } = msg
            try {
              const message = `vela:auth:${nonce}`
              const signature = (await window.ethereum!.request({
                method: 'personal_sign',
                params: [message, address],
              })) as string

              wsClient.authChallenge(address, signature, nonce)
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
            setState((s) => ({ ...s, isAuthenticated: true }))
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
    [state],
  )

  return (
    <AuthContext.Provider value={{ ...state, connect, signOut, signIn }}>
      {children}
    </AuthContext.Provider>
  )
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext)
  if (!ctx) throw new Error('useAuth must be used within <AuthProvider>.')
  return ctx
}
