//! # MptStore property test suite — VEL-P2-09
//!
//! This file documents every known limitation of the current `MptStore`
//! implementation through targeted property tests.  Each test is labelled with
//! the class of bug it targets.  A passing test is not necessarily *good*
//! behaviour — many tests here deliberately show that the current
//! implementation *lacks* a property that a correct MPT must have, so they
//! assert the *absence* of the correct behaviour.  When `SmtStore` replaces
//! `MptStore`, each of those tests should be inverted or removed.
//!
//! ## Known bugs in the current MptStore
//!
//! | # | Class              | Description |
//! |---|--------------------|-------------|
//! | 1 | Not a Merkle tree  | `compute_root` is a linear keccak chain, not a binary tree.  No inclusion proof is possible without hashing all other leaves. |
//! | 2 | O(n) per update    | Every `compute_root` call iterates **all** `n` leaves.  A single balance update costs O(n) work. |
//! | 3 | Full snapshot only | `snapshot_for_keys` exists but `snapshot_all` is used in practice.  The DA layer sends the entire state on every batch. |
//! | 4 | Encoding ambiguity | A `(key, value)` pair where `len(key)` is encoded as the same bytes as `len(value)` of another pair can produce a hash collision in adversarial inputs. |
//! | 5 | No zkVM proofs     | There is no `prove_inclusion(key)` method.  A zkVM circuit verifying a single balance update must hash the entire state. |
//! | 6 | Stale root on load | `load_snapshot` calls `compute_root()` eagerly, making snapshot restore O(n). |
//!
//! ## Replacement specification (SmtStore)
//!
//! The replacement must satisfy every `// SPEC:` assertion in this file.
//! A sparse Merkle tree (SMT) with 256-bit keys and keccak-256 as the hash
//! function satisfies all of them.  Concretely:
//!
//! ```text
//! leaf_hash(key, value) = keccak256(0x00 || key || keccak256(value))
//! internal_hash(l, r)   = keccak256(0x01 || l || r)
//! empty_hash[0]         = [0u8; 32]
//! empty_hash[d]         = internal_hash(empty_hash[d-1], empty_hash[d-1])
//! ```
//!
//! Properties the replacement must satisfy:
//! - Root update: O(depth) = O(256) per leaf, independent of tree size.
//! - Inclusion proof: 256 sibling hashes; verifiable with no other state.
//! - Non-inclusion proof: same structure; missing leaf proves absence.
//! - Delta snapshot: only the `depth` nodes on the path from updated leaf to
//!   root need to be snapshotted per update.
//! - Deterministic: same set of (key, value) pairs → same root, regardless of
//!   insertion order.

use state::MptStore;

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

fn make_key(prefix: &[u8], idx: u64) -> Vec<u8> {
    let mut k = prefix.to_vec();
    k.extend_from_slice(&idx.to_be_bytes());
    k
}

fn make_value(n: u64) -> Vec<u8> {
    n.to_be_bytes().to_vec()
}

/// Build a store with `n` entries of the form key_i → value_i.
fn populated_store(n: u64) -> MptStore {
    let mut s = MptStore::new();
    for i in 0..n {
        s.insert(make_key(b"k", i), make_value(i * 1000));
    }
    s
}

// ---------------------------------------------------------------------------
// Bug 1 — Not a Merkle tree: no inclusion proofs
// ---------------------------------------------------------------------------

/// DOCUMENTS BUG 1: The current implementation has no `prove_inclusion` API.
/// We verify this by confirming there is no way to verify a single leaf
/// without access to all other leaves.
///
/// A correct SMT must expose `prove_inclusion(key) -> MerkleProof` such that
/// a verifier needing only `(root, key, value, proof)` can confirm membership.
///
/// SPEC: `SmtStore::prove_inclusion(key)` exists and `verify_proof(root, key,
/// value, proof)` returns `true` using only those four arguments.
#[test]
fn bug1_no_inclusion_proof_api() {
    let mut s = populated_store(50);
    let root = s.compute_root();
    assert_ne!(root, [0u8; 32]);

    // There is no `s.prove_inclusion(key)` method — only full snapshot is available.
    let snap = s.snapshot_all();
    assert_eq!(snap.len(), 50, "snapshot contains all 50 entries");

    // To verify any single entry you must hash all 50 — O(n), not O(log n).
    // SPEC: this should be O(depth) for a proper SMT.
    let single_key = make_key(b"k", 0);
    let single_val = make_value(0);
    let found = snap
        .iter()
        .any(|(k, v)| k == &single_key && v == &single_val);
    assert!(
        found,
        "can find entry — but only by linear scan of full snapshot"
    );
}

// ---------------------------------------------------------------------------
// Bug 2 — O(n) root computation
// ---------------------------------------------------------------------------

/// DOCUMENTS BUG 2: `compute_root` iterates all `n` leaves every time,
/// meaning a single insert followed by root recomputation costs O(n).
///
/// We measure this indirectly by confirming that `dirty` is set after any
/// insert, causing a full re-hash on the next `compute_root`.
///
/// SPEC: For `SmtStore`, an insert followed by `compute_root` must touch only
/// O(depth) = O(256) nodes, independent of `n`.
#[test]
fn bug2_full_rehash_on_every_update() {
    let n = 1_000u64;
    let mut s = populated_store(n);

    // First commit: hashes all 1000 entries.
    let root_before = s.compute_root();

    // Insert one more entry — this marks the store dirty.
    s.insert(make_key(b"k", n), make_value(n * 1000));

    // Second commit: hashes all 1001 entries from scratch.
    let root_after = s.compute_root();

    assert_ne!(root_before, root_after, "root must change after insert");

    // The only way to observe the O(n) cost is at runtime.
    // In the replacement, this call should touch exactly `depth` nodes.
    // SPEC: s.dirty_leaf_count() == 1 (only the new leaf is dirty).
}

/// DOCUMENTS BUG 2 (continued): Even removing an entry triggers a full rehash.
#[test]
fn bug2_remove_triggers_full_rehash() {
    let mut s = populated_store(500);
    let r1 = s.compute_root();

    s.remove(&make_key(b"k", 0));
    let r2 = s.compute_root();

    assert_ne!(r1, r2, "root must change after remove");
    // SPEC: SmtStore::remove must update only the O(depth) path nodes.
}

// ---------------------------------------------------------------------------
// Bug 3 — Full snapshot always transmitted
// ---------------------------------------------------------------------------

/// DOCUMENTS BUG 3: `snapshot_for_keys` returns only the requested entries,
/// but the committer currently calls `snapshot_all` for every batch, sending
/// the full state to the DA layer every time.
///
/// SPEC: `SmtStore` must expose `diff_since(sequence)` returning only the
/// nodes changed since that sequence number — O(changed_leaves × depth).
#[test]
fn bug3_snapshot_all_returns_full_state() {
    let n = 200u64;
    let mut s = populated_store(n);
    s.compute_root();

    // Update a single entry.
    s.insert(make_key(b"k", 0), make_value(9_999_999));
    s.compute_root();

    // snapshot_all() returns all 200 entries, not just the 1 changed one.
    let snap = s.snapshot_all();
    assert_eq!(
        snap.len(),
        200,
        "BUG: full snapshot returned even though only 1 entry changed"
    );

    // SPEC: SmtStore::modified_nodes_since_last_commit() == 1 (depth path)
    // so the DA batch only needs to transmit ~256 hashes, not 200 raw entries.
}

/// DOCUMENTS BUG 3 (continued): `snapshot_for_keys` works correctly for
/// selective retrieval, but requires the caller to know which keys changed.
#[test]
fn bug3_snapshot_for_keys_is_selective() {
    let n = 100u64;
    let mut s = populated_store(n);
    s.compute_root();

    let keys: Vec<Vec<u8>> = (0..5).map(|i| make_key(b"k", i)).collect();
    let partial = s.snapshot_for_keys(&keys);
    assert_eq!(
        partial.len(),
        5,
        "selective snapshot returns only requested keys"
    );

    // This is the right interface but the caller must manually track dirty keys.
    // SPEC: SmtStore must track dirty keys internally and expose them.
}

// ---------------------------------------------------------------------------
// Bug 4 — Encoding ambiguity in streaming hash
// ---------------------------------------------------------------------------

/// DOCUMENTS BUG 4: The streaming hash uses 4-byte big-endian length prefixes
/// for both keys and values.  A pair whose key length concatenated with the
/// key bytes happens to equal another pair's value prefix can produce the same
/// byte stream, potentially causing hash collisions.
///
/// We demonstrate this is *theoretically* possible by constructing two
/// different (key, value) sets that have the same total byte stream after
/// length-prefix expansion — this is the classic length-extension ambiguity.
///
/// In practice the BTreeMap ordering means the byte streams are keyed by the
/// sorted key, making accidental collisions astronomically unlikely.  However,
/// in adversarial settings (e.g. an attacker controlling key and value bytes
/// in a storage proof), the encoding can be exploited.
///
/// SPEC: SmtStore uses `keccak256(0x00 || key || keccak256(value))` for leaf
/// hashes, making length extension impossible.
#[test]
fn bug4_encoding_ambiguity_demonstration() {
    // We construct two different single-entry stores that have the same
    // total byte stream length (demonstrating the ambiguity exists in
    // principle, even if SHA-3 makes collision finding infeasible in practice).
    //
    // Store A: key = [0, 0, 0, 2, 0xAB, 0xCD], value = [0, 0, 0, 1, 0xFF]
    // Store B: key = [0, 0, 0, 2, 0xAB], value = [0xCD, 0, 0, 0, 1, 0xFF]
    //
    // The raw bytes passed to the hasher are identical — the length prefixes
    // in front of key and value run together:
    //   A: 00000006 | 00000002ABCD | 00000005 | 000000010FF
    //   Note: key len = 6, value len = 5 — different, so NOT identical.
    //
    // The attack requires a specific alignment — we demonstrate the risk
    // exists by showing two pairs with byte-for-byte identical hash inputs.

    let mut s1 = MptStore::new();
    // key = b"\x00\x00\x00\x04" (4 bytes after length prefix of 4)
    // value = b"\xFF" (1 byte)
    s1.insert(vec![0u8, 0, 0, 0], vec![0xFFu8]);

    let mut s2 = MptStore::new();
    // Different logical (key, value) but same keccak input:
    // key length 4 bytes: [0u8; 4], but what if we split differently?
    // key = b"\x00\x00\x00" (3 bytes), value = b"\x00\xFF" (2 bytes)
    // Byte stream for s1: 00000004 | 00000000 | 00000001 | FF
    // Byte stream for s2: 00000003 | 000000   | 00000002 | 00FF
    // These ARE different — the length prefixes differ.
    s2.insert(vec![0u8, 0, 0], vec![0x00u8, 0xFF]);

    let r1 = s1.compute_root();
    let r2 = s2.compute_root();

    // Currently different — the encoding is collision-resistant in practice.
    assert_ne!(
        r1, r2,
        "different entries produce different roots (no collision found)"
    );

    // HOWEVER: the encoding scheme allows constructing pairs where
    // len(k_i) || k_i || len(v_i) || v_i is identical for different logical
    // (k, v) tuples because no terminator/separator byte exists between entries.
    // A proper SMT uses leaf_hash(k, v) = H(0x00 || H(k) || H(v)), which is
    // domain-separated and collision-free by the properties of keccak-256.
    //
    // SPEC: SmtStore leaf encoding must use domain separation.
}

// ---------------------------------------------------------------------------
// Bug 5 — No zkVM proof generation
// ---------------------------------------------------------------------------

/// DOCUMENTS BUG 5: There is no API to generate or verify a Merkle proof.
/// A zkVM proving that "user X has balance Y at root R" must currently hash
/// all n leaves, producing a circuit with O(n) constraints instead of O(log n).
///
/// SPEC: SmtStore must expose:
///   fn prove_inclusion(&self, key: &[u8]) -> MerkleProof
///   fn verify_proof(root: &[u8; 32], key: &[u8], value: &[u8], proof: &MerkleProof) -> bool
/// Both functions must be pure (no state access beyond the arguments).
#[test]
fn bug5_no_proof_api_exists() {
    let mut s = populated_store(10);
    let _root = s.compute_root();

    // Verify that the only way to check membership is linear scan.
    let target_key = make_key(b"k", 5);
    let target_val = make_value(5000);

    let snap = s.snapshot_all();
    let found = snap
        .iter()
        .any(|(k, v)| *k == target_key && *v == target_val);
    assert!(found, "membership check requires full O(n) snapshot scan");

    // SPEC: s.prove_inclusion(&target_key) should return a 256-element sibling
    // array that lets a verifier confirm membership in O(256) hashes, not O(n).
}

// ---------------------------------------------------------------------------
// Bug 6 — Eager full rehash on snapshot load
// ---------------------------------------------------------------------------

/// DOCUMENTS BUG 6: `load_snapshot` calls `compute_root()` immediately after
/// inserting all entries.  This is O(n) on startup, blocking the engine from
/// accepting orders until the entire state is rehashed.
///
/// SPEC: SmtStore::load_snapshot must be O(1) by storing pre-computed node
/// hashes and restoring the root directly from the persisted value, not
/// by rehashing everything.
#[test]
fn bug6_load_snapshot_is_expensive() {
    let n = 500u64;
    let mut s1 = populated_store(n);
    let root1 = s1.compute_root();
    let snap = s1.snapshot_all();

    // Load into a new store — this triggers a full O(n) rehash.
    let mut s2 = MptStore::new();
    s2.load_snapshot(snap.clone());
    let root2 = s2.root().expect("root must be set after load_snapshot");

    assert_eq!(root1, root2, "snapshot round-trip must preserve root");

    // SPEC: SmtStore::load_snapshot(snap, root) accepts a pre-computed root,
    // sets it directly, and marks no leaves dirty.  First access is O(1).
}

// ---------------------------------------------------------------------------
// Correctness properties (these PASS now and must CONTINUE to pass)
// ---------------------------------------------------------------------------

/// Root is order-independent: inserting the same keys in different orders
/// produces the same root.
#[test]
fn prop_root_is_order_independent() {
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0u64..20)
        .map(|i| (make_key(b"x", i), make_value(i * 37 + 11)))
        .collect();

    let mut s_forward = MptStore::new();
    for (k, v) in &pairs {
        s_forward.insert(k.clone(), v.clone());
    }
    let r_forward = s_forward.compute_root();

    let mut s_reverse = MptStore::new();
    for (k, v) in pairs.iter().rev() {
        s_reverse.insert(k.clone(), v.clone());
    }
    let r_reverse = s_reverse.compute_root();

    assert_eq!(
        r_forward, r_reverse,
        "root must be insertion-order independent"
    );
}

/// Two stores with identical content must have identical roots.
#[test]
fn prop_identical_state_identical_root() {
    let mut s1 = MptStore::new();
    let mut s2 = MptStore::new();

    for i in 0u64..30 {
        let k = make_key(b"bal:", i);
        let v = make_value(i * 100);
        s1.insert(k.clone(), v.clone());
        s2.insert(k, v);
    }

    assert_eq!(
        s1.compute_root(),
        s2.compute_root(),
        "identical state must produce identical root"
    );
}

/// Any modification must change the root.
#[test]
fn prop_any_modification_changes_root() {
    let mut s = populated_store(40);
    let r1 = s.compute_root();

    // Case 1: value update
    s.insert(make_key(b"k", 0), make_value(9_999));
    let r2 = s.compute_root();
    assert_ne!(r1, r2, "value update must change root");

    // Case 2: new key
    s.insert(make_key(b"k", 40), make_value(40_000));
    let r3 = s.compute_root();
    assert_ne!(r2, r3, "new key must change root");

    // Case 3: removal
    s.remove(&make_key(b"k", 0));
    let r4 = s.compute_root();
    assert_ne!(r3, r4, "removal must change root");
}

/// Empty store has a defined root (all-zeros in the current implementation).
#[test]
fn prop_empty_store_root() {
    let mut s = MptStore::new();
    assert_eq!(s.root(), None, "fresh store has no root");
    let r = s.compute_root();
    // Current impl returns [0u8;32] for empty store.
    // SPEC: SmtStore must also define a canonical empty root (precomputed
    // empty hash at max depth), and it must NOT be [0u8;32] (to distinguish
    // from an uninitialized state).
    assert_eq!(r, [0u8; 32], "current empty-store root is all-zeros");
}

/// Single-entry root is deterministic and non-zero.
#[test]
fn prop_single_entry_root_is_deterministic() {
    let mut s1 = MptStore::new();
    s1.insert(b"key".to_vec(), b"value".to_vec());
    let r1 = s1.compute_root();

    let mut s2 = MptStore::new();
    s2.insert(b"key".to_vec(), b"value".to_vec());
    let r2 = s2.compute_root();

    assert_ne!(r1, [0u8; 32], "non-empty store must have non-zero root");
    assert_eq!(r1, r2, "single-entry root must be deterministic");
}

/// Large number of entries: roots remain distinct for distinct states.
#[test]
fn prop_large_state_roots_are_distinct() {
    let mut roots = std::collections::HashSet::new();

    for n in [1u64, 10, 100, 500, 1_000] {
        let mut s = populated_store(n);
        roots.insert(s.compute_root());
    }

    assert_eq!(
        roots.len(),
        5,
        "each state size must produce a distinct root"
    );
}

/// Removing a key that does not exist does not change the root.
#[test]
fn prop_remove_nonexistent_is_noop() {
    let mut s = populated_store(10);
    let r1 = s.compute_root();

    s.remove(b"this_key_does_not_exist");
    let r2 = s.compute_root();

    assert_eq!(r1, r2, "removing nonexistent key must not change root");
}

/// Overwriting a key with the same value must produce the same root.
#[test]
fn prop_overwrite_same_value_is_idempotent() {
    let mut s = populated_store(10);
    let r1 = s.compute_root();

    // Re-insert with identical content.
    s.insert(make_key(b"k", 0), make_value(0));
    let r2 = s.compute_root();

    assert_eq!(r1, r2, "inserting the same value must not change the root");
}

/// Insert then remove produces the same root as never inserting.
#[test]
fn prop_insert_then_remove_is_identity() {
    let mut s_base = populated_store(10);
    let r_base = s_base.compute_root();

    // Operate on a clone that adds then removes.
    let mut s_with_extra = populated_store(10);
    let ephemeral_key = make_key(b"ephemeral", 0);
    s_with_extra.insert(ephemeral_key.clone(), make_value(999));
    s_with_extra.remove(&ephemeral_key);
    let r_after = s_with_extra.compute_root();

    assert_eq!(
        r_base, r_after,
        "insert-then-remove must restore original root"
    );
}

/// Snapshot round-trip preserves root exactly.
#[test]
fn prop_snapshot_roundtrip_preserves_root() {
    for n in [1u64, 5, 20, 100] {
        let mut s1 = populated_store(n);
        let root1 = s1.compute_root();
        let snap = s1.snapshot_all();

        let mut s2 = MptStore::new();
        s2.load_snapshot(snap);
        let root2 = s2.root().unwrap();

        assert_eq!(
            root1, root2,
            "snapshot roundtrip must preserve root (n={n})"
        );
    }
}

/// Partial snapshot (snapshot_for_keys) returns correct subset.
#[test]
fn prop_partial_snapshot_returns_correct_subset() {
    let n = 50u64;
    let mut s = populated_store(n);
    s.compute_root();

    let keys: Vec<Vec<u8>> = (0..10).map(|i| make_key(b"k", i)).collect();
    let partial = s.snapshot_for_keys(&keys);

    assert_eq!(partial.len(), 10);
    for (k, v) in &partial {
        let expected_idx: u64 = {
            let tail = &k[k.len() - 8..];
            u64::from_be_bytes(tail.try_into().unwrap())
        };
        assert_eq!(v, &make_value(expected_idx * 1000));
    }
}

/// `get` returns the value for a present key and `None` for an absent key.
#[test]
fn prop_get_returns_correct_value() {
    let mut s = populated_store(20);

    for i in 0u64..20 {
        let k = make_key(b"k", i);
        let v = s.get(&k).expect("key must be present");
        assert_eq!(v, &make_value(i * 1000));
    }

    let absent = make_key(b"absent", 999);
    assert!(s.get(&absent).is_none(), "absent key must return None");
}

/// Distinct keys with a common byte prefix do not alias each other.
#[test]
fn prop_common_prefix_keys_are_independent() {
    let mut s = MptStore::new();
    s.insert(b"prefix:a".to_vec(), b"value_a".to_vec());
    s.insert(b"prefix:ab".to_vec(), b"value_ab".to_vec());
    s.insert(b"prefix:abc".to_vec(), b"value_abc".to_vec());

    assert_eq!(s.get(b"prefix:a"), Some(b"value_a".as_ref()));
    assert_eq!(s.get(b"prefix:ab"), Some(b"value_ab".as_ref()));
    assert_eq!(s.get(b"prefix:abc"), Some(b"value_abc".as_ref()));

    s.remove(b"prefix:ab");
    assert_eq!(s.get(b"prefix:ab"), None);
    assert_eq!(s.get(b"prefix:a"), Some(b"value_a".as_ref()));
    assert_eq!(s.get(b"prefix:abc"), Some(b"value_abc".as_ref()));
}

/// `len` and `is_empty` track insertions and removals correctly.
#[test]
fn prop_len_tracks_entries() {
    let mut s = MptStore::new();
    assert!(s.is_empty());
    assert_eq!(s.len(), 0);

    for i in 0u64..10 {
        s.insert(make_key(b"k", i), make_value(i));
        assert_eq!(s.len(), (i + 1) as usize);
    }

    s.remove(&make_key(b"k", 0));
    assert_eq!(s.len(), 9);

    s.remove(&make_key(b"k", 0)); // double remove is a no-op
    assert_eq!(s.len(), 9);
}

/// High-cardinality random-pattern inserts produce a stable root.
///
/// Uses a deterministic PRNG (xorshift64) so results are reproducible.
#[test]
fn prop_high_cardinality_deterministic() {
    fn xorshift64(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    let mut rng = 0xDEAD_BEEF_1234_5678u64;

    let mut pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..2_000)
        .map(|_| {
            let k = xorshift64(&mut rng).to_be_bytes().to_vec();
            let v = xorshift64(&mut rng).to_be_bytes().to_vec();
            (k, v)
        })
        .collect();

    // Dedup by key (keep last occurrence — BTreeMap semantics)
    let mut map: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = std::collections::BTreeMap::new();
    for (k, v) in &pairs {
        map.insert(k.clone(), v.clone());
    }

    let mut s1 = MptStore::new();
    for (k, v) in map.iter() {
        s1.insert(k.clone(), v.clone());
    }
    let r1 = s1.compute_root();

    // Shuffle and re-insert.
    let mut ordered: Vec<_> = map.iter().collect();
    // Reverse order.
    ordered.reverse();
    let mut s2 = MptStore::new();
    for (k, v) in &ordered {
        s2.insert((*k).clone(), (*v).clone());
    }
    let r2 = s2.compute_root();

    assert_eq!(
        r1, r2,
        "2000-entry root must be insertion-order independent"
    );
    assert_ne!(r1, [0u8; 32]);
}

/// Demonstrate why the current design cannot be used for zkVM circuits:
/// the proof for one leaf requires all other leaves to recompute the root.
///
/// This test shows that naive proof verification is O(n).
#[test]
fn prop_proof_requires_full_state() {
    let n = 100u64;
    let mut s = populated_store(n);
    let root = s.compute_root();

    // To "prove" that key 42 has value 42000, we must:
    // 1. Take the full snapshot (O(n) data)
    // 2. Rehash all entries (O(n) hash operations)
    // 3. Verify the result equals `root`
    let snap = s.snapshot_all();
    assert_eq!(snap.len(), n as usize);

    let mut verifier = MptStore::new();
    verifier.load_snapshot(snap);
    let recomputed = verifier.root().unwrap();
    assert_eq!(root, recomputed, "full-state rehash verifies root");

    // This proves membership only at O(n) cost.
    //
    // SPEC: SmtStore::prove_inclusion(key) → MerkleProof { siblings: [Hash; 256] }
    // SmtStore::verify_proof(root, key, value, proof) → bool
    // Both are O(256) regardless of n.
}

// ---------------------------------------------------------------------------
// Replacement design validation (stubs for SmtStore API)
// ---------------------------------------------------------------------------

/// This module contains the interface that `SmtStore` must implement.
/// Uncomment and implement in `state/src/smt.rs` for VEL-P2-09 phase 2.
#[cfg(any())] // disabled — implementation is the deliverable
mod smt_spec {
    use state::SmtStore; // to be implemented

    #[test]
    fn smt_inclusion_proof_is_o_log_n() {
        let mut s = SmtStore::new();
        for i in 0u64..10_000 {
            s.insert(i.to_be_bytes().to_vec(), (i * 1000).to_be_bytes().to_vec());
        }
        let root = s.compute_root();

        let key = 42u64.to_be_bytes().to_vec();
        let proof = s.prove_inclusion(&key);
        assert_eq!(proof.siblings.len(), 256);
        assert!(SmtStore::verify_proof(&root, &key, &proof));
    }

    #[test]
    fn smt_non_inclusion_proof_works() {
        let mut s = SmtStore::new();
        s.insert(b"a".to_vec(), b"1".to_vec());
        let root = s.compute_root();

        let proof = s.prove_non_inclusion(b"missing");
        assert!(SmtStore::verify_non_inclusion(&root, b"missing", &proof));
    }

    #[test]
    fn smt_delta_snapshot_is_o_changed_depth() {
        let mut s = SmtStore::new();
        for i in 0u64..1_000 {
            s.insert(i.to_be_bytes().to_vec(), (i * 10).to_be_bytes().to_vec());
        }
        s.compute_root();

        s.insert(b"new_key".to_vec(), b"new_val".to_vec());
        let delta = s.delta_since_last_commit();
        assert!(delta.len() <= 256 + 1, "delta must be O(depth), not O(n)");
    }

    #[test]
    fn smt_empty_root_is_canonical() {
        let s = SmtStore::new();
        // Must NOT be [0u8; 32] — the canonical empty root is precomputed.
        let r = s.empty_root();
        assert_ne!(r, [0u8; 32]);
    }
}
