/// Faster-hashing HashMap alias used across the engine's hot paths.
///
/// Alias for `std::collections::HashMap` with `ahash::RandomState`. All
/// existing `HashMap<K, V>` method calls compile without change; only
/// construction sites need `EngineMap::default()` instead of `::new()`.
/// Serde round-trips are wire-compatible with the previous default
/// `RandomState` — the hasher is chosen at deserialize time.
pub type EngineMap<K, V> = std::collections::HashMap<K, V, ahash::RandomState>;

pub mod batch_dispatcher;
pub mod cow_cache;
pub mod credit;
pub mod delta_buffer;
pub mod market_shards;
pub mod matching_engine;
pub mod ofi;
pub mod order_book;
pub mod user_state;

pub use batch_dispatcher::{BatchDispatcher, BatchMetrics, BatchedRequest};
pub use credit::CreditSystem;
pub use delta_buffer::DeltaBuffer;
pub use market_shards::{MarketShard, MarketShards};
pub use matching_engine::MatchingEngine;
pub use ofi::ToxicityScorer;
pub use order_book::OrderBook;
pub use user_state::UserState;
