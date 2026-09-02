// ---------------------------------------------------------------------------
// Vela HTTP API client — typed against the Rust API handler
// ---------------------------------------------------------------------------

const API_URL =
  process.env.NEXT_PUBLIC_API_URL ?? 'https://vela-engine.fly.dev'

// ---------------------------------------------------------------------------
// Response shapes (mirrors api/src/types.rs)
// ---------------------------------------------------------------------------

export interface ApiResponse<T> {
  ok: boolean
  data?: T
  error?: string
}

export interface MarketResponse {
  id: string
  base: string
  quote: string
  best_bid?: string
  best_ask?: string
  spread?: string
}

export interface BookLevel {
  price: string
  quantity: string
}

export interface BookResponse {
  market: string
  bids: BookLevel[]
  asks: BookLevel[]
}

export interface BalanceResponse {
  asset: string
  available: string
  locked: string
  total: string
}

export interface Order {
  id: number
  market: string
  side: 'buy' | 'sell'
  order_type: 'limit' | 'market'
  price: string
  quantity: string
  filled_quantity: string
  status: string
  nonce: number
  client_order_id?: string
  timestamp: number
}

// ---------------------------------------------------------------------------
// Request bodies (mirrors api/src/types.rs)
// ---------------------------------------------------------------------------

export interface PostOrderBody {
  market: string
  side: 'buy' | 'sell'
  order_type: 'limit' | 'market'
  /** Raw integer price (scaled by PRICE_DECIMALS) */
  price: number
  /** Raw integer quantity (scaled by QUANTITY_DECIMALS) */
  quantity: number
  nonce: number
  client_order_id?: string
  address: string
  signature: string
}

export interface CancelOrderBody {
  order_id?: number
  client_order_id?: string
  nonce: number
  address: string
  signature: string
}

export interface WithdrawBody {
  asset: string
  /** Raw integer amount (8 decimals) */
  amount: number
  nonce: number
  address: string
  signature: string
}

// ---------------------------------------------------------------------------
// Fetch helper
// ---------------------------------------------------------------------------

async function apiFetch<T>(
  path: string,
  init?: RequestInit,
): Promise<ApiResponse<T>> {
  const res = await fetch(`${API_URL}${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...init,
  })
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText)
    return { ok: false, error: text }
  }
  return res.json() as Promise<ApiResponse<T>>
}

// ---------------------------------------------------------------------------
// Public endpoints
// ---------------------------------------------------------------------------

/** GET /markets */
export async function listMarkets(): Promise<ApiResponse<MarketResponse[]>> {
  const apiUrl = process.env.NEXT_PUBLIC_API_URL ?? 'https://vela-engine.fly.dev'
  try {
    const res = await fetch(`${apiUrl}/markets`, {
      cache: 'no-store',
      next: { revalidate: 0 },
    })
    if (!res.ok) {
      const text = await res.text().catch(() => res.statusText)
      return { ok: false, error: text }
    }
    return res.json() as Promise<ApiResponse<MarketResponse[]>>
  } catch (err) {
    console.error(err)
    return { ok: true, data: [] }
  }
}

/** GET /markets/:market/book */
export function getBook(market: string): Promise<ApiResponse<BookResponse>> {
  return apiFetch(`/markets/${encodeURIComponent(market)}/book`)
}

// ---------------------------------------------------------------------------
// Authenticated endpoints
// ---------------------------------------------------------------------------

/** GET /account/:address/balances */
export function getBalances(
  address: string,
): Promise<ApiResponse<BalanceResponse[]>> {
  return apiFetch(`/account/${encodeURIComponent(address)}/balances`)
}

/** GET /account/:address/orders */
export function getOrders(address: string): Promise<ApiResponse<Order[]>> {
  return apiFetch(`/account/${encodeURIComponent(address)}/orders`)
}

/** POST /orders */
export function postOrder(
  body: PostOrderBody,
): Promise<ApiResponse<unknown>> {
  return apiFetch('/orders', {
    method: 'POST',
    body: JSON.stringify(body),
  })
}

/** POST /orders/cancel */
export function cancelOrder(
  body: CancelOrderBody,
): Promise<ApiResponse<unknown>> {
  return apiFetch('/orders/cancel', {
    method: 'POST',
    body: JSON.stringify(body),
  })
}

/** POST /withdrawals */
export function initiateWithdrawal(
  body: WithdrawBody,
): Promise<ApiResponse<unknown>> {
  return apiFetch('/withdrawals', {
    method: 'POST',
    body: JSON.stringify(body),
  })
}

/** POST /withdrawals */
export async function withdraw(
  user: string,
  asset: string,
  amount: string,
  signature: string,
  nonce: number,
): Promise<ApiResponse<{ asset: string; amount: string }>> {
  const apiUrl = process.env.NEXT_PUBLIC_API_URL ?? 'https://vela-engine.fly.dev'
  const rawAmount = Math.round(parseFloat(amount) * 1_000_000)
  try {
    const res = await fetch(`${apiUrl}/withdrawals`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ address: user, asset, amount: rawAmount, signature, nonce }),
      cache: 'no-store',
    })
    return res.json()
  } catch {
    return { ok: false, error: 'Network error' }
  }
}

export interface OHLCVCandle {
  time: number
  open: number
  high: number
  low: number
  close: number
  volume: number
}

export async function fetchOHLCV(
  marketId: string,
  timeframe: string = '1H',
  limit: number = 200,
): Promise<{ candles: OHLCVCandle[]; hasRealData: boolean; hasLivePrices: boolean }> {
  const apiUrl = process.env.NEXT_PUBLIC_API_URL ?? 'https://vela-engine.fly.dev'
  try {
    const res = await fetch(
      `${apiUrl}/ohlcv/${marketId}?timeframe=${timeframe}&limit=${limit}`,
      { cache: 'no-store' },
    )
    const data = await res.json()
    if (!data.ok) return { candles: [], hasRealData: false, hasLivePrices: false }
    return {
      candles: data.data.candles,
      hasRealData: data.data.has_real_data,
      hasLivePrices: data.data.has_live_prices ?? false,
    }
  } catch {
    return { candles: [], hasRealData: false, hasLivePrices: false }
  }
}

export interface ReferralData {
  address: string
  referrer: string | null
  referred_count: number
  total_earnings_usdc: string
  referred_users: string[]
}

export interface LeaderboardTrader {
  address: string
  volume_usdc: string
  fill_count: number
  maker_count: number
  taker_count: number
}

export interface LeaderboardReferrer {
  address: string
  referred_count: number
  earnings_usdc: string
}

export interface LeaderboardData {
  top_traders: LeaderboardTrader[]
  top_referrers: LeaderboardReferrer[]
  period: string
}

export function getReferral(address: string): Promise<ApiResponse<ReferralData>> {
  return apiFetch(`/referral/${encodeURIComponent(address)}`)
}

export function registerReferral(body: {
  user: string
  ref: string
  signature: string
  nonce: number
}): Promise<ApiResponse<{ registered: boolean }>> {
  return apiFetch('/referral/register', {
    method: 'POST',
    body: JSON.stringify(body),
  })
}

export function getLeaderboard(): Promise<ApiResponse<LeaderboardData>> {
  return apiFetch('/leaderboard')
}

/** POST /deposit */
export async function deposit(
  user: string,
  asset: string,
  amount: string,
): Promise<ApiResponse<BalanceResponse[]>> {
  const apiUrl = process.env.NEXT_PUBLIC_API_URL ?? 'https://vela-engine.fly.dev'
  try {
    const res = await fetch(`${apiUrl}/deposit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ user, asset, amount }),
      cache: 'no-store',
    })
    return res.json()
  } catch {
    return { ok: false, error: 'Network error' }
  }
}

// ---------------------------------------------------------------------------
// Portfolio / PnL
// ---------------------------------------------------------------------------

export interface PortfolioLot {
  asset: string
  quantity: string
  cost_basis_usdc: string
  acquired_at: number
}

export interface PortfolioResponse {
  address: string
  realized_pnl_usdc: string
  unrealized_pnl_usdc: string
  cost_basis_method: 'FIFO' | 'HIFO'
  tax_lots: PortfolioLot[]
  per_market: { market: string; realized_usdc: string; unrealized_usdc: string }[]
}

/** GET /account/:address/portfolio */
export function getPortfolio(
  address: string,
  method: 'FIFO' | 'HIFO' = 'FIFO',
): Promise<ApiResponse<PortfolioResponse>> {
  return apiFetch(
    `/account/${encodeURIComponent(address)}/portfolio?method=${method}`,
  )
}

/** GET /account/:address/portfolio/csv (returns raw text — not JSON). */
export async function getPortfolioCsv(
  address: string,
  method: 'FIFO' | 'HIFO' = 'FIFO',
): Promise<{ ok: boolean; csv?: string; error?: string }> {
  try {
    const res = await fetch(
      `${API_URL}/account/${encodeURIComponent(address)}/portfolio/csv?method=${method}`,
      { cache: 'no-store' },
    )
    if (!res.ok) return { ok: false, error: res.statusText }
    const csv = await res.text()
    return { ok: true, csv }
  } catch (e) {
    return { ok: false, error: String(e) }
  }
}

// ---------------------------------------------------------------------------
// Analytics / batches / proofs / TEE / decisions
// ---------------------------------------------------------------------------

export interface AnalyticsSummary {
  timeframe: string
  markets: {
    market: string
    volume_micro_usdc: string
    trades: number
    spread_bps: number
    depth_micro_usdc: string
    slippage_bps: number
  }[]
}

export function getAnalytics(
  timeframe: '5m' | '1h' | '24h' = '1h',
): Promise<ApiResponse<AnalyticsSummary>> {
  return apiFetch(`/analytics?timeframe=${timeframe}`)
}

export interface ProofStats {
  total: number
  proven: number
  pending: number
  skipped: number
  failed: number
  provider: string
}

export function getProofStats(): Promise<ApiResponse<ProofStats>> {
  return apiFetch('/proofs/stats')
}

export function getProofs(limit = 50): Promise<ApiResponse<unknown[]>> {
  return apiFetch(`/proofs?limit=${limit}`)
}

export interface TeeStats {
  total_batches: number
  attested: number
  simulated: number
  pending: number
  failed: number
  platform: string
  binary_hash: string
  platform_status: string
}

export function getTeeStats(): Promise<ApiResponse<TeeStats>> {
  return apiFetch('/tee/stats')
}

export function getAttestations(limit = 50): Promise<ApiResponse<unknown[]>> {
  return apiFetch(`/attestations?limit=${limit}`)
}

export interface DecisionSummary {
  id: number
  title: string
  body: string
  status: 'PENDING' | 'ENACTED' | 'REJECTED'
  proposed_at: number
  enacted_at?: number
}

export function getDecisions(): Promise<ApiResponse<DecisionSummary[]>> {
  return apiFetch('/decisions')
}

// ---------------------------------------------------------------------------
// Perps
// ---------------------------------------------------------------------------

export interface PerpMarket {
  market: string
  mark_price_micro_usdc: number
  index_price_micro_usdc: number
  funding_index: number
  funding_rate_bps_per_hour: number
  gross_open_interest: number
  net_open_interest: number
  initial_margin_bps: number
  maintenance_margin_bps: number
  max_leverage: number
}

export interface PerpPosition {
  market: string
  size: string
  entry_price_micro_usdc: number
  realized_pnl_micro_usdc: string
  notional_micro_usdc: string
  unrealized_pnl_micro_usdc: string
  initial_requirement_micro_usdc: string
  maintenance_requirement_micro_usdc: string
  mark_price_micro_usdc: number
}

export function getPerpMarkets(): Promise<ApiResponse<PerpMarket[]>> {
  return apiFetch('/perp/markets')
}

export function getPerpAccount(
  address: string,
): Promise<ApiResponse<{ user: string; positions: PerpPosition[] }>> {
  return apiFetch(`/perp/account/${encodeURIComponent(address)}`)
}

export interface PerpLiquidationCandidate {
  user: string
  market: string
  size: string
  entry_price_micro_usdc: number
  mark_price_micro_usdc: number
  notional_micro_usdc: string
  maintenance_requirement_micro_usdc: string
  equity_micro_usdc: string
}

export function getLiquidatablePositions(): Promise<
  ApiResponse<PerpLiquidationCandidate[]>
> {
  return apiFetch('/perp/liquidatable')
}

// ---------------------------------------------------------------------------
// Algos (TWAP) / RFQ / sub-accounts / borrow-lend / vaults
// ---------------------------------------------------------------------------

export interface AlgoStatus {
  parent_id: string
  market: string
  side: 'buy' | 'sell'
  total_quantity: number
  filled_quantity: number
  status: 'active' | 'complete' | 'canceled'
  slices: { at: number; quantity: number; status: string }[]
}

export function getAlgoStatus(parentId: string): Promise<ApiResponse<AlgoStatus>> {
  return apiFetch(`/orders/algo/${encodeURIComponent(parentId)}`)
}

export interface RfqQuote {
  id: string
  market: string
  side: 'buy' | 'sell'
  size_micro: number
  price_micro_usdc: number
  maker: string
  expires_at: number
}

export function getRfqQuotes(): Promise<ApiResponse<RfqQuote[]>> {
  return apiFetch('/rfq/quotes')
}

export interface BorrowLendMarket {
  asset: string
  total_supply: string
  total_borrows: string
  utilization_bps: number
  borrow_rate_apr_bps: number
  supply_rate_apr_bps: number
  collateral_factor_bps: number
  liquidation_bonus_bps: number
  price_micro_usdc: number
}

export function getBorrowLendMarkets(): Promise<ApiResponse<BorrowLendMarket[]>> {
  return apiFetch('/borrow-lend/markets')
}

export interface BorrowLendAccount {
  user: string
  positions: {
    asset: string
    supply_native: string
    borrow_native: string
    supply_value_micro_usdc: string
    borrow_value_micro_usdc: string
  }[]
  borrowing_power_micro_usdc: string
  total_borrow_value_micro_usdc: string
  health_factor_bps: string
}

export function getBorrowLendAccount(
  address: string,
): Promise<ApiResponse<BorrowLendAccount>> {
  return apiFetch(`/borrow-lend/account/${encodeURIComponent(address)}`)
}

export interface VaultSummary {
  vault_id: string
  operator: string
  total_shares: string
  nav_micro_usdc: string
  drawdown_bps: number
}

export function listVaults(): Promise<ApiResponse<VaultSummary[]>> {
  return apiFetch('/vaults')
}

// ---------------------------------------------------------------------------
// Agent-tier badge
// ---------------------------------------------------------------------------

export interface AgentTier {
  address: string
  tier: 'green' | 'amber' | 'red'
  score: number
  cleared_until_ms?: number
}

export function getAgentTier(address: string): Promise<ApiResponse<AgentTier>> {
  return apiFetch(`/agents/tier/${encodeURIComponent(address)}`)
}

export interface ReputationScore {
  address: string
  fill_quality: number
  toxicity_avg: number
  uptime_bps: number
  score: number
}

export function getReputation(
  address: string,
): Promise<ApiResponse<ReputationScore>> {
  return apiFetch(`/reputation/${encodeURIComponent(address)}`)
}
