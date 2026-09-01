'use client'

import { AnimatePresence, motion } from 'framer-motion'
import { usePathname } from 'next/navigation'
import { AuthProvider } from '@/lib/auth'
import Nav from '@/components/Nav'
import BetaBanner from '@/components/BetaBanner'

/// Client-side wrapper around every route: owns the auth context, the
/// nav, and the framer-motion page-transition animation. The root
/// layout is a server component that mounts this shell once so that
/// only the interactive pieces below force a client boundary.
export default function AppShell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname()

  return (
    <AuthProvider>
      <div className="relative z-10" style={{ paddingTop: '96px' }}>
        <BetaBanner />
        <Nav />
        <main className="min-h-[calc(100vh-60px)]">
          <AnimatePresence mode="wait">
            <motion.div
              key={pathname}
              initial={{ opacity: 0, y: 16 }}
              animate={{ opacity: 1, y: 0 }}
              exit={{ opacity: 0, y: -16 }}
              transition={{ duration: 0.35, ease: [0.25, 0.1, 0.25, 1] }}
            >
              {children}
            </motion.div>
          </AnimatePresence>
        </main>
      </div>
    </AuthProvider>
  )
}
