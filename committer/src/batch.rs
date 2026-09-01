use std::collections::HashMap;
use types::{
    AssetId, Balance, BatchResult, DecryptionProof, MarketId, Request, Timestamp, UserId,
    UserMetadata,
};

#[derive(Debug, Clone)]
pub struct CommitBatch {
    pub sequence: u64,
    pub timestamp: Timestamp,
    pub balances: HashMap<(UserId, AssetId), Balance>,
    pub metadata: HashMap<UserId, UserMetadata>,
    pub requests: Vec<Request>,
    pub market_ids: Vec<MarketId>,
    pub decryption_proofs: Vec<DecryptionProof>,
    /// Wall-clock time from batch-window open to delta commit, nanoseconds.
    /// Zero when constructed outside a batch dispatch context.
    pub dispatch_latency_ns: u64,
}

impl CommitBatch {
    pub fn new(
        sequence: u64,
        timestamp: Timestamp,
        balances: HashMap<(UserId, AssetId), Balance>,
        metadata: HashMap<UserId, UserMetadata>,
        requests: Vec<Request>,
        market_ids: Vec<MarketId>,
    ) -> Self {
        CommitBatch {
            sequence,
            timestamp,
            balances,
            metadata,
            requests,
            market_ids,
            decryption_proofs: Vec::new(),
            dispatch_latency_ns: 0,
        }
    }

    /// Construct a `CommitBatch` from a [`BatchResult`] produced by the
    /// dispatcher, ensuring that decryption proofs and dispatch latency from
    /// all N orders in the window are included in a single commit entry.
    pub fn from_batch_result(
        sequence: u64,
        timestamp: Timestamp,
        balances: HashMap<(UserId, AssetId), Balance>,
        metadata: HashMap<UserId, UserMetadata>,
        requests: Vec<Request>,
        market_ids: Vec<MarketId>,
        batch_result: &BatchResult,
    ) -> Self {
        CommitBatch {
            sequence,
            timestamp,
            balances,
            metadata,
            requests,
            market_ids,
            decryption_proofs: batch_result.decryption_proofs.clone(),
            dispatch_latency_ns: batch_result.dispatch_latency_ns,
        }
    }

    pub fn request_count(&self) -> usize {
        self.requests.len()
    }

    pub fn balance_count(&self) -> usize {
        self.balances.len()
    }
}
