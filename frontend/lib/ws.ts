import { z } from 'zod'

export type WsClientMessage =
  | { type: 'subscribe'; channels: string[] }
  | { type: 'unsubscribe'; channels: string[] }
  | { type: 'request_challenge' }
  | { type: 'auth'; address: string; signature: string; nonce?: string; timestamp?: number }
  | { type: 'ping' }

/// Server → client message envelope. Every message from the engine
/// MUST match one of these variants at the wire boundary; unknown or
/// malformed messages are dropped by `handleRawMessage` with a
/// console.warn so a compromised or version-mismatched server can't
/// smuggle unexpected fields into downstream handlers.
const stringTuple = z.tuple([z.string(), z.string()])

const WsServerMessageSchema = z.discriminatedUnion('type', [
  z.object({ type: z.literal('subscribed'), channels: z.array(z.string()) }),
  z.object({
    type: z.literal('book_snapshot'),
    market: z.string(),
    bids: z.array(stringTuple),
    asks: z.array(stringTuple),
  }),
  z.object({
    type: z.literal('trade'),
    market: z.string(),
    price: z.string(),
    quantity: z.string(),
    side: z.string(),
    timestamp: z.number(),
  }),
  z.object({
    type: z.literal('order_update'),
    order_id: z.number(),
    status: z.string(),
    filled_quantity: z.string(),
  }),
  z.object({
    type: z.literal('fill'),
    maker_order_id: z.number(),
    taker_order_id: z.number(),
    price: z.string(),
    quantity: z.string(),
    side: z.string(),
    maker_fee: z.string(),
    taker_fee: z.string(),
    timestamp: z.number(),
  }),
  z.object({
    type: z.literal('balance_update'),
    asset: z.string(),
    available: z.string(),
    locked: z.string(),
  }),
  z.object({ type: z.literal('challenge'), nonce: z.string() }),
  z.object({ type: z.literal('authenticated'), address: z.string() }),
  z.object({ type: z.literal('error'), code: z.string(), message: z.string() }),
  z.object({ type: z.literal('pong') }),
])

export type WsServerMessage = z.infer<typeof WsServerMessageSchema>

export type MessageHandler = (msg: WsServerMessage) => void
export type StatusHandler = (status: WsStatus) => void
export type WsStatus = 'connecting' | 'connected' | 'reconnecting' | 'disconnected' | 'polling'

const HEARTBEAT_INTERVAL_MS = 30_000
const HEARTBEAT_TIMEOUT_MS = 10_000
const INITIAL_RECONNECT_DELAY_MS = 1_000
const MAX_RECONNECT_DELAY_MS = 30_000
const MAX_WS_ATTEMPTS = 3
const POLL_INTERVAL_MS = 2_000

function buildWsUrl(raw: string): string {
  const u = new URL(raw)
  if (u.protocol === 'http:') u.protocol = 'ws:'
  if (u.protocol === 'https:') u.protocol = 'wss:'
  if (!u.pathname.endsWith('/ws')) {
    u.pathname = u.pathname.replace(/\/*$/, '') + '/ws'
  }
  return u.toString()
}

type BookApiResponse = { bids: [string, string][]; asks: [string, string][] }

export class VelaWsClient {
  private ws: WebSocket | null = null
  private messageHandlers = new Set<MessageHandler>()
  private statusHandlers = new Set<StatusHandler>()
  private subscriptions = new Map<string, Set<(data: unknown) => void>>()
  private seqNums = new Map<string, number>()
  private reconnectDelay = INITIAL_RECONNECT_DELAY_MS
  private heartbeatInterval: ReturnType<typeof setInterval> | null = null
  private heartbeatTimeout: ReturnType<typeof setTimeout> | null = null
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private shouldReconnect = false
  private _status: WsStatus = 'disconnected'
  private connectionAttempts = 0
  private pollingMode = false
  private pollInterval: ReturnType<typeof setInterval> | null = null
  private subscribedChannels = new Set<string>()

  constructor(private readonly url: string) {}

  get status(): WsStatus {
    return this._status
  }

  connect(): void {
    if (typeof window === 'undefined') return
    this.shouldReconnect = true
    this.setStatus('connecting')
    this.openSocket()
  }

  disconnect(): void {
    this.shouldReconnect = false
    this.stopHeartbeat()
    this.stopPolling()
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
    this.ws?.close()
    this.ws = null
    this.setStatus('disconnected')
  }

  subscribe(channel: string, callback: (data: unknown) => void): () => void {
    if (!this.subscriptions.has(channel)) {
      this.subscriptions.set(channel, new Set())
    }
    this.subscriptions.get(channel)!.add(callback)
    this.subscribedChannels.add(channel)

    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'subscribe', channels: [channel] }))
    }

    return () => {
      const callbacks = this.subscriptions.get(channel)
      if (callbacks) {
        callbacks.delete(callback)
        if (callbacks.size === 0) {
          this.subscriptions.delete(channel)
          this.subscribedChannels.delete(channel)
          if (this.ws?.readyState === WebSocket.OPEN) {
            this.ws.send(JSON.stringify({ type: 'unsubscribe', channels: [channel] }))
          }
        }
      }
    }
  }

  auth(address: string, signature: string, timestamp: number): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'auth', address, signature, timestamp }))
    }
  }

  authChallenge(address: string, signature: string, nonce: string): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'auth', address, signature, nonce }))
    }
  }

  requestChallenge(): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'request_challenge' }))
    }
  }

  send(msg: WsClientMessage): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(msg))
    }
  }

  onMessage(handler: MessageHandler): () => void {
    this.messageHandlers.add(handler)
    return () => this.messageHandlers.delete(handler)
  }

  onStatus(handler: StatusHandler): () => void {
    this.statusHandlers.add(handler)
    return () => this.statusHandlers.delete(handler)
  }

  ping(): void {
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'ping' }))
    }
  }

  private openSocket(): void {
    try {
      this.ws = new WebSocket(this.url)
      this.ws.onopen = () => this.handleOpen()
      this.ws.onclose = () => this.handleClose()
      this.ws.onerror = () => this.handleClose()
      this.ws.onmessage = (e) => this.handleRawMessage(e)
    } catch {
      this.handleClose()
    }
  }

  private handleOpen(): void {
    this.connectionAttempts = 0
    this.reconnectDelay = INITIAL_RECONNECT_DELAY_MS
    this.setStatus('connected')
    this.startHeartbeat()

    for (const channel of this.subscribedChannels) {
      this.ws?.send(JSON.stringify({ type: 'subscribe', channels: [channel] }))
    }
  }

  private handleClose(): void {
    this.stopHeartbeat()
    if (!this.shouldReconnect) {
      this.setStatus('disconnected')
      return
    }
    this.connectionAttempts++
    if (this.connectionAttempts >= MAX_WS_ATTEMPTS) {
      this.pollingMode = true
      this.startPolling()
      return
    }
    this.setStatus('reconnecting')
    this.scheduleReconnect()
  }

  subscribeChannels(channels: string[]): void {
    channels.forEach((c) => this.subscribedChannels.add(c))
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'subscribe', channels }))
    }
  }

  unsubscribeChannels(channels: string[]): void {
    channels.forEach((c) => this.subscribedChannels.delete(c))
    if (this.ws?.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: 'unsubscribe', channels }))
    }
  }

  private handleRawMessage(event: MessageEvent): void {
    let raw: Record<string, unknown>
    try {
      raw = JSON.parse(event.data as string) as Record<string, unknown>
    } catch {
      return
    }

    if (raw['type'] === 'pong') {
      this.clearHeartbeatTimeout()
    }

    // Channel-envelope path: engine wraps typed payloads in
    // { type, channel, seq, timestamp, data }.
    if (typeof raw['channel'] === 'string' && typeof raw['seq'] === 'number') {
      const channel = raw['channel'] as string
      const seq = raw['seq'] as number
      const data = raw['data'] as unknown

      const lastSeq = this.seqNums.get(channel)
      if (lastSeq !== undefined && seq !== lastSeq + 1 && seq !== 1) {
        console.warn(`WS seq gap on ${channel}: expected ${lastSeq + 1}, got ${seq}`)
        if (this.ws?.readyState === WebSocket.OPEN) {
          this.ws.send(JSON.stringify({ type: 'subscribe', channels: [channel] }))
        }
      }
      this.seqNums.set(channel, seq)

      const callbacks = this.subscriptions.get(channel)
      if (callbacks) {
        callbacks.forEach((cb) => cb(data))
      }

      const d = (data ?? {}) as Record<string, unknown>
      const flatCandidate = {
        ...d,
        type: raw['type'],
        market: d['market_id'] ?? d['market'],
        timestamp: d['timestamp'] ?? raw['timestamp'],
      }
      const parsed = WsServerMessageSchema.safeParse(flatCandidate)
      if (parsed.success) {
        this.messageHandlers.forEach((h) => h(parsed.data))
      } else {
        console.warn(`WS envelope on ${channel} failed validation`, parsed.error.issues)
      }
      return
    }

    // Bare-message path: server sent a top-level typed message
    // (subscribed/challenge/authenticated/error/pong).
    const parsed = WsServerMessageSchema.safeParse(raw)
    if (parsed.success) {
      this.messageHandlers.forEach((h) => h(parsed.data))
    } else {
      console.warn('WS message failed validation', parsed.error.issues)
    }
  }

  private scheduleReconnect(): void {
    this.reconnectTimer = setTimeout(() => {
      this.reconnectDelay = Math.min(
        this.reconnectDelay * 2,
        MAX_RECONNECT_DELAY_MS,
      )
      this.openSocket()
    }, this.reconnectDelay)
  }

  private startPolling(): void {
    this.setStatus('polling')
    const apiUrl = process.env.NEXT_PUBLIC_API_URL ?? 'http://localhost:3001'
    const bookChannels = [...this.subscribedChannels]
      .filter((c) => c.startsWith('orderbook:') || c.startsWith('book:'))
      .map((c) => c.replace(/^orderbook:/, '').replace(/^book:/, ''))

    this.pollInterval = setInterval(() => {
      for (const market of bookChannels) {
        void fetch(`${apiUrl}/markets/${market}/book`)
          .then((res) => {
            if (!res.ok) return
            return res.json() as Promise<BookApiResponse>
          })
          .then((data) => {
            if (!data) return
            const callbacks = this.subscriptions.get(`orderbook:${market}`)
              ?? this.subscriptions.get(`book:${market}`)
            if (callbacks) {
              callbacks.forEach((cb) => cb({ bids: data.bids, asks: data.asks }))
            }
            const legacyMsg: WsServerMessage = {
              type: 'book_snapshot',
              market,
              bids: data.bids,
              asks: data.asks,
            }
            this.messageHandlers.forEach((h) => h(legacyMsg))
          })
          .catch(() => undefined)
      }
    }, POLL_INTERVAL_MS)
  }

  private stopPolling(): void {
    if (this.pollInterval !== null) {
      clearInterval(this.pollInterval)
      this.pollInterval = null
    }
    this.pollingMode = false
  }

  private startHeartbeat(): void {
    this.heartbeatInterval = setInterval(() => {
      this.ping()
      this.heartbeatTimeout = setTimeout(() => {
        this.ws?.close()
      }, HEARTBEAT_TIMEOUT_MS)
    }, HEARTBEAT_INTERVAL_MS)
  }

  private stopHeartbeat(): void {
    if (this.heartbeatInterval !== null) {
      clearInterval(this.heartbeatInterval)
      this.heartbeatInterval = null
    }
    this.clearHeartbeatTimeout()
  }

  private clearHeartbeatTimeout(): void {
    if (this.heartbeatTimeout !== null) {
      clearTimeout(this.heartbeatTimeout)
      this.heartbeatTimeout = null
    }
  }

  private setStatus(status: WsStatus): void {
    this._status = status
    this.statusHandlers.forEach((h) => h(status))
  }
}

export const velaWs = new VelaWsClient(
  process.env.NEXT_PUBLIC_WS_URL ?? 'wss://vela-engine.fly.dev/ws'
)

const clients = new Map<string, VelaWsClient>()

export function getWsClient(url?: string): VelaWsClient {
  if (!url) return velaWs
  const wsUrl = buildWsUrl(url)
  if (!clients.has(wsUrl)) {
    clients.set(wsUrl, new VelaWsClient(wsUrl))
  }
  return clients.get(wsUrl)!
}
