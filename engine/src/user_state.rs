use crate::credit::CreditSystem;
use crate::{EngineMap, MatchingEngine};
use types::{
    AssetId, Balance, CancelOrderRequest, DepositRequest, Market, MarketId, OrderId, OrderSide,
    PostOrderRequest, Timestamp, UserId, UserMetadata, VelaError, WithdrawalRequest,
};

pub struct UserState {
    pub balances: EngineMap<(UserId, AssetId), Balance>,
    pub metadata: EngineMap<UserId, UserMetadata>,
    pub credit_system: CreditSystem,
    pub fee_balances: EngineMap<String, u64>,
    pub next_order_id: OrderId,
    pub markets: EngineMap<MarketId, Market>,
    pub order_to_market: EngineMap<OrderId, MarketId>,
    pub client_order_to_market: EngineMap<(UserId, String), MarketId>,
}

impl UserState {
    pub fn new(default_credit_ratio: f64) -> Self {
        UserState {
            balances: EngineMap::default(),
            metadata: EngineMap::default(),
            credit_system: CreditSystem::new(default_credit_ratio),
            fee_balances: EngineMap::default(),
            next_order_id: 1,
            markets: EngineMap::default(),
            order_to_market: EngineMap::default(),
            client_order_to_market: EngineMap::default(),
        }
    }

    pub fn add_market(&mut self, market: Market) {
        self.markets.insert(market.id.clone(), market);
    }

    pub fn alloc_order_id(&mut self) -> OrderId {
        let id = self.next_order_id;
        self.next_order_id += 1;
        id
    }

    pub fn get_balance(&self, user: &UserId, asset: &AssetId) -> Balance {
        self.balances
            .get(&(user.clone(), *asset))
            .cloned()
            .unwrap_or_else(|| Balance {
                user: user.clone(),
                asset: *asset,
                available: 0,
                locked: 0,
            })
    }

    pub fn get_metadata(&self, user: &UserId) -> UserMetadata {
        self.metadata
            .get(user)
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

    /// Phase 1 (sharded dispatch): validate the request WITHOUT modifying nonces or balances.
    /// Uses the provided snapshot to check balance/nonce (snapshot was taken after deposits
    /// but before any order processing, so nonces are fresh and balances include deposits).
    /// Allocates an order_id and records routing in self.
    /// Returns (order_id, spend_asset, order_notional) so caller can track in-batch reservations.
    pub fn phase1_check_balance(
        &mut self,
        req: &PostOrderRequest,
        ts: Timestamp,
        reserved: &EngineMap<(UserId, AssetId), u64>,
        snap_balances: &EngineMap<(UserId, AssetId), Balance>,
        snap_metadata: &EngineMap<UserId, UserMetadata>,
    ) -> Result<(OrderId, AssetId, u64), VelaError> {
        // Validate market exists
        if !self.markets.contains_key(&req.market) {
            return Err(VelaError::MarketNotFound(req.market.to_string()));
        }
        let market = self.markets[&req.market].clone();

        // Check nonce validity against snapshot (nonces not yet consumed).
        // We don't accept it here; Phase 2 shard engine accepts it.
        let snap_meta = snap_metadata
            .get(&req.user)
            .cloned()
            .unwrap_or_else(|| UserMetadata {
                user: req.user.clone(),
                nonce_window: types::NonceWindow::new(),
                open_order_ids: [0u64; 64],
                credit_ratio: 1.0,
                total_quoted_notional: 0,
                actual_collateral: 0,
                ref_by: None,
                ref_earnings: 0,
                referred_users: vec![],
            });
        // Check nonce would be accepted (non-destructive read)
        // We simulate by cloning and trying to accept
        let mut tmp_nonce_window = snap_meta.nonce_window.clone();
        if !tmp_nonce_window.accept(req.nonce) {
            return Err(VelaError::InvalidNonce);
        }

        let spend_asset = match req.side {
            OrderSide::Bid => market.quote,
            OrderSide::Ask => market.base,
        };

        let order_notional = match req.side {
            OrderSide::Bid => crate::CreditSystem::compute_notional(req.price, req.quantity),
            OrderSide::Ask => req.quantity,
        };

        // Check available balance from snapshot, accounting for in-batch reservations
        let snap_bal = snap_balances
            .get(&(req.user.clone(), spend_asset))
            .cloned()
            .unwrap_or_else(|| Balance {
                user: req.user.clone(),
                asset: spend_asset,
                available: 0,
                locked: 0,
            });
        let already_reserved = reserved
            .get(&(req.user.clone(), spend_asset))
            .copied()
            .unwrap_or(0);
        let effective_available = snap_bal.available.saturating_sub(already_reserved);

        if effective_available < order_notional {
            return Err(VelaError::InsufficientBalance);
        }

        // Check credit limits
        let deposited = snap_bal.total();
        self.credit_system.check_credit(
            &req.user,
            deposited,
            snap_meta
                .total_quoted_notional
                .saturating_add(already_reserved),
            order_notional,
            ts,
        )?;

        // Allocate globally-unique order_id
        let order_id = self.alloc_order_id();

        // Record routing
        self.order_to_market.insert(order_id, req.market.clone());
        if let Some(coid) = &req.client_order_id {
            self.client_order_to_market
                .insert((req.user.clone(), coid.clone()), req.market.clone());
        }

        Ok((order_id, spend_asset, order_notional))
    }

    /// Phase 1 (non-sharded, for backward compat): validate and reserve balance.
    pub fn phase1_validate_only(
        &mut self,
        req: &PostOrderRequest,
        ts: Timestamp,
        reserved: &EngineMap<(UserId, AssetId), u64>,
    ) -> Result<(OrderId, AssetId, u64), VelaError> {
        let snap_balances = self.balances.clone();
        let snap_metadata = self.metadata.clone();
        self.phase1_check_balance(req, ts, reserved, &snap_balances, &snap_metadata)
    }

    /// Phase 1: validate the request, reserve the balance, alloc an order_id.
    /// Does NOT add the order to the book — that happens in Phase 2.
    /// This version actually locks the balance (used by non-sharded path if needed).
    pub fn phase1_reserve(
        &mut self,
        req: &PostOrderRequest,
        ts: Timestamp,
    ) -> Result<OrderId, VelaError> {
        let reserved = EngineMap::default();
        let snap_balances = self.balances.clone();
        let snap_metadata = self.metadata.clone();
        let (order_id, spend_asset, order_notional) =
            self.phase1_check_balance(req, ts, &reserved, &snap_balances, &snap_metadata)?;

        // Lock available → locked
        let key = (req.user.clone(), spend_asset);
        let bal_entry = self.balances.entry(key).or_insert_with(|| Balance {
            user: req.user.clone(),
            asset: spend_asset,
            available: 0,
            locked: 0,
        });
        bal_entry.available = bal_entry.available.saturating_sub(order_notional);
        bal_entry.locked += order_notional;

        // Update metadata
        let mut meta = self.get_metadata(&req.user);
        meta.total_quoted_notional += order_notional;
        self.metadata.insert(req.user.clone(), meta);

        Ok(order_id)
    }

    /// Phase 1: validate a cancel request.
    /// Returns (order_id, market_id) for routing to the shard.
    pub fn phase1_cancel_validate(
        &mut self,
        req: &CancelOrderRequest,
    ) -> Result<(OrderId, MarketId), VelaError> {
        let mut meta = self.get_metadata(&req.user);
        if !meta.nonce_window.accept(req.nonce) {
            return Err(VelaError::InvalidNonce);
        }
        self.metadata.insert(req.user.clone(), meta.clone());

        let order_id = if let Some(id) = req.order_id {
            id
        } else if let Some(coid) = &req.client_order_id {
            let key = (req.user.clone(), coid.clone());
            match self.client_order_to_market.get(&key) {
                Some(_) => {
                    // We need the actual order_id from the shard; for now use 0 as sentinel
                    // The shard will look it up by client_order_id
                    0
                }
                None => return Err(VelaError::OrderNotFound),
            }
        } else {
            return Err(VelaError::OrderNotFound);
        };

        let market_id = if order_id == 0 {
            // client_order_id path: find market from client_order_to_market
            let coid = req.client_order_id.as_deref().unwrap_or("");
            let key = (req.user.clone(), coid.to_string());
            self.client_order_to_market
                .get(&key)
                .cloned()
                .ok_or(VelaError::OrderNotFound)?
        } else {
            self.order_to_market
                .get(&order_id)
                .cloned()
                .ok_or(VelaError::OrderNotFound)?
        };

        // Note: we already accepted the nonce above
        let _ = meta;

        Ok((order_id, market_id))
    }

    pub fn phase1_deposit(&mut self, req: &DepositRequest) {
        // Check if this asset is a quote asset across any market
        let is_quote = self.markets.values().any(|m| m.quote == req.asset);
        if is_quote {
            let mut meta = self.get_metadata(&req.user);
            meta.actual_collateral += req.amount;
            self.metadata.insert(req.user.clone(), meta);
        }

        let key = (req.user.clone(), req.asset);
        let bal = self.balances.entry(key).or_insert_with(|| Balance {
            user: req.user.clone(),
            asset: req.asset,
            available: 0,
            locked: 0,
        });
        bal.available += req.amount;
    }

    pub fn phase1_withdraw(
        &mut self,
        req: &WithdrawalRequest,
        ts: Timestamp,
    ) -> Result<(), VelaError> {
        let mut meta = self.get_metadata(&req.user);
        if !meta.nonce_window.accept(req.nonce) {
            return Err(VelaError::InvalidNonce);
        }

        let key = (req.user.clone(), req.asset);
        let bal = self.balances.entry(key).or_insert_with(|| Balance {
            user: req.user.clone(),
            asset: req.asset,
            available: 0,
            locked: 0,
        });
        if bal.available < req.amount {
            return Err(VelaError::InsufficientBalance);
        }
        bal.available -= req.amount;
        self.metadata.insert(req.user.clone(), meta);
        let _ = ts;
        Ok(())
    }

    /// Apply `(final - snapshot)` balance delta to self.balances.
    /// Only iterates over keys in `final_bals` — keys only in snapshot were untouched
    /// by the shard and require no delta application.
    pub fn apply_balance_delta(
        &mut self,
        final_bals: &EngineMap<(UserId, AssetId), Balance>,
        snapshot: &EngineMap<(UserId, AssetId), Balance>,
    ) {
        for (key, final_bal) in final_bals {
            let snap_bal = snapshot.get(key).cloned().unwrap_or_else(|| Balance {
                user: key.0.clone(),
                asset: key.1,
                available: 0,
                locked: 0,
            });
            let final_bal = final_bal.clone();

            // delta = final - snapshot (signed arithmetic via i128)
            let avail_delta = final_bal.available as i128 - snap_bal.available as i128;
            let locked_delta = final_bal.locked as i128 - snap_bal.locked as i128;

            if avail_delta == 0 && locked_delta == 0 {
                continue;
            }

            let entry = self.balances.entry(key.clone()).or_insert_with(|| Balance {
                user: key.0.clone(),
                asset: key.1,
                available: 0,
                locked: 0,
            });

            if avail_delta >= 0 {
                entry.available = entry.available.saturating_add(avail_delta as u64);
            } else {
                entry.available = entry
                    .available
                    .saturating_sub(avail_delta.unsigned_abs() as u64);
            }
            if locked_delta >= 0 {
                entry.locked = entry.locked.saturating_add(locked_delta as u64);
            } else {
                entry.locked = entry
                    .locked
                    .saturating_sub(locked_delta.unsigned_abs() as u64);
            }
        }
    }

    /// Apply metadata delta: merge changes from shard execution.
    pub fn apply_metadata_delta(
        &mut self,
        final_meta: &EngineMap<UserId, UserMetadata>,
        snapshot_meta: &EngineMap<UserId, UserMetadata>,
    ) {
        // For each user changed in the shard, apply to current state
        for (user, final_m) in final_meta {
            let snap_m = snapshot_meta
                .get(user)
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
                });

            let current = self
                .metadata
                .entry(user.clone())
                .or_insert_with(|| snap_m.clone());

            // Merge nonce windows: union all shards' accepted nonces so no shard overwrites another.
            current.nonce_window.merge(&final_m.nonce_window);

            // Apply total_quoted_notional delta
            let notional_delta =
                final_m.total_quoted_notional as i128 - snap_m.total_quoted_notional as i128;
            if notional_delta >= 0 {
                current.total_quoted_notional = current
                    .total_quoted_notional
                    .saturating_add(notional_delta as u64);
            } else {
                current.total_quoted_notional = current
                    .total_quoted_notional
                    .saturating_sub(notional_delta.unsigned_abs() as u64);
            }

            // Apply actual_collateral delta
            let collateral_delta =
                final_m.actual_collateral as i128 - snap_m.actual_collateral as i128;
            if collateral_delta >= 0 {
                current.actual_collateral = current
                    .actual_collateral
                    .saturating_add(collateral_delta as u64);
            } else {
                current.actual_collateral = current
                    .actual_collateral
                    .saturating_sub(collateral_delta.unsigned_abs() as u64);
            }

            // Apply ref_earnings delta
            let ref_delta = final_m.ref_earnings as i128 - snap_m.ref_earnings as i128;
            if ref_delta > 0 {
                current.ref_earnings = current.ref_earnings.saturating_add(ref_delta as u64);
            }

            // Merge open_order_ids: add new ones, remove gone ones
            let snap_ids: std::collections::HashSet<u64> = snap_m.iter_order_ids().collect();
            let final_ids: std::collections::HashSet<u64> = final_m.iter_order_ids().collect();
            let added: Vec<u64> = final_ids.difference(&snap_ids).copied().collect();
            let removed: Vec<u64> = snap_ids.difference(&final_ids).copied().collect();
            for id in removed {
                current.remove_order_id(id);
            }
            for id in added {
                current.push_order_id(id);
            }
        }
    }

    /// Apply fee balance delta.
    pub fn apply_fee_delta(
        &mut self,
        final_fees: &EngineMap<String, u64>,
        snap_fees: &EngineMap<String, u64>,
    ) {
        let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
        for k in final_fees.keys() {
            keys.insert(k.clone());
        }
        for k in snap_fees.keys() {
            keys.insert(k.clone());
        }

        for key in keys {
            let final_val = final_fees.get(&key).copied().unwrap_or(0);
            let snap_val = snap_fees.get(&key).copied().unwrap_or(0);
            let delta = final_val as i128 - snap_val as i128;
            if delta == 0 {
                continue;
            }
            let entry = self.fee_balances.entry(key).or_insert(0);
            if delta >= 0 {
                *entry = entry.saturating_add(delta as u64);
            } else {
                *entry = entry.saturating_sub(delta.unsigned_abs() as u64);
            }
        }
    }

    /// Remove order routing entries after fills/cancels.
    pub fn remove_order_routing(
        &mut self,
        order_id: OrderId,
        user: &UserId,
        client_order_id: Option<&str>,
    ) {
        self.order_to_market.remove(&order_id);
        if let Some(coid) = client_order_id {
            self.client_order_to_market
                .remove(&(user.clone(), coid.to_string()));
        }
    }

    /// Initialize UserState from an existing MatchingEngine (called at startup).
    pub fn sync_from_engine(&mut self, engine: &MatchingEngine) {
        self.balances = engine.balances.clone();
        self.metadata = engine.metadata.clone();
        self.credit_system = engine.credit_system.clone();
        self.fee_balances = engine.fee_balances.clone();
        self.next_order_id = engine.next_order_id();
        self.markets = engine.markets.clone();

        // Rebuild order_to_market from order books
        self.order_to_market.clear();
        self.client_order_to_market.clear();
        for (market_id, book) in &engine.order_books {
            for order in book.all_orders() {
                self.order_to_market.insert(order.id, market_id.clone());
                if let Some(coid) = &order.client_order_id {
                    self.client_order_to_market
                        .insert((order.user.clone(), coid.clone()), market_id.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{NonceWindow, UserId, NONCE_WINDOW_SIZE};

    fn test_user() -> UserId {
        UserId([0xAB; 20])
    }

    fn make_meta(uid: &UserId, nonces: &[u64]) -> UserMetadata {
        let mut w = NonceWindow::new();
        for &n in nonces {
            w.accept(n);
        }
        UserMetadata {
            user: uid.clone(),
            nonce_window: w,
            open_order_ids: [0u64; 64],
            credit_ratio: 1.0,
            total_quoted_notional: 0,
            actual_collateral: 0,
            ref_by: None,
            ref_earnings: 0,
            referred_users: vec![],
        }
    }

    fn active_nonces(state: &UserState, uid: &UserId) -> Vec<u64> {
        let w = &state.metadata[uid].nonce_window;
        let mut v: Vec<u64> = w.iter_active().collect();
        v.sort_unstable();
        v
    }

    // (b) Two shards, different nonces — both must appear in the merged window.
    #[test]
    fn nonce_merge_different_nonces() {
        let mut state = UserState::new(1.0);
        let uid = test_user();

        let snap = make_meta(&uid, &[]);
        let final_a = make_meta(&uid, &[1]);
        let final_b = make_meta(&uid, &[2]);

        let mut snap_map: EngineMap<UserId, UserMetadata> = EngineMap::default();
        snap_map.insert(uid.clone(), snap);

        let mut final_map_a: EngineMap<UserId, UserMetadata> = EngineMap::default();
        final_map_a.insert(uid.clone(), final_a);
        let mut final_map_b: EngineMap<UserId, UserMetadata> = EngineMap::default();
        final_map_b.insert(uid.clone(), final_b);

        state.apply_metadata_delta(&final_map_a, &snap_map);
        state.apply_metadata_delta(&final_map_b, &snap_map);

        let nonces = active_nonces(&state, &uid);
        assert!(nonces.contains(&1), "nonce 1 from shard A must be retained");
        assert!(nonces.contains(&2), "nonce 2 from shard B must be retained");
    }

    // (c) Two shards, same nonce — merge must be idempotent; nonce appears once.
    #[test]
    fn nonce_merge_idempotent() {
        let mut state = UserState::new(1.0);
        let uid = test_user();

        let snap = make_meta(&uid, &[]);
        let final_a = make_meta(&uid, &[5]);
        let final_b = make_meta(&uid, &[5]);

        let mut snap_map: EngineMap<UserId, UserMetadata> = EngineMap::default();
        snap_map.insert(uid.clone(), snap);

        let mut final_map_a: EngineMap<UserId, UserMetadata> = EngineMap::default();
        final_map_a.insert(uid.clone(), final_a);
        let mut final_map_b: EngineMap<UserId, UserMetadata> = EngineMap::default();
        final_map_b.insert(uid.clone(), final_b);

        state.apply_metadata_delta(&final_map_a, &snap_map);
        state.apply_metadata_delta(&final_map_b, &snap_map);

        let nonces = active_nonces(&state, &uid);
        let count = nonces.iter().filter(|&&n| n == 5).count();
        assert_eq!(
            count, 1,
            "duplicate nonce must appear exactly once after merge"
        );
    }

    // (d) Overflow: merging more than NONCE_WINDOW_SIZE total nonces evicts the oldest.
    #[test]
    fn nonce_merge_overflow_evicts_oldest() {
        let mut state = UserState::new(1.0);
        let uid = test_user();

        // Shard A: nonces 1..=NONCE_WINDOW_SIZE (oldest)
        // Shard B: nonces (NONCE_WINDOW_SIZE+1)..=(2*NONCE_WINDOW_SIZE) (newest)
        let a_nonces: Vec<u64> = (1..=(NONCE_WINDOW_SIZE as u64)).collect();
        let b_nonces: Vec<u64> =
            ((NONCE_WINDOW_SIZE as u64 + 1)..=(2 * NONCE_WINDOW_SIZE as u64)).collect();

        let snap = make_meta(&uid, &[]);
        let final_a = make_meta(&uid, &a_nonces);
        let final_b = make_meta(&uid, &b_nonces);

        let mut snap_map: EngineMap<UserId, UserMetadata> = EngineMap::default();
        snap_map.insert(uid.clone(), snap);

        let mut final_map_a: EngineMap<UserId, UserMetadata> = EngineMap::default();
        final_map_a.insert(uid.clone(), final_a);
        let mut final_map_b: EngineMap<UserId, UserMetadata> = EngineMap::default();
        final_map_b.insert(uid.clone(), final_b);

        state.apply_metadata_delta(&final_map_a, &snap_map);
        state.apply_metadata_delta(&final_map_b, &snap_map);

        let nonces = active_nonces(&state, &uid);
        assert_eq!(
            nonces.len(),
            NONCE_WINDOW_SIZE,
            "window must not exceed NONCE_WINDOW_SIZE"
        );
        for &n in &b_nonces {
            assert!(nonces.contains(&n), "newest nonce {} must be retained", n);
        }
        for &n in &a_nonces {
            assert!(
                !nonces.contains(&n),
                "oldest nonce {} must be evicted on overflow",
                n
            );
        }
    }
}
