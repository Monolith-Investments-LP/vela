use crate::EngineMap;
use types::{AssetId, Balance, MarketId, Order, OrderId, Quantity, UserId, UserMetadata};

#[derive(Debug, Clone)]
enum DeltaEntry {
    Insert { market: MarketId, order: Order },
    Remove { market: MarketId, order_id: OrderId },
    PartialFill { market: MarketId, order_id: OrderId, additional_filled: Quantity },
}

#[derive(Debug, Default)]
pub struct DeltaBuffer {
    balance_overlay: EngineMap<(UserId, AssetId), Balance>,
    metadata_overlay: EngineMap<UserId, UserMetadata>,
    entries: Vec<DeltaEntry>,
    fee_delta: EngineMap<String, i64>,
}

impl DeltaBuffer {
    pub fn new() -> Self {
        DeltaBuffer::default()
    }

    pub fn is_empty(&self) -> bool {
        self.balance_overlay.is_empty() && self.metadata_overlay.is_empty() && self.entries.is_empty()
    }

    pub fn get_balance(
        &self,
        user: &UserId,
        asset: &AssetId,
        base: &EngineMap<(UserId, AssetId), Balance>,
    ) -> Balance {
        self.balance_overlay
            .get(&(user.clone(), *asset))
            .or_else(|| base.get(&(user.clone(), *asset)))
            .cloned()
            .unwrap_or_else(|| Balance { user: user.clone(), asset: *asset, available: 0, locked: 0 })
    }

    pub fn set_balance(&mut self, balance: Balance) {
        self.balance_overlay.insert((balance.user.clone(), balance.asset), balance);
    }

    pub fn credit_available(
        &mut self,
        user: &UserId,
        asset: &AssetId,
        amount: u64,
        base: &EngineMap<(UserId, AssetId), Balance>,
    ) {
        let mut bal = self.get_balance(user, asset, base);
        bal.available += amount;
        self.set_balance(bal);
    }

    pub fn debit_locked(
        &mut self,
        user: &UserId,
        asset: &AssetId,
        amount: u64,
        base: &EngineMap<(UserId, AssetId), Balance>,
    ) {
        let mut bal = self.get_balance(user, asset, base);
        bal.locked = bal.locked.saturating_sub(amount);
        self.set_balance(bal);
    }

    pub fn debit_available(
        &mut self,
        user: &UserId,
        asset: &AssetId,
        amount: u64,
        base: &EngineMap<(UserId, AssetId), Balance>,
    ) {
        let mut bal = self.get_balance(user, asset, base);
        bal.available = bal.available.saturating_sub(amount);
        self.set_balance(bal);
    }

    pub fn lock_available(
        &mut self,
        user: &UserId,
        asset: &AssetId,
        amount: u64,
        base: &EngineMap<(UserId, AssetId), Balance>,
    ) {
        let mut bal = self.get_balance(user, asset, base);
        bal.available = bal.available.saturating_sub(amount);
        bal.locked += amount;
        self.set_balance(bal);
    }

    pub fn unlock_to_available(
        &mut self,
        user: &UserId,
        asset: &AssetId,
        amount: u64,
        base: &EngineMap<(UserId, AssetId), Balance>,
    ) {
        let mut bal = self.get_balance(user, asset, base);
        bal.locked = bal.locked.saturating_sub(amount);
        bal.available += amount;
        self.set_balance(bal);
    }

    pub fn get_metadata(&self, user: &UserId, base: &EngineMap<UserId, UserMetadata>) -> UserMetadata {
        self.metadata_overlay
            .get(user)
            .or_else(|| base.get(user))
            .cloned()
            .unwrap_or_else(|| UserMetadata {
                user: user.clone(),
                nonce_window: types::NonceWindow::new(),
                open_order_ids: [0u64; 64],
                credit_ratio: 1.0,
                total_quoted_notional: 0,
                actual_collateral: 0,
                ref_by: None,
                ref_earnings: 0,
                referred_users: vec![],
            })
    }

    pub fn set_metadata(&mut self, metadata: UserMetadata) {
        self.metadata_overlay.insert(metadata.user.clone(), metadata);
    }

    pub fn record_insert(&mut self, market: MarketId, order: Order) {
        self.entries.push(DeltaEntry::Insert { market, order });
    }

    pub fn record_remove(&mut self, market: MarketId, order_id: OrderId) {
        self.entries.push(DeltaEntry::Remove { market, order_id });
    }

    pub fn record_partial_fill(&mut self, market: MarketId, order_id: OrderId, additional_filled: Quantity) {
        self.entries.push(DeltaEntry::PartialFill { market, order_id, additional_filled });
    }

    pub fn add_exchange_fee(&mut self, asset: &str, amount: i64) {
        *self.fee_delta.entry(asset.to_string()).or_insert(0) += amount;
    }

    pub fn commit(self, engine: &mut crate::MatchingEngine) {
        engine.balances.extend(self.balance_overlay);
        engine.metadata.extend(self.metadata_overlay);
        for (asset, delta) in self.fee_delta {
            let entry = engine.fee_balances.entry(asset).or_insert(0);
            if delta >= 0 {
                *entry = entry.saturating_add(delta as u64);
            } else {
                *entry = entry.saturating_sub(delta.unsigned_abs());
            }
        }
        for entry in self.entries {
            match entry {
                DeltaEntry::Insert { market, order } => {
                    if let Some(book) = engine.order_books.get_mut(&market) {
                        let _ = book.insert_resting(order);
                    }
                }
                DeltaEntry::Remove { market, order_id } => {
                    if let Some(book) = engine.order_books.get_mut(&market) {
                        book.remove_order(order_id);
                    }
                }
                DeltaEntry::PartialFill { market, order_id, additional_filled } => {
                    if let Some(book) = engine.order_books.get_mut(&market) {
                        book.update_filled_quantity(order_id, additional_filled);
                    }
                }
            }
        }
    }

    pub fn rollback(self) {}
}
