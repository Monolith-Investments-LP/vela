use crate::batch::CommitBatch;
use crate::da::{DaRecord, DataAvailabilityClient};
use crate::forced_inclusion::{DelayedInbox, ForcedEntry};
use state::mpt::Hash;
use state::StateManager;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};
use types::Request;
use zkvm::{BatchProof, ProofRequest, ZkProver};

// ---------------------------------------------------------------------------
// Payload serialized to the DA layer for each committed batch.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct DaBatch<'a> {
    sequence: u64,
    root: Hash,
    snapshot: &'a [(Vec<u8>, Vec<u8>)],
    requests: &'a [Request],
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct CommitResult {
    pub sequence: u64,
    pub root: Hash,
    pub batch_size: usize,
    pub timestamp: u64,
    /// How many requests in this batch were forced via the delayed inbox.
    pub forced_count: usize,
    /// Populated when the committer has a DA client wired in.
    pub da_record: Option<DaRecord>,
}

pub struct CommitterConfig {
    pub batch_interval: Duration,
    pub disk_path: Option<PathBuf>,
    pub max_batch_size: usize,
    /// How long a transaction must sit in the delayed inbox before the
    /// committer is required to include it.  Defaults to 24 hours, matching
    /// the typical Arbitrum delayed-inbox window.  Set to `Duration::ZERO` in
    /// tests to make every forced tx immediately eligible.
    pub forced_inclusion_timeout: Duration,
    pub prover: Option<Arc<dyn ZkProver>>,
    pub proof_store: Option<Arc<Mutex<HashMap<u64, BatchProof>>>>,
}

impl Default for CommitterConfig {
    fn default() -> Self {
        CommitterConfig {
            batch_interval: Duration::from_millis(500),
            disk_path: None,
            max_batch_size: 10_000,
            forced_inclusion_timeout: Duration::from_secs(24 * 60 * 60),
            prover: None,
            proof_store: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Committer
// ---------------------------------------------------------------------------

pub struct Committer {
    rx: mpsc::Receiver<CommitBatch>,
    forced_rx: mpsc::Receiver<ForcedEntry>,
    result_tx: Option<mpsc::Sender<CommitResult>>,
    state_manager: StateManager,
    config: CommitterConfig,
    pending_requests: Vec<Request>,
    total_committed: u64,
    da_client: Option<Box<dyn DataAvailabilityClient>>,
    inbox: DelayedInbox,
    prover: Option<Arc<dyn ZkProver>>,
    proof_store: Option<Arc<Mutex<HashMap<u64, BatchProof>>>>,
    prev_root: Option<Hash>,
}

impl Committer {
    pub fn new(
        rx: mpsc::Receiver<CommitBatch>,
        forced_rx: mpsc::Receiver<ForcedEntry>,
        result_tx: Option<mpsc::Sender<CommitResult>>,
        config: CommitterConfig,
        da_client: Option<Box<dyn DataAvailabilityClient>>,
    ) -> Self {
        let disk_path = config.disk_path.clone();
        let prover = config.prover.clone();
        let proof_store = config.proof_store.clone();
        Committer {
            rx,
            forced_rx,
            result_tx,
            state_manager: StateManager::new(disk_path),
            config,
            pending_requests: vec![],
            total_committed: 0,
            da_client,
            inbox: DelayedInbox::new(),
            prover,
            proof_store,
            prev_root: None,
        }
    }

    pub fn load_from_disk(&mut self) -> anyhow::Result<()> {
        self.state_manager.load_from_disk()
    }

    pub fn current_root(&self) -> Option<Hash> {
        self.state_manager.current_root()
    }

    pub fn total_committed(&self) -> u64 {
        self.total_committed
    }

    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(self.config.batch_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                Some(batch) = self.rx.recv() => {
                    self.accumulate_batch(batch);

                    if self.pending_requests.len() >= self.config.max_batch_size {
                        let forced_count = self.drain_and_prepend_forced();
                        self.commit_pending(forced_count).await;
                    }
                }

                _ = interval.tick() => {
                    let forced_count = self.drain_and_prepend_forced();
                    if forced_count > 0
                        || !self.pending_requests.is_empty()
                        || self.state_manager.dirty_count() > 0
                    {
                        self.commit_pending(forced_count).await;
                    }
                }
            }
        }
    }

    fn accumulate_batch(&mut self, batch: CommitBatch) {
        for request in batch.requests {
            self.pending_requests.push(request);
        }
        for ((user, asset), balance) in &batch.balances {
            self.state_manager
                .observe_balance_change(user, asset, balance);
        }
        for meta in batch.metadata.values() {
            self.state_manager.observe_metadata_change(meta);
        }
    }

    /// Receive all pending entries from the forced-inclusion channel into the
    /// inbox, then drain the inbox for entries that have waited past the
    /// configured timeout.  Eligible transactions are prepended to
    /// `pending_requests` so they execute first in the next batch.
    ///
    /// Returns the number of forced transactions prepended.
    fn drain_and_prepend_forced(&mut self) -> usize {
        // Drain the channel into the inbox (non-blocking).
        while let Ok(entry) = self.forced_rx.try_recv() {
            self.inbox.push(entry);
        }

        let forced = self
            .inbox
            .drain_eligible(self.config.forced_inclusion_timeout);
        let count = forced.len();

        if count > 0 {
            // Prepend forced transactions ahead of normal pending requests.
            let normal = std::mem::take(&mut self.pending_requests);
            self.pending_requests = forced;
            self.pending_requests.extend(normal);
            info!(
                count = count,
                "forced-inclusion transactions prepended to batch"
            );
        }

        count
    }

    async fn commit_pending(&mut self, forced_count: usize) {
        let batch_size = self.pending_requests.len();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        // Take the pre-commit snapshot for DA submission before mutating state.
        let snapshot = self.state_manager.take_snapshot();
        let requests_for_da = self.pending_requests.clone();

        let state_root_before = self
            .prev_root
            .map(|r| format!("0x{}", hex::encode(r)))
            .unwrap_or_else(|| format!("0x{}", "0".repeat(64)));

        let root = self.state_manager.commit_full(snapshot.clone());
        self.total_committed += batch_size as u64;
        let sequence = self.state_manager.batch_sequence;

        info!(
            sequence = sequence,
            batch_size = batch_size,
            forced_count = forced_count,
            root = hex::encode(root),
            "committed batch"
        );

        // Publish the batch (snapshot + requests) to the DA layer so anyone
        // can reconstruct state and generate independent ZK proofs.
        let da_record = self.submit_to_da(sequence, root, &snapshot, &requests_for_da);

        self.pending_requests.clear();

        if let (Some(prover), Some(proof_store)) = (self.prover.clone(), self.proof_store.clone()) {
            let state_root_after = format!("0x{}", hex::encode(root));
            let request = ProofRequest {
                batch_id: sequence,
                state_root_before,
                state_root_after,
                fills: vec![],
                orders_processed: self.total_committed,
                timestamp: ts,
            };
            tokio::spawn(async move {
                let result = prover.prove_batch(request).await;
                proof_store.lock().await.insert(sequence, result.proof);
            });
        }

        self.prev_root = Some(root);

        if let Some(tx) = &self.result_tx {
            let result = CommitResult {
                sequence,
                root,
                batch_size,
                timestamp: ts,
                forced_count,
                da_record,
            };
            if tx.send(result).await.is_err() {
                warn!("commit result receiver dropped");
            }
        }
    }

    /// Serialize and submit the batch to the DA client.  Failures are logged
    /// but never propagate — DA unavailability must not block state commits.
    fn submit_to_da(
        &self,
        sequence: u64,
        root: Hash,
        snapshot: &[(Vec<u8>, Vec<u8>)],
        requests: &[Request],
    ) -> Option<DaRecord> {
        let client = self.da_client.as_ref()?;

        let payload = DaBatch {
            sequence,
            root,
            snapshot,
            requests,
        };
        let data = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                warn!(sequence = sequence, error = %e, "DA serialization failed");
                return None;
            }
        };

        match client.submit(sequence, &data) {
            Ok(receipt) => {
                info!(
                    sequence = sequence,
                    backend = client.name(),
                    hash = hex::encode(receipt.content_hash),
                    "batch posted to DA"
                );
                Some(DaRecord {
                    receipt,
                    backend: client.name().to_string(),
                })
            }
            Err(e) => {
                warn!(sequence = sequence, backend = client.name(), error = %e, "DA submission failed");
                None
            }
        }
    }
}
