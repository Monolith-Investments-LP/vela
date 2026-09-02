'use client'

// ---------------------------------------------------------------------------
// Portfolio page.
//
// Backend surface: `/account/:address/portfolio` (JSON) and
// `/account/:address/portfolio/csv` (CSV blob for Koinly / CoinTracker).
// Both are already served by the api crate; this page is the first
// user-visible surface for them (audit item 8 / BUILDPLAN Tier 1
// portfolio dashboard).
// ---------------------------------------------------------------------------

import { useCallback, useEffect, useMemo, useState } from 'react'
import Link from 'next/link'
import { useAuth } from '@/lib/auth'
import { getPortfolio, getPortfolioCsv, type PortfolioResponse } from '@/lib/api'
import { Card } from '@/components/ui/Card'
import { Badge } from '@/components/ui/Badge'
import { Button } from '@/components/ui/Button'
import { FullPageSpinner } from '@/components/ui/Spinner'

type CostBasisMethod = 'FIFO' | 'HIFO'

function formatMicroUsdc(raw: string | number | undefined): string {
  if (raw === undefined || raw === null) return '—'
  const n = typeof raw === 'string' ? parseFloat(raw) : raw
  if (!Number.isFinite(n)) return '—'
  const usd = n / 1_000_000
  const sign = usd < 0 ? '-' : ''
  const abs = Math.abs(usd)
  return `${sign}$${abs.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`
}

function pnlVariant(pnl: string | undefined): 'success' | 'error' | 'neutral' {
  const n = pnl ? parseFloat(pnl) : 0
  if (n > 0) return 'success'
  if (n < 0) return 'error'
  return 'neutral'
}

function downloadBlob(name: string, content: string, mime: string) {
  const blob = new Blob([content], { type: mime })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = name
  document.body.appendChild(a)
  a.click()
  document.body.removeChild(a)
  URL.revokeObjectURL(url)
}

export default function PortfolioPage() {
  const { address, isConnected, connect } = useAuth()
  const [method, setMethod] = useState<CostBasisMethod>('FIFO')
  const [data, setData] = useState<PortfolioResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [exporting, setExporting] = useState(false)

  const load = useCallback(async () => {
    if (!address) return
    setLoading(true)
    setError(null)
    const res = await getPortfolio(address, method)
    if (res.ok && res.data) {
      setData(res.data)
    } else {
      setError(res.error ?? 'Failed to load portfolio')
    }
    setLoading(false)
  }, [address, method])

  useEffect(() => {
    load()
  }, [load])

  const totalPnl = useMemo(() => {
    if (!data) return '0'
    const realized = parseFloat(data.realized_pnl_usdc) || 0
    const unrealized = parseFloat(data.unrealized_pnl_usdc) || 0
    return String(realized + unrealized)
  }, [data])

  const handleExport = useCallback(async () => {
    if (!address) return
    setExporting(true)
    const res = await getPortfolioCsv(address, method)
    setExporting(false)
    if (res.ok && res.csv) {
      const stamp = new Date().toISOString().slice(0, 10)
      downloadBlob(`vela-portfolio-${address.slice(0, 8)}-${stamp}.csv`, res.csv, 'text/csv')
    } else {
      setError(res.error ?? 'Failed to export CSV')
    }
  }, [address, method])

  if (!isConnected || !address) {
    return (
      <div className="min-h-[70vh] flex items-center justify-center px-6">
        <Card className="max-w-md w-full p-8 text-center flex flex-col gap-6">
          <div>
            <div className="text-brown uppercase tracking-[0.18em] text-xs mb-2">Portfolio</div>
            <h1 className="text-ink text-2xl font-serif">Connect your wallet</h1>
            <p className="text-brown text-sm mt-3">
              PnL, cost basis, and tax-lot exports are gated to the connected
              wallet. Nothing is stored server-side beyond your fills.
            </p>
          </div>
          <Button onClick={() => connect().catch(() => setError('Wallet connect rejected.'))}>
            Connect wallet
          </Button>
          <Link href="/" className="text-brown text-xs uppercase tracking-[0.18em]">
            Back to home
          </Link>
        </Card>
      </div>
    )
  }

  return (
    <div className="px-4 sm:px-6 lg:px-10 py-8 lg:py-12 max-w-6xl mx-auto">
      <div className="flex flex-col sm:flex-row sm:items-end sm:justify-between gap-4 mb-8">
        <div>
          <div className="text-brown uppercase tracking-[0.18em] text-xs mb-2">Portfolio</div>
          <h1 className="text-ink text-3xl lg:text-4xl font-serif">
            {address.slice(0, 6)}…{address.slice(-4)}
          </h1>
        </div>
        <div className="flex flex-wrap gap-2 items-center">
          <div className="flex border border-border">
            {(['FIFO', 'HIFO'] as CostBasisMethod[]).map((m) => (
              <button
                key={m}
                onClick={() => setMethod(m)}
                className={`px-4 py-2 text-xs uppercase tracking-[0.14em] transition-colors ${
                  m === method
                    ? 'bg-ink text-parchment'
                    : 'text-brown hover:text-ink'
                }`}
              >
                {m}
              </button>
            ))}
          </div>
          <Button variant="ghost" onClick={load} disabled={loading}>
            Refresh
          </Button>
          <Button onClick={handleExport} disabled={exporting || !data}>
            {exporting ? 'Exporting…' : 'Export CSV'}
          </Button>
        </div>
      </div>

      {loading && !data && <FullPageSpinner />}

      {error && (
        <Card className="p-4 mb-6 border-l-2 border-l-terra">
          <p className="text-terra text-sm">{error}</p>
        </Card>
      )}

      {data && (
        <>
          <div className="grid grid-cols-1 sm:grid-cols-3 gap-4 mb-8">
            <Card className="p-6">
              <div className="text-brown uppercase tracking-[0.18em] text-[10px] mb-2">
                Realized PnL
              </div>
              <div className="text-ink text-2xl font-mono">
                {formatMicroUsdc(data.realized_pnl_usdc)}
              </div>
              <Badge variant={pnlVariant(data.realized_pnl_usdc)} className="mt-3">
                {data.cost_basis_method}
              </Badge>
            </Card>
            <Card className="p-6">
              <div className="text-brown uppercase tracking-[0.18em] text-[10px] mb-2">
                Unrealized PnL
              </div>
              <div className="text-ink text-2xl font-mono">
                {formatMicroUsdc(data.unrealized_pnl_usdc)}
              </div>
              <Badge variant={pnlVariant(data.unrealized_pnl_usdc)} className="mt-3">
                Mark-to-market
              </Badge>
            </Card>
            <Card className="p-6">
              <div className="text-brown uppercase tracking-[0.18em] text-[10px] mb-2">
                Combined PnL
              </div>
              <div className="text-ink text-2xl font-mono">{formatMicroUsdc(totalPnl)}</div>
              <Badge variant={pnlVariant(totalPnl)} className="mt-3">
                Since inception
              </Badge>
            </Card>
          </div>

          <Card className="mb-8 overflow-hidden">
            <div className="px-6 py-4 border-b border-border">
              <h2 className="text-ink font-serif text-lg">Per-market PnL</h2>
            </div>
            {data.per_market.length === 0 ? (
              <div className="px-6 py-8 text-brown text-sm text-center">
                No trading activity yet.
              </div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="text-brown uppercase tracking-[0.12em] text-[10px] border-b border-border">
                      <th className="text-left px-6 py-3 font-medium">Market</th>
                      <th className="text-right px-6 py-3 font-medium">Realized</th>
                      <th className="text-right px-6 py-3 font-medium">Unrealized</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.per_market.map((row) => (
                      <tr key={row.market} className="border-b border-border last:border-0">
                        <td className="px-6 py-3 text-ink font-mono">{row.market}</td>
                        <td className="px-6 py-3 text-right font-mono text-ink">
                          {formatMicroUsdc(row.realized_usdc)}
                        </td>
                        <td className="px-6 py-3 text-right font-mono text-ink">
                          {formatMicroUsdc(row.unrealized_usdc)}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Card>

          <Card className="overflow-hidden">
            <div className="px-6 py-4 border-b border-border">
              <h2 className="text-ink font-serif text-lg">Tax lots ({data.cost_basis_method})</h2>
            </div>
            {data.tax_lots.length === 0 ? (
              <div className="px-6 py-8 text-brown text-sm text-center">
                No open lots.
              </div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="text-brown uppercase tracking-[0.12em] text-[10px] border-b border-border">
                      <th className="text-left px-6 py-3 font-medium">Asset</th>
                      <th className="text-right px-6 py-3 font-medium">Quantity</th>
                      <th className="text-right px-6 py-3 font-medium">Cost basis</th>
                      <th className="text-right px-6 py-3 font-medium">Acquired</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.tax_lots.map((lot, i) => (
                      <tr key={`${lot.asset}-${i}`} className="border-b border-border last:border-0">
                        <td className="px-6 py-3 text-ink font-mono">{lot.asset}</td>
                        <td className="px-6 py-3 text-right font-mono text-ink">{lot.quantity}</td>
                        <td className="px-6 py-3 text-right font-mono text-ink">
                          {formatMicroUsdc(lot.cost_basis_usdc)}
                        </td>
                        <td className="px-6 py-3 text-right font-mono text-brown">
                          {new Date(lot.acquired_at * 1000).toLocaleDateString()}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Card>
        </>
      )}
    </div>
  )
}
