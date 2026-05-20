pub mod anchor;
pub mod auth;
pub mod da;
pub mod feeds;
pub mod handler;
pub mod mm;
pub mod rate_limit;
pub mod snapshot;
pub mod toxicity_feed;
pub mod types;
pub mod wal;
pub mod ws;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::Mutex;
use engine::MatchingEngine;
use feeds::FeedManager;
use rate_limit::RateLimiter;
use crate::types::{AnchorRecord, Decision, Incident, RegisteredMM, StoredFill, StoredOrder, WsEnvelope};
use zkvm::{BatchProof, ZkProver};
use tee::{AttestationRecord, TeeAttester};

pub struct OrderChannelItem {
    pub req: ::types::PostOrderRequest,
    pub ts: u64,
    pub response_tx: tokio::sync::oneshot::Sender<Vec<::types::Response>>,
}

pub struct AppState {
    pub engine: Arc<Mutex<MatchingEngine>>,
    pub feeds: Arc<Mutex<FeedManager>>,
    pub order_limiter: Arc<RateLimiter>,
    pub deposit_limiter: Arc<RateLimiter>,
    pub general_limiter: Arc<RateLimiter>,
    pub start_time: std::time::Instant,
    pub fills: Arc<Mutex<Vec<StoredFill>>>,
    pub stored_orders: Arc<Mutex<HashMap<u64, StoredOrder>>>,
    pub order_tx: tokio::sync::mpsc::Sender<OrderChannelItem>,
    pub da: Arc<da::DaSubmitter>,
    pub ws_tx: Arc<tokio::sync::broadcast::Sender<WsEnvelope>>,
    pub ws_seqs: Arc<dashmap::DashMap<String, AtomicU64>>,
    pub engine_version: &'static str,
    pub ws_client_count: Arc<AtomicUsize>,
    pub orders_today: Arc<AtomicU64>,
    pub fills_today: Arc<AtomicU64>,
    pub volume_today_usdc: Arc<AtomicU64>,
    pub last_restart_reason: Arc<std::sync::Mutex<Option<String>>>,
    pub last_snapshot_ts: Arc<AtomicU64>,
    pub total_taker_fees_collected: Arc<AtomicU64>,
    pub total_maker_rebates_paid: Arc<AtomicU64>,
    pub fees_collected_today: Arc<AtomicU64>,
    pub anchors: Arc<Mutex<Vec<AnchorRecord>>>,
    pub anchor_count: Arc<AtomicU64>,
    pub last_anchor_tx: Arc<Mutex<Option<String>>>,
    pub last_anchor_time: Arc<AtomicU64>,
    pub incidents: Arc<Mutex<Vec<Incident>>>,
    pub decisions: Arc<Mutex<Vec<Decision>>>,
    pub registered_mms: Arc<Mutex<Vec<RegisteredMM>>>,
    pub proofs: Arc<Mutex<HashMap<u64, BatchProof>>>,
    pub prover: Arc<dyn ZkProver>,
    pub attestations: Arc<Mutex<HashMap<u64, AttestationRecord>>>,
    pub attester: Arc<dyn TeeAttester>,
    pub wal: Arc<wal::Wal>,
}

impl AppState {
    pub fn new(engine: MatchingEngine, wal: Arc<wal::Wal>) -> Arc<Self> {
        let (order_tx, order_rx) = tokio::sync::mpsc::channel::<OrderChannelItem>(1024);
        let engine_arc = Arc::new(Mutex::new(engine));
        let (ws_bcast_tx, _) = tokio::sync::broadcast::channel::<WsEnvelope>(4096);

        let da_dir = std::env::var("DA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/data/da"));

        let prover: Arc<dyn ZkProver> = Arc::new(zkvm::PlaceholderProver);
        let attester: Arc<dyn TeeAttester> = Arc::new(tee::PlaceholderAttester::new());

        let state = Arc::new(AppState {
            engine: Arc::clone(&engine_arc),
            feeds: Arc::new(Mutex::new(FeedManager::new())),
            order_limiter: Arc::new(RateLimiter::new(20, 60)),
            deposit_limiter: Arc::new(RateLimiter::new(5, 60)),
            general_limiter: Arc::new(RateLimiter::new(100, 60)),
            start_time: std::time::Instant::now(),
            fills: Arc::new(Mutex::new(Vec::new())),
            stored_orders: Arc::new(Mutex::new(HashMap::new())),
            order_tx,
            da: Arc::new(da::DaSubmitter::new(Arc::new(da::LocalDaClient::new(da_dir)))),
            ws_tx: Arc::new(ws_bcast_tx),
            ws_seqs: Arc::new(dashmap::DashMap::new()),
            engine_version: "0.2.0",
            ws_client_count: Arc::new(AtomicUsize::new(0)),
            orders_today: Arc::new(AtomicU64::new(0)),
            fills_today: Arc::new(AtomicU64::new(0)),
            volume_today_usdc: Arc::new(AtomicU64::new(0)),
            last_restart_reason: Arc::new(std::sync::Mutex::new(None)),
            last_snapshot_ts: Arc::new(AtomicU64::new(0)),
            total_taker_fees_collected: Arc::new(AtomicU64::new(0)),
            total_maker_rebates_paid: Arc::new(AtomicU64::new(0)),
            fees_collected_today: Arc::new(AtomicU64::new(0)),
            anchors: Arc::new(Mutex::new(Vec::new())),
            anchor_count: Arc::new(AtomicU64::new(0)),
            last_anchor_tx: Arc::new(Mutex::new(None)),
            last_anchor_time: Arc::new(AtomicU64::new(0)),
            incidents: Arc::new(Mutex::new(Vec::new())),
            decisions: Arc::new(Mutex::new(Vec::new())),
            registered_mms: Arc::new(Mutex::new(Vec::new())),
            proofs: Arc::new(Mutex::new(HashMap::new())),
            prover,
            attestations: Arc::new(Mutex::new(HashMap::new())),
            attester,
            wal,
        });

        tokio::spawn(engine_order_task(order_rx, engine_arc));
        tokio::spawn(ws::run_background_task(Arc::clone(&state)));
        tokio::spawn(midnight_reset_task(Arc::clone(&state)));

        state
    }
}

pub async fn engine_order_task(
    mut rx: tokio::sync::mpsc::Receiver<OrderChannelItem>,
    engine: Arc<Mutex<MatchingEngine>>,
) {
    use tokio::time::{timeout_at, Instant};
    use std::time::Duration;
    use ::types::Request;

    loop {
        let first = match rx.recv().await {
            Some(item) => item,
            None => break,
        };

        let mut batch = vec![first];
        let deadline = Instant::now() + Duration::from_millis(1);

        loop {
            if batch.len() >= 50 {
                break;
            }
            match timeout_at(deadline, rx.recv()).await {
                Ok(Some(item)) => batch.push(item),
                _ => break,
            }
        }

        let mut eng = engine.lock().await;
        for item in batch {
            let responses = eng.process(Request::PostOrder(item.req), item.ts);
            let _ = item.response_tx.send(responses);
        }
    }
}

async fn midnight_reset_task(state: Arc<AppState>) {
    loop {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let next_midnight = (now / 86400 + 1) * 86400;
        let sleep_secs = next_midnight.saturating_sub(now);
        tokio::time::sleep(std::time::Duration::from_secs(sleep_secs)).await;
        state.orders_today.store(0, Ordering::Relaxed);
        state.fills_today.store(0, Ordering::Relaxed);
        state.volume_today_usdc.store(0, Ordering::Relaxed);
        state.fees_collected_today.store(0, Ordering::Relaxed);
    }
}

pub use handler::build_router;
