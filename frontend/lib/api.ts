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
