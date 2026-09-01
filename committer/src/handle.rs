use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use types::UserId;
use crate::batch::CommitBatch;
use crate::committer::{CommitResult, Committer, CommitterConfig};
use crate::da::DataAvailabilityClient;
use crate::forced_inclusion::ForcedEntry;

pub struct CommitterHandle {
    tx: mpsc::Sender<CommitBatch>,
    /// Sender side of the forced-inclusion channel.  Dropping this side closes
    /// the channel; the committer will observe the closure gracefully.
    forced_tx: mpsc::Sender<ForcedEntry>,
    pub result_rx: Option<mpsc::Receiver<CommitResult>>,
    join_handle: JoinHandle<()>,
}

impl CommitterHandle {
    /// Spawn a committer without a DA client, using default config.
    pub fn spawn(
        batch_interval: Duration,
        disk_path: Option<PathBuf>,
        channel_capacity: usize,
        with_results: bool,
    ) -> Self {
        let config = CommitterConfig {
            batch_interval,
            disk_path,
            ..Default::default()
        };
        Self::spawn_full(config, channel_capacity, with_results, None)
    }

    /// Spawn a committer with an optional DA client, using default config.
    pub fn spawn_with_da(
        batch_interval: Duration,
        disk_path: Option<PathBuf>,
        channel_capacity: usize,
        with_results: bool,
        da_client: Option<Box<dyn DataAvailabilityClient>>,
    ) -> Self {
        let config = CommitterConfig {
            batch_interval,
            disk_path,
            ..Default::default()
        };
        Self::spawn_full(config, channel_capacity, with_results, da_client)
    }

    /// Spawn a committer with full control over config and DA client.
    ///
    /// Use this in tests when you need to customise
    /// `CommitterConfig::forced_inclusion_timeout`.
    pub fn spawn_full(
        config: CommitterConfig,
        channel_capacity: usize,
        with_results: bool,
        da_client: Option<Box<dyn DataAvailabilityClient>>,
    ) -> Self {
        let (batch_tx, batch_rx) = mpsc::channel(channel_capacity);
        let (forced_tx, forced_rx) = mpsc::channel(channel_capacity);
        let (result_tx, result_rx) = if with_results {
            let (tx, rx) = mpsc::channel(256);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let committer = Committer::new(batch_rx, forced_rx, result_tx, config, da_client);
        let join_handle = tokio::spawn(committer.run());

        CommitterHandle {
            tx: batch_tx,
            forced_tx,
            result_rx,
            join_handle,
        }
    }

    pub async fn send_batch(&self, batch: CommitBatch) -> anyhow::Result<()> {
        self.tx.send(batch).await.map_err(|_| anyhow::anyhow!("committer channel closed"))
    }

    pub fn try_send_batch(&self, batch: CommitBatch) -> anyhow::Result<()> {
        self.tx.try_send(batch).map_err(|e| anyhow::anyhow!("committer send failed: {}", e))
    }

    /// Submit a forced-inclusion request, bypassing the operator.
    ///
    /// The request is queued in the delayed inbox and will be prepended to the
    /// next committed batch once `CommitterConfig::forced_inclusion_timeout` has
    /// elapsed.
    pub async fn force_include(
        &self,
        request: types::Request,
        from: UserId,
    ) -> anyhow::Result<()> {
        let entry = ForcedEntry {
            request,
            from,
            submitted_at: std::time::SystemTime::now(),
        };
        self.forced_tx
            .send(entry)
            .await
            .map_err(|_| anyhow::anyhow!("forced-inclusion channel closed"))
    }

    pub fn is_alive(&self) -> bool {
        !self.join_handle.is_finished()
    }

    pub async fn next_result(&mut self) -> Option<CommitResult> {
        if let Some(rx) = &mut self.result_rx {
            rx.recv().await
        } else {
            None
        }
    }

    pub fn abort(self) {
        self.join_handle.abort();
    }
}

pub fn make_commit_batch(
    engine: &engine::MatchingEngine,
    requests: Vec<types::Request>,
    sequence: u64,
) -> CommitBatch {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;

    CommitBatch::new(
        sequence,
        ts,
        engine.snapshot_balances().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        engine.snapshot_metadata().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        requests,
        engine.markets.keys().cloned().collect(),
    )
}
