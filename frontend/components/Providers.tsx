'use client'

// ---------------------------------------------------------------------------
// Client-side provider tree. Everything below WagmiProvider needs "use
// client" so we split it out of AppShell to keep provider setup separate
// from the visual chrome.
// ---------------------------------------------------------------------------

import { useState, type ReactNode } from 'react'
import { WagmiProvider } from 'wagmi'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { wagmiConfig } from '@/lib/wagmi'
import { AuthProvider } from '@/lib/auth'

export default function Providers({ children }: { children: ReactNode }) {
  // One QueryClient per browser session — memoised via useState so a
  // Fast-Refresh re-render doesn't blow away the in-flight query cache.
  const [queryClient] = useState(() => new QueryClient())

  return (
    <WagmiProvider config={wagmiConfig}>
      <QueryClientProvider client={queryClient}>
        <AuthProvider>{children}</AuthProvider>
      </QueryClientProvider>
    </WagmiProvider>
  )
}
