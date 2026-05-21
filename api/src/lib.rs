pub mod anchor;
pub mod auth;
pub mod committee_handler;
pub mod da;
pub mod feeds;
pub mod handler;
pub mod mm;
pub mod pyth;
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
use engine::{BatchDispatcher, BatchMetrics, MatchingEngine};
use engine::batch_dispatcher::BatchedRequest;
use feeds::FeedManager;
use rate_limit::RateLimiter;
use crate::types::{AnchorRecord, Decision, Incident, RegisteredMM, StoredFill, StoredOrder, WsEnvelope};
use zkvm::{BatchProof, ZkProver};
use tee::{AttestationRecord, TeeAttester};
use committee::ThresholdDecryptor;
use crate::committee_handler::PendingEncryptedOrders;

/// Re-export so that handler modules can use a stable local name.
pub use engine::batch_dispatcher::BatchedRequest as OrderChannelItem;

pub struct AppState {
    pub engine: Arc<Mutex<MatchingEngine>>,
    pub feeds: Arc<Mutex<FeedManager>>,
    pub order_limiter: Arc<RateLimiter>,
    pub deposit_limiter: Arc<RateLimiter>,
    pub general_limiter: Arc<RateLimiter>,
    pub start_time: std::time::Instant,
    pub fills: Arc<Mutex<Vec<StoredFill>>>,
    pub stored_orders: Arc<Mutex<HashMap<u64, StoredOrder>>>,
    /// Sender side of the batch-dispatcher ingestion channel.
    pub order_tx: tokio::sync::mpsc::Sender<BatchedRequest>,
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
    // TEOB: threshold encrypted order book
    pub pending_encrypted: PendingEncryptedOrders,
    pub threshold_decryptor: Arc<Mutex<ThresholdDecryptor>>,
    /// Per-node HMAC-SHA256 keys (node_index → 32-byte key).
    pub committee_keys: HashMap<u8, [u8; 32]>,
    pub committee_config: Arc<::types::CommitteeConfig>,
    pub decryption_proofs: Arc<Mutex<Vec<::types::DecryptionProof>>>,
    /// Live batch-dispatcher metrics (batch_size histogram, latency, ops/sec).
    pub batch_metrics: Arc<BatchMetrics>,
}

impl AppState {
    pub fn new(engine: MatchingEngine, wal: Arc<wal::Wal>) -> Arc<Self> {
        let window_us: u64 = std::env::var("VELA_BATCH_WINDOW_US")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(500);
        let max_batch_size: usize = std::env::var("VELA_BATCH_MAX_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256);

        let (order_tx, order_rx) = tokio::sync::mpsc::channel::<BatchedRequest>(1024);
        let engine_arc = Arc::new(Mutex::new(engine));
        let (ws_bcast_tx, _) = tokio::sync::broadcast::channel::<WsEnvelope>(4096);

        let da_dir = std::env::var("DA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/data/da"));

        let prover: Arc<dyn ZkProver> = Arc::new(zkvm::PlaceholderProver);
        let attester: Arc<dyn TeeAttester> = Arc::new(tee::PlaceholderAttester::new());

        let (t, n) = committee::committee_config_from_env();

        let committee_keys: HashMap<u8, [u8; 32]> = (0..n)
            .filter_map(|i| {
                let val = std::env::var(format!("VELA_COMMITTEE_KEY_{i}")).ok()?;
                let bytes = hex::decode(val.strip_prefix("0x").unwrap_or(&val)).ok()?;
                if bytes.len() != 32 {
                    return None;
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Some((i, arr))
            })
            .collect();

        let pub_key_bytes: [u8; 48] = std::env::var("VELA_COMMITTEE_PUBKEY")
            .ok()
            .and_then(|v| {
                let b = hex::decode(v.strip_prefix("0x").unwrap_or(&v)).ok()?;
                if b.len() != 48 { return None; }
                let mut arr = [0u8; 48];
                arr.copy_from_slice(&b);
                Some(arr)
            })
            .unwrap_or([0u8; 48]);

        let committee_config = Arc::new(::types::CommitteeConfig {
            t,
            n,
            pub_key: ::types::G1Affine(pub_key_bytes),
        });

        let pending_encrypted = committee_handler::new_pending_queue();
        let threshold_decryptor = Arc::new(Mutex::new(
            committee::ThresholdDecryptor::new(t, n),
        ));

        let batch_metrics = BatchMetrics::new();

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
            pending_encrypted: Arc::clone(&pending_encrypted),
            threshold_decryptor,
            committee_keys,
            committee_config,
            decryption_proofs: Arc::new(Mutex::new(Vec::new())),
            batch_metrics: Arc::clone(&batch_metrics),
        });

        let dispatcher = BatchDispatcher::new(window_us, max_batch_size);
        tokio::spawn(dispatcher.run(order_rx, Arc::clone(&engine_arc), batch_metrics, None));
        tokio::spawn(ws::run_background_task(Arc::clone(&state)));
        tokio::spawn(midnight_reset_task(Arc::clone(&state)));
        tokio::spawn(committee_handler::eviction_task(pending_encrypted, Arc::clone(&state)));

        state
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
