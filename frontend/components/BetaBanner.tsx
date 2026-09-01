'use client'

import { useState, useEffect } from 'react'
import { motion } from 'framer-motion'

const STATUS_API_URL = process.env.NEXT_PUBLIC_API_URL ?? 'https://vela-engine.fly.dev'

type EngineStatus = 'operational' | 'degraded' | 'starting' | null

export default function BetaBanner() {
  const [engineStatus, setEngineStatus] = useState<EngineStatus>(null)

  useEffect(() => {
    let mounted = true

    async function fetchEngineStatus() {
      try {
        const res = await fetch(`${STATUS_API_URL}/status`)
        const data = await res.json()
        if (mounted && data.ok) setEngineStatus(data.data.status)
      } catch {
        // silently ignore
      }
    }

    fetchEngineStatus()
    const t = setInterval(fetchEngineStatus, 60_000)
    return () => {
      mounted = false
      clearInterval(t)
    }
  }, [])

  const dotColor = engineStatus === 'operational'
    ? '#6B8A5A'
    : engineStatus === 'degraded'
    ? '#CC3333'
    : 'rgba(232,228,216,0.5)'

  const statusText = engineStatus === 'degraded' ? 'DEGRADED' : 'PUBLIC BETA'

  return (
    <div
      style={{
        background: '#0C0C0C',
        borderBottom: '1px solid rgba(232,228,216,0.08)',
        padding: '8px 24px',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        gap: 12,
        position: 'fixed',
        top: 0,
        left: 0,
        right: 0,
        height: '36px',
        zIndex: 200,
        fontFamily: 'var(--font-inter-sans), sans-serif',
        fontSize: '0.72rem',
        letterSpacing: '0.08em',
      }}
    >
      <span style={{ display: 'flex', alignItems: 'center', gap: 5, color: engineStatus === 'degraded' ? '#CC3333' : 'rgba(232,228,216,0.5)' }}>
        {engineStatus === 'starting' ? (
          <motion.span
            style={{ color: dotColor }}
            animate={{ opacity: [1, 0.15, 1] }}
            transition={{ duration: 2, repeat: Infinity, ease: 'easeInOut' }}
          >
            ●
          </motion.span>
        ) : engineStatus === null ? (
          <motion.span
            style={{ color: 'rgba(232,228,216,0.5)' }}
            animate={{ opacity: [1, 0.15, 1] }}
            transition={{ duration: 2, repeat: Infinity, ease: 'easeInOut' }}
          >
            ●
          </motion.span>
        ) : (
          <span style={{ color: dotColor }}>●</span>
        )}
        {statusText}
      </span>
      <span style={{ color: 'rgba(232,228,216,0.25)' }}>
        Ethereum Sepolia Testnet — Do not use real funds
      </span>
      <a
        href="https://monolithsystematicllc.mintlify.app/introduction/what-is-vela"
        target="_blank"
        rel="noopener noreferrer"
        style={{ color: 'rgba(232,228,216,0.4)', textDecoration: 'none' }}
        onMouseEnter={(e) => (e.currentTarget.style.color = '#E8E4D8')}
        onMouseLeave={(e) => (e.currentTarget.style.color = 'rgba(232,228,216,0.4)')}
      >
        Learn more →
      </a>
    </div>
  )
}
