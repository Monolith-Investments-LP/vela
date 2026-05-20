use std::collections::HashMap;
use types::{AssetId, Balance, DecryptionProof, MarketId, Request, Timestamp, UserId, UserMetadata};

#[derive(Debug, Clone)]
pub struct CommitBatch {
    pub sequence: u64,
    pub timestamp: Timestamp,
    pub balances: HashMap<(UserId, AssetId), Balance>,
    pub metadata: HashMap<UserId, UserMetadata>,
    pub requests: Vec<Request>,
    pub market_ids: Vec<MarketId>,
    pub decryption_proofs: Vec<DecryptionProof>,
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
        CommitBatch { sequence, timestamp, balances, metadata, requests, market_ids, decryption_proofs: Vec::new() }
    }

    pub fn request_count(&self) -> usize {
        self.requests.len()
    }

    pub fn balance_count(&self) -> usize {
        self.balances.len()
    }
}
