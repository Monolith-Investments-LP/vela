use sha3::{Digest, Keccak256};
use std::collections::BTreeMap;

pub type Hash = [u8; 32];

pub fn keccak256(data: &[u8]) -> Hash {
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

pub struct MptStore {
    nodes: BTreeMap<Vec<u8>, Vec<u8>>,
    root: Option<Hash>,
    dirty: bool,
}

impl MptStore {
    pub fn new() -> Self {
        MptStore {
            nodes: BTreeMap::new(),
            root: None,
            dirty: false,
        }
    }

    pub fn root(&self) -> Option<Hash> {
        self.root
    }

    pub fn insert(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.nodes.insert(key, value);
        self.dirty = true;
    }

    pub fn remove(&mut self, key: &[u8]) {
        if self.nodes.remove(key).is_some() {
            self.dirty = true;
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        self.nodes.get(key).map(|v| v.as_slice())
    }

    pub fn compute_root(&mut self) -> Hash {
        if !self.dirty {
            return self.root.unwrap_or([0u8; 32]);
        }
        let mut hasher = Keccak256::new();
        for (k, v) in &self.nodes {
            hasher.update((k.len() as u32).to_be_bytes());
            hasher.update(k);
            hasher.update((v.len() as u32).to_be_bytes());
            hasher.update(v);
        }
        let root: Hash = hasher.finalize().into();
        self.root = Some(root);
        self.dirty = false;
        root
    }

    pub fn snapshot_for_keys(&self, keys: &[Vec<u8>]) -> Vec<(Vec<u8>, Vec<u8>)> {
        keys.iter()
            .filter_map(|k| self.nodes.get(k).map(|v| (k.clone(), v.clone())))
            .collect()
    }

    pub fn snapshot_all(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.nodes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn load_snapshot(&mut self, snapshot: Vec<(Vec<u8>, Vec<u8>)>) {
        for (k, v) in snapshot {
            self.nodes.insert(k, v);
        }
        self.dirty = true;
        self.compute_root();
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl Default for MptStore {
    fn default() -> Self {
        Self::new()
    }
}
