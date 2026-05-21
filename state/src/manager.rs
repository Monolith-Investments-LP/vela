use std::collections::HashMap;
use std::path::PathBuf;
use types::{AssetId, Balance, Request, Response, UserId, UserMetadata};
use crate::cache::StateCache;
use crate::keys::StateKey;
use crate::mpt::{Hash, MptStore};
use crate::smt::SmtStore;

pub struct StateManager {
    pub cache: StateCache,
    pub mpt: MptStore,
    /// Incremental SMT for zkVM-compatible proofs and delta DA snapshots.
    /// Mirrors the same key-value pairs as `mpt` but with O(depth) update cost
    /// and per-key inclusion proof generation.
    pub smt: SmtStore,
    pub batch_sequence: u64,
    disk_path: Option<PathBuf>,
    pending_batch: Vec<(Request, Vec<Response>)>,
}

impl StateManager {
    pub fn new(disk_path: Option<PathBuf>) -> Self {
        StateManager {
            cache: StateCache::new(),
            mpt: MptStore::new(),
            smt: SmtStore::new(),
            batch_sequence: 0,
            disk_path,
            pending_batch: vec![],
        }
    }

    pub fn load_from_disk(&mut self) -> anyhow::Result<()> {
        let path = match &self.disk_path {
            Some(p) => p.join("snapshot.json"),
            None => return Ok(()),
        };
        if !path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(&path)?;
        let snapshot: Vec<(Vec<u8>, Vec<u8>)> = serde_json::from_slice(&bytes)?;
        self.mpt.load_snapshot(snapshot);
        self.cache.load_from_mpt(&self.mpt);
        Ok(())
    }

    pub fn observe_balance_change(&mut self, user: &UserId, asset: &AssetId, balance: &Balance) {
        self.cache.set_balance(balance);
        let key = crate::keys::StateKey::Balance { user: user.clone(), asset: *asset }.encode();
        let val = serde_json::to_vec(balance).unwrap_or_default();
        self.smt.insert(key, val);
    }

    pub fn observe_metadata_change(&mut self, meta: &UserMetadata) {
        self.cache.set_metadata(meta);
        let key = crate::keys::StateKey::Metadata { user: meta.user.clone() }.encode();
        let val = serde_json::to_vec(meta).unwrap_or_default();
        self.smt.insert(key, val);
    }

    pub fn record_request_response(&mut self, request: Request, responses: Vec<Response>) {
        self.pending_batch.push((request, responses));
    }

    pub fn commit_batch(
        &mut self,
        balances: &HashMap<(UserId, AssetId), Balance>,
        metadata: &HashMap<UserId, UserMetadata>,
    ) -> Hash {
        for balance in balances.values() {
            self.cache.set_balance(balance);
        }
        for meta in metadata.values() {
            self.cache.set_metadata(meta);
        }

        self.batch_sequence += 1;
        self.cache.set_sequence(self.batch_sequence);

        let root = self.cache.commit_to_mpt(&mut self.mpt);

        if let Err(e) = self.persist_snapshot() {
            eprintln!("snapshot persist failed: {}", e);
        }

        self.pending_batch.clear();
        self.smt.mark_committed();
        root
    }

    fn persist_snapshot(&self) -> anyhow::Result<()> {
        let path = match &self.disk_path {
            Some(p) => p.join("snapshot.json"),
            None => return Ok(()),
        };
        std::fs::create_dir_all(path.parent().unwrap())?;
        let snapshot = self.mpt.snapshot_all();
        let bytes = serde_json::to_vec(&snapshot)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    pub fn take_snapshot(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.mpt.snapshot_all()
    }

    pub fn commit_full(&mut self, _pre_snapshot: Vec<(Vec<u8>, Vec<u8>)>) -> crate::mpt::Hash {
        self.cache.set_sequence(self.batch_sequence + 1);
        let root = self.cache.commit_to_mpt(&mut self.mpt);
        self.batch_sequence += 1;
        if let Err(e) = self.persist_snapshot() {
            eprintln!("snapshot persist error: {}", e);
        }
        self.pending_batch.clear();
        root
    }

    pub fn current_root(&self) -> Option<Hash> {
        self.mpt.root()
    }

    /// Compute and return the SMT root.  Only processes dirty paths — O(dirty × 32).
    pub fn smt_root(&mut self) -> Hash {
        self.smt.compute_root()
    }

    /// Delta of SMT nodes modified since the last `commit_batch`.
    /// Used by the committer to post a compact DA blob instead of full state.
    pub fn smt_delta(&self) -> Vec<((usize, u32), Hash)> {
        self.smt.delta_since_last_commit()
    }

    /// Generate an inclusion/non-inclusion proof for a raw state key.
    /// Requires `smt_root()` to have been called since the last modification.
    pub fn smt_prove(&self, raw_key: &[u8]) -> crate::smt::SmtProof {
        self.smt.prove(raw_key)
    }

    pub fn dirty_count(&self) -> usize {
        self.cache.dirty_count()
    }

    pub fn snapshot_for_batch(&self, keys: &[StateKey]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let encoded_keys: Vec<Vec<u8>> = keys.iter().map(|k| k.encode()).collect();
        self.mpt.snapshot_for_keys(&encoded_keys)
    }
}
