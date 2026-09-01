use std::collections::HashMap;
use types::{OrderId, Price, Quantity, UserId, VelaError, PRICE_SCALE, QUANTITY_SCALE};

/// Time-bounded credit multiplier applied to a maker after a toxic taker fill.
#[derive(Clone)]
pub struct CreditPenalty {
    /// Expiry in microseconds (same unit as engine timestamp).
    pub expires_at: u64,
    /// Multiplicative factor on the base credit ratio, e.g. 0.8.
    pub multiplier: f64,
}

#[derive(Clone)]
pub struct CreditSystem {
    credit_ratios: HashMap<UserId, f64>,
    default_ratio: f64,
    /// Active per-user credit penalties; cleared lazily when they expire.
    pub penalties: HashMap<UserId, CreditPenalty>,
}

impl CreditSystem {
    pub fn new(default_ratio: f64) -> Self {
        CreditSystem {
            credit_ratios: HashMap::new(),
            default_ratio,
            penalties: HashMap::new(),
        }
    }

    pub fn set_ratio(&mut self, user: UserId, ratio: f64) {
        self.credit_ratios.insert(user, ratio);
    }

    /// Base ratio without penalty consideration.
    pub fn ratio(&self, user: &UserId) -> f64 {
        *self.credit_ratios.get(user).unwrap_or(&self.default_ratio)
    }

    /// Effective ratio at `now_us`, applying any active credit penalty.
    pub fn effective_ratio(&self, user: &UserId, now_us: u64) -> f64 {
        let base = self.ratio(user);
        if let Some(p) = self.penalties.get(user) {
            if now_us < p.expires_at {
                return base * p.multiplier;
            }
        }
        base
    }

    /// Evict all penalty entries that have expired at `now_us`.
    /// Call periodically (e.g., from the batch dispatch loop) to bound map size
    /// for the process lifetime.
    pub fn evict_expired_penalties(&mut self, now_us: u64) {
        self.penalties.retain(|_, p| p.expires_at > now_us);
    }

    /// Apply a multiplicative credit penalty to `user` that expires after
    /// `duration_us` microseconds from `now_us`.
    /// Overwrites any existing penalty (resets the clock).
    /// Also evicts all already-expired penalties to bound map size.
    pub fn apply_penalty(&mut self, user: UserId, multiplier: f64, duration_us: u64, now_us: u64) {
        self.evict_expired_penalties(now_us);
        self.penalties.insert(
            user,
            CreditPenalty {
                expires_at: now_us.saturating_add(duration_us),
                multiplier,
            },
        );
    }

    pub fn compute_notional(price: Price, quantity: Quantity) -> u64 {
        let p = price as u128;
        let q = quantity as u128;
        ((p * q) / QUANTITY_SCALE as u128) as u64
    }

    pub fn check_credit(
        &self,
        user: &UserId,
        deposited: u64,
        current_quoted: u64,
        new_order_notional: u64,
        now_us: u64,
    ) -> Result<(), VelaError> {
        let ratio = self.effective_ratio(user, now_us);
        let max_quoted = (deposited as f64 * ratio) as u64;
        if current_quoted.saturating_add(new_order_notional) > max_quoted {
            return Err(VelaError::CreditLimitExceeded);
        }
        Ok(())
    }

    pub fn orders_to_cancel_after_fill(
        &self,
        user: &UserId,
        deposited: u64,
        current_quoted: u64,
        open_orders: &[(OrderId, u64)],
        now_us: u64,
    ) -> Vec<OrderId> {
        let ratio = self.effective_ratio(user, now_us);
        let max_quoted = (deposited as f64 * ratio) as u64;

        if current_quoted <= max_quoted {
            return vec![];
        }

        let mut to_cancel = vec![];
        let mut remaining_quoted = current_quoted;
        let mut sorted = open_orders.to_vec();
        sorted.sort_by_key(|(_, notional)| *notional);

        for (order_id, notional) in sorted {
            if remaining_quoted <= max_quoted {
                break;
            }
            to_cancel.push(order_id);
            remaining_quoted = remaining_quoted.saturating_sub(notional);
        }

        to_cancel
    }
}

pub fn compute_notional_f(price: Price, quantity: Quantity) -> f64 {
    (price as f64 * quantity as f64) / ((PRICE_SCALE as f64) * (QUANTITY_SCALE as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credit_multiplier_application() {
        let mut cs = CreditSystem::new(5.0);
        let user = UserId([0u8; 20]);
        let now_us: u64 = 1_000_000;

        // No penalty yet — effective ratio equals base ratio.
        assert!((cs.effective_ratio(&user, now_us) - 5.0).abs() < 1e-9);

        // Apply 0.8× penalty for 10 seconds.
        cs.apply_penalty(user.clone(), 0.8, 10_000_000, now_us);

        // During penalty window: effective ratio = 5.0 × 0.8 = 4.0.
        let mid = cs.effective_ratio(&user, now_us + 5_000_000);
        assert!((mid - 4.0).abs() < 1e-9, "penalized ratio: {mid}");

        // check_credit with penalty active rejects orders that would fit without penalty.
        let deposited: u64 = 1_000;
        // Without penalty: max_quoted = 1000 × 5 = 5000, so 4500 fits.
        assert!(cs
            .check_credit(&user, deposited, 0, 4_500, now_us + 11_000_000)
            .is_ok());
        // With penalty: max_quoted = 1000 × 4 = 4000, so 4500 does not fit.
        assert!(cs
            .check_credit(&user, deposited, 0, 4_500, now_us + 1_000_000)
            .is_err());

        // After penalty expires: effective ratio returns to base.
        let post = cs.effective_ratio(&user, now_us + 11_000_000);
        assert!((post - 5.0).abs() < 1e-9, "post-penalty ratio: {post}");
    }
}
