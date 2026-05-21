'use client'

import { useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import HexCanvas from '@/components/HexCanvas'
import HeroOrderBook from '@/components/HeroOrderBook'

const PF = "var(--font-playfair), 'Playfair Display', serif"
const IN = "var(--font-inter-sans), 'Inter', sans-serif"

const STATS = [
  {
    num: '1.066 μs',
    label: 'MATCH LATENCY',
    sub: 'p50, Apple M3, release build',
  },
  {
    num: '85.4k',
    label: 'OPERATIONS / SECOND',
    sub: 'Realistic MM workload, 98% cancel / 2% fill',
  },
  {
    num: '7.4×',
    label: 'FASTER THAN PULSE',
    sub: 'Per-operation vs. the leading open-source DEX',
  },
]

const HOW_IT_WORKS = [
  {
    num: '01',
    title: 'WALLET-SIGNED ORDERS',
    body: 'Every order carries your wallet signature. The engine verifies it before matching. No one can place or cancel orders on your behalf.',
  },
  {
    num: '02',
    title: 'OPTIMISTIC-ZK PROOFS',
    body: 'Every batch of trades is provable on-chain. Challenge any batch and receive a cryptographic fraud proof. Trust the math, not the operator.',
  },
  {
    num: '03',
    title: 'ON-CHAIN SETTLEMENT',
    body: 'Funds are held in a smart contract on Ethereum. Withdraw directly to your wallet. Emergency exit available after a 7-day timelock — no permission needed.',
  },
]

const FOOTER_LINKS = [
  { label: 'Privacy Policy', href: '/privacy' },
  { label: 'Terms', href: '/privacy-disclosure' },
  { label: 'Docs', href: 'https://monolithsystematicllc.mintlify.app' },
  { label: 'GitHub', href: 'https://github.com/arpjw/vela' },
]

export default function HomePage() {
  const router = useRouter()
  const [email, setEmail] = useState('')
  const [submitted, setSubmitted] = useState(false)

  useEffect(() => {
    const params = new URLSearchParams(window.location.search)
    const ref = params.get('ref')
    if (ref) {
      localStorage.setItem('vela_referrer', ref)
    }
  }, [])

  function handleEmailSubmit(e: React.FormEvent) {
    e.preventDefault()
    if (email.includes('@') && email.includes('.')) {
      localStorage.setItem('vela_beta_email', email)
      setSubmitted(true)
    }
  }

  return (
    <div style={{ background: '#0C0C0C' }}>
      <section
        style={{
          position: 'relative',
          minHeight: 'calc(100vh - 96px)',
          background: '#0C0C0C',
          overflow: 'hidden',
        }}
      >
        <HexCanvas />
        <div style={{ position: 'relative', zIndex: 2 }} className="px-6 pt-16 pb-12 lg:px-[52px] lg:pt-[100px] lg:pb-[80px]">
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-[60px] items-center">
            <div>
              <div
                style={{
                  fontFamily: IN,
                  fontSize: '10px',
                  letterSpacing: '0.2em',
                  color: 'rgba(232,228,216,0.3)',
                  textTransform: 'uppercase',
                  marginBottom: '24px',
                }}
              >
                Monolith Systematic — Public Beta
              </div>
              <div>
                <span
                  style={{ display: 'block', fontFamily: PF, fontWeight: 900, color: '#E8E4D8', lineHeight: 0.95 }}
                  className="text-[44px] lg:text-[72px]"
                >
                  Trade with
                </span>
                <span
                  style={{ display: 'block', fontFamily: PF, fontWeight: 400, fontStyle: 'italic', color: 'rgba(232,228,216,0.38)', lineHeight: 0.95 }}
                  className="text-[44px] lg:text-[72px]"
                >
                  provable
                </span>
                <span
                  style={{ display: 'block', fontFamily: PF, fontWeight: 900, color: '#E8E4D8', lineHeight: 0.95 }}
                  className="text-[44px] lg:text-[72px]"
                >
                  fairness.
                </span>
              </div>
              <p
                style={{
                  fontFamily: IN,
                  fontWeight: 300,
                  fontSize: '15px',
                  lineHeight: 1.75,
                  color: 'rgba(232,228,216,0.35)',
                  maxWidth: '480px',
                  marginTop: '28px',
                }}
              >
                A central limit order book exchange where every match is cryptographically verified. The first DEX that doesn&apos;t make you choose between speed and transparency.
              </p>
              <div className="flex flex-col sm:flex-row gap-3 mt-10">
                <button
                  type="button"
                  onClick={() => router.push('/markets/ETH-USDC')}
                  style={{
                    background: '#E8E4D8',
                    color: '#0C0C0C',
                    fontFamily: IN,
                    fontWeight: 600,
                    fontSize: '13px',
                    padding: '14px 36px',
                    border: 'none',
                    borderRadius: 0,
                    cursor: 'pointer',
                  }}
                >
                  Launch Exchange →
                </button>
                <button
                  type="button"
                  onClick={() => window.open('https://ssrn.com/abstract=6579199', '_blank')}
                  style={{
                    background: 'transparent',
                    border: '1px solid rgba(232,228,216,0.15)',
                    color: 'rgba(232,228,216,0.5)',
                    fontFamily: IN,
                    fontSize: '13px',
                    padding: '14px 28px',
                    borderRadius: 0,
                    cursor: 'pointer',
                  }}
                >
                  Read the paper
                </button>
              </div>
            </div>
            <div
              className="hero-orderbook-col hidden sm:flex justify-end items-center"
            >
              <HeroOrderBook />
            </div>
          </div>
        </div>
      </section>

      <section
        style={{
          background: '#E8E4D8',
          overflow: 'hidden',
        }}
        className="px-6 py-16 lg:px-[52px] lg:py-[90px]"
      >
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-[80px] items-start">
          <div>
            <div
              style={{
                fontFamily: IN,
                fontSize: '11px',
                letterSpacing: '0.18em',
                color: 'rgba(12,12,12,0.4)',
                textTransform: 'uppercase',
                marginBottom: '20px',
              }}
            >
              Performance
            </div>
            <div>
              <div
                style={{
                  fontFamily: PF,
                  fontWeight: 900,
                  fontSize: '48px',
                  color: '#0C0C0C',
                  lineHeight: 1.02,
                }}
              >
                Built for speed
              </div>
              <div
                style={{
                  fontFamily: PF,
                  fontWeight: 400,
                  fontStyle: 'italic',
                  fontSize: '48px',
                  color: 'rgba(12,12,12,0.35)',
                  lineHeight: 1.02,
                }}
              >
                and verification.
              </div>
            </div>
            <p
              style={{
                fontFamily: IN,
                fontWeight: 300,
                fontSize: '14px',
                lineHeight: 1.8,
                color: 'rgba(12,12,12,0.45)',
                maxWidth: '400px',
                marginTop: '22px',
              }}
            >
              Vela&apos;s matching engine runs at sub-microsecond latency while keeping a full cryptographic record of every trade. Performance and transparency — for the first time, together.
            </p>
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: '1px' }}>
            {STATS.map((stat) => (
              <div
                key={stat.label}
                style={{
                  background: 'rgba(12,12,12,0.04)',
                  borderLeft: '3px solid rgba(12,12,12,0.08)',
                  padding: '26px 30px',
                }}
              >
                <div
                  style={{
                    fontFamily: PF,
                    fontWeight: 900,
                    fontSize: '40px',
                    color: '#0C0C0C',
                  }}
                >
                  {stat.num}
                </div>
                <div
                  style={{
                    fontFamily: IN,
                    fontSize: '9px',
                    letterSpacing: '0.2em',
                    color: 'rgba(12,12,12,0.3)',
                    textTransform: 'uppercase',
                    marginTop: '4px',
                  }}
                >
                  {stat.label}
                </div>
                <div
                  style={{
                    fontFamily: IN,
                    fontSize: '11px',
                    color: 'rgba(12,12,12,0.28)',
                    marginTop: '3px',
                  }}
                >
                  {stat.sub}
                </div>
              </div>
            ))}
          </div>
        </div>
      </section>

      <section
        style={{
          position: 'relative',
          background: '#111110',
          overflow: 'hidden',
        }}
        className="px-6 py-16 lg:px-[52px] lg:py-[90px]"
      >
        <HexCanvas />
        <div style={{ position: 'relative', zIndex: 1 }}>
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-[80px] items-end mb-16">
            <div>
              <div
                style={{
                  fontFamily: IN,
                  fontSize: '11px',
                  letterSpacing: '0.18em',
                  color: 'rgba(232,228,216,0.3)',
                  textTransform: 'uppercase',
                  marginBottom: '16px',
                }}
              >
                How it works
              </div>
              <div>
                <div
                  style={{
                    fontFamily: PF,
                    fontWeight: 900,
                    fontSize: '46px',
                    color: '#E8E4D8',
                    lineHeight: 1.05,
                  }}
                >
                  Exchange-grade
                </div>
                <div
                  style={{
                    fontFamily: PF,
                    fontWeight: 900,
                    fontSize: '46px',
                    color: '#E8E4D8',
                    lineHeight: 1.05,
                  }}
                >
                  infrastructure.
                </div>
                <div
                  style={{
                    fontFamily: PF,
                    fontWeight: 400,
                    fontStyle: 'italic',
                    fontSize: '46px',
                    color: 'rgba(232,228,216,0.3)',
                    lineHeight: 1.05,
                  }}
                >
                  On-chain proof.
                </div>
              </div>
            </div>
            <div>
              <p
                style={{
                  fontFamily: IN,
                  fontWeight: 300,
                  fontSize: '14px',
                  lineHeight: 1.8,
                  color: 'rgba(232,228,216,0.32)',
                }}
              >
                Every order signed by your wallet. Every batch provable on-chain. Funds held in a smart contract. Withdraw without asking anyone&apos;s permission — ever.
              </p>
            </div>
          </div>

          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-[1px]" style={{ background: 'rgba(232,228,216,0.06)' }}>
            {HOW_IT_WORKS.map((card) => (
              <div key={card.num} style={{ background: '#111110', padding: '36px 28px' }}>
                <div
                  style={{
                    fontFamily: PF,
                    fontStyle: 'italic',
                    fontSize: '42px',
                    color: 'rgba(232,228,216,0.07)',
                    marginBottom: '24px',
                    lineHeight: 1,
                  }}
                >
                  {card.num}
                </div>
                <div
                  style={{
                    fontFamily: IN,
                    fontWeight: 600,
                    fontSize: '11px',
                    letterSpacing: '0.14em',
                    textTransform: 'uppercase',
                    color: '#E8E4D8',
                    marginBottom: '14px',
                  }}
                >
                  {card.title}
                </div>
                <p
                  style={{
                    fontFamily: IN,
                    fontWeight: 300,
                    fontSize: '13px',
                    lineHeight: 1.75,
                    color: 'rgba(232,228,216,0.28)',
                  }}
                >
                  {card.body}
                </p>
              </div>
            ))}
          </div>
        </div>
      </section>

      <section
        style={{
          background: '#E8E4D8',
          overflow: 'hidden',
        }}
        className="px-6 py-16 lg:px-[52px] lg:py-[90px]"
      >
        <div
          style={{
            maxWidth: '580px',
            margin: '0 auto',
            textAlign: 'center',
          }}
        >
          <div
            style={{
              fontFamily: IN,
              fontSize: '10px',
              letterSpacing: '0.2em',
              color: 'rgba(12,12,12,0.3)',
              textTransform: 'uppercase',
              marginBottom: '16px',
            }}
          >
            Early access
          </div>
          <div>
            <span
              style={{
                display: 'block',
                fontFamily: PF,
                fontWeight: 900,
                fontSize: '54px',
                color: '#0C0C0C',
                lineHeight: 0.98,
              }}
            >
              Join the
            </span>
            <span
              style={{
                display: 'block',
                fontFamily: PF,
                fontWeight: 400,
                fontStyle: 'italic',
                fontSize: '54px',
                color: 'rgba(12,12,12,0.32)',
                lineHeight: 0.98,
              }}
            >
              beta.
            </span>
          </div>
          <p
            style={{
              fontFamily: IN,
              fontWeight: 300,
              fontSize: '14px',
              lineHeight: 1.75,
              color: 'rgba(12,12,12,0.42)',
              marginTop: '18px',
            }}
          >
            Get early access updates, trading incentives, and the mainnet launch announcement.
          </p>
          {submitted ? (
            <div
              style={{
                fontFamily: IN,
                fontSize: '13px',
                color: 'rgba(12,12,12,0.5)',
                marginTop: '36px',
              }}
            >
              You&apos;re on the list. We&apos;ll be in touch.
            </div>
          ) : (
            <form onSubmit={handleEmailSubmit} className="flex flex-col sm:flex-row mt-9">
              <input
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="your@email.com"
                style={{
                  flex: 1,
                  fontFamily: IN,
                  fontSize: '13px',
                  color: '#0C0C0C',
                  background: 'white',
                  border: '1px solid rgba(12,12,12,0.18)',
                  borderRight: 'none',
                  padding: '14px 18px',
                  borderRadius: 0,
                  outline: 'none',
                  minWidth: 0,
                }}
                className="sm:border-r-0 border-b-0 sm:border-b border-[rgba(12,12,12,0.18)]"
              />
              <button
                type="submit"
                style={{
                  fontFamily: IN,
                  fontWeight: 600,
                  fontSize: '12px',
                  color: '#E8E4D8',
                  background: '#0C0C0C',
                  padding: '14px 26px',
                  border: 'none',
                  borderRadius: 0,
                  cursor: 'pointer',
                  whiteSpace: 'nowrap',
                }}
              >
                Get started →
              </button>
            </form>
          )}
          <div
            style={{
              fontFamily: IN,
              fontSize: '10px',
              color: 'rgba(12,12,12,0.28)',
              letterSpacing: '0.05em',
              marginTop: '14px',
            }}
          >
            Public beta · Ethereum Sepolia · No real funds
          </div>
        </div>
      </section>

      <section
        style={{
          position: 'relative',
          background: '#0C0C0C',
          minHeight: '400px',
          overflow: 'hidden',
        }}
      >
        <HexCanvas />
        <div
          style={{ position: 'relative', zIndex: 2, textAlign: 'center' }}
          className="px-6 pt-16 sm:pt-[80px] lg:px-[52px]"
        >
          <div
            style={{
              fontFamily: PF,
              fontWeight: 700,
              fontSize: '46px',
              color: '#E8E4D8',
            }}
          >
            Ready to trade?
          </div>
          <div
            style={{
              fontFamily: PF,
              fontWeight: 400,
              fontStyle: 'italic',
              fontSize: '46px',
              color: 'rgba(232,228,216,0.35)',
            }}
          >
            Start now.
          </div>
          <div>
            <button
              type="button"
              onClick={() => router.push('/markets/ETH-USDC')}
              style={{
                background: '#E8E4D8',
                color: '#0C0C0C',
                fontFamily: IN,
                fontWeight: 600,
                fontSize: '13px',
                padding: '14px 44px',
                border: 'none',
                borderRadius: 0,
                cursor: 'pointer',
                display: 'inline-block',
                marginTop: '32px',
              }}
            >
              Launch Exchange →
            </button>
          </div>
          <span
            style={{
              display: 'block',
              fontFamily: PF,
              fontWeight: 900,
              fontStyle: 'italic',
              fontSize: '140px',
              color: 'rgba(232,228,216,0.04)',
              letterSpacing: '-4px',
              lineHeight: 1,
              textAlign: 'center',
              paddingBottom: 0,
              marginTop: '40px',
            }}
          >
            Vela
          </span>
        </div>
      </section>

      <footer
        style={{
          background: '#0C0C0C',
          borderTop: '1px solid rgba(232,228,216,0.06)',
        }}
        className="px-6 py-6 lg:px-[52px] flex flex-col sm:flex-row justify-between items-center gap-4"
      >
        <div style={{ display: 'flex', gap: '24px' }}>
          {FOOTER_LINKS.map(({ label, href }) => (
            <a
              key={label}
              href={href}
              target={href.startsWith('http') ? '_blank' : undefined}
              rel={href.startsWith('http') ? 'noopener noreferrer' : undefined}
              style={{
                fontFamily: IN,
                fontSize: '11px',
                color: 'rgba(232,228,216,0.2)',
                textDecoration: 'none',
              }}
              onMouseEnter={(e) => (e.currentTarget.style.color = 'rgba(232,228,216,0.45)')}
              onMouseLeave={(e) => (e.currentTarget.style.color = 'rgba(232,228,216,0.2)')}
            >
              {label}
            </a>
          ))}
        </div>
        <span
          style={{
            fontFamily: IN,
            fontSize: '11px',
            color: 'rgba(232,228,216,0.2)',
          }}
        >
          © 2026 Monolith Systematic LLC
        </span>
      </footer>
    </div>
  )
}
