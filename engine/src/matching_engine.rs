use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;
use types::{
    AssetId, Balance, CancelOrderRequest, DepositRequest, ErrorCode, ErrorResponse, FeeConfig,
    Fill, Market, MarketId, Order, OrderCanceledResponse, OrderId, OrderPostedResponse, OrderSide,
    OrderStatus, OrderType, PostOrderRequest, Request, Response, Timestamp, UserId, UserMetadata,
    VelaError, WithdrawalRequest,
};
use crate::{DeltaBuffer, CreditSystem, EngineMap, OrderBook};
use crate::ofi::{ToxicityScorer, TOXICITY_PENALTY_THRESHOLD, CREDIT_PENALTY_MULTIPLIER, CREDIT_PENALTY_DURATION_US};

type ExpiryCandidate = (Timestamp, OrderId, u64, MarketId, OrderSide, u64, u64, Option<String>);

pub struct MatchingEngine {
    pub order_books: EngineMap<MarketId, OrderBook>,
    pub balances: EngineMap<(UserId, AssetId), Balance>,
    pub metadata: EngineMap<UserId, UserMetadata>,
    pub markets: EngineMap<MarketId, Market>,
    pub fee_config: FeeConfig,
    pub fee_balances: EngineMap<String, u64>,
    pub credit_system: CreditSystem,
    pub timestamp: Timestamp,
    next_order_id: OrderId,
    /// Interior-mutable toxicity scorer; updated on every taker fill without
    /// requiring &mut self in match_order (which is called from &self context).
    toxicity_scorer: RefCell<ToxicityScorer>,
    /// Used by MarketShards sharded dispatch to inject a shared balance snapshot.
    /// When Some, DeltaBuffer base calls use this instead of self.balances.
    pub(crate) snapshot_balances: Option<Arc<EngineMap<(UserId, AssetId), Balance>>>,
    pub(crate) snapshot_metadata: Option<Arc<EngineMap<UserId, UserMetadata>>>,
}

impl MatchingEngine {
    pub fn new(fee_config: FeeConfig, default_credit_ratio: f64) -> Self {
        MatchingEngine {
            order_books: EngineMap::default(),
            balances: EngineMap::default(),
            metadata: EngineMap::default(),
            markets: EngineMap::default(),
            fee_config,
            fee_balances: EngineMap::default(),
            credit_system: CreditSystem::new(default_credit_ratio),
            timestamp: 0,
            next_order_id: 1,
            toxicity_scorer: RefCell::new(ToxicityScorer::new()),
            snapshot_balances: None,
            snapshot_metadata: None,
        }
    }

    pub fn set_credit_ratio(&mut self, user: UserId, ratio: f64) {
        self.credit_system.set_ratio(user, ratio);
    }

    pub fn add_market(&mut self, market: Market) {
        let book = OrderBook::new(market.id.clone(), market.max_orders);
        self.order_books.insert(market.id.clone(), book);
        self.markets.insert(market.id.clone(), market);
    }

    pub fn process(&mut self, request: Request, ts: Timestamp) -> Vec<Response> {
        self.timestamp = ts;
        match request {
            Request::PostOrder(req) => self.process_post_order(req),
            Request::CancelOrder(req) => self.process_cancel_order(req),
            Request::Deposit(req) => self.process_deposit(req),
            Request::Withdrawal(req) => self.process_withdrawal(req),
        }
    }

    fn alloc_order_id(&mut self) -> OrderId {
        let id = self.next_order_id;
        self.next_order_id += 1;
        id
    }

    fn get_metadata(&self, user: &UserId) -> UserMetadata {
        self.metadata.get(user).cloned().unwrap_or_else(|| UserMetadata {
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

    fn process_post_order(&mut self, req: PostOrderRequest) -> Vec<Response> {
        let order_id = self.alloc_order_id();
        let mut delta = DeltaBuffer::new();
        // Bind result before match so the &self borrow from try_post_order
        // is released before we take &mut self for penalty application and commit.
        let result = self.try_post_order(req, order_id, &mut delta);
        match result {
            Ok(responses) => {
                // Apply credit penalties for any high-toxicity fills — must happen
                // before delta.commit() so penalties are visible to subsequent orders
                // in the same microsecond batch.
                let mut penalized: HashSet<UserId> = HashSet::new();
                for resp in &responses {
                    if let Response::OrderFilled(fill) = resp {
                        if fill.toxicity_score > TOXICITY_PENALTY_THRESHOLD
                            && penalized.insert(fill.maker.clone())
                        {
                            self.credit_system.apply_penalty(
                                fill.maker.clone(),
                                CREDIT_PENALTY_MULTIPLIER,
                                CREDIT_PENALTY_DURATION_US,
                                self.timestamp,
                            );
                        }
                    }
                }
                drop(penalized);
                delta.commit(self);
                responses
            }
            Err(e) => {
                delta.rollback();
                vec![Response::Error(e.into())]
            }
        }
    }

    fn make_room_for_order(
        &self,
        user: &UserId,
        meta: &mut UserMetadata,
        deposited: u64,
        order_notional: u64,
        delta: &mut DeltaBuffer,
    ) -> Vec<Response> {
        let ratio = self.credit_system.effective_ratio(user, self.timestamp);
        let max_quoted = (deposited as f64 * ratio) as u64;

        if meta.total_quoted_notional.saturating_add(order_notional) <= max_quoted {
            return vec![];
        }

        let bal_base: &EngineMap<(UserId, AssetId), Balance> =
            self.snapshot_balances.as_deref().unwrap_or(&self.balances);

        let mut candidates: Vec<ExpiryCandidate> =
            meta.iter_order_ids().filter_map(|oid| {
                for (mid, book) in &self.order_books {
                    if let Some(o) = book.get_order(oid) {
                        let notional = CreditSystem::compute_notional(o.price, o.remaining_quantity());
                        return Some((
                            o.timestamp, oid, notional, mid.clone(),
                            o.side, o.price, o.remaining_quantity(), o.client_order_id.clone(),
                        ));
                    }
                }
                None
            })
            .collect();
        candidates.sort_by_key(|(ts, ..)| *ts);

        let mut responses = vec![];

        for (_, oid, notional, market_id, side, price, remaining, coid) in candidates {
            if meta.total_quoted_notional.saturating_add(order_notional) <= max_quoted {
                break;
            }
            let market = match self.markets.get(&market_id) {
                Some(m) => m,
                None => continue,
            };
            let (cancel_asset, unlock_amount) = match side {
                OrderSide::Bid => (market.quote, CreditSystem::compute_notional(price, remaining)),
                OrderSide::Ask => (market.base, remaining),
            };
            delta.record_remove(market_id, oid);
            delta.unlock_to_available(user, &cancel_asset, unlock_amount, bal_base);
            meta.remove_order_id(oid);
            meta.total_quoted_notional = meta.total_quoted_notional.saturating_sub(notional);
            responses.push(Response::OrderCanceled(OrderCanceledResponse {
                order_id: oid,
                client_order_id: coid,
            }));
        }

        responses
    }

    fn try_post_order(
        &self,
        req: PostOrderRequest,
        order_id: OrderId,
        delta: &mut DeltaBuffer,
    ) -> Result<Vec<Response>, VelaError> {
        let market = self
            .markets
            .get(&req.market)
            .ok_or_else(|| VelaError::MarketNotFound(req.market.to_string()))?;

        let book = self
            .order_books
            .get(&req.market)
            .ok_or_else(|| VelaError::MarketNotFound(req.market.to_string()))?;

        if let Some(coid) = &req.client_order_id {
            if coid.len() > 64 {
                return Err(VelaError::InvalidClientOrderId);
            }
            if !coid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(VelaError::InvalidClientOrderId);
            }
            let duplicate = self.order_books.values().any(|b| b.find_by_client_order_id(&req.user, coid).is_some());
            if duplicate {
                return Err(VelaError::DuplicateClientOrderId);
            }
        }

        let meta_base: &EngineMap<UserId, UserMetadata> =
            self.snapshot_metadata.as_deref().unwrap_or(&self.metadata);
        let bal_base: &EngineMap<(UserId, AssetId), Balance> =
            self.snapshot_balances.as_deref().unwrap_or(&self.balances);

        let mut meta = delta.get_metadata(&req.user, meta_base);
        if !meta.nonce_window.accept(req.nonce) {
            return Err(VelaError::InvalidNonce);
        }

        let (spend_asset, receive_asset) = match req.side {
            OrderSide::Bid => (market.quote, market.base),
            OrderSide::Ask => (market.base, market.quote),
        };

        let spend_balance = delta.get_balance(&req.user, &spend_asset, bal_base);

        let order_notional = match req.side {
            OrderSide::Bid => CreditSystem::compute_notional(req.price, req.quantity),
            OrderSide::Ask => req.quantity,
        };

        let deposited = spend_balance.total();
        let mut room_cancels: Vec<Response> = vec![];
        if order_notional > deposited {
            return Err(VelaError::InsufficientBalance);
        }
        if spend_balance.available < order_notional {
            let cancels = self.make_room_for_order(&req.user, &mut meta, deposited, order_notional, delta);
            room_cancels.extend(cancels);
            self.credit_system.check_credit(
                &req.user,
                deposited,
                meta.total_quoted_notional,
                order_notional,
                self.timestamp,
            )?;
        }

        if req.order_type == OrderType::PostOnly && book.would_match(req.side, req.price) {
            return Err(VelaError::PostOnlyWouldMatch);
        }

        let mut order = Order {
            id: order_id,
            user: req.user.clone(),
            market: req.market.clone(),
            side: req.side,
            order_type: req.order_type,
            price: req.price,
            quantity: req.quantity,
            filled_quantity: 0,
            nonce: req.nonce,
            client_order_id: req.client_order_id.clone(),
            timestamp: self.timestamp,
            status: OrderStatus::Open,
        };

        let (fills, auto_canceled) = self.match_order(&order, market, delta)?;

        let total_filled: u64 = fills.iter().map(|f| f.quantity).sum();
        order.filled_quantity = total_filled;

        if req.order_type == OrderType::FillOrKill && total_filled < req.quantity {
            return Err(VelaError::FokNotFilled);
        }

        let remaining = req.quantity.saturating_sub(total_filled);
        let will_rest = remaining > 0
            && matches!(req.order_type, OrderType::GoodTillCanceled | OrderType::PostOnly);

        if will_rest {
            if book.is_full() {
                return Err(VelaError::OrderBookFull);
            }
            let resting_spend = match req.side {
                OrderSide::Bid => CreditSystem::compute_notional(req.price, remaining),
                OrderSide::Ask => remaining,
            };
            delta.lock_available(&req.user, &spend_asset, resting_spend, bal_base);
            order.status = if total_filled > 0 { OrderStatus::PartiallyFilled } else { OrderStatus::Open };
            meta.push_order_id(order.id);
            meta.total_quoted_notional += CreditSystem::compute_notional(req.price, remaining);
            delta.set_metadata(meta);
            delta.record_insert(req.market.clone(), order.clone());
        } else {
            order.status = if total_filled >= req.quantity { OrderStatus::Filled } else { OrderStatus::Canceled };
            delta.set_metadata(meta);
        }

        let _ = receive_asset;

        let mut responses: Vec<Response> = room_cancels;
        responses.extend(fills.into_iter().map(Response::OrderFilled));
        for canceled in auto_canceled {
            responses.push(Response::OrderCanceled(canceled));
        }
        responses.push(Response::OrderPosted(OrderPostedResponse {
            order_id: order.id,
            client_order_id: order.client_order_id,
            status: order.status,
        }));

        Ok(responses)
    }

    fn match_order(
        &self,
        order: &Order,
        market: &Market,
        delta: &mut DeltaBuffer,
    ) -> Result<(Vec<Fill>, Vec<OrderCanceledResponse>), VelaError> {
        let meta_base: &EngineMap<UserId, UserMetadata> =
            self.snapshot_metadata.as_deref().unwrap_or(&self.metadata);

        let book = match self.order_books.get(&order.market) {
            Some(b) => b,
            None => return Ok((vec![], vec![])),
        };

        // Capture best opposing level depth at arrival time for the size-fraction component.
        let top_depth: u64 = match order.side {
            OrderSide::Bid => book.depth_asks(1).first().map(|(_, q)| *q).unwrap_or(0),
            OrderSide::Ask => book.depth_bids(1).first().map(|(_, q)| *q).unwrap_or(0),
        };

        let mut fills: Vec<Fill> = vec![];
        let mut taker_remaining = order.quantity;
        let mut locally_consumed: EngineMap<OrderId, u64> = EngineMap::default();
        let mut affected_makers: HashSet<UserId> = HashSet::new();
        // Count distinct price levels that actually produced fills (no allocation).
        let mut levels_with_fills: u32 = 0;

        let matchable_iter: Box<dyn Iterator<Item = (u64, &std::collections::VecDeque<Order>)>> =
            match order.side {
                OrderSide::Bid => Box::new(book.matchable_asks_ref(order.price)),
                OrderSide::Ask => Box::new(book.matchable_bids_ref(order.price)),
            };

        'outer: for (fill_price, level_orders) in matchable_iter {
            let fills_before = fills.len();
            for resting in level_orders {
                if taker_remaining == 0 { break 'outer; }
                if resting.user == order.user { continue; }

                let consumed = locally_consumed.get(&resting.id).copied().unwrap_or(0);
                let remaining_at_start = resting.remaining_quantity();
                let resting_remaining = remaining_at_start.saturating_sub(consumed);
                if resting_remaining == 0 { continue; }

                let fill_qty = taker_remaining.min(resting_remaining);
                let fill_notional = CreditSystem::compute_notional(fill_price, fill_qty);

                let taker_fee = (fill_notional as i64 * market.taker_fee_bps) / 10_000;
                let maker_fee = (fill_notional as i64 * market.maker_fee_bps) / 10_000;

                let fill = Fill {
                    maker_order_id: resting.id,
                    taker_order_id: order.id,
                    maker: resting.user.clone(),
                    taker: order.user.clone(),
                    market: order.market.clone(),
                    side: order.side,
                    price: fill_price,
                    quantity: fill_qty,
                    maker_fee,
                    taker_fee,
                    timestamp: self.timestamp,
                    // Populated below after scoring.
                    toxicity_score: 0.0,
                    ofi_snapshot: 0,
                };

                self.apply_fill_balances(&fill, market, delta);

                let new_consumed = consumed + fill_qty;
                locally_consumed.insert(resting.id, new_consumed);

                let fully_consumed = new_consumed >= remaining_at_start;
                let is_bid_maker = resting.side == OrderSide::Bid;
                if fully_consumed {
                    delta.record_remove(order.market.clone(), resting.id);
                    let mut maker_meta = delta.get_metadata(&resting.user, meta_base);
                    maker_meta.remove_order_id(resting.id);
                    let resting_notional = CreditSystem::compute_notional(resting.price, resting.remaining_quantity());
                    maker_meta.total_quoted_notional =
                        maker_meta.total_quoted_notional.saturating_sub(resting_notional);
                    if is_bid_maker {
                        maker_meta.actual_collateral =
                            maker_meta.actual_collateral.saturating_sub(fill_notional);
                    }
                    delta.set_metadata(maker_meta);
                } else {
                    delta.record_partial_fill(order.market.clone(), resting.id, fill_qty);
                    let mut maker_meta = delta.get_metadata(&resting.user, meta_base);
                    maker_meta.total_quoted_notional = maker_meta.total_quoted_notional
                        .saturating_sub(CreditSystem::compute_notional(resting.price, fill_qty));
                    if is_bid_maker {
                        maker_meta.actual_collateral =
                            maker_meta.actual_collateral.saturating_sub(fill_notional);
                    }
                    delta.set_metadata(maker_meta);
                }

                affected_makers.insert(resting.user.clone());
                taker_remaining -= fill_qty;
                fills.push(fill);
            }
            // Track levels that produced at least one fill (no allocation).
            if fills.len() > fills_before {
                levels_with_fills += 1;
            }
        }

        // ── Toxicity scoring ────────────────────────────────────────────────────
        // Completed before delta.commit() so it's part of the same logical batch.
        // Uses RefCell interior mutability to avoid requiring &mut self here.
        let total_fill_qty: u64 = fills.iter().map(|f| f.quantity).sum();
        if total_fill_qty > 0 {
            let signed_qty: i64 = match order.side {
                OrderSide::Bid =>  total_fill_qty as i64,
                OrderSide::Ask => -(total_fill_qty as i64),
            };
            let walked_book = levels_with_fills > 1;
            let (score, ofi_snap) = self.toxicity_scorer.borrow_mut().score_and_update(
                &order.market,
                signed_qty,
                top_depth,
                total_fill_qty,
                walked_book,
            );
            for fill in &mut fills {
                fill.toxicity_score = score;
                fill.ofi_snapshot = ofi_snap;
            }
        }
        // ────────────────────────────────────────────────────────────────────────

        let mut auto_canceled: Vec<OrderCanceledResponse> = vec![];

        if order.side == OrderSide::Ask {
            for maker_user in &affected_makers {
                let mut maker_meta = delta.get_metadata(maker_user, meta_base);
                let maker_deposited = maker_meta.actual_collateral;

                let open_orders: Vec<(OrderId, u64)> = maker_meta
                    .iter_order_ids()
                    .filter_map(|oid| {
                        for book in self.order_books.values() {
                            if let Some(o) = book.get_order(oid) {
                                let consumed = locally_consumed.get(&oid).copied().unwrap_or(0);
                                let remaining = o.remaining_quantity().saturating_sub(consumed);
                                if remaining > 0 {
                                    return Some((oid, CreditSystem::compute_notional(o.price, remaining)));
                                }
                                return None;
                            }
                        }
                        None
                    })
                    .collect();

                let to_cancel = self.credit_system.orders_to_cancel_after_fill(
                    maker_user,
                    maker_deposited,
                    maker_meta.total_quoted_notional,
                    &open_orders,
                    self.timestamp,
                );

                for oid in to_cancel {
                    'find_order: for book in self.order_books.values() {
                        if let Some(o) = book.get_order(oid) {
                            let consumed = locally_consumed.get(&oid).copied().unwrap_or(0);
                            let remaining = o.remaining_quantity().saturating_sub(consumed);
                            let notional = CreditSystem::compute_notional(o.price, remaining);
                            let unlock_amt = CreditSystem::compute_notional(o.price, remaining);

                            let bal_base_inner: &EngineMap<(UserId, AssetId), Balance> =
                                self.snapshot_balances.as_deref().unwrap_or(&self.balances);
                            delta.record_remove(o.market.clone(), oid);
                            delta.unlock_to_available(
                                maker_user,
                                &market.quote,
                                unlock_amt,
                                bal_base_inner,
                            );
                            maker_meta.remove_order_id(oid);
                            maker_meta.total_quoted_notional =
                                maker_meta.total_quoted_notional.saturating_sub(notional);

                            auto_canceled.push(OrderCanceledResponse {
                                order_id: oid,
                                client_order_id: o.client_order_id.clone(),
                            });
                            break 'find_order;
                        }
                    }
                }

                delta.set_metadata(maker_meta);
            }
        }

        Ok((fills, auto_canceled))
    }

    fn apply_fill_balances(&self, fill: &Fill, market: &Market, delta: &mut DeltaBuffer) {
        let bal_base: &EngineMap<(UserId, AssetId), Balance> =
            self.snapshot_balances.as_deref().unwrap_or(&self.balances);
        let meta_base: &EngineMap<UserId, UserMetadata> =
            self.snapshot_metadata.as_deref().unwrap_or(&self.metadata);
        let fill_notional = CreditSystem::compute_notional(fill.price, fill.quantity);

        match fill.side {
            OrderSide::Bid => {
                delta.debit_locked(&fill.taker, &market.quote, fill_notional, bal_base);
                delta.credit_available(&fill.taker, &market.base, fill.quantity, bal_base);
                if fill.taker_fee > 0 {
                    delta.debit_available(&fill.taker, &market.quote, fill.taker_fee as u64, bal_base);
                }
                delta.debit_locked(&fill.maker, &market.base, fill.quantity, bal_base);
                delta.credit_available(&fill.maker, &market.quote, fill_notional, bal_base);
                if fill.maker_fee < 0 {
                    delta.credit_available(&fill.maker, &market.quote, fill.maker_fee.unsigned_abs(), bal_base);
                } else if fill.maker_fee > 0 {
                    delta.debit_available(&fill.maker, &market.quote, fill.maker_fee as u64, bal_base);
                }
            }
            OrderSide::Ask => {
                delta.debit_locked(&fill.taker, &market.base, fill.quantity, bal_base);
                delta.credit_available(&fill.taker, &market.quote, fill_notional, bal_base);
                if fill.taker_fee > 0 {
                    delta.debit_available(&fill.taker, &market.quote, fill.taker_fee as u64, bal_base);
                }
                delta.debit_locked(&fill.maker, &market.quote, fill_notional, bal_base);
                delta.credit_available(&fill.maker, &market.base, fill.quantity, bal_base);
                if fill.maker_fee < 0 {
                    delta.credit_available(&fill.maker, &market.quote, fill.maker_fee.unsigned_abs(), bal_base);
                } else if fill.maker_fee > 0 {
                    delta.debit_available(&fill.maker, &market.quote, fill.maker_fee as u64, bal_base);
                }
            }
        }

        let net_exchange_fee = fill.taker_fee + fill.maker_fee;
        delta.add_exchange_fee(market.quote.as_str(), net_exchange_fee);

        if fill.taker_fee > 0 {
            let taker_meta = delta.get_metadata(&fill.taker, meta_base);
            if let Some(ref_hex) = taker_meta.ref_by {
                if let Ok(ref_user) = UserId::from_hex(&ref_hex) {
                    let referral_amount = (fill.taker_fee as u64) / 5;
                    if referral_amount > 0 {
                        delta.credit_available(&ref_user, &market.quote, referral_amount, bal_base);
                        delta.add_exchange_fee(market.quote.as_str(), -(referral_amount as i64));
                        let mut ref_meta = delta.get_metadata(&ref_user, meta_base);
                        ref_meta.ref_earnings = ref_meta.ref_earnings.saturating_add(referral_amount);
                        delta.set_metadata(ref_meta);
                    }
                }
            }
        }
    }

    fn process_cancel_order(&mut self, req: CancelOrderRequest) -> Vec<Response> {
        let mut meta = self.get_metadata(&req.user);
        if !meta.nonce_window.accept(req.nonce) {
            return vec![Response::Error(ErrorResponse {
                code: ErrorCode::InvalidNonce,
                message: "invalid nonce".into(),
            })];
        }

        let order_id = if let Some(id) = req.order_id {
            id
        } else if let Some(coid) = &req.client_order_id {
            let found = self.order_books.values().find_map(|b| b.find_by_client_order_id(&req.user, coid));
            match found {
                Some(id) => id,
                None => return vec![Response::Error(ErrorResponse {
                    code: ErrorCode::OrderNotFound,
                    message: "order not found".into(),
                })],
            }
        } else {
            return vec![Response::Error(ErrorResponse {
                code: ErrorCode::OrderNotFound,
                message: "must provide order_id or client_order_id".into(),
            })];
        };

        let removed = self.order_books.values_mut().find_map(|b| b.remove_order(order_id));

        match removed {
            Some(order) => {
                meta.remove_order_id(order_id);
                let market = self.markets.get(&order.market).cloned();
                if let Some(market) = market {
                    let (spend_asset, spend_amount) = match order.side {
                        OrderSide::Bid => (
                            market.quote,
                            CreditSystem::compute_notional(order.price, order.remaining_quantity()),
                        ),
                        OrderSide::Ask => (market.base, order.remaining_quantity()),
                    };
                    self.unlock_balance(&req.user, &spend_asset, spend_amount);
                }
                meta.total_quoted_notional = meta.total_quoted_notional
                    .saturating_sub(CreditSystem::compute_notional(order.price, order.remaining_quantity()));
                self.metadata.insert(req.user.clone(), meta);
                vec![Response::OrderCanceled(types::OrderCanceledResponse {
                    order_id,
                    client_order_id: order.client_order_id,
                })]
            }
            None => vec![Response::Error(ErrorResponse {
                code: ErrorCode::OrderNotFound,
                message: "order not found".into(),
            })],
        }
    }

    fn unlock_balance(&mut self, user: &UserId, asset: &AssetId, amount: u64) {
        let key = (user.clone(), *asset);
        let bal = self.balances.entry(key).or_insert_with(|| Balance {
            user: user.clone(),
            asset: *asset,
            available: 0,
            locked: 0,
        });
        bal.locked = bal.locked.saturating_sub(amount);
        bal.available += amount;
    }

    fn process_deposit(&mut self, req: DepositRequest) -> Vec<Response> {
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
        vec![Response::BalanceUpdated(types::BalanceUpdatedResponse {
            user: req.user,
            asset: req.asset,
            available: bal.available,
            locked: bal.locked,
        })]
    }

    fn process_withdrawal(&mut self, req: WithdrawalRequest) -> Vec<Response> {
        let mut meta = self.get_metadata(&req.user);
        if !meta.nonce_window.accept(req.nonce) {
            return vec![Response::Error(ErrorResponse {
                code: ErrorCode::InvalidNonce,
                message: "invalid nonce".into(),
            })];
        }
        let key = (req.user.clone(), req.asset);
        let bal = self.balances.entry(key).or_insert_with(|| Balance {
            user: req.user.clone(),
            asset: req.asset,
            available: 0,
            locked: 0,
        });
        if bal.available < req.amount {
            return vec![Response::Error(ErrorResponse {
                code: ErrorCode::InsufficientBalance,
                message: "insufficient available balance".into(),
            })];
        }
        bal.available -= req.amount;
        self.metadata.insert(req.user.clone(), meta);
        vec![Response::BalanceUpdated(types::BalanceUpdatedResponse {
            user: req.user,
            asset: req.asset,
            available: bal.available,
            locked: bal.locked,
        })]
    }

}

impl MatchingEngine {
    pub fn changed_balance_keys(&self) -> Vec<(UserId, AssetId)> {
        self.balances.keys().cloned().collect()
    }

    pub fn changed_metadata_keys(&self) -> Vec<UserId> {
        self.metadata.keys().cloned().collect()
    }

    pub fn snapshot_balances(&self) -> &EngineMap<(UserId, AssetId), Balance> {
        &self.balances
    }

    pub fn snapshot_metadata(&self) -> &EngineMap<UserId, UserMetadata> {
        &self.metadata
    }

    pub fn snapshot_fee_balances(&self) -> &EngineMap<String, u64> {
        &self.fee_balances
    }

    pub fn next_order_id(&self) -> OrderId {
        self.next_order_id
    }

    pub fn set_next_order_id(&mut self, id: OrderId) {
        self.next_order_id = id;
    }

    /// Inject shared balance/metadata snapshots for sharded dispatch.
    /// When set, the engine uses these as the base for all DeltaBuffer lookups
    /// instead of self.balances / self.metadata.
    pub fn set_snapshot(
        &mut self,
        balances: Option<Arc<EngineMap<(UserId, AssetId), Balance>>>,
        metadata: Option<Arc<EngineMap<UserId, UserMetadata>>>,
    ) {
        self.snapshot_balances = balances;
        self.snapshot_metadata = metadata;
    }

    /// Process a batch of requests under a single mutable borrow (i.e., under
    /// a single mutex acquisition at the caller level).
    ///
    /// Each request still gets its own internal [`DeltaBuffer`] so rollback
    /// semantics remain correct for failed orders (e.g., FOK not filled).
    /// The caller holds `&mut self` for the entire batch, which is the key
    /// optimisation: mutex lock cost is amortised across all N orders instead
    /// of paid N times.
    ///
    /// Returns one `Vec<Response>` per input request, in submission order.
    pub fn process_batch(&mut self, requests: Vec<(Request, Timestamp)>) -> Vec<Vec<Response>> {
        let ts = requests.first().map(|(_, ts)| *ts).unwrap_or(self.timestamp);
        self.credit_system.evict_expired_penalties(ts);
        requests
            .into_iter()
            .map(|(req, ts)| self.process(req, ts))
            .collect()
    }
}
