/// Faster-hashing HashMap alias used across the engine's hot paths.
///
/// Alias for `std::collections::HashMap` with `ahash::RandomState`. All
/// existing `HashMap<K, V>` method calls compile without change; only
/// construction sites need `EngineMap::default()` instead of `::new()`.
/// Serde round-trips are wire-compatible with the previous default
/// `RandomState` — the hasher is chosen at deserialize time.
pub type EngineMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;

pub mod batch_dispatcher;
pub mod matching_engine;
pub mod order_book;
pub mod cow_cache;
pub mod delta_buffer;
pub mod credit;
pub mod ofi;
pub mod user_state;
pub mod market_shards;

pub use batch_dispatcher::{BatchDispatcher, BatchedRequest, BatchMetrics};
pub use matching_engine::MatchingEngine;
pub use order_book::OrderBook;
pub use delta_buffer::DeltaBuffer;
pub use credit::CreditSystem;
pub use ofi::ToxicityScorer;
pub use user_state::UserState;
pub use market_shards::{MarketShard, MarketShards};
