use std::collections::{HashMap, HashSet};
use serde_json;
use types::{AssetId, Balance, Market, MarketId, UserId, UserMetadata};
use crate::keys::StateKey;
use crate::mpt::{Hash, MptStore};

pub struct StateCache {
    data: HashMap<Vec<u8>, Vec<u8>>,
    dirty: HashSet<Vec<u8>>,
}

impl StateCache {
    pub fn new() -> Self {
        StateCache {
            data: HashMap::new(),
            dirty: HashSet::new(),
        }
    }

    fn raw_set(&mut self, key: StateKey, value: Vec<u8>) {
        let encoded = key.encode();
        self.data.insert(encoded.clone(), value);
        self.dirty.insert(encoded);
    }

    fn raw_get(&self, key: &StateKey) -> Option<&[u8]> {
        self.data.get(&key.encode()).map(|v| v.as_slice())
    }

    pub fn set_balance(&mut self, balance: &Balance) {
        let key = StateKey::Balance { user: balance.user.clone(), asset: balance.asset };
        let val = serde_json::to_vec(balance)
            .expect("Balance serialization must not fail: all fields are serializable");
        self.raw_set(key, val);
    }

    pub fn get_balance(&self, user: &UserId, asset: &AssetId) -> Option<Balance> {
        let key = StateKey::Balance { user: user.clone(), asset: *asset };
        let raw = self.raw_get(&key)?;
        serde_json::from_slice(raw).ok()
    }

    pub fn set_metadata(&mut self, meta: &UserMetadata) {
        let key = StateKey::Metadata { user: meta.user.clone() };
        let val = serde_json::to_vec(meta)
            .expect("UserMetadata serialization must not fail: all fields are serializable");
        self.raw_set(key, val);
    }

    pub fn get_metadata(&self, user: &UserId) -> Option<UserMetadata> {
        let key = StateKey::Metadata { user: user.clone() };
        let raw = self.raw_get(&key)?;
        serde_json::from_slice(raw).ok()
    }

    pub fn set_market(&mut self, market: &Market) {
        let key = StateKey::MarketConfig { market: market.id.clone() };
        let val = serde_json::to_vec(market)
            .expect("Market serialization must not fail: all fields are serializable");
        self.raw_set(key, val);
    }

    pub fn get_market(&self, market_id: &MarketId) -> Option<Market> {
        let key = StateKey::MarketConfig { market: market_id.clone() };
        let raw = self.raw_get(&key)?;
        serde_json::from_slice(raw).ok()
    }

    pub fn set_sequence(&mut self, seq: u64) {
        self.raw_set(StateKey::GlobalSequence, seq.to_be_bytes().to_vec());
    }

    pub fn get_sequence(&self) -> u64 {
        self.raw_get(&StateKey::GlobalSequence)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_be_bytes)
            .unwrap_or(0)
    }

    pub fn dirty_count(&self) -> usize {
        self.dirty.len()
    }

    pub fn commit_to_mpt(&mut self, mpt: &mut MptStore) -> Hash {
        for key in self.dirty.drain() {
            if let Some(val) = self.data.get(&key) {
                mpt.insert(key, val.clone());
            }
        }
        mpt.compute_root()
    }

    pub fn load_from_mpt(&mut self, mpt: &MptStore) {
        for (k, v) in mpt.snapshot_all() {
            self.data.insert(k, v);
        }
        self.dirty.clear();
    }

    pub fn mark_dirty(&mut self, key: StateKey) {
        self.dirty.insert(key.encode());
    }

    pub fn all_balances(&self) -> Vec<Balance> {
        self.data.iter()
            .filter(|(k, _)| k.starts_with(b"bal:"))
            .filter_map(|(_, v)| serde_json::from_slice(v).ok())
            .collect()
    }

    pub fn all_metadata(&self) -> Vec<UserMetadata> {
        self.data.iter()
            .filter(|(k, _)| k.starts_with(b"meta:"))
            .filter_map(|(_, v)| serde_json::from_slice(v).ok())
            .collect()
    }

    pub fn all_markets(&self) -> Vec<Market> {
        self.data.iter()
            .filter(|(k, _)| k.starts_with(b"mkt:"))
            .filter_map(|(_, v)| serde_json::from_slice(v).ok())
            .collect()
    }
}

impl Default for StateCache {
    fn default() -> Self {
        Self::new()
    }
}
