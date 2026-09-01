use crate::EngineMap;
use std::collections::{BTreeMap, VecDeque};
use types::{Order, OrderId, OrderSide, Price, Quantity, VelaError};

#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: Price,
    pub orders: VecDeque<Order>,
}

impl PriceLevel {
    pub fn new(price: Price) -> Self {
        PriceLevel {
            price,
            orders: VecDeque::new(),
        }
    }

    /// Full remaining quantity across all orders at this level.
    /// Includes iceberg hidden reserves. Used by matching-side code
    /// (top-of-book depth for toxicity scoring) that needs to see the
    /// real matchable size.
    pub fn total_quantity(&self) -> Quantity {
        self.orders.iter().map(|o| o.remaining_quantity()).sum()
    }

    /// Publicly-visible quantity across all orders at this level.
    /// Iceberg orders contribute only their `display_quantity`. Used by
    /// public book-depth queries (`depth_bids`/`depth_asks`) and
    /// WebSocket broadcasts.
    pub fn visible_quantity(&self) -> Quantity {
        self.orders.iter().map(|o| o.visible_quantity()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct OrderBook {
    pub market: types::MarketId,
    bids: BTreeMap<Price, PriceLevel>,
    asks: BTreeMap<Price, PriceLevel>,
    order_index: EngineMap<OrderId, (OrderSide, Price)>,
    client_order_id_index: EngineMap<(types::UserId, String), OrderId>,
    pub max_orders: usize,
    pub total_orders: usize,
}

impl OrderBook {
    pub fn new(market: types::MarketId, max_orders: usize) -> Self {
        OrderBook {
            market,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_index: EngineMap::default(),
            client_order_id_index: EngineMap::default(),
            max_orders,
            total_orders: 0,
        }
    }

    pub fn is_full(&self) -> bool {
        self.total_orders >= self.max_orders
    }

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    pub fn spread(&self) -> Option<Price> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        ask.checked_sub(bid)
    }

    pub fn would_match(&self, side: OrderSide, price: Price) -> bool {
        match side {
            OrderSide::Bid => self.best_ask().map(|a| price >= a).unwrap_or(false),
            OrderSide::Ask => self.best_bid().map(|b| price <= b).unwrap_or(false),
        }
    }

    pub fn insert_resting(&mut self, order: Order) -> Result<(), VelaError> {
        if self.is_full() {
            return Err(VelaError::OrderBookFull);
        }
        if let Some(coid) = &order.client_order_id {
            let key = (order.user.clone(), coid.clone());
            if self.client_order_id_index.contains_key(&key) {
                return Err(VelaError::DuplicateClientOrderId);
            }
            self.client_order_id_index.insert(key, order.id);
        }
        self.order_index.insert(order.id, (order.side, order.price));
        let levels = match order.side {
            OrderSide::Bid => &mut self.bids,
            OrderSide::Ask => &mut self.asks,
        };
        levels
            .entry(order.price)
            .or_insert_with(|| PriceLevel::new(order.price))
            .orders
            .push_back(order);
        self.total_orders += 1;
        Ok(())
    }

    pub fn remove_order(&mut self, order_id: OrderId) -> Option<Order> {
        let (side, price) = self.order_index.remove(&order_id)?;
        let levels = match side {
            OrderSide::Bid => &mut self.bids,
            OrderSide::Ask => &mut self.asks,
        };
        let level = levels.get_mut(&price)?;
        let pos = level.orders.iter().position(|o| o.id == order_id)?;
        let order = level.orders.remove(pos)?;
        if let Some(coid) = &order.client_order_id {
            self.client_order_id_index
                .remove(&(order.user.clone(), coid.clone()));
        }
        if level.is_empty() {
            levels.remove(&price);
        }
        self.total_orders = self.total_orders.saturating_sub(1);
        Some(order)
    }

    pub fn find_by_client_order_id(&self, user: &types::UserId, coid: &str) -> Option<OrderId> {
        self.client_order_id_index
            .get(&(user.clone(), coid.to_string()))
            .copied()
    }

    pub fn get_order(&self, order_id: OrderId) -> Option<&Order> {
        let (side, price) = self.order_index.get(&order_id)?;
        let levels = match side {
            OrderSide::Bid => &self.bids,
            OrderSide::Ask => &self.asks,
        };
        levels.get(price)?.orders.iter().find(|o| o.id == order_id)
    }

    pub fn best_asks_mut(&mut self) -> impl Iterator<Item = &mut PriceLevel> {
        self.asks.values_mut()
    }

    pub fn best_bids_mut(&mut self) -> impl Iterator<Item = &mut PriceLevel> {
        self.bids.values_mut().rev()
    }

    pub fn remove_empty_levels(&mut self) {
        self.bids.retain(|_, v| !v.is_empty());
        self.asks.retain(|_, v| !v.is_empty());
    }

    pub fn update_filled_quantity(&mut self, order_id: OrderId, additional_filled: Quantity) {
        let (side, price) = match self.order_index.get(&order_id) {
            Some(v) => *v,
            None => return,
        };
        let levels = match side {
            OrderSide::Bid => &mut self.bids,
            OrderSide::Ask => &mut self.asks,
        };
        if let Some(level) = levels.get_mut(&price) {
            if let Some(order) = level.orders.iter_mut().find(|o| o.id == order_id) {
                order.filled_quantity += additional_filled;
                if order.filled_quantity >= order.quantity {
                    order.status = types::OrderStatus::Filled;
                } else {
                    order.status = types::OrderStatus::PartiallyFilled;
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn matchable_asks(&self, bid_price: Price) -> Vec<(Price, Vec<Order>)> {
        self.asks
            .iter()
            .take_while(|(p, _)| **p <= bid_price)
            .map(|(p, l)| (*p, l.orders.iter().cloned().collect()))
            .collect()
    }

    #[allow(dead_code)]
    pub fn matchable_bids(&self, ask_price: Price) -> Vec<(Price, Vec<Order>)> {
        self.bids
            .iter()
            .rev()
            .take_while(|(p, _)| **p >= ask_price)
            .map(|(p, l)| (*p, l.orders.iter().cloned().collect()))
            .collect()
    }

    /// Zero-allocation variant: yields references into existing price levels.
    /// Used by the hot-path `match_order` to avoid cloning every resting Order.
    pub fn matchable_asks_ref(
        &self,
        bid_price: Price,
    ) -> impl Iterator<Item = (Price, &VecDeque<Order>)> {
        self.asks
            .iter()
            .take_while(move |(p, _)| **p <= bid_price)
            .map(|(p, l)| (*p, &l.orders))
    }

    pub fn matchable_bids_ref(
        &self,
        ask_price: Price,
    ) -> impl Iterator<Item = (Price, &VecDeque<Order>)> {
        self.bids
            .iter()
            .rev()
            .take_while(move |(p, _)| **p >= ask_price)
            .map(|(p, l)| (*p, &l.orders))
    }

    /// Public depth: sums `visible_quantity()` per order so iceberg
    /// hidden reserves are masked. Matching-side callers should use
    /// `top_bid_quantity`/`top_ask_quantity` (which use full remaining)
    /// so the matcher can walk hidden liquidity.
    pub fn depth_bids(&self, levels: usize) -> Vec<(Price, Quantity)> {
        self.bids
            .iter()
            .rev()
            .take(levels)
            .map(|(p, l)| (*p, l.visible_quantity()))
            .collect()
    }

    pub fn depth_asks(&self, levels: usize) -> Vec<(Price, Quantity)> {
        self.asks
            .iter()
            .take(levels)
            .map(|(p, l)| (*p, l.visible_quantity()))
            .collect()
    }

    /// Zero-allocation best-level quantity queries — used instead of depth_asks/bids(1)
    /// to avoid a Vec allocation on every match_order call.
    pub fn top_ask_quantity(&self) -> Quantity {
        self.asks
            .values()
            .next()
            .map(|l| l.total_quantity())
            .unwrap_or(0)
    }

    pub fn top_bid_quantity(&self) -> Quantity {
        self.bids
            .values()
            .next_back()
            .map(|l| l.total_quantity())
            .unwrap_or(0)
    }

    pub fn all_orders(&self) -> Vec<Order> {
        let mut orders = Vec::new();
        for level in self.bids.values() {
            orders.extend(level.orders.iter().cloned());
        }
        for level in self.asks.values() {
            orders.extend(level.orders.iter().cloned());
        }
        orders
    }
}
