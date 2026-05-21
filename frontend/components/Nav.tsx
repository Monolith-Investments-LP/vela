'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { useState, useEffect } from 'react'
import { motion } from 'framer-motion'
import { useAuth } from '@/lib/auth'
import { Spinner } from '@/components/ui/Spinner'

const LINKS = [
  { href: '/markets',      label: 'Markets'      },
  { href: '/dashboard',    label: 'Dashboard'    },
  { href: '/analytics',    label: 'Analytics'    },
  { href: '/benchmarks',   label: 'Benchmarks'   },
  { href: '/history',      label: 'History'      },
  { href: '/transparency', label: 'Transparency' },
  { href: '/decisions',    label: 'Decisions'    },
  { href: '/leaderboard',  label: 'Leaderboard'  },
]

const PF = "var(--font-playfair), 'Playfair Display', serif"
const IN = "var(--font-inter-sans), 'Inter', sans-serif"

function MobileMenu({ onClose, isConnected, address, connect, signOut }: {
  onClose: () => void
  isConnected: boolean
  address: string | null
  connect: () => Promise<string>
  signOut: () => void
}) {
  const pathname = usePathname()
  const [connecting, setConnecting] = useState(false)

  const allLinks = [
    ...LINKS,
    ...(isConnected ? [{ href: '/deposit', label: 'Deposit' }, { href: '/withdraw', label: 'Withdraw' }] : []),
    { href: 'https://monolithsystematicllc.mintlify.app', label: 'Docs' },
  ]

  async function handleConnect() {
    setConnecting(true)
    try { await connect() } catch {}
    finally { setConnecting(false); onClose() }
  }

  const shortAddress = address ? `${address.slice(0, 6)}…${address.slice(-4)}` : null

  return (
    <div
      style={{ position: 'fixed', inset: 0, zIndex: 300, background: '#0C0C0C', display: 'flex', flexDirection: 'column', padding: '24px' }}
    >
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: '32px' }}>
        <span style={{ fontFamily: PF, fontStyle: 'italic', fontSize: '24px', color: '#E8E4D8', lineHeight: 1 }}>Vela</span>
        <button
          onClick={onClose}
          style={{ background: 'transparent', border: 'none', color: '#E8E4D8', fontSize: '24px', cursor: 'pointer', padding: '4px', lineHeight: 1 }}
          aria-label="Close menu"
        >
          ×
        </button>
      </div>

      <nav style={{ flex: 1 }}>
        {allLinks.map(({ href, label }) => {
          const isExternal = href.startsWith('http')
          const active = !isExternal && pathname.startsWith(href)
          return (
            <Link
              key={href}
              href={href}
              target={isExternal ? '_blank' : undefined}
              rel={isExternal ? 'noopener noreferrer' : undefined}
              onClick={onClose}
              style={{
                display: 'block',
                fontFamily: IN,
                fontWeight: 500,
                fontSize: '16px',
                color: active ? '#E8E4D8' : 'rgba(232,228,216,0.6)',
                textDecoration: 'none',
                padding: '16px 0',
                borderBottom: '1px solid rgba(232,228,216,0.06)',
              }}
            >
              {label}
            </Link>
          )
        })}
      </nav>

      <div style={{ marginTop: '32px' }}>
        {isConnected && shortAddress ? (
          <button
            type="button"
            onClick={() => { signOut(); onClose() }}
            style={{ width: '100%', fontFamily: IN, fontWeight: 600, fontSize: '13px', color: '#0C0C0C', background: '#E8E4D8', padding: '14px', border: 'none', borderRadius: 0, cursor: 'pointer' }}
          >
            {shortAddress} — Sign out
          </button>
        ) : (
          <button
            type="button"
            onClick={handleConnect}
            disabled={connecting}
            style={{ width: '100%', fontFamily: IN, fontWeight: 600, fontSize: '13px', color: '#0C0C0C', background: '#E8E4D8', padding: '14px', border: 'none', borderRadius: 0, cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', gap: '8px' }}
          >
            {connecting && <Spinner size="xs" className="text-ink" />}
            Get started
          </button>
        )}
      </div>
    </div>
  )
}

export default function Nav() {
  const pathname = usePathname()
  const { address, isConnected, connect, signOut } = useAuth()
  const [connecting, setConnecting] = useState(false)
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false)

  useEffect(() => {
    if (!mobileMenuOpen) return
    const handleKey = (e: KeyboardEvent) => { if (e.key === 'Escape') setMobileMenuOpen(false) }
    window.addEventListener('keydown', handleKey)
    return () => window.removeEventListener('keydown', handleKey)
  }, [mobileMenuOpen])

  async function handleConnect() {
    setConnecting(true)
    try {
      await connect()
    } catch (err) {
      console.error(err)
    } finally {
      setConnecting(false)
    }
  }

  const shortAddress = address
    ? `${address.slice(0, 6)}…${address.slice(-4)}`
    : null

  return (
    <>
      <header
        style={{
          background: '#0C0C0C',
          borderBottom: '1px solid rgba(232,228,216,0.08)',
          position: 'fixed',
          top: '36px',
          left: 0,
          right: 0,
          zIndex: 100,
        }}
      >
        <div className="max-w-7xl mx-auto px-4 sm:px-6 h-[60px] flex items-center justify-between gap-6">
          <Link href="/" className="flex items-center shrink-0">
            <span
              style={{
                fontFamily: PF,
                fontStyle: 'italic',
                fontSize: '24px',
                color: '#E8E4D8',
                lineHeight: 1,
              }}
            >
              Vela
            </span>
          </Link>

          <nav className="hidden md:flex items-center gap-0">
            {LINKS.map(({ href, label }, i) => {
              const active = pathname.startsWith(href)
              return (
                <motion.div
                  key={href}
                  initial={{ opacity: 0, y: -8 }}
                  animate={{ opacity: 1, y: 0 }}
                  transition={{ delay: 0.2 + i * 0.08, duration: 0.35, ease: [0.25, 0.1, 0.25, 1] }}
                >
                  <Link
                    href={href}
                    style={{
                      fontFamily: IN,
                      fontSize: '12px',
                      letterSpacing: '0.02em',
                      color: active ? '#E8E4D8' : 'rgba(232,228,216,0.38)',
                      textDecoration: 'none',
                      display: 'block',
                      padding: '18px 16px',
                      position: 'relative',
                      textTransform: 'uppercase',
                      transition: 'color 150ms ease',
                    }}
                    onMouseEnter={(e) => { if (!active) e.currentTarget.style.color = 'rgba(232,228,216,0.65)' }}
                    onMouseLeave={(e) => { if (!active) e.currentTarget.style.color = 'rgba(232,228,216,0.38)' }}
                  >
                    {label}
                    {active && (
                      <span
                        style={{
                          position: 'absolute',
                          bottom: 0,
                          left: '16px',
                          right: '16px',
                          height: '2px',
                          background: '#E8E4D8',
                        }}
                      />
                    )}
                  </Link>
                </motion.div>
              )
            })}
            {isConnected && (
              <motion.div
                initial={{ opacity: 0, y: -8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ delay: 0.2 + LINKS.length * 0.08, duration: 0.35, ease: [0.25, 0.1, 0.25, 1] }}
              >
                <Link
                  href="/deposit"
                  style={{
                    fontFamily: IN,
                    fontSize: '12px',
                    letterSpacing: '0.02em',
                    color: pathname.startsWith('/deposit') ? '#E8E4D8' : 'rgba(232,228,216,0.38)',
                    textDecoration: 'none',
                    display: 'block',
                    padding: '18px 16px',
                    position: 'relative',
                    textTransform: 'uppercase',
                  }}
                >
                  Deposit
                  {pathname.startsWith('/deposit') && (
                    <span
                      style={{
                        position: 'absolute',
                        bottom: 0,
                        left: '16px',
                        right: '16px',
                        height: '2px',
                        background: '#E8E4D8',
                      }}
                    />
                  )}
                </Link>
              </motion.div>
            )}
            {isConnected && (
              <motion.div
                initial={{ opacity: 0, y: -8 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ delay: 0.2 + (LINKS.length + 1) * 0.08, duration: 0.35, ease: [0.25, 0.1, 0.25, 1] }}
              >
                <Link
                  href="/withdraw"
                  style={{
                    fontFamily: IN,
                    fontSize: '12px',
                    letterSpacing: '0.02em',
                    color: pathname.startsWith('/withdraw') ? '#E8E4D8' : 'rgba(232,228,216,0.38)',
                    textDecoration: 'none',
                    display: 'block',
                    padding: '18px 16px',
                    position: 'relative',
                    textTransform: 'uppercase',
                  }}
                >
                  Withdraw
                  {pathname.startsWith('/withdraw') && (
                    <span
                      style={{
                        position: 'absolute',
                        bottom: 0,
                        left: '16px',
                        right: '16px',
                        height: '2px',
                        background: '#E8E4D8',
                      }}
                    />
                  )}
                </Link>
              </motion.div>
            )}
            <motion.div
              initial={{ opacity: 0, y: -8 }}
              animate={{ opacity: 1, y: 0 }}
              transition={{ delay: 0.2 + (LINKS.length + 2) * 0.08, duration: 0.35, ease: [0.25, 0.1, 0.25, 1] }}
            >
              <a
                href="https://monolithsystematicllc.mintlify.app"
                target="_blank"
                rel="noopener noreferrer"
                style={{
                  fontFamily: IN,
                  fontSize: '12px',
                  letterSpacing: '0.02em',
                  color: 'rgba(232,228,216,0.38)',
                  textDecoration: 'none',
                  display: 'block',
                  padding: '18px 16px',
                  textTransform: 'uppercase',
                }}
                onMouseEnter={(e) => (e.currentTarget.style.color = 'rgba(232,228,216,0.65)')}
                onMouseLeave={(e) => (e.currentTarget.style.color = 'rgba(232,228,216,0.38)')}
              >
                Docs
              </a>
            </motion.div>
          </nav>

          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
            {isConnected && shortAddress ? (
              <button
                type="button"
                onClick={signOut}
                className="hidden md:block"
                style={{
                  fontFamily: IN,
                  fontWeight: 600,
                  fontSize: '12px',
                  color: '#0C0C0C',
                  background: '#E8E4D8',
                  padding: '8px 20px',
                  border: 'none',
                  borderRadius: 0,
                  cursor: 'pointer',
                }}
              >
                {shortAddress}
              </button>
            ) : (
              <>
                <motion.button
                  type="button"
                  onClick={handleConnect}
                  disabled={connecting}
                  whileHover={{ opacity: 0.8 }}
                  whileTap={{ scale: 0.97 }}
                  transition={{ duration: 0.15 }}
                  style={{
                    fontFamily: IN,
                    fontSize: '12px',
                    color: 'rgba(232,228,216,0.55)',
                    background: 'transparent',
                    border: '1px solid rgba(232,228,216,0.18)',
                    padding: '8px 18px',
                    borderRadius: 0,
                    cursor: 'pointer',
                    display: 'flex',
                    alignItems: 'center',
                    gap: '6px',
                  }}
                >
                  {connecting ? <Spinner size="xs" className="text-ink" /> : null}
                  Log in
                </motion.button>
                <motion.button
                  type="button"
                  onClick={handleConnect}
                  disabled={connecting}
                  whileHover={{ opacity: 0.85 }}
                  whileTap={{ scale: 0.97 }}
                  transition={{ duration: 0.15 }}
                  className="hidden sm:block"
                  style={{
                    fontFamily: IN,
                    fontWeight: 600,
                    fontSize: '12px',
                    color: '#0C0C0C',
                    background: '#E8E4D8',
                    padding: '8px 20px',
                    border: 'none',
                    borderRadius: 0,
                    cursor: 'pointer',
                  }}
                >
                  Get started
                </motion.button>
              </>
            )}
            <button
              type="button"
              className="md:hidden"
              onClick={() => setMobileMenuOpen(true)}
              aria-label="Open menu"
              style={{ background: 'transparent', border: 'none', cursor: 'pointer', padding: '4px', display: 'flex', alignItems: 'center' }}
            >
              <svg width="20" height="20" viewBox="0 0 20 20" fill="none">
                <rect y="3" width="20" height="2" rx="1" fill="#E8E4D8" />
                <rect y="9" width="20" height="2" rx="1" fill="#E8E4D8" />
                <rect y="15" width="20" height="2" rx="1" fill="#E8E4D8" />
              </svg>
            </button>
          </div>
        </div>
      </header>

      {mobileMenuOpen && (
        <MobileMenu
          onClose={() => setMobileMenuOpen(false)}
          isConnected={isConnected}
          address={address}
          connect={connect}
          signOut={signOut}
        />
      )}
    </>
  )
}
