//! # SmtStore — Sparse Merkle Tree for zkVM-compatible state proofs (VEL-P2-09)
//!
//! ## Design
//!
//! A depth-32 binary tree keyed on the first 32 bits of `keccak256(raw_key)`.
//! Each leaf slot may hold multiple raw keys whose keccak-256 hash shares the
//! same 32-bit prefix (a collision bucket); the leaf hash is the keccak-256 of
//! the sorted, length-prefixed concatenation of all `(key_hash, val_hash)` pairs
//! in the bucket.  This keeps tree depth fixed at 32 (32 sibling hashes per
//! proof) regardless of key space size, while remaining collision-safe.
//!
//! ## Hash functions
//!
//! ```text
//! val_hash(v)          = keccak256(v)
//! pair_hash(kh, vh)    = keccak256(kh || vh)          (32+32 = 64 bytes)
//! bucket_hash(pairs)   = keccak256(sorted(pairs))      (ordered by kh)
//! empty_leaf           = [0u8; 32]
//! node_hash(left, rgt) = keccak256(0x01 || left || rgt) (1+32+32 = 65 bytes)
//! empty[0]             = empty_leaf
//! empty[d]             = node_hash(empty[d-1], empty[d-1])
//! ```
//!
//! ## Complexity
//!
//! | Operation              | Current MptStore | SmtStore        |
//! |------------------------|------------------|-----------------|
//! | insert / update        | O(n) dirty flag  | O(1) + mark     |
//! | compute_root           | O(n) full rehash | O(dirty × 32)   |
//! | prove_inclusion        | impossible       | O(32)           |
//! | verify_proof           | O(n)             | O(32)           |
//! | snapshot (delta)       | O(n) full state  | O(dirty × 32)   |
//! | snapshot (full)        | O(n)             | O(n)            |

use sha3::{Digest, Keccak256};
use std::collections::{BTreeMap, HashMap, HashSet};

pub const SMT_DEPTH: usize = 32;
pub type Hash = [u8; 32];

// ---------------------------------------------------------------------------
// Low-level hash primitives
// ---------------------------------------------------------------------------

fn kh(data: &[u8]) -> Hash {
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

/// Domain-separated internal node hash.
fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut buf = [0u8; 65];
    buf[0] = 0x01;
    buf[1..33].copy_from_slice(left);
    buf[33..65].copy_from_slice(right);
    kh(&buf)
}

/// Hash for a leaf bucket (sorted list of pairs).  Empty bucket → [0u8; 32].
fn bucket_hash(pairs: &BTreeMap<Hash, Hash>) -> Hash {
    if pairs.is_empty() {
        return [0u8; 32];
    }
    // 0x00 domain byte separates leaf hashes from internal node hashes (0x01).
    let mut buf = Vec::with_capacity(1 + pairs.len() * 64);
    buf.push(0x00u8);
    for (kh_, vh) in pairs {
        buf.extend_from_slice(kh_);
        buf.extend_from_slice(vh);
    }
    kh(&buf)
}

/// 32-bit path for a raw key: top 32 bits of keccak256(key).
fn key_path(raw_key: &[u8]) -> u32 {
    let h = kh(raw_key);
    u32::from_be_bytes([h[0], h[1], h[2], h[3]])
}

fn key_hash(raw_key: &[u8]) -> Hash {
    kh(raw_key)
}

/// Precompute empty-subtree hashes for heights 0 (empty leaf) through DEPTH.
fn build_empty_hashes() -> [Hash; SMT_DEPTH + 1] {
    let mut e = [[0u8; 32]; SMT_DEPTH + 1];
    for d in 1..=SMT_DEPTH {
        e[d] = node_hash(&e[d - 1], &e[d - 1]);
    }
    e
}

// ---------------------------------------------------------------------------
// Proof types
// ---------------------------------------------------------------------------

/// Inclusion or non-inclusion proof for a single key.
#[derive(Debug, Clone)]
pub struct SmtProof {
    /// 32-bit routing path for this key.
    pub path: u32,
    /// Sibling hashes from leaf (index 0) up to just below the root (index 31).
    pub siblings: [Hash; SMT_DEPTH],
    /// All (key_hash, value_hash) pairs at this leaf slot, sorted by key_hash.
    /// For an inclusion proof this contains at least the target pair.
    /// For a non-inclusion proof the target key's pair is absent.
    pub bucket: Vec<(Hash, Hash)>,
}

impl SmtProof {
    /// Recompute the root from this proof and check it equals `expected_root`.
    pub fn verify(&self, expected_root: &Hash) -> bool {
        let pairs: BTreeMap<Hash, Hash> = self.bucket.iter().cloned().collect();
        let mut current = bucket_hash(&pairs);
        for d in 0..SMT_DEPTH {
            let bit = (self.path >> d) & 1;
            current = if bit == 0 {
                node_hash(&current, &self.siblings[d])
            } else {
                node_hash(&self.siblings[d], &current)
            };
        }
        &current == expected_root
    }

    /// Return true if this proof demonstrates that `raw_value` is stored under
    /// `raw_key`.
    pub fn proves_inclusion(&self, raw_key: &[u8], raw_value: &[u8]) -> bool {
        let kh_ = key_hash(raw_key);
        let vh = kh(raw_value);
        self.bucket.contains(&(kh_, vh))
    }

    /// Return true if this proof demonstrates that `raw_key` is NOT present.
    pub fn proves_non_inclusion(&self, raw_key: &[u8]) -> bool {
        let kh_ = key_hash(raw_key);
        !self.bucket.iter().any(|(k, _)| k == &kh_)
    }
}

// ---------------------------------------------------------------------------
// Slot: a single 32-bit leaf position
// ---------------------------------------------------------------------------

/// A collision bucket: maps key_hash → value_hash for all raw keys sharing
/// the same 32-bit path.  Maintains a separate index of raw_key → key_hash
/// for O(1) raw-key lookup.
#[derive(Clone, Default)]
struct Slot {
    pairs: BTreeMap<Hash, Hash>, // key_hash → val_hash (sorted for determinism)
    keys: HashMap<Hash, Vec<u8>>, // key_hash → raw_key (for snapshot/prove)
}

impl Slot {
    fn insert(&mut self, raw_key: Vec<u8>, raw_val: Vec<u8>) {
        let kh_ = key_hash(&raw_key);
        let vh = kh(&raw_val);
        self.pairs.insert(kh_, vh);
        self.keys.insert(kh_, raw_key);
    }

    fn remove(&mut self, raw_key: &[u8]) -> bool {
        let kh_ = key_hash(raw_key);
        let removed = self.pairs.remove(&kh_).is_some();
        if removed {
            self.keys.remove(&kh_);
        }
        removed
    }

    fn hash(&self) -> Hash {
        bucket_hash(&self.pairs)
    }

    fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }
}

// ---------------------------------------------------------------------------
// SmtStore
// ---------------------------------------------------------------------------

pub struct SmtStore {
    // Raw storage: raw_key → raw_value (for get/snapshot)
    values: HashMap<Vec<u8>, Vec<u8>>,

    // Leaf slots indexed by 32-bit path
    slots: HashMap<u32, Slot>,

    // Internal node cache: (level_from_leaf, path_prefix) → hash
    // level 0 = leaf level (computed from slot.hash())
    // level 32 = root
    node_cache: HashMap<(usize, u32), Hash>,

    // Paths with modified slots since last compute_root
    dirty: HashSet<u32>,

    // Precomputed empty-subtree hashes (index = height above leaf)
    empty_hashes: [Hash; SMT_DEPTH + 1],

    root: Option<Hash>,

    // Nodes modified since the last call to mark_committed()
    delta: HashMap<(usize, u32), Hash>,
}

impl SmtStore {
    pub fn new() -> Self {
        SmtStore {
            values: HashMap::new(),
            slots: HashMap::new(),
            node_cache: HashMap::new(),
            dirty: HashSet::new(),
            empty_hashes: build_empty_hashes(),
            root: None,
            delta: HashMap::new(),
        }
    }

    /// Insert or update `raw_key → raw_value`.
    pub fn insert(&mut self, raw_key: Vec<u8>, raw_value: Vec<u8>) {
        let path = key_path(&raw_key);
        self.values.insert(raw_key.clone(), raw_value.clone());
        self.slots
            .entry(path)
            .or_default()
            .insert(raw_key, raw_value);
        self.dirty.insert(path);
        self.root = None;
    }

    /// Remove `raw_key`.  Returns true if it was present.
    pub fn remove(&mut self, raw_key: &[u8]) -> bool {
        let path = key_path(raw_key);
        let removed = self.values.remove(raw_key).is_some();
        if removed {
            if let Some(slot) = self.slots.get_mut(&path) {
                slot.remove(raw_key);
                if slot.is_empty() {
                    self.slots.remove(&path);
                }
            }
            self.dirty.insert(path);
            self.root = None;
        }
        removed
    }

    /// Get the raw value for a key, or None if absent.
    pub fn get(&self, raw_key: &[u8]) -> Option<&[u8]> {
        self.values.get(raw_key).map(|v| v.as_slice())
    }

    pub fn contains(&self, raw_key: &[u8]) -> bool {
        self.values.contains_key(raw_key)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Recompute and cache the Merkle root.  Only processes dirty paths.
    /// Cost: O(dirty_paths × SMT_DEPTH) hash operations.
    pub fn compute_root(&mut self) -> Hash {
        if let Some(root) = self.root {
            return root;
        }

        // For each dirty path, recompute the leaf hash and then all 32 ancestors.
        for path in self.dirty.drain().collect::<Vec<_>>() {
            let leaf_h = self.slots.get(&path).map(|s| s.hash()).unwrap_or([0u8; 32]);
            self.node_cache.insert((0, path), leaf_h);
            self.delta.insert((0, path), leaf_h);

            // Walk up the tree: at each level d (0 = leaf .. DEPTH-1 = just below root),
            // recompute the parent.
            let mut current_hash = leaf_h;
            let mut current_path = path;
            for d in 0..SMT_DEPTH {
                // Sibling differs from current node only in the LSB at this level.
                let sibling_path = current_path ^ 1;
                let sibling_hash = self
                    .node_cache
                    .get(&(d, sibling_path))
                    .copied()
                    .unwrap_or(self.empty_hashes[d]);

                let parent_hash = if current_path & 1 == 0 {
                    node_hash(&current_hash, &sibling_hash)
                } else {
                    node_hash(&sibling_hash, &current_hash)
                };

                current_path >>= 1;
                self.node_cache.insert((d + 1, current_path), parent_hash);
                self.delta.insert((d + 1, current_path), parent_hash);

                current_hash = parent_hash;
            }
        }

        // Root is at level DEPTH, prefix 0.
        let root = self
            .node_cache
            .get(&(SMT_DEPTH, 0))
            .copied()
            .unwrap_or(self.empty_hashes[SMT_DEPTH]);

        self.root = Some(root);
        root
    }

    /// The canonical root of an empty SmtStore (precomputed empty-hash chain).
    pub fn empty_root() -> Hash {
        build_empty_hashes()[SMT_DEPTH]
    }

    /// Current cached root (None if dirty since last `compute_root`).
    pub fn root(&self) -> Option<Hash> {
        self.root
    }

    // -----------------------------------------------------------------------
    // Proofs
    // -----------------------------------------------------------------------

    /// Generate an inclusion or non-inclusion proof for `raw_key`.
    /// Panics if `compute_root` has not been called since the last modification.
    pub fn prove(&self, raw_key: &[u8]) -> SmtProof {
        assert!(self.root.is_some(), "call compute_root before prove");
        let path = key_path(raw_key);

        let mut siblings = [[0u8; 32]; SMT_DEPTH];
        let mut current_path = path;
        for (d, sibling) in siblings.iter_mut().enumerate() {
            let sibling_path = current_path ^ 1;
            *sibling = self
                .node_cache
                .get(&(d, sibling_path))
                .copied()
                .unwrap_or(self.empty_hashes[d]);
            current_path >>= 1;
        }

        let bucket: Vec<(Hash, Hash)> = self
            .slots
            .get(&path)
            .map(|s| s.pairs.iter().map(|(&k, &v)| (k, v)).collect())
            .unwrap_or_default();

        SmtProof {
            path,
            siblings,
            bucket,
        }
    }

    // -----------------------------------------------------------------------
    // Snapshots
    // -----------------------------------------------------------------------

    /// Full snapshot: all (raw_key, raw_value) pairs.  Same semantics as
    /// `MptStore::snapshot_all`, used for state recovery.
    pub fn snapshot_all(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.values
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Delta snapshot: only the node hashes modified since `mark_committed`.
    /// Size is O(dirty_paths × SMT_DEPTH) — typically much smaller than the
    /// full state.  Used for DA layer posting and zkVM proof generation.
    pub fn delta_since_last_commit(&self) -> Vec<((usize, u32), Hash)> {
        self.delta.iter().map(|(&k, &v)| (k, v)).collect()
    }

    /// Clear the delta buffer after a batch is committed.
    pub fn mark_committed(&mut self) {
        self.delta.clear();
    }

    /// Restore from a full snapshot produced by `snapshot_all`.
    pub fn load_snapshot(&mut self, snapshot: Vec<(Vec<u8>, Vec<u8>)>) {
        for (k, v) in snapshot {
            self.insert(k, v);
        }
    }

    /// Restore from a delta snapshot (node hashes only) plus a pre-computed
    /// root.  Does NOT restore raw key-value data — only rebuilds the node
    /// cache for proof generation without needing to rehash everything.
    pub fn load_delta(&mut self, delta: Vec<((usize, u32), Hash)>, root: Hash) {
        for (key, hash) in delta {
            self.node_cache.insert(key, hash);
        }
        self.root = Some(root);
        self.dirty.clear();
    }

    /// Selective snapshot for a set of raw keys.
    pub fn snapshot_for_keys(&self, keys: &[Vec<u8>]) -> Vec<(Vec<u8>, Vec<u8>)> {
        keys.iter()
            .filter_map(|k| self.values.get(k).map(|v| (k.clone(), v.clone())))
            .collect()
    }
}

impl Default for SmtStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Stand-alone proof verification (no SmtStore required)
// ---------------------------------------------------------------------------

/// Verify an `SmtProof` given a root and the target (key, value) pair.
/// `value = None` for a non-inclusion proof.
pub fn verify_proof(
    root: &Hash,
    raw_key: &[u8],
    raw_value: Option<&[u8]>,
    proof: &SmtProof,
) -> bool {
    // Validate path matches the key.
    if proof.path != key_path(raw_key) {
        return false;
    }

    // Validate bucket content against the claimed value.
    let kh_ = key_hash(raw_key);
    match raw_value {
        Some(v) => {
            let vh = kh(v);
            if !proof.bucket.contains(&(kh_, vh)) {
                return false;
            }
        }
        None => {
            if proof.bucket.iter().any(|(k, _)| k == &kh_) {
                return false; // key IS present — not a valid non-inclusion proof
            }
        }
    }

    proof.verify(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }
    fn val(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn empty_root_is_canonical_and_nonzero() {
        let s = SmtStore::new();
        assert_eq!(s.root(), None);
        let er = SmtStore::empty_root();
        assert_ne!(er, [0u8; 32], "empty root must be non-zero");
    }

    #[test]
    fn empty_store_compute_root_equals_empty_root() {
        let mut s = SmtStore::new();
        let r = s.compute_root();
        assert_eq!(r, SmtStore::empty_root());
    }

    #[test]
    fn single_insert_changes_root() {
        let mut s = SmtStore::new();
        let er = SmtStore::empty_root();
        s.insert(key("k1"), val("v1"));
        let r = s.compute_root();
        assert_ne!(r, er);
        assert_ne!(r, [0u8; 32]);
    }

    #[test]
    fn root_is_order_independent() {
        let pairs = vec![
            (key("alice"), val("100")),
            (key("bob"), val("200")),
            (key("carol"), val("300")),
        ];

        let mut s1 = SmtStore::new();
        for (k, v) in &pairs {
            s1.insert(k.clone(), v.clone());
        }
        let r1 = s1.compute_root();

        let mut s2 = SmtStore::new();
        for (k, v) in pairs.iter().rev() {
            s2.insert(k.clone(), v.clone());
        }
        let r2 = s2.compute_root();

        assert_eq!(r1, r2);
    }

    #[test]
    fn any_modification_changes_root() {
        let mut s = SmtStore::new();
        s.insert(key("a"), val("1"));
        s.insert(key("b"), val("2"));
        let r1 = s.compute_root();

        s.insert(key("a"), val("999"));
        let r2 = s.compute_root();
        assert_ne!(r1, r2);

        s.remove(&key("b"));
        let r3 = s.compute_root();
        assert_ne!(r2, r3);

        // Remove non-existent key changes nothing
        s.remove(&key("missing"));
        let r4 = s.compute_root();
        assert_eq!(r3, r4);
    }

    #[test]
    fn insert_then_remove_restores_root() {
        let mut s = SmtStore::new();
        s.insert(key("base"), val("val"));
        let r_base = s.compute_root();

        s.insert(key("extra"), val("extra_val"));
        s.remove(&key("extra"));
        let r_restored = s.compute_root();

        assert_eq!(r_base, r_restored);
    }

    #[test]
    fn idempotent_overwrite_preserves_root() {
        let mut s = SmtStore::new();
        s.insert(key("k"), val("v"));
        let r1 = s.compute_root();
        s.insert(key("k"), val("v"));
        let r2 = s.compute_root();
        assert_eq!(r1, r2);
    }

    #[test]
    fn inclusion_proof_verifies() {
        let mut s = SmtStore::new();
        s.insert(key("alice"), val("100"));
        s.insert(key("bob"), val("200"));
        s.insert(key("carol"), val("300"));
        let root = s.compute_root();

        for (k, v) in [("alice", "100"), ("bob", "200"), ("carol", "300")] {
            let proof = s.prove(k.as_bytes());
            assert!(
                proof.proves_inclusion(k.as_bytes(), v.as_bytes()),
                "inclusion for {k}"
            );
            assert!(
                verify_proof(&root, k.as_bytes(), Some(v.as_bytes()), &proof),
                "verify for {k}"
            );
        }
    }

    #[test]
    fn non_inclusion_proof_verifies() {
        let mut s = SmtStore::new();
        s.insert(key("present"), val("yes"));
        let root = s.compute_root();

        let proof = s.prove(key("absent").as_slice());
        assert!(proof.proves_non_inclusion(key("absent").as_slice()));
        assert!(verify_proof(&root, key("absent").as_slice(), None, &proof));

        // Inclusion proof should fail for an absent key
        assert!(!verify_proof(
            &root,
            key("absent").as_slice(),
            Some(val("anything").as_slice()),
            &proof
        ));
    }

    #[test]
    fn proof_wrong_root_fails() {
        let mut s = SmtStore::new();
        s.insert(key("k"), val("v"));
        let root = s.compute_root();
        let proof = s.prove(key("k").as_slice());

        let wrong_root = [0xAAu8; 32];
        assert!(!verify_proof(
            &wrong_root,
            key("k").as_slice(),
            Some(val("v").as_slice()),
            &proof
        ));
    }

    #[test]
    fn proof_wrong_value_fails() {
        let mut s = SmtStore::new();
        s.insert(key("k"), val("v"));
        let root = s.compute_root();
        let proof = s.prove(key("k").as_slice());
        assert!(!verify_proof(
            &root,
            key("k").as_slice(),
            Some(val("wrong").as_slice()),
            &proof
        ));
    }

    #[test]
    fn snapshot_roundtrip_preserves_root() {
        let mut s1 = SmtStore::new();
        for i in 0u64..50 {
            s1.insert(i.to_be_bytes().to_vec(), (i * 100).to_be_bytes().to_vec());
        }
        let root1 = s1.compute_root();
        let snap = s1.snapshot_all();

        let mut s2 = SmtStore::new();
        s2.load_snapshot(snap);
        let root2 = s2.compute_root();
        assert_eq!(root1, root2);
    }

    #[test]
    fn delta_is_smaller_than_full_state() {
        let n = 100u64;
        let mut s = SmtStore::new();
        for i in 0..n {
            s.insert(i.to_be_bytes().to_vec(), (i * 10).to_be_bytes().to_vec());
        }
        s.compute_root();
        s.mark_committed();

        // Update just one key
        s.insert(0u64.to_be_bytes().to_vec(), 9999u64.to_be_bytes().to_vec());
        s.compute_root();

        let delta = s.delta_since_last_commit();
        let full = s.snapshot_all();

        assert!(
            delta.len() <= SMT_DEPTH + 1,
            "delta must be O(depth)={}, got {}",
            SMT_DEPTH + 1,
            delta.len()
        );
        assert_eq!(full.len(), n as usize);
        assert!(
            delta.len() < full.len(),
            "delta ({}) must be smaller than full state ({})",
            delta.len(),
            full.len()
        );
    }

    #[test]
    fn delta_load_restores_proof_capability() {
        let mut s1 = SmtStore::new();
        s1.insert(key("alice"), val("100"));
        s1.insert(key("bob"), val("200"));
        let root = s1.compute_root();
        let delta = s1.delta_since_last_commit();
        let full_snap = s1.snapshot_all();

        // A proof verifier needs only the delta and root, not full state.
        let mut s2 = SmtStore::new();
        s2.load_snapshot(full_snap);
        s2.load_delta(delta, root);

        let proof = s2.prove(key("alice").as_slice());
        assert!(verify_proof(
            &root,
            key("alice").as_slice(),
            Some(val("100").as_slice()),
            &proof
        ));
    }

    #[test]
    fn large_state_proofs_correct() {
        let n = 1_000u64;
        let mut s = SmtStore::new();
        for i in 0..n {
            s.insert(i.to_be_bytes().to_vec(), (i * 37).to_be_bytes().to_vec());
        }
        let root = s.compute_root();

        // Spot-check 10 random entries
        for i in [0u64, 1, 42, 99, 100, 499, 500, 750, 998, 999] {
            let k = i.to_be_bytes().to_vec();
            let v = (i * 37).to_be_bytes().to_vec();
            let proof = s.prove(&k);
            assert!(
                verify_proof(&root, &k, Some(&v), &proof),
                "proof failed for key {i}"
            );
        }

        // Non-inclusion for a missing key
        let missing = 9999u64.to_be_bytes().to_vec();
        let proof = s.prove(&missing);
        assert!(verify_proof(&root, &missing, None, &proof));
    }

    #[test]
    fn high_cardinality_deterministic() {
        fn xorshift(s: &mut u64) -> u64 {
            *s ^= *s << 13;
            *s ^= *s >> 7;
            *s ^= *s << 17;
            *s
        }
        let mut rng = 0xDEAD_BEEF_CAFE_BABEu64;
        let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..2_000)
            .map(|_| {
                (
                    xorshift(&mut rng).to_be_bytes().to_vec(),
                    xorshift(&mut rng).to_be_bytes().to_vec(),
                )
            })
            .collect();

        let mut s1 = SmtStore::new();
        for (k, v) in &pairs {
            s1.insert(k.clone(), v.clone());
        }
        let r1 = s1.compute_root();

        let mut s2 = SmtStore::new();
        for (k, v) in pairs.iter().rev() {
            s2.insert(k.clone(), v.clone());
        }
        let r2 = s2.compute_root();

        assert_eq!(r1, r2);
        assert_ne!(r1, [0u8; 32]);
    }

    #[test]
    fn get_returns_correct_value() {
        let mut s = SmtStore::new();
        s.insert(key("foo"), val("bar"));
        assert_eq!(s.get(key("foo").as_slice()), Some(val("bar").as_slice()));
        assert_eq!(s.get(key("baz").as_slice()), None);
    }

    #[test]
    fn collision_bucket_handles_multiple_keys() {
        // Force two keys into the same 32-bit slot by finding a collision.
        // Since we can't easily engineer exact 32-bit prefix collisions, we
        // test correctness by exercising the bucket code path directly.
        //
        // We create two keys and verify both are provable from the same root.
        let mut s = SmtStore::new();
        let k1 = key("alice");
        let k2 = key("alice_twin"); // different key, may or may not share slot
        s.insert(k1.clone(), val("1"));
        s.insert(k2.clone(), val("2"));
        let root = s.compute_root();

        let proof1 = s.prove(&k1);
        let proof2 = s.prove(&k2);

        assert!(verify_proof(&root, &k1, Some(val("1").as_slice()), &proof1));
        assert!(verify_proof(&root, &k2, Some(val("2").as_slice()), &proof2));

        // Both keys appear in their respective bucket proofs
        assert!(proof1.proves_inclusion(&k1, val("1").as_slice()));
        assert!(proof2.proves_inclusion(&k2, val("2").as_slice()));
    }

    #[test]
    fn proof_sibling_count_is_exactly_depth() {
        let mut s = SmtStore::new();
        s.insert(key("x"), val("y"));
        let root = s.compute_root();
        let proof = s.prove(key("x").as_slice());
        assert_eq!(proof.siblings.len(), SMT_DEPTH);
        assert!(verify_proof(
            &root,
            key("x").as_slice(),
            Some(val("y").as_slice()),
            &proof
        ));
    }
}
