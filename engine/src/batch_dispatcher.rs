use crate::MatchingEngine;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use tokio::time::Duration;
use types::{BatchResult, DecryptionProof, Request, Response, Timestamp};

// ---------------------------------------------------------------------------
// BatchedRequest
// ---------------------------------------------------------------------------

/// A single request submitted to the [`BatchDispatcher`], paired with its
/// one-shot response channel and an optional TEOB decryption proof.
pub struct BatchedRequest {
    pub request: Request,
    pub ts: Timestamp,
    /// Sender side of the per-order oneshot; the dispatcher sends back the
    /// engine's `Vec<Response>` for this specific request.
    pub responder: oneshot::Sender<Vec<Response>>,
    /// Present when this request originated from a threshold-decrypted order.
    pub decryption_proof: Option<DecryptionProof>,
}

// ---------------------------------------------------------------------------
// BatchMetrics
// ---------------------------------------------------------------------------

/// Thread-safe metrics updated by the dispatcher after every batch.
pub struct BatchMetrics {
    /// Raw batch sizes, one entry per dispatch cycle (unbounded; callers may
    /// snapshot and clear periodically).
    batch_size_histogram: Mutex<Vec<u64>>,
    /// Nanoseconds from window-open to delta-commit for the most recent batch.
    last_dispatch_latency_ns: AtomicU64,
    /// Ring buffer for the rolling 1-second orders-per-second window:
    /// each entry is `(arrival_instant, order_count)`.
    orders_window: Mutex<VecDeque<(Instant, usize)>>,
}

impl BatchMetrics {
    pub fn new() -> Arc<Self> {
        Arc::new(BatchMetrics {
            batch_size_histogram: Mutex::new(Vec::new()),
            last_dispatch_latency_ns: AtomicU64::new(0),
            orders_window: Mutex::new(VecDeque::new()),
        })
    }

    pub fn record_batch(&self, batch_size: usize, dispatch_latency_ns: u64) {
        self.batch_size_histogram
            .lock()
            .unwrap()
            .push(batch_size as u64);

        self.last_dispatch_latency_ns
            .store(dispatch_latency_ns, Ordering::Relaxed);

        let now = Instant::now();
        let mut window = self.orders_window.lock().unwrap();
        window.push_back((now, batch_size));
        // Evict entries older than 1 second.
        let cutoff = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
        while window.front().map(|(t, _)| *t < cutoff).unwrap_or(false) {
            window.pop_front();
        }
    }

    /// Time from window open to delta commit for the most recently completed batch.
    pub fn batch_dispatch_latency_ns(&self) -> u64 {
        self.last_dispatch_latency_ns.load(Ordering::Relaxed)
    }

    /// Approximate orders processed in the last 1-second sliding window.
    pub fn orders_per_second(&self) -> u64 {
        self.orders_window
            .lock()
            .unwrap()
            .iter()
            .map(|(_, n)| *n as u64)
            .sum()
    }

    /// Snapshot of all recorded batch sizes (not sorted).
    /// Does NOT clear the histogram; use `snapshot_and_clear` for that.
    pub fn batch_size_histogram_snapshot(&self) -> Vec<u64> {
        self.batch_size_histogram.lock().unwrap().clone()
    }

    /// Drain the histogram and return all recorded batch sizes.
    /// Callers must invoke this periodically to prevent unbounded growth.
    pub fn snapshot_and_clear(&self) -> Vec<u64> {
        let mut hist = self.batch_size_histogram.lock().unwrap();
        std::mem::take(&mut *hist)
    }
}

impl Default for BatchMetrics {
    fn default() -> Self {
        BatchMetrics {
            batch_size_histogram: Mutex::new(Vec::new()),
            last_dispatch_latency_ns: AtomicU64::new(0),
            orders_window: Mutex::new(VecDeque::new()),
        }
    }
}

// ---------------------------------------------------------------------------
// BatchDispatcher
// ---------------------------------------------------------------------------

/// Coalesces incoming orders into fixed-size time windows and dispatches them
/// as a single engine pass, amortising the CoW-cache clone and mutex
/// acquisition cost across up to `max_batch_size` orders per window.
///
/// # Dispatch window
/// The window opens on the **first** request arrival and closes at the earlier
/// of `window_us` microseconds elapsed or `max_batch_size` requests accumulated.
/// If `max_batch_size` is reached, the batch fires immediately without waiting
/// for the window to expire.  If the channel is empty when the window expires,
/// the dispatcher blocks on the next arrival (no spinning).
///
/// # Configuration
/// Both parameters can be overridden at runtime via environment variables:
/// - `VELA_BATCH_WINDOW_US`   (default 500)
/// - `VELA_BATCH_MAX_SIZE`    (default 256)
pub struct BatchDispatcher {
    pub window_us: u64,
    pub max_batch_size: usize,
}

impl BatchDispatcher {
    pub fn new(window_us: u64, max_batch_size: usize) -> Self {
        BatchDispatcher {
            window_us,
            max_batch_size,
        }
    }

    /// Construct a `BatchDispatcher` from environment variables, falling back
    /// to (500 µs, 256) if the variables are absent or unparseable.
    pub fn from_env() -> Self {
        let window_us = std::env::var("VELA_BATCH_WINDOW_US")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500u64);
        let max_batch_size = std::env::var("VELA_BATCH_MAX_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256usize);
        BatchDispatcher {
            window_us,
            max_batch_size,
        }
    }

    /// Process a collected batch against the engine.
    ///
    /// Acquires the engine lock **once**, runs every request through
    /// [`MatchingEngine::process_batch`], sends each per-order response via
    /// its paired oneshot, and returns the aggregated [`BatchResult`].
    ///
    /// This is the hot path: one mutex acquisition + one commit per window.
    pub async fn dispatch_batch(
        pending: Vec<BatchedRequest>,
        engine: &TokioMutex<MatchingEngine>,
        window_open: Instant,
    ) -> BatchResult {
        let batch_size = pending.len();

        // Collect request + timestamp pairs, preserving submission order.
        let request_ts: Vec<(Request, Timestamp)> =
            pending.iter().map(|r| (r.request.clone(), r.ts)).collect();

        // Acquire engine lock ONCE for the entire batch.
        let all_responses: Vec<Vec<Response>> = {
            let mut eng = engine.lock().await;
            eng.process_batch(request_ts)
        };

        let dispatch_latency_ns = window_open.elapsed().as_nanos() as u64;

        // Distribute per-order responses and flatten for BatchResult.
        let mut flat_responses = Vec::with_capacity(batch_size * 2);
        let mut decryption_proofs = Vec::new();

        for (item, responses) in pending.into_iter().zip(all_responses) {
            flat_responses.extend_from_slice(&responses);
            if let Some(proof) = item.decryption_proof {
                decryption_proofs.push(proof);
            }
            // Fire-and-forget: if the caller dropped the receiver, skip silently.
            let _ = item.responder.send(responses);
        }

        BatchResult {
            responses: flat_responses,
            batch_size,
            dispatch_latency_ns,
            decryption_proofs,
        }
    }

    /// Run the dispatcher as an infinite async task.
    ///
    /// Blocks on `rx.recv()` when idle (no spinning).  On each batch:
    ///   1. Waits for the first request.
    ///   2. Opens a `window_us`-microsecond drain window.
    ///   3. Fires immediately if `max_batch_size` is reached before the window
    ///      closes.
    ///   4. Calls [`Self::dispatch_batch`] to process the batch under a single
    ///      engine lock.
    ///   5. Updates `metrics`.
    ///   6. Sends `BatchResult` to `result_tx` if wired (optional).
    pub async fn run(
        self,
        mut rx: mpsc::Receiver<BatchedRequest>,
        engine: Arc<TokioMutex<MatchingEngine>>,
        metrics: Arc<BatchMetrics>,
        result_tx: Option<mpsc::Sender<BatchResult>>,
    ) {
        loop {
            // Block until at least one request arrives — no spinning.
            let first = match rx.recv().await {
                Some(item) => item,
                None => break, // Channel closed; exit cleanly.
            };

            let window_open = Instant::now();
            let deadline = tokio::time::Instant::now() + Duration::from_micros(self.window_us);

            let mut pending = Vec::with_capacity(self.max_batch_size);
            pending.push(first);

            // Drain the channel until the window closes or max_batch_size hit.
            while pending.len() < self.max_batch_size {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some(item)) => pending.push(item),
                    _ => break, // Timeout or channel closed.
                }
            }

            let batch_result = Self::dispatch_batch(pending, &engine, window_open).await;

            metrics.record_batch(batch_result.batch_size, batch_result.dispatch_latency_ns);

            if let Some(ref tx) = result_tx {
                // Non-blocking: drop if the consumer is behind.
                let _ = tx.try_send(batch_result);
            }
        }
    }
}
