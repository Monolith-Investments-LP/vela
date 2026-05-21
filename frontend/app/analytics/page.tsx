'use client'

import { useEffect, useMemo, useReducer, useRef, useState, useCallback } from 'react'
import { listMarkets, getBook, type MarketResponse } from '@/lib/api'
import { getWsClient } from '@/lib/ws'
import { Badge } from '@/components/ui/Badge'
import { Card } from '@/components/ui/Card'
import { FullPageSpinner } from '@/components/ui/Spinner'
import { Tooltip } from '@/components/ui/Tooltip'
import HexCanvas from '@/components/HexCanvas'
import { useAuth } from '@/lib/auth'

const PF = "'Playfair Display', serif"
const IN = 'Inter, sans-serif'
const CN = "'Courier New', monospace"

const API_URL = process.env.NEXT_PUBLIC_API_URL ?? 'https://vela-engine.fly.dev'

const WINDOW_MS = 5 * 60 * 1000

type Timeframe = '1H' | '24H' | '7D'

interface AnalyticsMarket {
  market_id: string
  current_spread_bps: number | null
  current_bid: number | null
  current_ask: number | null
  current_mid: number | null
  slippage_1k_usdc: number | null
  slippage_10k_usdc: number | null
  slippage_100k_usdc: number | null
  total_volume_usdc: string
  fill_count: number
  avg_fill_size_usdc: string
  largest_fill_usdc: string
  depth_1pct_bid_usdc: string
  depth_1pct_ask_usdc: string
  depth_1pct_total_usdc: string
}

function spreadColor(bps: number | null): string {
  if (bps === null) return 'rgba(12,12,12,0.3)'
  if (bps < 5) return '#6B8A5A'
  if (bps <= 20) return 'rgba(12,12,12,0.6)'
  return '#CC3333'
}

function fmtBps(v: number | null): string {
  if (v === null) return '—'
  return `${v.toFixed(1)} bps`
}

function fmtUsd(s: string): string {
  const n = parseFloat(s)
  if (isNaN(n)) return '—'
  return `$${n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`
}

function ticker(market_id: string): string {
  return market_id.replace('-USDC', '')
}

interface SlippageBarChartProps {
  slippage1k: number | null
  slippage10k: number | null
  slippage100k: number | null
}

function SlippageBarChart({ slippage1k, slippage10k, slippage100k }: SlippageBarChartProps) {
  const values = [slippage1k, slippage10k, slippage100k]
  const labels = ['$1k', '$10k', '$100k']
  const max = Math.max(...values.filter((v): v is number => v !== null), 1)

  function barColor(v: number | null): string {
    if (v === null) return 'rgba(232,228,216,0.1)'
    if (v < 5) return '#6B8A5A'
    if (v <= 20) return 'rgba(232,228,216,0.4)'
    return '#CC3333'
  }

  return (
    <div style={{ display: 'flex', alignItems: 'flex-end', gap: '32px', height: '160px' }}>
      {values.map((v, i) => {
        const height = v !== null ? Math.max((v / max) * 120, 4) : 0
        return (
          <div key={labels[i]} style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: '8px' }}>
            <span style={{ fontFamily: IN, fontSize: '10px', color: '#E8E4D8' }}>
              {v !== null ? `${v.toFixed(1)} bps` : '—'}
            </span>
            <div style={{ width: '48px', height: '120px', display: 'flex', alignItems: 'flex-end' }}>
              <div style={{ width: '100%', height: `${height}px`, background: barColor(v) }} />
            </div>
            <span style={{ fontFamily: IN, fontSize: '11px', color: 'rgba(232,228,216,0.5)' }}>{labels[i]}</span>
          </div>
        )
      })}
    </div>
  )
}

// ─── existing real-time analytics state ───────────────────────────────────────

interface TradePoint {
  ts: number
  price: number
  size: number
  side: 'buy' | 'sell'
}

interface SpreadPoint {
  ts: number
  bps: number
}

interface DepthState {
  bids: [string, string][]
  asks: [string, string][]
  ofi: number
  spreadBps: number | null
  ts: number
}

interface AnalyticsState {
  markets: MarketResponse[]
  selectedMarket: string | null
  tradesByMarket: Record<string, TradePoint[]>
  spreadByMarket: Record<string, SpreadPoint[]>
  depthByMarket: Record<string, DepthState>
  marketsLoading: boolean
}

type AnalyticsAction =
  | { type: 'INIT_MARKETS'; markets: MarketResponse[] }
  | { type: 'SELECT_MARKET'; market: string }
  | { type: 'ADD_TRADE'; market: string; trade: TradePoint }
  | { type: 'DEPTH_POLL'; market: string; bids: [string, string][]; asks: [string, string][] }
  | { type: 'PRUNE'; cutoff: number }

const initState: AnalyticsState = {
  markets: [],
  selectedMarket: null,
  tradesByMarket: {},
  spreadByMarket: {},
  depthByMarket: {},
  marketsLoading: true,
}

function analyticsReducer(state: AnalyticsState, action: AnalyticsAction): AnalyticsState {
  switch (action.type) {
    case 'INIT_MARKETS':
      return {
        ...state,
        markets: action.markets,
        selectedMarket: state.selectedMarket ?? action.markets[0]?.id ?? null,
        marketsLoading: false,
      }
    case 'SELECT_MARKET':
      return { ...state, selectedMarket: action.market }
    case 'ADD_TRADE': {
      const prev = state.tradesByMarket[action.market] ?? []
      return {
        ...state,
        tradesByMarket: { ...state.tradesByMarket, [action.market]: [...prev, action.trade].slice(-3000) },
      }
    }
    case 'DEPTH_POLL': {
      const bids10 = action.bids.slice(0, 10)
      const asks10 = action.asks.slice(0, 10)
      const bidDepth = bids10.reduce((s, [, sz]) => s + parseFloat(sz), 0)
      const askDepth = asks10.reduce((s, [, sz]) => s + parseFloat(sz), 0)
      const total = bidDepth + askDepth
      const ofi = total > 0 ? (bidDepth - askDepth) / total : 0
      const bestBid = action.bids[0] ? parseFloat(action.bids[0][0]) : null
      const bestAsk = action.asks[0] ? parseFloat(action.asks[0][0]) : null
      const mid = bestBid !== null && bestAsk !== null ? (bestBid + bestAsk) / 2 : null
      const spreadBps =
        bestBid !== null && bestAsk !== null && mid !== null && mid > 0
          ? ((bestAsk - bestBid) / mid) * 10_000
          : null
      const prevSpread = state.spreadByMarket[action.market] ?? []
      const newSpread =
        spreadBps !== null
          ? [...prevSpread, { ts: Date.now(), bps: spreadBps }].slice(-1500)
          : prevSpread
      return {
        ...state,
        depthByMarket: {
          ...state.depthByMarket,
          [action.market]: { bids: action.bids, asks: action.asks, ofi, spreadBps, ts: Date.now() },
        },
        spreadByMarket: { ...state.spreadByMarket, [action.market]: newSpread },
      }
    }
    case 'PRUNE': {
      const prunedTrades: Record<string, TradePoint[]> = {}
      for (const [mkt, trades] of Object.entries(state.tradesByMarket)) {
        prunedTrades[mkt] = trades.filter((t) => t.ts >= action.cutoff)
      }
      const prunedSpread: Record<string, SpreadPoint[]> = {}
      for (const [mkt, pts] of Object.entries(state.spreadByMarket)) {
        prunedSpread[mkt] = pts.filter((p) => p.ts >= action.cutoff)
      }
      return { ...state, tradesByMarket: prunedTrades, spreadByMarket: prunedSpread }
    }
    default:
      return state
  }
}

function stepPath(
  levels: { price: number; cum: number }[],
  toX: (p: number) => number,
  toY: (c: number) => number,
  bottomY: number,
): string {
  if (levels.length === 0) return ''
  const pts: string[] = [`M ${toX(levels[0].price)} ${bottomY}`]
  for (let i = 0; i < levels.length; i++) {
    pts.push(`L ${toX(levels[i].price)} ${toY(levels[i].cum)}`)
    if (i < levels.length - 1) {
      pts.push(`L ${toX(levels[i + 1].price)} ${toY(levels[i].cum)}`)
    }
  }
  pts.push(`L ${toX(levels[levels.length - 1].price)} ${bottomY} Z`)
  return pts.join(' ')
}

function ChartEmpty({ label }: { label: string }) {
  return (
    <div className="flex items-center justify-center h-full text-xs text-brown font-mono">
      {label}
    </div>
  )
}

function SpreadChart({ history }: { history: SpreadPoint[] }) {
  const W = 600
  const H = 140
  const PAD = { top: 14, right: 14, bottom: 32, left: 46 }
  const innerW = W - PAD.left - PAD.right
  const innerH = H - PAD.top - PAD.bottom
  const bottomY = PAD.top + innerH
  const now = Date.now()
  const windowStart = now - WINDOW_MS
  const visible = history.filter((p) => p.ts >= windowStart)
  if (visible.length < 2) {
    return <div style={{ height: H }}><ChartEmpty label="Waiting for data…" /></div>
  }
  const bpsValues = visible.map((p) => p.bps)
  const minBps = Math.min(...bpsValues)
  const maxBps = Math.max(...bpsValues)
  const bpsRange = maxBps - minBps || 1
  const pad = bpsRange * 0.1
  const toX = (ts: number) => PAD.left + ((ts - windowStart) / (now - windowStart)) * innerW
  const toY = (bps: number) => PAD.top + innerH - ((bps - (minBps - pad)) / (bpsRange + 2 * pad)) * innerH
  const linePoints = visible.map((p) => `${toX(p.ts)},${toY(p.bps)}`).join(' ')
  const lastPt = visible[visible.length - 1]!
  const fillPoints = [`${toX(visible[0]!.ts)},${bottomY}`, linePoints, `${toX(lastPt.ts)},${bottomY}`].join(' ')
  const yLabels = [minBps - pad, (minBps + maxBps) / 2, maxBps + pad]
  const minuteMarks = [0, 1, 2, 3, 4, 5].map((m) => ({ ts: now - m * 60_000, label: m === 0 ? 'now' : `-${m}m` }))
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" width="100%" height={H}>
      {[0.25, 0.5, 0.75].map((f) => (
        <line key={f} x1={PAD.left} y1={PAD.top + innerH * f} x2={PAD.left + innerW} y2={PAD.top + innerH * f}
          stroke="rgba(232,228,216,0.04)" strokeWidth="0.5" strokeDasharray="3 3" />
      ))}
      <polygon points={fillPoints} fill="#6B8A5A" fillOpacity="0.08" />
      <polyline points={linePoints} fill="none" stroke="#6B8A5A" strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
      <circle cx={toX(lastPt.ts)} cy={toY(lastPt.bps)} r="3" fill="#6B8A5A" />
      {minuteMarks.map(({ ts, label }) => (
        <text key={label} x={toX(ts)} y={H - 6} textAnchor="middle" fontSize="9" fill="rgba(232,228,216,0.35)" fontFamily="Courier New, monospace">{label}</text>
      ))}
      {yLabels.map((v) => (
        <text key={v} x={PAD.left - 6} y={toY(v) + 3} textAnchor="end" fontSize="9" fill="rgba(232,228,216,0.35)" fontFamily="Courier New, monospace">{v.toFixed(1)}</text>
      ))}
      <line x1={PAD.left} y1={PAD.top} x2={PAD.left} y2={bottomY} stroke="rgba(232,228,216,0.06)" strokeWidth="0.5" />
      <line x1={PAD.left} y1={bottomY} x2={PAD.left + innerW} y2={bottomY} stroke="rgba(232,228,216,0.06)" strokeWidth="0.5" />
    </svg>
  )
}

function OFIBar({ ofi }: { ofi: number }) {
  const bidPct = Math.max(0, ofi) * 50
  const askPct = Math.max(0, -ofi) * 50
  const isBidHeavy = ofi >= 0
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between text-[10px] font-medium text-brown uppercase tracking-wider">
        <span>Ask pressure</span>
        <span className={['text-xs font-bold tabular-nums', isBidHeavy ? 'text-sage' : 'text-terra'].join(' ')}>
          {ofi >= 0 ? '+' : ''}{(ofi * 100).toFixed(1)}%
        </span>
        <span>Bid pressure</span>
      </div>
      <div className="relative h-7 bg-[rgba(232,228,216,0.04)] overflow-hidden">
        <div className="absolute left-1/2 top-0 bottom-0 w-px bg-[rgba(232,228,216,0.15)] z-10" />
        {isBidHeavy ? (
          <div className="absolute top-0 bottom-0 bg-sage transition-[width] duration-300" style={{ left: '50%', width: `${bidPct}%` }} />
        ) : (
          <div className="absolute top-0 bottom-0 bg-terra transition-[width] duration-300" style={{ right: '50%', width: `${askPct}%` }} />
        )}
        <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
          <span className="text-[10px] font-semibold text-brown tabular-nums uppercase tracking-[0.1em]">
            {isBidHeavy ? 'Bid-heavy' : 'Ask-heavy'}
          </span>
        </div>
      </div>
      <div className="flex justify-between text-[10px] text-brown">
        <span>−100%</span><span>0</span><span>+100%</span>
      </div>
    </div>
  )
}

function DepthChart({ depth }: { depth: DepthState | null }) {
  const W = 600
  const H = 160
  const PAD = { top: 14, right: 14, bottom: 36, left: 48 }
  const innerW = W - PAD.left - PAD.right
  const innerH = H - PAD.top - PAD.bottom
  const bottomY = PAD.top + innerH
  if (!depth || (depth.bids.length === 0 && depth.asks.length === 0)) {
    return <div style={{ height: H }}><ChartEmpty label="Waiting for depth data…" /></div>
  }
  const bids10 = depth.bids.slice(0, 10)
  const asks10 = depth.asks.slice(0, 10)
  let bidCum = 0
  const bidLevels = bids10.map(([p, s]) => { bidCum += parseFloat(s); return { price: parseFloat(p), cum: bidCum } })
  let askCum = 0
  const askLevels = asks10.map(([p, s]) => { askCum += parseFloat(s); return { price: parseFloat(p), cum: askCum } })
  const allPrices = [...bidLevels.map((l) => l.price), ...askLevels.map((l) => l.price)]
  if (allPrices.length === 0) {
    return <div style={{ height: H }}><ChartEmpty label="No price data" /></div>
  }
  const minPrice = Math.min(...allPrices)
  const maxPrice = Math.max(...allPrices)
  const priceRange = maxPrice - minPrice || 1
  const pricePad = priceRange * 0.05
  const maxCum = Math.max(bidCum, askCum, 0.001)
  const toX = (p: number) => PAD.left + ((p - (minPrice - pricePad)) / (priceRange + 2 * pricePad)) * innerW
  const toY = (c: number) => PAD.top + innerH - (c / (maxCum * 1.05)) * innerH
  const bidPath = stepPath(bidLevels, toX, toY, bottomY)
  const askPath = stepPath(askLevels, toX, toY, bottomY)
  const midPrice = bidLevels[0] && askLevels[0] ? (bidLevels[0].price + askLevels[0].price) / 2 : null
  const priceTickCount = 5
  const priceTicks = Array.from({ length: priceTickCount }, (_, i) => minPrice - pricePad + ((priceRange + 2 * pricePad) * i) / (priceTickCount - 1))
  const cumTicks = [0, maxCum * 0.5, maxCum]
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" width="100%" height={H}>
      {[0.25, 0.5, 0.75].map((f) => (
        <line key={f} x1={PAD.left} y1={PAD.top + innerH * f} x2={PAD.left + innerW} y2={PAD.top + innerH * f}
          stroke="rgba(232,228,216,0.04)" strokeWidth="0.5" strokeDasharray="3 3" />
      ))}
      {bidPath && (<><path d={bidPath} fill="rgba(107,138,90,0.08)" /><path d={bidPath} fill="none" stroke="#6B8A5A" strokeWidth="1.5" strokeLinejoin="round" /></>)}
      {askPath && (<><path d={askPath} fill="rgba(204,51,51,0.08)" /><path d={askPath} fill="none" stroke="#CC3333" strokeWidth="1.5" strokeLinejoin="round" /></>)}
      {midPrice !== null && <line x1={toX(midPrice)} y1={PAD.top} x2={toX(midPrice)} y2={bottomY} stroke="rgba(232,228,216,0.2)" strokeWidth="1" strokeDasharray="4 3" />}
      {priceTicks.map((p, i) => <text key={i} x={toX(p)} y={H - 6} textAnchor="middle" fontSize="8.5" fill="rgba(232,228,216,0.35)" fontFamily="Courier New, monospace">{p.toFixed(2)}</text>)}
      {cumTicks.map((c) => <text key={c} x={PAD.left - 6} y={toY(c) + 3} textAnchor="end" fontSize="8.5" fill="rgba(232,228,216,0.35)" fontFamily="Courier New, monospace">{c.toFixed(2)}</text>)}
      <line x1={PAD.left} y1={PAD.top} x2={PAD.left} y2={bottomY} stroke="rgba(232,228,216,0.06)" strokeWidth="0.5" />
      <line x1={PAD.left} y1={bottomY} x2={PAD.left + innerW} y2={bottomY} stroke="rgba(232,228,216,0.06)" strokeWidth="0.5" />
    </svg>
  )
}

function VWASIndicator({ current, ago60s }: { current: number | null; ago60s: number | null }) {
  const trend = current !== null && ago60s !== null
    ? current > ago60s + 0.01 ? 'up' : current < ago60s - 0.01 ? 'down' : 'flat'
    : null
  return (
    <div>
      <div className="text-[9px] font-medium text-brown uppercase tracking-[0.15em] mb-2">VW Avg Spread</div>
      <div className="text-[1.8rem] font-mono font-semibold tabular-nums text-ochre">
        {current !== null ? current.toFixed(2) : '—'}
        <span className="text-sm font-normal text-brown ml-1">bps</span>
      </div>
      {trend !== null && (
        <div className={['flex items-center gap-1 mt-1.5 text-sm font-medium', trend === 'up' ? 'text-terra' : trend === 'down' ? 'text-sage' : 'text-brown'].join(' ')}>
          <span className="text-lg leading-none">{trend === 'up' ? '↑' : trend === 'down' ? '↓' : '→'}</span>
          <span className="text-xs text-brown font-normal">vs 60s ago</span>
        </div>
      )}
      {ago60s !== null && <div className="text-xs text-brown mt-1 tabular-nums">60s ago: {ago60s.toFixed(2)} bps</div>}
    </div>
  )
}

interface FillRateRow {
  market: string
  trades1m: number
  avgSize: number | null
  spreadBps: number | null
  ofi: number | null
}

function FillRateTable({ rows }: { rows: FillRateRow[] }) {
  return (
    <Card padding="none">
      <div className="px-6 py-4 border-b border-border flex items-center justify-between">
        <span className="text-[0.65rem] uppercase tracking-[0.15em] text-brown font-medium">Fill Rate by Market</span>
        <Badge variant="success" dot size="sm">Live</Badge>
      </div>
      {rows.length === 0 ? (
        <p className="px-6 py-8 text-sm text-brown text-center">No market data</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left border-b border-border bg-canvas sticky top-0">
                <th className="px-5 py-3 text-[0.65rem] font-medium text-brown uppercase tracking-[0.12em]">Market</th>
                <th className="px-5 py-3 text-right text-[0.65rem] font-medium text-brown uppercase tracking-[0.12em]">
                  <Tooltip content="Number of trades executed in the last 60 seconds">Trades (1m)</Tooltip>
                </th>
                <th className="px-5 py-3 text-right text-[0.65rem] font-medium text-brown uppercase tracking-[0.12em]">
                  <Tooltip content="Average trade size over the 5-minute window">Avg Size</Tooltip>
                </th>
                <th className="px-5 py-3 text-right text-[0.65rem] font-medium text-brown uppercase tracking-[0.12em]">
                  <Tooltip content="Current bid-ask spread in basis points">Spread (bps)</Tooltip>
                </th>
                <th className="px-5 py-3 min-w-[180px] text-[0.65rem] font-medium text-brown uppercase tracking-[0.12em]">
                  <Tooltip content="Order flow imbalance = (bid10 − ask10) / total10. Positive = buy pressure.">OFI</Tooltip>
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row, idx) => (
                <tr key={row.market} className={['hover:bg-[rgba(232,228,216,0.04)] transition-colors duration-100 border-b border-border last:border-0', idx % 2 === 0 ? 'bg-parchment' : 'bg-canvas'].join(' ')}>
                  <td className="px-5 py-3 font-semibold text-ink text-xs">{row.market}</td>
                  <td className="px-5 py-3 text-right tabular-nums text-xs text-ink font-mono">{row.trades1m}</td>
                  <td className="px-5 py-3 text-right tabular-nums text-xs text-ink font-mono">{row.avgSize !== null ? row.avgSize.toFixed(4) : '—'}</td>
                  <td className="px-5 py-3 text-right tabular-nums">
                    {row.spreadBps !== null ? (
                      <Badge variant={row.spreadBps < 10 ? 'success' : row.spreadBps < 50 ? 'warning' : 'error'} size="sm">{row.spreadBps.toFixed(2)}</Badge>
                    ) : <span className="text-xs text-brown">—</span>}
                  </td>
                  <td className="px-5 py-3 min-w-[180px]">
                    {row.ofi !== null ? (
                      <div className="flex items-center gap-2">
                        <div className="flex-1 h-1.5 bg-[rgba(232,228,216,0.04)] overflow-hidden relative">
                          <div className="absolute left-1/2 top-0 bottom-0 w-px bg-[rgba(232,228,216,0.15)]" />
                          {row.ofi >= 0 ? (
                            <div className="absolute top-0 bottom-0 bg-sage transition-[width] duration-300" style={{ left: '50%', width: `${Math.abs(row.ofi) * 50}%` }} />
                          ) : (
                            <div className="absolute top-0 bottom-0 bg-terra transition-[width] duration-300" style={{ right: '50%', width: `${Math.abs(row.ofi) * 50}%` }} />
                          )}
                        </div>
                        <span className={['text-[11px] font-medium tabular-nums w-12 text-right font-mono', row.ofi >= 0 ? 'text-sage' : 'text-terra'].join(' ')}>
                          {row.ofi >= 0 ? '+' : ''}{(row.ofi * 100).toFixed(1)}%
                        </span>
                      </div>
                    ) : <span className="text-xs text-brown">—</span>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  )
}

// ─── toxicity feed ────────────────────────────────────────────────────────────

const WS_BASE = (process.env.NEXT_PUBLIC_WS_URL ?? 'wss://vela-engine.fly.dev/ws').replace(/\/ws$/, '')
const TOXICITY_WINDOW = 120

interface ToxicityPoint {
  ts: number
  score: number
  side: string
}

type ToxicityStatus = 'idle' | 'connecting' | 'authenticating' | 'streaming' | 'error'

function ToxicityChart({ points }: { points: ToxicityPoint[] }) {
  const W = 600
  const H = 120
  const PAD = { top: 12, right: 12, bottom: 28, left: 40 }
  const innerW = W - PAD.left - PAD.right
  const innerH = H - PAD.top - PAD.bottom
  const bottomY = PAD.top + innerH
  if (points.length < 2) {
    return (
      <div style={{ height: H }}>
        <ChartEmpty label="Waiting for fills…" />
      </div>
    )
  }
  const minTs = points[0]!.ts
  const maxTs = points[points.length - 1]!.ts
  const tsRange = maxTs - minTs || 1
  const toX = (ts: number) => PAD.left + ((ts - minTs) / tsRange) * innerW
  const toY = (s: number) => PAD.top + innerH - s * innerH
  const linePoints = points.map((p) => `${toX(p.ts)},${toY(p.score)}`).join(' ')
  const thresholdY = toY(0.75)
  const yLabels = [0, 0.25, 0.5, 0.75, 1.0]
  return (
    <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" width="100%" height={H}>
      {[0.25, 0.5, 0.75].map((f) => (
        <line key={f} x1={PAD.left} y1={PAD.top + innerH * (1 - f)} x2={PAD.left + innerW} y2={PAD.top + innerH * (1 - f)}
          stroke="rgba(232,228,216,0.04)" strokeWidth="0.5" strokeDasharray="3 3" />
      ))}
      <line x1={PAD.left} y1={thresholdY} x2={PAD.left + innerW} y2={thresholdY}
        stroke="rgba(204,51,51,0.3)" strokeWidth="1" strokeDasharray="4 3" />
      <text x={PAD.left + innerW - 2} y={thresholdY - 3} textAnchor="end" fontSize="8" fill="rgba(204,51,51,0.5)" fontFamily="Courier New, monospace">0.75 threshold</text>
      <polyline points={linePoints} fill="none" stroke="rgba(232,228,216,0.7)" strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
      {points.map((p, i) => (
        <circle key={i} cx={toX(p.ts)} cy={toY(p.score)} r="2.5"
          fill={p.score > 0.75 ? '#CC3333' : p.score > 0.4 ? 'rgba(232,228,216,0.6)' : '#6B8A5A'} />
      ))}
      {yLabels.map((v) => (
        <text key={v} x={PAD.left - 5} y={toY(v) + 3} textAnchor="end" fontSize="8.5"
          fill="rgba(232,228,216,0.35)" fontFamily="Courier New, monospace">{v.toFixed(2)}</text>
      ))}
      <line x1={PAD.left} y1={PAD.top} x2={PAD.left} y2={bottomY} stroke="rgba(232,228,216,0.06)" strokeWidth="0.5" />
      <line x1={PAD.left} y1={bottomY} x2={PAD.left + innerW} y2={bottomY} stroke="rgba(232,228,216,0.06)" strokeWidth="0.5" />
    </svg>
  )
}

function ToxicityFeedPanel({ market }: { market: string | null }) {
  const { address } = useAuth()
  const [points, setPoints] = useState<ToxicityPoint[]>([])
  const [status, setStatus] = useState<ToxicityStatus>('idle')
  const wsRef = useRef<WebSocket | null>(null)
  const statusRef = useRef<ToxicityStatus>('idle')

  useEffect(() => {
    statusRef.current = status
  }, [status])

  useEffect(() => {
    if (!address) {
      setStatus('idle')
      wsRef.current?.close()
      return
    }

    const url = market
      ? `${WS_BASE}/feed/toxicity?market=${encodeURIComponent(market)}`
      : `${WS_BASE}/feed/toxicity`

    setStatus('connecting')
    setPoints([])
    const ws = new WebSocket(url)
    wsRef.current = ws

    ws.onmessage = async (e: MessageEvent) => {
      let msg: Record<string, unknown>
      try { msg = JSON.parse(e.data as string) as Record<string, unknown> } catch { return }

      if (msg['type'] === 'challenge') {
        setStatus('authenticating')
        const nonce = msg['nonce'] as string
        try {
          const message = `vela:auth:${nonce}`
          const signature = (await window.ethereum!.request({
            method: 'personal_sign',
            params: [message, address],
          })) as string
          ws.send(JSON.stringify({ type: 'auth', address, signature, nonce }))
        } catch {
          setStatus('error')
          ws.close()
        }
      } else if (msg['type'] === 'authenticated') {
        setStatus('streaming')
      } else if (msg['type'] === 'error') {
        setStatus('error')
        ws.close()
      } else if (typeof msg['toxicity_score'] === 'number') {
        const pt: ToxicityPoint = {
          ts: Date.now(),
          score: msg['toxicity_score'] as number,
          side: (msg['side'] as string) ?? 'bid',
        }
        setPoints((prev) => [...prev, pt].slice(-TOXICITY_WINDOW))
      }
    }

    ws.onerror = () => setStatus('error')
    ws.onclose = () => {
      if (statusRef.current !== 'error') setStatus('idle')
    }

    return () => {
      ws.close()
      wsRef.current = null
    }
  }, [address, market])

  const lastPoint = points[points.length - 1] ?? null
  const highCount = points.filter((p) => p.score > 0.75).length

  const statusBadge = () => {
    if (status === 'streaming') return <Badge variant="success" dot size="sm">Live</Badge>
    if (status === 'connecting' || status === 'authenticating') return <Badge variant="warning" size="sm">{status === 'authenticating' ? 'Authenticating…' : 'Connecting…'}</Badge>
    if (status === 'error') return <Badge variant="error" size="sm">Error</Badge>
    return null
  }

  return (
    <Card padding="none">
      <div className="px-5 py-3 border-b border-border flex items-center justify-between">
        <span className="text-[0.65rem] font-medium text-brown uppercase tracking-[0.15em]">
          Toxicity Score Feed{market ? ` — ${market}` : ''}
        </span>
        <div className="flex items-center gap-3">
          {statusBadge()}
          {lastPoint !== null && (
            <span className={['text-xs font-mono tabular-nums font-semibold', lastPoint.score > 0.75 ? 'text-terra' : lastPoint.score > 0.4 ? 'text-ink' : 'text-sage'].join(' ')}>
              {lastPoint.score.toFixed(3)}
            </span>
          )}
        </div>
      </div>

      {!address ? (
        <div className="px-5 py-6 text-xs text-brown text-center">
          Connect wallet to view the toxicity feed
        </div>
      ) : (
        <>
          <div className="px-2 py-2">
            <ToxicityChart points={points} />
          </div>
          {points.length > 0 && (
            <div className="px-5 pb-3 flex items-center gap-6 text-[10px] text-brown">
              <span>
                <span className="text-terra font-semibold">{highCount}</span> / {points.length} fills above 0.75 threshold
              </span>
              <span className="text-[9px] text-brown opacity-50 uppercase tracking-wider">
                Last {TOXICITY_WINDOW} fills · ws:/feed/toxicity
              </span>
            </div>
          )}
        </>
      )}
    </Card>
  )
}

export default function AnalyticsPage() {
  // ── new: analytics API state ───────────────────────────────────────────────
  const [analyticsMarkets, setAnalyticsMarkets] = useState<AnalyticsMarket[] | null>(null)
  const [timeframe, setTimeframe] = useState<Timeframe>('24H')
  const [slippageMarket, setSlippageMarket] = useState('ETH-USDC')
  const [analyticsError, setAnalyticsError] = useState(false)

  const fetchAnalytics = useCallback(async () => {
    try {
      const res = await fetch(`${API_URL}/analytics?timeframe=${timeframe}`)
      const data = await res.json()
      if (data.ok && Array.isArray(data.data?.markets)) {
        setAnalyticsMarkets(data.data.markets)
        setAnalyticsError(false)
      } else {
        setAnalyticsError(true)
      }
    } catch {
      setAnalyticsError(true)
    }
  }, [timeframe])

  useEffect(() => {
    fetchAnalytics()
    const t = setInterval(fetchAnalytics, 60_000)
    return () => clearInterval(t)
  }, [fetchAnalytics])

  const slippageData = analyticsMarkets?.find((m) => m.market_id === slippageMarket) ?? null

  // ── existing: real-time state ──────────────────────────────────────────────
  const [state, dispatch] = useReducer(analyticsReducer, initState)
  const wsRef = useRef(getWsClient())

  useEffect(() => {
    listMarkets().then((res) => {
      dispatch({ type: 'INIT_MARKETS', markets: res.ok && res.data ? res.data : [] })
    })
  }, [])

  useEffect(() => {
    if (state.markets.length === 0) return
    const ws = wsRef.current
    ws.connect()
    const channels = state.markets.map((m) => `trades:${m.id}`)
    ws.subscribeChannels(channels)
    const unsub = ws.onMessage((msg) => {
      if (msg.type === 'trade') {
        dispatch({ type: 'ADD_TRADE', market: msg.market, trade: { ts: msg.timestamp, price: parseFloat(msg.price), size: parseFloat(msg.quantity), side: msg.side as 'buy' | 'sell' } })
      }
    })
    return () => { unsub(); ws.unsubscribeChannels(channels) }
  }, [state.markets])

  useEffect(() => {
    if (state.markets.length === 0) return
    async function poll() {
      await Promise.all(
        state.markets.map(async (m) => {
          const res = await getBook(m.id)
          if (res.ok && res.data) {
            dispatch({ type: 'DEPTH_POLL', market: m.id, bids: res.data.bids.map((l) => [l.price, l.quantity] as [string, string]), asks: res.data.asks.map((l) => [l.price, l.quantity] as [string, string]) })
          }
        }),
      )
    }
    poll()
    const id = setInterval(poll, 2000)
    return () => clearInterval(id)
  }, [state.markets])

  useEffect(() => {
    const id = setInterval(() => dispatch({ type: 'PRUNE', cutoff: Date.now() - WINDOW_MS - 60_000 }), 30_000)
    return () => clearInterval(id)
  }, [])

  const selectedDepth = useMemo(
    () => state.selectedMarket ? (state.depthByMarket[state.selectedMarket] ?? null) : null,
    [state.selectedMarket, state.depthByMarket],
  )

  const selectedSpreadHistory = useMemo(
    () => state.selectedMarket ? (state.spreadByMarket[state.selectedMarket] ?? []) : [],
    [state.selectedMarket, state.spreadByMarket],
  )

  const vwas = useMemo(() => {
    if (!state.selectedMarket) return { current: null, ago60s: null }
    const trades = state.tradesByMarket[state.selectedMarket] ?? []
    const spreads = state.spreadByMarket[state.selectedMarket] ?? []
    if (trades.length === 0 || spreads.length === 0) return { current: null, ago60s: null }
    function spreadAt(ts: number): number | null {
      let last: SpreadPoint | null = null
      for (const sp of spreads) { if (sp.ts <= ts) last = sp; else break }
      return last?.bps ?? null
    }
    function computeWindow(from: number, to: number): number | null {
      const window = trades.filter((t) => t.ts >= from && t.ts <= to)
      if (window.length === 0) return null
      let weightedSum = 0; let totalSize = 0
      for (const t of window) { const sp = spreadAt(t.ts); if (sp !== null) { weightedSum += sp * t.size; totalSize += t.size } }
      return totalSize > 0 ? weightedSum / totalSize : null
    }
    const now = Date.now()
    return { current: computeWindow(now - 60_000, now), ago60s: computeWindow(now - 120_000, now - 60_000) }
  }, [state.selectedMarket, state.tradesByMarket, state.spreadByMarket])

  const fillRateRows = useMemo((): FillRateRow[] => {
    const now = Date.now()
    return state.markets.map((m) => {
      const trades = state.tradesByMarket[m.id] ?? []
      const depth = state.depthByMarket[m.id] ?? null
      const trades1m = trades.filter((t) => t.ts >= now - 60_000).length
      const avgSize = trades.length > 0 ? trades.reduce((s, t) => s + t.size, 0) / trades.length : null
      return { market: m.id, trades1m, avgSize, spreadBps: depth?.spreadBps ?? null, ofi: depth?.ofi ?? null }
    })
  }, [state.markets, state.tradesByMarket, state.depthByMarket])

  if (state.marketsLoading && analyticsMarkets === null) return <FullPageSpinner />

  const tfBtn = (tf: Timeframe) => ({
    fontFamily: IN,
    fontSize: '11px',
    textTransform: 'uppercase' as const,
    letterSpacing: '0.1em',
    color: timeframe === tf ? '#0C0C0C' : 'rgba(12,12,12,0.35)',
    background: 'transparent',
    border: 'none',
    borderBottom: timeframe === tf ? '2px solid #0C0C0C' : '2px solid transparent',
    padding: '8px 16px',
    cursor: 'pointer',
  })

  return (
    <div style={{ background: '#0C0C0C', minHeight: '100vh' }}>

      {/* ── PAGE HEADER ──────────────────────────────────────────────────── */}
      <div style={{ position: 'relative', background: '#0C0C0C', overflow: 'hidden' }} className="px-6 pt-12 pb-10 lg:px-[52px] lg:pt-[60px] lg:pb-[40px]">
        <HexCanvas />
        <div style={{ position: 'relative', zIndex: 1 }}>
          <p style={{ fontFamily: IN, fontSize: '10px', textTransform: 'uppercase', letterSpacing: '0.2em', color: 'rgba(232,228,216,0.3)', marginBottom: '16px' }}>
            Vela Exchange — Market Analytics
          </p>
          <div>
            <span style={{ fontFamily: PF, fontWeight: 700, fontSize: '48px', color: '#E8E4D8', display: 'block', lineHeight: 1.1 }}>
              Market quality.
            </span>
            <span style={{ fontFamily: PF, fontWeight: 400, fontStyle: 'italic', fontSize: '48px', color: 'rgba(232,228,216,0.3)', display: 'block', lineHeight: 1.1 }}>
              Measured and published.
            </span>
          </div>
          <p style={{ fontFamily: IN, fontWeight: 300, fontSize: '14px', lineHeight: 1.75, color: 'rgba(232,228,216,0.4)', maxWidth: '540px', marginTop: '20px' }}>
            Spread, slippage, depth, and volume for all 16 markets. Updated every 60 seconds. No other exchange publishes this data.
          </p>
        </div>
      </div>

      {/* ── TIMEFRAME SELECTOR ───────────────────────────────────────────── */}
      <div style={{ background: '#E8E4D8' }} className="px-6 lg:px-[52px]" >
        <div style={{ paddingTop: '24px', borderBottom: '1px solid rgba(12,12,12,0.07)', display: 'flex' }}>
          {(['1H', '24H', '7D'] as Timeframe[]).map((tf) => (
            <button key={tf} onClick={() => setTimeframe(tf)} style={tfBtn(tf)}>{tf}</button>
          ))}
        </div>
      </div>

      {/* ── MARKET ANALYTICS TABLE ───────────────────────────────────────── */}
      <div style={{ background: '#E8E4D8' }} className="px-6 pb-12 lg:px-[52px] lg:pb-[52px]">
        <div style={{ paddingTop: '28px' }}>
          {analyticsError ? (
            <p style={{ fontFamily: IN, fontSize: '13px', color: 'rgba(12,12,12,0.35)', textAlign: 'center', padding: '48px 0' }}>
              Analytics unavailable.
            </p>
          ) : analyticsMarkets === null ? (
            <div>
              {Array.from({ length: 6 }).map((_, i) => (
                <div key={i} style={{ padding: '14px 0', borderBottom: '1px solid rgba(12,12,12,0.05)', display: 'grid', gridTemplateColumns: '120px 80px 80px 80px 80px 100px 1fr', gap: '0 16px' }}>
                  {Array.from({ length: 7 }).map((_, j) => (
                    <div key={j} style={{ height: '12px', background: 'rgba(12,12,12,0.06)', width: `${60 + (j * 10) % 40}%` }} />
                  ))}
                </div>
              ))}
            </div>
          ) : (
            <>
              <div style={{ display: 'grid', gridTemplateColumns: '120px 80px 80px 80px 80px 100px 1fr', gap: '0 16px', padding: '0 0 8px', borderBottom: '1px solid rgba(12,12,12,0.08)' }}>
                {[
                  { label: 'MARKET', tip: undefined },
                  { label: 'SPREAD', tip: 'Current bid-ask spread in basis points (1 bps = 0.01%)' },
                  { label: '$1K SLIP', tip: 'Estimated slippage for a $1,000 market order' },
                  { label: '$10K SLIP', tip: 'Estimated slippage for a $10,000 market order' },
                  { label: '$100K SLIP', tip: 'Estimated slippage for a $100,000 market order' },
                  { label: 'VOLUME', tip: 'Total trading volume in the selected timeframe' },
                  { label: 'DEPTH', tip: 'Total liquidity within 1% of mid price' },
                ].map(({ label, tip }) => (
                  <span
                    key={label}
                    title={tip}
                    style={{ fontFamily: IN, fontSize: '8px', textTransform: 'uppercase', letterSpacing: '0.15em', color: 'rgba(12,12,12,0.35)', cursor: tip ? 'help' : undefined }}
                  >
                    {label}
                  </span>
                ))}
              </div>
              {analyticsMarkets.map((m) => (
                <div
                  key={m.market_id}
                  style={{ display: 'grid', gridTemplateColumns: '120px 80px 80px 80px 80px 100px 1fr', gap: '0 16px', padding: '14px 0', borderBottom: '1px solid rgba(12,12,12,0.05)', alignItems: 'center' }}
                >
                  <span style={{ fontFamily: IN, fontWeight: 600, fontSize: '12px', color: '#0C0C0C' }}>
                    {ticker(m.market_id)}/USDC
                  </span>
                  <span style={{ fontFamily: CN, fontSize: '11px', color: spreadColor(m.current_spread_bps) }}>
                    {fmtBps(m.current_spread_bps)}
                  </span>
                  <span style={{ fontFamily: CN, fontSize: '11px', color: spreadColor(m.slippage_1k_usdc) }}>
                    {fmtBps(m.slippage_1k_usdc)}
                  </span>
                  <span style={{ fontFamily: CN, fontSize: '11px', color: spreadColor(m.slippage_10k_usdc) }}>
                    {fmtBps(m.slippage_10k_usdc)}
                  </span>
                  <span style={{ fontFamily: CN, fontSize: '11px', color: spreadColor(m.slippage_100k_usdc) }}>
                    {fmtBps(m.slippage_100k_usdc)}
                  </span>
                  <span style={{ fontFamily: CN, fontSize: '11px', color: 'rgba(12,12,12,0.6)' }}>
                    {fmtUsd(m.total_volume_usdc)}
                  </span>
                  <span style={{ fontFamily: CN, fontSize: '11px', color: 'rgba(12,12,12,0.5)' }}>
                    {fmtUsd(m.depth_1pct_total_usdc)}
                  </span>
                </div>
              ))}
            </>
          )}
        </div>
      </div>

      {/* ── SLIPPAGE CURVES ──────────────────────────────────────────────── */}
      <div style={{ position: 'relative', background: '#0C0C0C', overflow: 'hidden' }} className="px-6 py-12 lg:px-[52px] lg:py-[52px]">
        <HexCanvas />
        <div style={{ position: 'relative', zIndex: 1 }}>
          <p style={{ fontFamily: IN, fontSize: '10px', textTransform: 'uppercase', letterSpacing: '0.2em', color: 'rgba(232,228,216,0.3)', marginBottom: '12px' }}>
            Slippage Curves
          </p>
          <h2 style={{ fontFamily: PF, fontWeight: 700, fontSize: '32px', color: '#E8E4D8', margin: '0 0 12px' }}>
            Cost of trading by size.
          </h2>
          <p style={{ fontFamily: IN, fontWeight: 300, fontSize: '14px', lineHeight: 1.75, color: 'rgba(232,228,216,0.38)', maxWidth: '500px', marginBottom: '32px' }}>
            Estimated market impact for each market across standard order sizes. Lower is better.
          </p>

          <div style={{ display: 'flex', gap: '8px', marginBottom: '32px', flexWrap: 'wrap' }}>
            {analyticsMarkets?.map((m) => (
              <button
                key={m.market_id}
                onClick={() => setSlippageMarket(m.market_id)}
                style={{
                  fontFamily: IN,
                  fontSize: '10px',
                  textTransform: 'uppercase',
                  letterSpacing: '0.1em',
                  color: slippageMarket === m.market_id ? '#0C0C0C' : 'rgba(232,228,216,0.4)',
                  background: slippageMarket === m.market_id ? '#E8E4D8' : 'transparent',
                  border: '1px solid rgba(232,228,216,0.15)',
                  padding: '5px 12px',
                  cursor: 'pointer',
                }}
              >
                {ticker(m.market_id)}
              </button>
            ))}
          </div>

          {slippageData ? (
            <SlippageBarChart
              slippage1k={slippageData.slippage_1k_usdc}
              slippage10k={slippageData.slippage_10k_usdc}
              slippage100k={slippageData.slippage_100k_usdc}
            />
          ) : (
            <p style={{ fontFamily: IN, fontSize: '13px', color: 'rgba(232,228,216,0.3)' }}>—</p>
          )}
        </div>
      </div>

      {/* ── METHODOLOGY ──────────────────────────────────────────────────── */}
      <div style={{ background: '#E8E4D8' }} className="px-6 py-12 lg:px-[52px] lg:py-[52px]">
        <p style={{ fontFamily: IN, fontSize: '10px', textTransform: 'uppercase', letterSpacing: '0.2em', color: 'rgba(12,12,12,0.3)', marginBottom: '12px' }}>
          Methodology
        </p>
        <h2 style={{ fontFamily: PF, fontWeight: 700, fontSize: '28px', color: '#0C0C0C', margin: '0 0 32px' }}>
          How these numbers are computed.
        </h2>

        <div className="grid grid-cols-1 lg:grid-cols-3 gap-[1px]" style={{ background: 'rgba(12,12,12,0.07)' }}>
          {[
            {
              title: 'SPREAD',
              body: 'Spread is the difference between the best ask and best bid, expressed in basis points relative to the mid price. Computed from the live order book in real time.',
            },
            {
              title: 'SLIPPAGE',
              body: 'Slippage estimates are computed by walking the live order book to simulate the average fill price for a market order of the given size. Actual slippage may vary based on order timing and market conditions.',
            },
            {
              title: 'DEPTH',
              body: 'Market depth shows the total available liquidity within 1% of the current mid price on both the bid and ask sides. Higher depth means larger orders can be filled with less slippage.',
            },
          ].map((c) => (
            <div key={c.title} style={{ background: 'white', padding: '28px' }}>
              <p style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.15em', color: 'rgba(12,12,12,0.3)', margin: '0 0 12px' }}>{c.title}</p>
              <p style={{ fontFamily: IN, fontWeight: 300, fontSize: '13px', color: 'rgba(12,12,12,0.7)', lineHeight: 1.8, margin: 0 }}>{c.body}</p>
            </div>
          ))}
        </div>

        <p style={{ fontFamily: IN, fontSize: '10px', color: 'rgba(12,12,12,0.35)', marginTop: '20px' }}>
          All analytics are computed from the live Vela order book and trade history. Methodology is open source at github.com/arpjw/vela.
        </p>

        <div style={{ marginTop: '40px', paddingTop: '32px', borderTop: '1px solid rgba(12,12,12,0.07)' }}>
          <p style={{ fontFamily: IN, fontSize: '10px', textTransform: 'uppercase', letterSpacing: '0.2em', color: 'rgba(12,12,12,0.3)', marginBottom: '16px' }}>
            Engine Performance Context
          </p>
          <div className="grid grid-cols-1 lg:grid-cols-2 gap-[1px]" style={{ background: 'rgba(12,12,12,0.07)', marginBottom: '16px' }}>
            <div style={{ background: 'white', padding: '24px' }}>
              <p style={{ fontFamily: PF, fontWeight: 900, fontSize: '30px', color: '#0C0C0C', margin: '0 0 4px', lineHeight: 1 }}>2.5M ops/sec</p>
              <p style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.15em', color: 'rgba(12,12,12,0.35)', margin: '0 0 8px' }}>
                Vela engine throughput (isolated microbench)
              </p>
              <p style={{ fontFamily: IN, fontWeight: 300, fontSize: '11px', color: 'rgba(12,12,12,0.5)', margin: 0, lineHeight: 1.6 }}>
                vs. Hyperliquid&apos;s 200k ops/sec execution ceiling¹
              </p>
            </div>
            <div style={{ background: 'white', padding: '24px' }}>
              <p style={{ fontFamily: PF, fontWeight: 900, fontSize: '30px', color: '#0C0C0C', margin: '0 0 4px', lineHeight: 1 }}>16.9 ms</p>
              <p style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.15em', color: 'rgba(12,12,12,0.35)', margin: '0 0 8px' }}>
                Vela end-to-end p50 (single client, loopback)
              </p>
              <p style={{ fontFamily: IN, fontWeight: 300, fontSize: '11px', color: 'rgba(12,12,12,0.5)', margin: 0, lineHeight: 1.6 }}>
                vs. Hyperliquid&apos;s 200 ms p50 (colocated, includes HyperBFT consensus)¹
              </p>
            </div>
          </div>
          <p style={{ fontFamily: IN, fontSize: '10px', color: 'rgba(12,12,12,0.35)', lineHeight: 1.7 }}>
            ¹ Vela engine benchmarks measure the isolated matching layer only (no network transit, no BFT consensus). Hyperliquid&apos;s published figures include HyperBFT consensus and network overhead for colocated clients. Vela does not currently have a consensus layer. End-to-end figures are in-process loopback measurements. Hyperliquid&apos;s 200k ops/sec ceiling is self-reported and execution-bottlenecked per their own documentation. All comparisons should be interpreted as execution-layer vs. execution-layer, not full-system vs. full-system.
          </p>
        </div>
      </div>

      {/* ── LIVE ANALYTICS (existing real-time sections) ─────────────────── */}
      <div className="max-w-7xl mx-auto px-4 sm:px-6 py-8">
        <div className="flex items-center gap-2 flex-wrap mb-6">
          {state.markets.map((m) => (
            <button
              key={m.id}
              type="button"
              onClick={() => dispatch({ type: 'SELECT_MARKET', market: m.id })}
              className={['px-3 py-1.5 text-sm font-medium transition-colors duration-150 uppercase tracking-[0.08em]',
                state.selectedMarket === m.id ? 'bg-[#E8E4D8] text-[#0C0C0C]' : 'border border-border text-brown bg-transparent hover:bg-canvas'].join(' ')}
            >
              {m.base}/{m.quote}
            </button>
          ))}
          <div className="ml-auto"><Badge variant="success" dot size="sm">Live</Badge></div>
        </div>

        {state.selectedMarket && (
          <>
            <div className="grid grid-cols-1 lg:grid-cols-[1fr_260px] gap-5 mb-5">
              <Card padding="none">
                <div className="px-5 py-3 border-b border-border flex items-center justify-between">
                  <span className="text-[0.65rem] font-medium text-brown uppercase tracking-[0.15em]">Bid-Ask Spread — 5 min rolling</span>
                  {selectedDepth?.spreadBps !== null && selectedDepth?.spreadBps !== undefined && (
                    <Badge variant={selectedDepth.spreadBps < 10 ? 'success' : selectedDepth.spreadBps < 50 ? 'warning' : 'error'} size="sm">
                      {selectedDepth.spreadBps.toFixed(2)} bps
                    </Badge>
                  )}
                </div>
                <div className="px-2 py-2"><SpreadChart history={selectedSpreadHistory} /></div>
              </Card>
              <Card>
                <VWASIndicator current={vwas.current} ago60s={vwas.ago60s} />
                <div className="mt-5 pt-5 border-t border-border">
                  <div className="text-[10px] font-medium text-brown uppercase tracking-wider mb-3">Order Flow Imbalance</div>
                  {selectedDepth ? <OFIBar ofi={selectedDepth.ofi} /> : <p className="text-xs text-brown">Waiting for data…</p>}
                </div>
              </Card>
            </div>

            <Card padding="none" className="mb-5">
              <div className="px-5 py-3 border-b border-border flex items-center justify-between">
                <span className="text-[0.65rem] font-medium text-brown uppercase tracking-[0.15em]">Depth Chart — 10 levels</span>
                <div className="flex items-center gap-3">
                  <span className="flex items-center gap-1.5 text-[10px] text-brown"><span className="w-3 h-0.5 bg-sage inline-block" />Bids</span>
                  <span className="flex items-center gap-1.5 text-[10px] text-brown"><span className="w-3 h-0.5 bg-terra inline-block" />Asks</span>
                </div>
              </div>
              <div className="px-2 py-2"><DepthChart depth={selectedDepth} /></div>
            </Card>

            <div className="mb-5">
              <ToxicityFeedPanel market={state.selectedMarket} />
            </div>
          </>
        )}

        <FillRateTable rows={fillRateRows} />
      </div>
    </div>
  )
}
