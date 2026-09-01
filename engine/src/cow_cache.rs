//! **EXPERIMENTAL — NOT ON THE PRODUCTION PATH.**
//!
//! `CowCache` is a copy-on-write cache prototype that was superseded by
//! `DeltaBuffer` on the live engine.  Its benchmark numbers do not reflect
//! production behavior.  Retained here for potential future use only.

use crate::order_book::OrderBook;
use std::collections::HashMap;
use types::{AssetId, Balance, OrderId, Quantity, UserId, UserMetadata};

#[derive(Debug, Clone)]
pub enum Delta {
    OrderBookInsert {
        market: types::MarketId,
        order: types::Order,
    },
    OrderBookRemove {
        market: types::MarketId,
        order_id: OrderId,
    },
    OrderBookPartialFill {
        market: types::MarketId,
        order_id: OrderId,
        additional_filled: Quantity,
    },
}

#[derive(Debug, Default)]
pub struct CowCache {
    pub balance_overlay: HashMap<(UserId, AssetId), Balance>,
    pub metadata_overlay: HashMap<UserId, UserMetadata>,
    deltas: Vec<Delta>,
}

impl CowCache {
    pub fn new() -> Self {
        CowCache::default()
    }

    pub fn get_balance<'a>(
        &'a self,
        user: &UserId,
        asset: &AssetId,
        base: &'a HashMap<(UserId, AssetId), Balance>,
    ) -> Balance {
        self.balance_overlay
            .get(&(user.clone(), *asset))
            .or_else(|| base.get(&(user.clone(), *asset)))
            .cloned()
            .unwrap_or_else(|| Balance {
                user: user.clone(),
                asset: *asset,
                available: 0,
                locked: 0,
            })
    }

    pub fn set_balance(&mut self, balance: Balance) {
        let key = (balance.user.clone(), balance.asset);
        self.balance_overlay.insert(key, balance);
    }

    pub fn credit_available(
        &mut self,
        user: &UserId,
        asset: &AssetId,
        amount: u64,
        base: &HashMap<(UserId, AssetId), Balance>,
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
        base: &HashMap<(UserId, AssetId), Balance>,
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
        base: &HashMap<(UserId, AssetId), Balance>,
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
        base: &HashMap<(UserId, AssetId), Balance>,
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
        base: &HashMap<(UserId, AssetId), Balance>,
    ) {
        let mut bal = self.get_balance(user, asset, base);
        bal.locked = bal.locked.saturating_sub(amount);
        bal.available += amount;
        self.set_balance(bal);
    }

    pub fn get_metadata(
        &self,
        user: &UserId,
        base: &HashMap<UserId, UserMetadata>,
    ) -> UserMetadata {
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
        self.metadata_overlay
            .insert(metadata.user.clone(), metadata);
    }

    pub fn record_insert(&mut self, market: types::MarketId, order: types::Order) {
        self.deltas.push(Delta::OrderBookInsert { market, order });
    }

    pub fn record_remove(&mut self, market: types::MarketId, order_id: OrderId) {
        self.deltas
            .push(Delta::OrderBookRemove { market, order_id });
    }

    pub fn record_partial_fill(
        &mut self,
        market: types::MarketId,
        order_id: OrderId,
        additional_filled: Quantity,
    ) {
        self.deltas.push(Delta::OrderBookPartialFill {
            market,
            order_id,
            additional_filled,
        });
    }

    pub fn commit(
        self,
        balances: &mut HashMap<(UserId, AssetId), Balance>,
        metadata: &mut HashMap<UserId, UserMetadata>,
        order_books: &mut HashMap<types::MarketId, OrderBook>,
    ) {
        // Drain overlays directly — no redundant delta replay needed for balance/metadata.
        balances.extend(self.balance_overlay);
        metadata.extend(self.metadata_overlay);

        // Only order-book deltas remain (insert/remove/partial-fill).
        for delta in self.deltas {
            match delta {
                Delta::OrderBookInsert { market, order } => {
                    if let Some(book) = order_books.get_mut(&market) {
                        let _ = book.insert_resting(order);
                    }
                }
                Delta::OrderBookRemove { market, order_id } => {
                    if let Some(book) = order_books.get_mut(&market) {
                        book.remove_order(order_id);
                    }
                }
                Delta::OrderBookPartialFill {
                    market,
                    order_id,
                    additional_filled,
                } => {
                    if let Some(book) = order_books.get_mut(&market) {
                        book.update_filled_quantity(order_id, additional_filled);
                    }
                }
            }
        }
    }

    pub fn rollback(self) {}
}
