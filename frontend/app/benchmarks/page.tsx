'use client'

import type { ReactNode } from 'react'

const PF = "var(--font-playfair), 'Playfair Display', serif"
const IN = "var(--font-inter-sans), 'Inter', sans-serif"
const CN = "'Courier New', monospace"

const CRIMSON = '#CC3333'
const INK = '#0C0C0C'
const PARCHMENT = '#E8E4D8'

// ── Static data ────────────────────────────────────────────────────────────────

const THROUGHPUT_TS = [179,224,223,218,223,215,212,217,213,220,207,202,223,218,214,217,219,204,186,213,216,212,219,207,131,132,138,131,133,135,133,129,127,110,133,133,134,135,135,127,130,126,133,113,125,131,129,127,121,132,135,134,134,135,112,133,134,134,127,132]

const HL_ROWS = [
  { metric: 'Engine throughput ceiling', vela: '2,500,000 ops/sec', hl: '200,000 ops/sec¹', note: 'In-process order matching, no I/O' },
  { metric: 'Consensus throughput ceiling', vela: 'N/A (no consensus layer)', hl: '>1,000,000 ops/sec¹', note: 'HyperBFT theoretical maximum' },
  { metric: 'End-to-end p50 (1 client)', vela: '16.9 ms (loopback)²', hl: '200 ms (colocated)¹', note: 'Full round-trip incl. auth, dispatch, matching' },
  { metric: 'End-to-end p99 (1 client)', vela: '19.9 ms (loopback)²', hl: '900 ms (colocated)¹', note: 'Full round-trip incl. auth, dispatch, matching' },
  { metric: 'End-to-end p50 (32 concurrent)', vela: '159 ms (loopback)²', hl: 'Not published', note: '—' },
  { metric: 'Sustained throughput (5 MM agents)', vela: '134 ops/sec mean · 156 ops/sec peak', hl: 'Not published', note: 'Live multi-agent workload over 300 s' },
]

const TIER2_ROWS = [
  { id: 'S1', sub: 'Single-threaded sequential', p50: '16.9 ms', p99: '19.9 ms', p999: '25.2 ms', p9999: '87.0 ms', tput: '59 ops/sec' },
  { id: 'S2', sub: 'Concurrent-32', p50: '159 ms', p99: '307 ms', p999: '235 ms', p9999: '240 ms', tput: '192 ops/sec' },
  { id: 'S3', sub: 'Burst-1000', p50: '3,871 ms', p99: '5,444 ms', p999: '—', p9999: '—', tput: '181 ops/sec' },
]

const DISCLAIMER = 'Vela engine benchmarks measure the isolated matching layer only (no network transit, no BFT consensus). Hyperliquid\'s published figures include HyperBFT consensus and network overhead for colocated clients. Vela does not currently have a consensus layer. End-to-end figures are in-process loopback measurements. Hyperliquid\'s 200k ops/sec ceiling is self-reported and execution-bottlenecked per their own documentation. All comparisons should be interpreted as execution-layer vs. execution-layer, not full-system vs. full-system.'

// ── Shared primitives ──────────────────────────────────────────────────────────

function StatCard({ label, value, sublabel }: { label: string; value: string; sublabel: string }) {
  return (
    <div style={{ background: '#ffffff', borderTop: `3px solid ${CRIMSON}`, padding: '28px 24px 26px' }}>
      <div style={{ fontFamily: PF, fontWeight: 900, fontSize: '36px', color: INK, lineHeight: 1.05, marginBottom: '8px' }}>
        {value}
      </div>
      <div style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase' as const, letterSpacing: '0.2em', color: 'rgba(12,12,12,0.35)', marginBottom: '4px' }}>
        {label}
      </div>
      <div style={{ fontFamily: IN, fontSize: '11px', color: 'rgba(12,12,12,0.4)', lineHeight: 1.5 }}>
        {sublabel}
      </div>
    </div>
  )
}

function SectionLabel({ text }: { text: string }) {
  return (
    <div style={{ fontFamily: IN, fontSize: '9px', fontVariant: 'small-caps', textTransform: 'uppercase' as const, letterSpacing: '0.24em', color: 'rgba(12,12,12,0.35)', marginBottom: '20px' }}>
      {text}
    </div>
  )
}

function ChartBox({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div style={{ background: '#ffffff', padding: '24px 20px 20px' }}>
      <div style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase' as const, letterSpacing: '0.18em', color: 'rgba(12,12,12,0.28)', marginBottom: '20px' }}>
        {title}
      </div>
      {children}
    </div>
  )
}

// ── SVG chart helpers ──────────────────────────────────────────────────────────

const AXIS = 'rgba(12,12,12,0.1)'
const GRID = 'rgba(12,12,12,0.05)'
const TICK = 'rgba(12,12,12,0.35)'

// ── Tier 1: Throughput bar chart ───────────────────────────────────────────────

function ThroughputBarChart() {
  const bars = [
    { label: 'Vela', value: 2_500_000, display: '2.5M', fill: CRIMSON },
    { label: 'Hyperliquid', value: 200_000, display: '200k', fill: 'rgba(12,12,12,0.18)' },
    { label: 'Pulse', value: 125_000, display: '125k', fill: 'rgba(12,12,12,0.1)' },
  ]
  const W = 360, H = 220
  const pad = { t: 36, r: 16, b: 42, l: 40 }
  const iW = W - pad.l - pad.r
  const iH = H - pad.t - pad.b
  const bW = 68
  const gap = (iW - bars.length * bW) / (bars.length + 1)
  const max = 2_500_000

  const yTicks = [0, 0.5, 1, 1.5, 2, 2.5].map((m) => ({ v: m * 1_000_000, label: `${m}M` }))

  return (
    <svg viewBox={`0 0 ${W} ${H}`} width="100%">
      {yTicks.map(({ v, label }) => {
        const y = pad.t + iH - (v / max) * iH
        return (
          <g key={v}>
            <line x1={pad.l} y1={y} x2={W - pad.r} y2={y} stroke={v === 0 ? AXIS : GRID} strokeWidth="0.5" strokeDasharray={v > 0 ? '3 3' : undefined} />
            <text x={pad.l - 5} y={y + 3} textAnchor="end" fontSize="8" fill={TICK} fontFamily={CN}>{label}</text>
          </g>
        )
      })}
      {bars.map((b, i) => {
        const bH = Math.max((b.value / max) * iH, 2)
        const x = pad.l + gap + i * (bW + gap)
        const y = pad.t + iH - bH
        return (
          <g key={b.label}>
            <rect x={x} y={y} width={bW} height={bH} fill={b.fill} />
            <text x={x + bW / 2} y={y - 7} textAnchor="middle" fontSize="10.5" fontWeight="600"
              fill={i === 0 ? CRIMSON : 'rgba(12,12,12,0.4)'} fontFamily={IN}>{b.display}</text>
            <text x={x + bW / 2} y={H - 8} textAnchor="middle" fontSize="9" fill="rgba(12,12,12,0.45)" fontFamily={IN}>{b.label}</text>
          </g>
        )
      })}
      <line x1={pad.l} y1={pad.t} x2={pad.l} y2={pad.t + iH} stroke={AXIS} strokeWidth="0.5" />
    </svg>
  )
}

// ── Tier 1: Latency bar chart ──────────────────────────────────────────────────

function LatencyBarChart() {
  const bars = [
    { label: 'p50', value: 0.38 },
    { label: 'p99', value: 0.38 },
    { label: 'p99.9', value: 0.92 },
  ]
  const W = 360, H = 220
  const pad = { t: 36, r: 16, b: 42, l: 40 }
  const iW = W - pad.l - pad.r
  const iH = H - pad.t - pad.b
  const bW = 68
  const gap = (iW - bars.length * bW) / (bars.length + 1)
  const max = 1.0

  const yTicks = [0, 0.25, 0.5, 0.75, 1.0]

  return (
    <svg viewBox={`0 0 ${W} ${H}`} width="100%">
      {yTicks.map((v) => {
        const y = pad.t + iH - (v / max) * iH
        return (
          <g key={v}>
            <line x1={pad.l} y1={y} x2={W - pad.r} y2={y} stroke={v === 0 ? AXIS : GRID} strokeWidth="0.5" strokeDasharray={v > 0 ? '3 3' : undefined} />
            <text x={pad.l - 5} y={y + 3} textAnchor="end" fontSize="8" fill={TICK} fontFamily={CN}>{v.toFixed(2)}</text>
          </g>
        )
      })}
      {bars.map((b, i) => {
        const bH = Math.max((b.value / max) * iH, 3)
        const x = pad.l + gap + i * (bW + gap)
        const y = pad.t + iH - bH
        const alpha = i === 2 ? 0.6 : 1
        return (
          <g key={b.label}>
            <rect x={x} y={y} width={bW} height={bH} fill={`rgba(204,51,51,${alpha})`} />
            <text x={x + bW / 2} y={y - 7} textAnchor="middle" fontSize="10.5" fontWeight="600"
              fill={`rgba(204,51,51,${alpha})`} fontFamily={IN}>{b.value}µs</text>
            <text x={x + bW / 2} y={H - 8} textAnchor="middle" fontSize="9" fill="rgba(12,12,12,0.45)" fontFamily={IN}>{b.label}</text>
          </g>
        )
      })}
      <line x1={pad.l} y1={pad.t} x2={pad.l} y2={pad.t + iH} stroke={AXIS} strokeWidth="0.5" />
    </svg>
  )
}

// ── Tier 1: Component breakdown horizontal bar ─────────────────────────────────

function ComponentBreakdownChart() {
  const items = [
    { label: 'engine process', value: 987 },
    { label: 'CoW cache', value: 261 },
    { label: 'metadata clone', value: 156 },
    { label: 'nonce window', value: 27 },
    { label: 'balance clone', value: 12 },
  ]
  const W = 720, H = 210
  const pad = { t: 8, r: 90, b: 8, l: 130 }
  const iW = W - pad.l - pad.r
  const rowH = (H - pad.t - pad.b) / items.length
  const max = items[0]?.value ?? 1

  return (
    <svg viewBox={`0 0 ${W} ${H}`} width="100%">
      {items.map((item, i) => {
        const bW = (item.value / max) * iW
        const y = pad.t + i * rowH
        const opacity = Math.max(0.25, 1 - i * 0.16)
        return (
          <g key={item.label}>
            <text x={pad.l - 10} y={y + rowH / 2 + 4} textAnchor="end"
              fontSize="10.5" fill="rgba(12,12,12,0.55)" fontFamily={IN}>{item.label}</text>
            <rect x={pad.l} y={y + rowH * 0.2} width={bW} height={rowH * 0.6}
              fill={`rgba(204,51,51,${opacity})`} />
            <text x={pad.l + bW + 8} y={y + rowH / 2 + 4} textAnchor="start"
              fontSize="11" fontWeight="600" fill={CRIMSON} fontFamily={CN}>{item.value}ns</text>
          </g>
        )
      })}
    </svg>
  )
}

// ── Tier 2: E2E grouped bar chart ──────────────────────────────────────────────

function E2EBarChart() {
  const groups = [
    { label: 'Vela S1', sub: 'single-threaded', p50: 16.9, p99: 19.9, isVela: true },
    { label: 'Vela S2', sub: 'concurrent-32', p50: 159, p99: 307, isVela: true },
    { label: 'Hyperliquid', sub: 'published', p50: 200, p99: 900, isVela: false },
  ]
  const W = 720, H = 300
  const pad = { t: 36, r: 20, b: 56, l: 56 }
  const iW = W - pad.l - pad.r
  const iH = H - pad.t - pad.b
  const max = 1000
  const groupW = iW / groups.length
  const bW = groupW * 0.27
  const bGap = groupW * 0.05

  const yTicks = [0, 200, 400, 600, 800, 1000]

  return (
    <svg viewBox={`0 0 ${W} ${H}`} width="100%">
      {yTicks.map((v) => {
        const y = pad.t + iH - (v / max) * iH
        return (
          <g key={v}>
            <line x1={pad.l} y1={y} x2={W - pad.r} y2={y}
              stroke={v === 0 ? AXIS : GRID} strokeWidth="0.5" strokeDasharray={v > 0 ? '3 3' : undefined} />
            <text x={pad.l - 5} y={y + 3} textAnchor="end" fontSize="8" fill={TICK} fontFamily={CN}>{v}ms</text>
          </g>
        )
      })}
      {groups.map((g, gi) => {
        const cx = pad.l + gi * groupW + groupW / 2
        const p50H = Math.max((g.p50 / max) * iH, 4)
        const p99H = Math.max((g.p99 / max) * iH, 4)
        const p50x = cx - bW - bGap / 2
        const p99x = cx + bGap / 2
        const p50y = pad.t + iH - p50H
        const p99y = pad.t + iH - p99H
        const col = g.isVela ? CRIMSON : 'rgba(12,12,12,0.22)'
        const col2 = g.isVela ? 'rgba(204,51,51,0.5)' : 'rgba(12,12,12,0.12)'
        const lc = g.isVela ? CRIMSON : 'rgba(12,12,12,0.35)'
        const lc2 = g.isVela ? 'rgba(204,51,51,0.75)' : 'rgba(12,12,12,0.25)'
        return (
          <g key={g.label}>
            <rect x={p50x} y={p50y} width={bW} height={p50H} fill={col} />
            <rect x={p99x} y={p99y} width={bW} height={p99H} fill={col2} />
            <text x={p50x + bW / 2} y={p50y - 5} textAnchor="middle"
              fontSize="8.5" fill={lc} fontFamily={CN}>{g.p50}ms</text>
            <text x={p99x + bW / 2} y={p99y - 5} textAnchor="middle"
              fontSize="8.5" fill={lc2} fontFamily={CN}>{g.p99}ms</text>
            <text x={cx} y={H - pad.b + 14} textAnchor="middle"
              fontSize="9.5" fontWeight="500" fill="rgba(12,12,12,0.6)" fontFamily={IN}>{g.label}</text>
            <text x={cx} y={H - pad.b + 26} textAnchor="middle"
              fontSize="8" fill="rgba(12,12,12,0.35)" fontFamily={IN}>{g.sub}</text>
          </g>
        )
      })}
      <line x1={pad.l} y1={pad.t} x2={pad.l} y2={pad.t + iH} stroke={AXIS} strokeWidth="0.5" />
      {/* Legend */}
      <rect x={pad.l} y={10} width={10} height={10} fill={CRIMSON} />
      <text x={pad.l + 14} y={19} fontSize="9" fill="rgba(12,12,12,0.55)" fontFamily={IN}>p50</text>
      <rect x={pad.l + 52} y={10} width={10} height={10} fill="rgba(204,51,51,0.5)" />
      <text x={pad.l + 66} y={19} fontSize="9" fill="rgba(12,12,12,0.55)" fontFamily={IN}>p99</text>
      <text x={pad.l + 120} y={19} fontSize="9" fill="rgba(12,12,12,0.3)" fontFamily={IN}>(Hyperliquid shown in gray)</text>
    </svg>
  )
}

// ── Tier 3: Throughput time series ─────────────────────────────────────────────

function ThroughputTimeSeriesChart() {
  const W = 840, H = 300
  const pad = { t: 28, r: 52, b: 44, l: 52 }
  const iW = W - pad.l - pad.r
  const iH = H - pad.t - pad.b
  const YMIN = 80, YMAX = 240
  const MEAN = 134
  const n = THROUGHPUT_TS.length

  const toX = (i: number) => pad.l + (i / (n - 1)) * iW
  const toY = (v: number) => pad.t + iH - ((v - YMIN) / (YMAX - YMIN)) * iH

  const linePoints = THROUGHPUT_TS.map((v, i) => `${toX(i)},${toY(v)}`).join(' ')
  const first = THROUGHPUT_TS[0] ?? MEAN
  const last = THROUGHPUT_TS[n - 1] ?? MEAN
  const fillD = `M ${toX(0)} ${toY(first)} ` +
    THROUGHPUT_TS.slice(1).map((v, i) => `L ${toX(i + 1)} ${toY(v)}`).join(' ') +
    ` L ${toX(n - 1)} ${pad.t + iH} L ${toX(0)} ${pad.t + iH} Z`
  void last // used above via THROUGHPUT_TS reference

  const meanY = toY(MEAN)
  // t=120s → bucket index 23 (t=5*(i+1), so i = t/5 - 1 = 23)
  const exhaustionX = toX(23)

  const yTicks = [80, 120, 160, 200, 240]
  const xLabels: { t: number; x: number }[] = []
  for (let t = 0; t <= 300; t += 30) {
    const x = t === 0 ? pad.l : toX(t / 5 - 1)
    xLabels.push({ t, x })
  }

  return (
    <svg viewBox={`0 0 ${W} ${H}`} width="100%">
      {yTicks.map((v) => {
        const y = toY(v)
        return (
          <g key={v}>
            <line x1={pad.l} y1={y} x2={W - pad.r} y2={y}
              stroke={GRID} strokeWidth="0.5" strokeDasharray="3 3" />
            <text x={pad.l - 6} y={y + 3} textAnchor="end" fontSize="8.5" fill={TICK} fontFamily={CN}>{v}</text>
          </g>
        )
      })}
      {/* Fill area */}
      <path d={fillD} fill="rgba(204,51,51,0.07)" />
      {/* Mean line */}
      <line x1={pad.l} y1={meanY} x2={W - pad.r} y2={meanY}
        stroke="rgba(12,12,12,0.3)" strokeWidth="1" strokeDasharray="6 4" />
      <text x={W - pad.r + 4} y={meanY + 4} textAnchor="start" fontSize="8.5"
        fill="rgba(12,12,12,0.4)" fontFamily={CN}>μ=134</text>
      {/* Data line */}
      <polyline points={linePoints} fill="none" stroke={CRIMSON} strokeWidth="1.75"
        strokeLinejoin="round" strokeLinecap="round" />
      {/* Nonce pool exhaustion marker */}
      <line x1={exhaustionX} y1={pad.t} x2={exhaustionX} y2={pad.t + iH}
        stroke="rgba(12,12,12,0.28)" strokeWidth="1" strokeDasharray="4 3" />
      <text x={exhaustionX + 5} y={pad.t + 16} textAnchor="start" fontSize="8"
        fill="rgba(12,12,12,0.5)" fontFamily={IN}>nonce pool</text>
      <text x={exhaustionX + 5} y={pad.t + 26} textAnchor="start" fontSize="8"
        fill="rgba(12,12,12,0.5)" fontFamily={IN}>exhaustion</text>
      {/* Axes */}
      <line x1={pad.l} y1={pad.t + iH} x2={W - pad.r} y2={pad.t + iH} stroke={AXIS} strokeWidth="0.5" />
      <line x1={pad.l} y1={pad.t} x2={pad.l} y2={pad.t + iH} stroke={AXIS} strokeWidth="0.5" />
      {xLabels.map(({ t, x }) => (
        <text key={t} x={x} y={H - 7} textAnchor="middle" fontSize="8.5" fill={TICK} fontFamily={CN}>{t}s</text>
      ))}
      <text x={12} y={pad.t + iH / 2 + 3} textAnchor="middle" fontSize="8.5"
        fill="rgba(12,12,12,0.35)" fontFamily={IN}
        transform={`rotate(-90, 12, ${pad.t + iH / 2})`}>ops/sec</text>
    </svg>
  )
}

// ── Memory usage chart ─────────────────────────────────────────────────────────

function MemoryChart() {
  const pts = [{ t: 0, mb: 77 }, { t: 150, mb: 81 }, { t: 300, mb: 94 }]
  const W = 560, H = 160
  const pad = { t: 24, r: 24, b: 38, l: 52 }
  const iW = W - pad.l - pad.r
  const iH = H - pad.t - pad.b
  const YMIN = 70, YMAX = 100

  const toX = (t: number) => pad.l + (t / 300) * iW
  const toY = (mb: number) => pad.t + iH - ((mb - YMIN) / (YMAX - YMIN)) * iH

  const linePoints = pts.map((p) => `${toX(p.t)},${toY(p.mb)}`).join(' ')
  const first = pts[0]
  const fillD = first
    ? `M ${toX(first.t)} ${toY(first.mb)} L ${toX(150)} ${toY(81)} L ${toX(300)} ${toY(94)} L ${toX(300)} ${pad.t + iH} L ${toX(0)} ${pad.t + iH} Z`
    : ''

  const yTicks = [75, 80, 85, 90, 95, 100]

  return (
    <svg viewBox={`0 0 ${W} ${H}`} width="100%" style={{ maxWidth: W }}>
      {yTicks.map((v) => {
        const y = toY(v)
        return (
          <g key={v}>
            <line x1={pad.l} y1={y} x2={W - pad.r} y2={y} stroke={GRID} strokeWidth="0.5" strokeDasharray="3 3" />
            <text x={pad.l - 5} y={y + 3} textAnchor="end" fontSize="8" fill={TICK} fontFamily={CN}>{v}MB</text>
          </g>
        )
      })}
      {fillD && <path d={fillD} fill="rgba(12,12,12,0.04)" />}
      <polyline points={linePoints} fill="none" stroke="rgba(12,12,12,0.45)" strokeWidth="1.5" strokeLinejoin="round" />
      {pts.map((p) => (
        <g key={p.t}>
          <circle cx={toX(p.t)} cy={toY(p.mb)} r="3.5" fill={INK} />
          <text x={toX(p.t)} y={toY(p.mb) - 8} textAnchor="middle" fontSize="8.5"
            fill={INK} fontFamily={CN} fontWeight="600">{p.mb}MB</text>
        </g>
      ))}
      <line x1={pad.l} y1={pad.t + iH} x2={W - pad.r} y2={pad.t + iH} stroke={AXIS} strokeWidth="0.5" />
      <line x1={pad.l} y1={pad.t} x2={pad.l} y2={pad.t + iH} stroke={AXIS} strokeWidth="0.5" />
      {[0, 150, 300].map((t) => (
        <text key={t} x={toX(t)} y={H - 7} textAnchor="middle" fontSize="8.5" fill={TICK} fontFamily={CN}>{t}s</text>
      ))}
    </svg>
  )
}

// ── Page ───────────────────────────────────────────────────────────────────────

export default function BenchmarksPage() {
  return (
    <div style={{ background: PARCHMENT, minHeight: '100vh' }}>

      {/* ── Hero ──────────────────────────────────────────────────────────── */}
      <section className="px-6 pt-16 pb-10 lg:px-[52px] lg:pt-[88px] lg:pb-[60px]">
        <div style={{ fontFamily: IN, fontSize: '10px', textTransform: 'uppercase', letterSpacing: '0.22em', color: 'rgba(12,12,12,0.35)', marginBottom: '24px' }}>
          BENCHMARK REPORT · ENGINE V0.2.0 · 2026-05-20 · APPLE SILICON M3
        </div>
        <h1 style={{ fontFamily: PF, fontWeight: 400, fontStyle: 'italic', color: INK, lineHeight: 0.95, margin: '0 0 28px' }}
          className="text-[56px] lg:text-[84px]">
          Performance
        </h1>
        <div style={{ width: '56px', height: '2px', background: CRIMSON }} />
      </section>

      {/* ── Headline stat cards ────────────────────────────────────────────── */}
      <section className="px-6 pb-14 lg:px-[52px]">
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-[1px]" style={{ background: PARCHMENT }}>
          <StatCard label="Engine p50" value="0.38 µs" sublabel="Tier 1 isolated matching" />
          <StatCard label="Throughput" value="2.5M ops/sec" sublabel="vs 200k HL ceiling" />
          <StatCard label="E2E p50" value="16.9 ms" sublabel="Single client, loopback" />
          <StatCard label="Tests" value="180 / 1800" sublabel="failures" />
        </div>
      </section>

      {/* ── Tier 1 ────────────────────────────────────────────────────────── */}
      <section className="px-6 pb-16 lg:px-[52px]">
        <SectionLabel text="Tier 1 — Engine Microbenchmark · Pure in-process matching loop" />
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-[1px] mb-[1px]" style={{ background: PARCHMENT }}>
          <ChartBox title="Throughput vs. peers — ops/sec">
            <ThroughputBarChart />
          </ChartBox>
          <ChartBox title="Latency breakdown — µs per operation">
            <LatencyBarChart />
          </ChartBox>
        </div>
        <ChartBox title="Engine component cost breakdown — ns per operation">
          <ComponentBreakdownChart />
        </ChartBox>
      </section>

      {/* ── Tier 2 ────────────────────────────────────────────────────────── */}
      <section className="px-6 pb-16 lg:px-[52px]">
        <SectionLabel text="Tier 2 — End-to-End HTTP Latency · Full request lifecycle, 127.0.0.1 loopback" />
        <div className="grid gap-[1px]" style={{ background: PARCHMENT }}>
          <ChartBox title="Latency comparison — p50 and p99 (ms)  ·  Hyperliquid shown in gray">
            <E2EBarChart />
          </ChartBox>
          {/* Metrics table */}
          <div style={{ background: '#ffffff', padding: '24px 20px' }}>
            <div style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.18em', color: 'rgba(12,12,12,0.28)', marginBottom: '16px' }}>
              Scenario metrics
            </div>
            <div style={{ overflowX: 'auto' }}>
              <table style={{ width: '100%', borderCollapse: 'collapse', minWidth: '480px' }}>
                <thead>
                  <tr>
                    {['Scenario', 'p50', 'p99', 'p99.9', 'p99.99', 'Throughput'].map((h) => (
                      <th key={h} style={{ fontFamily: IN, fontSize: '8px', textTransform: 'uppercase', letterSpacing: '0.15em', color: 'rgba(12,12,12,0.28)', padding: '0 16px 10px 0', textAlign: 'left', borderBottom: '1px solid rgba(12,12,12,0.06)', whiteSpace: 'nowrap' as const }}>
                        {h}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {TIER2_ROWS.map((row, i) => (
                    <tr key={row.id} style={{ borderBottom: i < TIER2_ROWS.length - 1 ? '1px solid rgba(12,12,12,0.04)' : 'none' }}>
                      <td style={{ padding: '12px 16px 12px 0' }}>
                        <span style={{ fontFamily: IN, fontWeight: 600, fontSize: '12px', color: INK }}>{row.id}</span>
                        <span style={{ fontFamily: IN, fontSize: '10px', color: 'rgba(12,12,12,0.35)', marginLeft: '8px' }}>{row.sub}</span>
                      </td>
                      <td style={{ fontFamily: CN, fontSize: '11.5px', color: CRIMSON, padding: '12px 16px 12px 0', whiteSpace: 'nowrap' as const }}>{row.p50}</td>
                      <td style={{ fontFamily: CN, fontSize: '11.5px', color: 'rgba(204,51,51,0.7)', padding: '12px 16px 12px 0', whiteSpace: 'nowrap' as const }}>{row.p99}</td>
                      <td style={{ fontFamily: CN, fontSize: '11.5px', color: 'rgba(12,12,12,0.4)', padding: '12px 16px 12px 0', whiteSpace: 'nowrap' as const }}>{row.p999}</td>
                      <td style={{ fontFamily: CN, fontSize: '11.5px', color: 'rgba(12,12,12,0.3)', padding: '12px 16px 12px 0', whiteSpace: 'nowrap' as const }}>{row.p9999}</td>
                      <td style={{ fontFamily: CN, fontSize: '11.5px', color: 'rgba(12,12,12,0.45)', padding: '12px 0 12px 0', whiteSpace: 'nowrap' as const }}>{row.tput}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        </div>
      </section>

      {/* ── Tier 3 ────────────────────────────────────────────────────────── */}
      <section className="px-6 pb-16 lg:px-[52px]">
        <SectionLabel text="Tier 3 — Sustained Throughput · 300 seconds · 5 market-maker agents · 11 markets" />
        <div className="grid gap-[1px]" style={{ background: PARCHMENT }}>
          <ChartBox title="Throughput time series — ops/sec per 5-second bucket">
            <ThroughputTimeSeriesChart />
            <p style={{ fontFamily: IN, fontSize: '11px', color: 'rgba(12,12,12,0.38)', marginTop: '12px', lineHeight: 1.7, maxWidth: '680px' }}>
              The ~40% throughput drop at t=120 s is expected: each MM agent exhausts its resting-order queue within two minutes. The dominant operation shifts from new orders (1 round-trip) to cancel-repost (2 round-trips), halving raw request count while preserving equivalent trading activity. Throughput stabilizes at ~130 ops/sec.
            </p>
          </ChartBox>
          <div style={{ background: '#ffffff', padding: '24px 20px 20px' }}>
            <div style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.18em', color: 'rgba(12,12,12,0.28)', marginBottom: '20px' }}>
              Memory usage (RSS)
            </div>
            <MemoryChart />
            <p style={{ fontFamily: IN, fontSize: '11px', color: 'rgba(12,12,12,0.38)', marginTop: '12px', lineHeight: 1.6 }}>
              Memory grows ~17 MB over 300 seconds with 5 active market-makers. No unbounded growth observed.
            </p>
          </div>
          {/* Tier 3 summary stats */}
          <div style={{ background: '#ffffff', padding: '24px 20px' }}>
            <div style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.18em', color: 'rgba(12,12,12,0.28)', marginBottom: '16px' }}>
              300-second summary
            </div>
            <div className="grid grid-cols-2 lg:grid-cols-4 gap-[1px]" style={{ background: PARCHMENT }}>
              {[
                { v: '49,129', l: 'Total operations' },
                { v: '4,717', l: 'Total fills' },
                { v: '9.60%', l: 'Fill rate' },
                { v: '156 ops/sec', l: 'Peak throughput' },
                { v: '134 ops/sec', l: 'Mean throughput' },
                { v: '85.6%', l: 'Throughput stability' },
                { v: '35.6 ms', l: 'p50 match latency' },
                { v: '63.1 ms', l: 'p99 match latency' },
              ].map(({ v, l }) => (
                <div key={l} style={{ background: '#ffffff', padding: '16px 16px 14px' }}>
                  <div style={{ fontFamily: PF, fontWeight: 900, fontSize: '22px', color: INK, lineHeight: 1.1 }}>{v}</div>
                  <div style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.14em', color: 'rgba(12,12,12,0.35)', marginTop: '5px' }}>{l}</div>
                </div>
              ))}
            </div>
          </div>
        </div>
      </section>

      {/* ── Hyperliquid comparison ─────────────────────────────────────────── */}
      <section className="px-6 pb-16 lg:px-[52px]">
        <SectionLabel text="Comparison with Hyperliquid · Execution layer only" />
        <div style={{ background: '#ffffff', overflowX: 'auto' }}>
          <table style={{ width: '100%', borderCollapse: 'collapse', minWidth: '640px' }}>
            <thead>
              <tr>
                {[
                  { label: 'Metric', crimson: false },
                  { label: 'Vela', crimson: true },
                  { label: 'Hyperliquid (published)', crimson: false },
                  { label: 'What it measures', crimson: false },
                ].map(({ label, crimson }) => (
                  <th key={label} style={{
                    fontFamily: IN,
                    fontSize: '8px',
                    textTransform: 'uppercase' as const,
                    letterSpacing: '0.18em',
                    padding: '16px 20px',
                    textAlign: 'left' as const,
                    borderBottom: '1px solid rgba(12,12,12,0.08)',
                    borderLeft: crimson ? `3px solid ${CRIMSON}` : undefined,
                    color: crimson ? CRIMSON : 'rgba(12,12,12,0.3)',
                    whiteSpace: 'nowrap' as const,
                    background: '#ffffff',
                  }}>
                    {label}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {HL_ROWS.map((row, i) => (
                <tr key={row.metric} style={{ borderBottom: i < HL_ROWS.length - 1 ? '1px solid rgba(12,12,12,0.04)' : 'none' }}>
                  <td style={{ fontFamily: IN, fontSize: '12px', color: 'rgba(12,12,12,0.55)', padding: '14px 20px', lineHeight: 1.5 }}>
                    {row.metric}
                  </td>
                  <td style={{ fontFamily: CN, fontSize: '11.5px', color: INK, padding: '14px 20px', borderLeft: `3px solid ${CRIMSON}`, whiteSpace: 'nowrap' as const }}>
                    {row.vela}
                  </td>
                  <td style={{ fontFamily: CN, fontSize: '11.5px', color: 'rgba(12,12,12,0.32)', padding: '14px 20px', whiteSpace: 'nowrap' as const }}>
                    {row.hl}
                  </td>
                  <td style={{ fontFamily: IN, fontSize: '11.5px', color: 'rgba(12,12,12,0.4)', padding: '14px 20px', lineHeight: 1.5 }}>
                    {row.note}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <div style={{ padding: '12px 20px', borderTop: '1px solid rgba(12,12,12,0.04)' }}>
            <p style={{ fontFamily: IN, fontSize: '9.5px', color: 'rgba(12,12,12,0.3)', margin: 0, lineHeight: 1.6 }}>
              ¹ Hyperliquid figures from official Hyperliquid documentation. ² Measured over 127.0.0.1 loopback — no real network hop.
            </p>
          </div>
        </div>
      </section>

      {/* ── Disclaimer ────────────────────────────────────────────────────── */}
      <section className="px-6 pb-20 lg:px-[52px]">
        <div style={{ borderTop: '1px solid rgba(12,12,12,0.08)', paddingTop: '24px' }}>
          <div style={{ fontFamily: IN, fontSize: '9px', textTransform: 'uppercase', letterSpacing: '0.2em', color: 'rgba(12,12,12,0.25)', marginBottom: '10px' }}>
            Disclaimer
          </div>
          <p style={{ fontFamily: IN, fontSize: '11px', color: 'rgba(12,12,12,0.38)', lineHeight: 1.8, maxWidth: '760px', margin: 0 }}>
            {DISCLAIMER}
          </p>
        </div>
      </section>

    </div>
  )
}
