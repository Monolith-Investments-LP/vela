use state::{MptStore, StateCache, StateKey, StateManager};
use types::{AssetId, Balance, MarketId, UserId, UserMetadata};

fn user(i: u8) -> UserId {
    let mut a = [0u8; 20];
    a[19] = i;
    UserId(a)
}
fn usdc() -> AssetId {
    AssetId::from_str("USDC")
}
fn btc() -> AssetId {
    AssetId::from_str("BTC")
}

fn balance(user: UserId, asset: AssetId, available: u64, locked: u64) -> Balance {
    Balance {
        user,
        asset,
        available,
        locked,
    }
}

#[test]
fn test_set_get_balance() {
    let mut cache = StateCache::new();
    let u = user(1);
    let bal = balance(u.clone(), usdc(), 1000, 500);
    cache.set_balance(&bal);
    let retrieved = cache.get_balance(&u, &usdc()).unwrap();
    assert_eq!(retrieved.available, 1000);
    assert_eq!(retrieved.locked, 500);
}

#[test]
fn test_dirty_tracking() {
    let mut cache = StateCache::new();
    assert_eq!(cache.dirty_count(), 0);
    let u = user(1);
    cache.set_balance(&balance(u.clone(), usdc(), 100, 0));
    assert_eq!(cache.dirty_count(), 1);
    cache.set_balance(&balance(u.clone(), btc(), 50, 0));
    assert_eq!(cache.dirty_count(), 2);
}

#[test]
fn test_commit_to_mpt_produces_root() {
    let mut cache = StateCache::new();
    let mut mpt = MptStore::new();

    cache.set_balance(&balance(user(1), usdc(), 1000, 0));
    cache.set_balance(&balance(user(2), usdc(), 500, 250));

    assert_eq!(cache.dirty_count(), 2);
    let root = cache.commit_to_mpt(&mut mpt);

    assert_ne!(root, [0u8; 32]);
    assert_eq!(cache.dirty_count(), 0);
    assert_eq!(mpt.len(), 2);
}

#[test]
fn test_root_changes_on_new_data() {
    let mut cache1 = StateCache::new();
    let mut mpt1 = MptStore::new();
    cache1.set_balance(&balance(user(1), usdc(), 1000, 0));
    let root1 = cache1.commit_to_mpt(&mut mpt1);

    let mut cache2 = StateCache::new();
    let mut mpt2 = MptStore::new();
    cache2.set_balance(&balance(user(1), usdc(), 2000, 0));
    let root2 = cache2.commit_to_mpt(&mut mpt2);

    assert_ne!(root1, root2, "different state must produce different roots");
}

#[test]
fn test_deterministic_root() {
    let u1 = user(1);
    let u2 = user(2);

    let mut cache_a = StateCache::new();
    let mut mpt_a = MptStore::new();
    cache_a.set_balance(&balance(u1.clone(), usdc(), 1000, 0));
    cache_a.set_balance(&balance(u2.clone(), usdc(), 500, 0));
    let root_a = cache_a.commit_to_mpt(&mut mpt_a);

    let mut cache_b = StateCache::new();
    let mut mpt_b = MptStore::new();
    cache_b.set_balance(&balance(u2.clone(), usdc(), 500, 0));
    cache_b.set_balance(&balance(u1.clone(), usdc(), 1000, 0));
    let root_b = cache_b.commit_to_mpt(&mut mpt_b);

    assert_eq!(root_a, root_b, "insertion order must not affect root");
}

#[test]
fn test_load_from_mpt_restores_cache() {
    let mut cache = StateCache::new();
    let mut mpt = MptStore::new();

    let u = user(1);
    cache.set_balance(&balance(u.clone(), usdc(), 9999, 111));
    cache.commit_to_mpt(&mut mpt);

    let mut cache2 = StateCache::new();
    cache2.load_from_mpt(&mpt);

    let retrieved = cache2.get_balance(&u, &usdc()).unwrap();
    assert_eq!(retrieved.available, 9999);
    assert_eq!(retrieved.locked, 111);
    assert_eq!(cache2.dirty_count(), 0);
}

#[test]
fn test_sequence_tracking() {
    let mut cache = StateCache::new();
    assert_eq!(cache.get_sequence(), 0);
    cache.set_sequence(42);
    assert_eq!(cache.get_sequence(), 42);
    cache.set_sequence(43);
    assert_eq!(cache.get_sequence(), 43);
}

#[test]
fn test_state_key_encode_decode_roundtrip() {
    let keys = vec![
        StateKey::Balance {
            user: user(1),
            asset: usdc(),
        },
        StateKey::Metadata { user: user(2) },
        StateKey::MarketConfig {
            market: MarketId::new("BTC", "USDC"),
        },
        StateKey::GlobalSequence,
    ];
    for key in &keys {
        let encoded = key.encode();
        let decoded = StateKey::decode(&encoded);
        assert!(decoded.is_some(), "key {:?} failed to decode", key);
        assert_eq!(&decoded.unwrap(), key);
    }
}

#[test]
fn test_commit_clears_dirty_set() {
    let mut cache = StateCache::new();
    let mut mpt = MptStore::new();
    cache.set_balance(&balance(user(1), usdc(), 100, 0));
    cache.set_balance(&balance(user(2), usdc(), 200, 0));
    assert_eq!(cache.dirty_count(), 2);
    cache.commit_to_mpt(&mut mpt);
    assert_eq!(cache.dirty_count(), 0);
    cache.set_balance(&balance(user(1), usdc(), 150, 0));
    assert_eq!(cache.dirty_count(), 1);
    cache.commit_to_mpt(&mut mpt);
    assert_eq!(cache.dirty_count(), 0);
}

#[test]
fn test_mpt_snapshot_roundtrip() {
    let mut mpt = MptStore::new();
    mpt.insert(b"key1".to_vec(), b"val1".to_vec());
    mpt.insert(b"key2".to_vec(), b"val2".to_vec());
    let root1 = mpt.compute_root();
    let snapshot = mpt.snapshot_all();
    let mut mpt2 = MptStore::new();
    mpt2.load_snapshot(snapshot);
    let root2 = mpt2.compute_root();
    assert_eq!(root1, root2, "snapshot roundtrip must preserve root");
}

#[test]
fn test_state_manager_commit_batch() {
    use engine::MatchingEngine;
    use std::collections::HashMap;
    use types::{
        DepositRequest, FeeConfig, Market, OrderSide, OrderType, PostOrderRequest, Request,
        PRICE_SCALE, QUANTITY_SCALE,
    };

    let mut engine = MatchingEngine::new(FeeConfig::default(), 1.0);
    engine.add_market(Market {
        id: MarketId::new("BTC", "USDC"),
        base: btc(),
        quote: usdc(),
        max_orders: 1000,
        min_order_size: 1,
        price_tick: 1,
        quantity_tick: 1,
        maker_fee_bps: -1,
        taker_fee_bps: 5,
    });

    let u = user(1);
    engine.process(
        Request::Deposit(DepositRequest {
            user: u.clone(),
            asset: usdc(),
            amount: 100_000 * PRICE_SCALE,
            l1_tx_hash: [0u8; 32],
        }),
        1,
    );

    let mut manager = StateManager::new(None);
    let root = manager.commit_batch(engine.snapshot_balances(), engine.snapshot_metadata());

    assert_ne!(root, [0u8; 32]);
    assert_eq!(manager.batch_sequence, 1);
    assert_eq!(manager.dirty_count(), 0);

    let cached_bal = manager.cache.get_balance(&u, &usdc()).unwrap();
    assert_eq!(cached_bal.available, 100_000 * PRICE_SCALE);
}
