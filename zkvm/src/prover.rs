use serde::{Deserialize, Serialize};
use state::mpt::Hash;
use state::StateKey;
use types::{Balance, Request, Response};

#[derive(Debug, Serialize, Deserialize)]
pub struct ZkvmInput {
    pub snapshot: Vec<(Vec<u8>, Vec<u8>)>,
    pub requests: Vec<Request>,
    pub pre_root: Option<Hash>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ZkvmOutput {
    pub post_root: Hash,
    pub responses: Vec<Vec<Response>>,
    /// Final balances of all accounts touched during execution.
    pub balance_deltas: Vec<Balance>,
}

/// Re-executes the STF deterministically from a snapshot, seeding the matching
/// engine with pre-batch state (balances, metadata, markets) so that stateful
/// operations produce the same result as the original operator run.
///
/// This is the function a challenger calls to generate a fraud proof: if the
/// returned `post_root` differs from the operator's claimed root, the batch is
/// fraudulent.
pub fn verify_execution(input: ZkvmInput) -> anyhow::Result<ZkvmOutput> {
    let mut mpt = state::MptStore::new();
    mpt.load_snapshot(input.snapshot.clone());

    let fee_config = types::FeeConfig::default();
    let mut engine = engine::MatchingEngine::new(fee_config, 1.0);

    // Seed engine with pre-batch state decoded from the snapshot.
    for (key_bytes, val_bytes) in &input.snapshot {
        match StateKey::decode(key_bytes) {
            Some(StateKey::Balance { user, asset }) => {
                if let Ok(balance) = serde_json::from_slice::<types::Balance>(val_bytes) {
                    engine.balances.insert((user, asset), balance);
                }
            }
            Some(StateKey::Metadata { user }) => {
                if let Ok(meta) = serde_json::from_slice::<types::UserMetadata>(val_bytes) {
                    engine.metadata.insert(user, meta);
                }
            }
            Some(StateKey::MarketConfig { .. }) => {
                if let Ok(market) = serde_json::from_slice::<types::Market>(val_bytes) {
                    engine.add_market(market);
                }
            }
            _ => {}
        }
    }

    let mut all_responses = Vec::new();
    for (i, request) in input.requests.into_iter().enumerate() {
        let responses = engine.process(request, i as u64);
        all_responses.push(responses);
    }

    // Write the engine's final state into the MPT and compute the post-root.
    let mut cache = state::StateCache::new();
    for bal in engine.snapshot_balances().values() {
        cache.set_balance(bal);
    }
    for meta in engine.snapshot_metadata().values() {
        cache.set_metadata(meta);
    }
    let post_root = cache.commit_to_mpt(&mut mpt);

    let balance_deltas: Vec<Balance> = engine.snapshot_balances().values().cloned().collect();

    Ok(ZkvmOutput {
        post_root,
        responses: all_responses,
        balance_deltas,
    })
}

/// Executes the STF from scratch (no pre-seeded state).  Suitable for batches
/// that are self-contained (e.g. pure deposit batches) or for benchmarking.
pub fn execute_stf(input: ZkvmInput) -> anyhow::Result<ZkvmOutput> {
    let mut mpt = state::MptStore::new();
    mpt.load_snapshot(input.snapshot);

    let fee_config = types::FeeConfig::default();
    let mut engine = engine::MatchingEngine::new(fee_config, 1.0);

    let mut all_responses = Vec::new();
    for (i, request) in input.requests.into_iter().enumerate() {
        let responses = engine.process(request, i as u64);
        all_responses.push(responses);
    }

    let mut cache = state::StateCache::new();
    for bal in engine.snapshot_balances().values() {
        cache.set_balance(bal);
    }
    for meta in engine.snapshot_metadata().values() {
        cache.set_metadata(meta);
    }
    let post_root = cache.commit_to_mpt(&mut mpt);

    let balance_deltas: Vec<Balance> = engine.snapshot_balances().values().cloned().collect();

    Ok(ZkvmOutput {
        post_root,
        responses: all_responses,
        balance_deltas,
    })
}
