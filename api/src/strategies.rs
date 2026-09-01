//! Strategy contracts (agent copy-trading).
//!
//! A **strategy** is a published trading identity that other users
//! (agents or humans) can subscribe to. When the strategy owner sends
//! a signed order, Vela mirrors it into follower accounts scaled by
//! each follower's subscription ratio. Followers keep custody of their
//! own funds; the strategy owner never touches follower balances.
//!
//! Why on-venue
//! ------------
//! Copy-trading today happens on centralized wrappers (eToro, Bybit
//! Copy, dYdX's third-party integrations) that add trust-you-the-
//! platform on top of trust-the-strategist. On Vela the strategy owner
//! signs each intent, followers sign each subscription, and every
//! mirrored order is an ordinary signed order attributed to the
//! follower — so the chain of custody is entirely inside the standard
//! auth model. The strategy owner cannot pull funds, cannot skim, and
//! cannot silently change the strategy identity.
//!
//! v1 mechanics
//! ------------
//! - Owner publishes: `POST /strategies/publish` — signed. Returns a
//!   `strategy_id`. Strategy is discoverable via `GET /strategies`.
//! - Follower subscribes: `POST /strategies/:strategy_id/subscribe` —
//!   signed by the follower with an `allocation_bps` (e.g. 500 = 5% of
//!   the strategy's notional). Optional `max_notional_micro_usdc` per
//!   trade cap. Optional `expires_at_ms` unsubscribe deadline.
//! - Follower unsubscribes any time via `POST /strategies/.../unsub`.
//! - Owner's order flow: the owner submits normal `POST /orders` with
//!   the strategy_id tagged in the client_order_id prefix
//!   (`strat_<id>_<user_ref>`). A dispatch fanout task (v1 skeleton
//!   below) reads each fill on the owner's account, computes each
//!   follower's mirrored quantity, and enqueues follower orders. The
//!   actual dispatcher hook lives in the fills-watcher wiring and is
//!   a follow-up; this module owns the subscription state, signing
//!   schemas, and mirror-math.
//!
//! Deferred
//! --------
//! - Profit-share fees (owner takes N bps of follower P&L).
//!   Complicated by per-follower cost basis; needs its own module.
//! - Per-market subscription filters (follow only BTC-USDC, ignore
//!   others).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::auth::verify_matches_async;
use crate::types::ApiResponse;
use crate::AppState;

pub static NEXT_STRATEGY_ID: AtomicU64 = AtomicU64::new(1);

fn next_strategy_id() -> u64 {
    NEXT_STRATEGY_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub strategy_id: u64,
    pub owner: String,
    pub name: String,
    /// Short human-facing description shown on `GET /strategies`.
    pub description: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub strategy_id: u64,
    pub follower: String,
    /// Follower notional as a fraction of the strategy owner's notional,
    /// in bps. 10_000 = follow 1:1. Must be > 0.
    pub allocation_bps: u16,
    /// Optional per-trade cap in micro-USDC. If a mirrored order would
    /// exceed this, the mirror is skipped for that trade only.
    #[serde(default)]
    pub max_notional_micro_usdc: Option<u64>,
    /// Optional wall-clock ms after which the subscription auto-expires.
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    pub subscribed_at_ms: u64,
}

pub struct StrategyRegistry {
    pub strategies: DashMap<u64, Strategy>,
    /// (strategy_id, follower_lower) → subscription. Compound key so we
    /// can iterate followers of a given strategy cheaply.
    pub subscriptions: DashMap<(u64, String), Subscription>,
}

impl StrategyRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            strategies: DashMap::new(),
            subscriptions: DashMap::new(),
        })
    }

    pub fn followers_of(&self, strategy_id: u64) -> Vec<Subscription> {
        self.subscriptions
            .iter()
            .filter(|entry| entry.key().0 == strategy_id)
            .map(|entry| entry.value().clone())
            .collect()
    }
}

pub fn publish_message(owner: &str, name: &str, nonce: u64) -> String {
    format!("vela:strategy:publish:{owner}:{name}:{nonce}")
}

pub fn subscribe_message(
    strategy_id: u64,
    follower: &str,
    allocation_bps: u16,
    nonce: u64,
) -> String {
    format!("vela:strategy:subscribe:{strategy_id}:{follower}:{allocation_bps}:{nonce}")
}

pub fn unsubscribe_message(strategy_id: u64, follower: &str, nonce: u64) -> String {
    format!("vela:strategy:unsubscribe:{strategy_id}:{follower}:{nonce}")
}

/// Compute the mirrored quantity for one follower on a given owner
/// trade. Returns None if the resulting quantity would round to zero or
/// would exceed the follower's per-trade cap.
pub fn mirror_quantity(
    owner_quantity: u64,
    owner_price_micro_usdc: u64,
    sub: &Subscription,
) -> Option<u64> {
    if sub.allocation_bps == 0 {
        return None;
    }
    let scaled = ((owner_quantity as u128) * (sub.allocation_bps as u128) + 5_000) / 10_000;
    if scaled == 0 {
        return None;
    }
    if let Some(cap) = sub.max_notional_micro_usdc {
        // Notional in micro-USDC = qty * price / 1e6 (both are
        // fixed-point-6). Integer-safe form:
        let notional = (scaled * owner_price_micro_usdc as u128) / 1_000_000;
        if notional > cap as u128 {
            return None;
        }
    }
    Some(scaled as u64)
}

// ---------- HTTP handlers ----------

#[derive(Debug, Clone, Deserialize)]
pub struct PublishBody {
    pub owner: String,
    pub signature: String,
    pub name: String,
    pub description: String,
    pub nonce: u64,
}

pub async fn publish_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PublishBody>,
) -> axum::response::Response {
    if body.name.trim().is_empty() || body.name.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "name must be 1..=64 non-whitespace bytes",
            )),
        )
            .into_response();
    }
    if body.description.len() > 512 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err("description must be ≤ 512 bytes")),
        )
            .into_response();
    }

    let msg = publish_message(&body.owner, &body.name, body.nonce);
    if verify_matches_async(msg.into_bytes(), body.signature.clone(), body.owner.clone())
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("signature verification failed")),
        )
            .into_response();
    }

    let strategy_id = next_strategy_id();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let s = Strategy {
        strategy_id,
        owner: body.owner.to_ascii_lowercase(),
        name: body.name,
        description: body.description,
        created_at_ms: now_ms,
    };
    state.strategies.strategies.insert(strategy_id, s.clone());
    tracing::info!(
        target: "strategy",
        strategy_id,
        owner = %s.owner,
        "strategy published"
    );
    (StatusCode::OK, Json(ApiResponse::ok(s))).into_response()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubscribeBody {
    pub follower: String,
    pub signature: String,
    pub allocation_bps: u16,
    #[serde(default)]
    pub max_notional_micro_usdc: Option<u64>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    pub nonce: u64,
}

pub async fn subscribe_handler(
    State(state): State<Arc<AppState>>,
    Path(strategy_id): Path<u64>,
    Json(body): Json<SubscribeBody>,
) -> axum::response::Response {
    if body.allocation_bps == 0 || body.allocation_bps > 10_000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiResponse::<()>::err(
                "allocation_bps must be in (0, 10_000]",
            )),
        )
            .into_response();
    }
    if !state.strategies.strategies.contains_key(&strategy_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("strategy_id not found")),
        )
            .into_response();
    }
    let msg = subscribe_message(strategy_id, &body.follower, body.allocation_bps, body.nonce);
    if verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.follower.clone(),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("signature verification failed")),
        )
            .into_response();
    }
    let follower_lower = body.follower.to_ascii_lowercase();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let sub = Subscription {
        strategy_id,
        follower: follower_lower.clone(),
        allocation_bps: body.allocation_bps,
        max_notional_micro_usdc: body.max_notional_micro_usdc,
        expires_at_ms: body.expires_at_ms,
        subscribed_at_ms: now_ms,
    };
    state
        .strategies
        .subscriptions
        .insert((strategy_id, follower_lower.clone()), sub.clone());
    tracing::info!(
        target: "strategy",
        strategy_id,
        follower = %follower_lower,
        allocation_bps = body.allocation_bps,
        "strategy subscription opened"
    );
    (StatusCode::OK, Json(ApiResponse::ok(sub))).into_response()
}

#[derive(Debug, Clone, Deserialize)]
pub struct UnsubscribeBody {
    pub follower: String,
    pub signature: String,
    pub nonce: u64,
}

pub async fn unsubscribe_handler(
    State(state): State<Arc<AppState>>,
    Path(strategy_id): Path<u64>,
    Json(body): Json<UnsubscribeBody>,
) -> axum::response::Response {
    let msg = unsubscribe_message(strategy_id, &body.follower, body.nonce);
    if verify_matches_async(
        msg.into_bytes(),
        body.signature.clone(),
        body.follower.clone(),
    )
    .await
    .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ApiResponse::<()>::err("signature verification failed")),
        )
            .into_response();
    }
    let follower_lower = body.follower.to_ascii_lowercase();
    let removed = state
        .strategies
        .subscriptions
        .remove(&(strategy_id, follower_lower.clone()));
    match removed {
        Some((_, sub)) => {
            tracing::info!(
                target: "strategy",
                strategy_id,
                follower = %follower_lower,
                "strategy subscription closed"
            );
            (StatusCode::OK, Json(ApiResponse::ok(sub))).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("no matching subscription")),
        )
            .into_response(),
    }
}

pub async fn list_handler(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let all: Vec<Strategy> = state
        .strategies
        .strategies
        .iter()
        .map(|e| e.value().clone())
        .collect();
    (StatusCode::OK, Json(ApiResponse::ok(all))).into_response()
}

pub async fn get_handler(
    State(state): State<Arc<AppState>>,
    Path(strategy_id): Path<u64>,
) -> axum::response::Response {
    match state.strategies.strategies.get(&strategy_id) {
        Some(s) => {
            let s = s.value().clone();
            let followers = state.strategies.followers_of(strategy_id);
            let payload = serde_json::json!({
                "strategy": s,
                "follower_count": followers.len(),
                "total_allocation_bps": followers
                    .iter()
                    .map(|f| f.allocation_bps as u64)
                    .sum::<u64>(),
            });
            (StatusCode::OK, Json(ApiResponse::ok(payload))).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::err("strategy_id not found")),
        )
            .into_response(),
    }
}

pub async fn list_subscriptions_handler(
    State(state): State<Arc<AppState>>,
    Path(strategy_id): Path<u64>,
) -> axum::response::Response {
    let subs = state.strategies.followers_of(strategy_id);
    (StatusCode::OK, Json(ApiResponse::ok(subs))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub_with(alloc_bps: u16, cap: Option<u64>) -> Subscription {
        Subscription {
            strategy_id: 1,
            follower: "0xabc".to_string(),
            allocation_bps: alloc_bps,
            max_notional_micro_usdc: cap,
            expires_at_ms: None,
            subscribed_at_ms: 0,
        }
    }

    #[test]
    fn mirror_scales_by_bps() {
        let s = sub_with(2_500, None);
        // 100 * 2500 / 10000 = 25
        assert_eq!(mirror_quantity(100, 1_000_000, &s), Some(25));
    }

    #[test]
    fn mirror_rounds_to_nearest() {
        let s = sub_with(3_333, None);
        // 100 * 3333 / 10000 = 33.33 → 33 with round-half-up bias +0.5
        assert_eq!(mirror_quantity(100, 1_000_000, &s), Some(33));
    }

    #[test]
    fn mirror_returns_none_on_zero_qty() {
        let s = sub_with(1, None);
        assert_eq!(mirror_quantity(1, 1_000_000, &s), None);
    }

    #[test]
    fn mirror_respects_notional_cap() {
        // qty 100 * price 5_000_000 (== $5) → notional = 500 μUSDC.
        // With allocation 100% and cap 400 μUSDC, mirror should be
        // skipped.
        let s = sub_with(10_000, Some(400));
        assert_eq!(mirror_quantity(100, 5_000_000, &s), None);
        // Cap 1_000 μUSDC → mirror passes at 100 qty.
        let s = sub_with(10_000, Some(1_000));
        assert_eq!(mirror_quantity(100, 5_000_000, &s), Some(100));
    }

    #[test]
    fn publish_message_is_stable() {
        assert_eq!(
            publish_message("0xabc", "delta-neutral-v1", 7),
            "vela:strategy:publish:0xabc:delta-neutral-v1:7"
        );
    }

    #[test]
    fn subscribe_message_is_stable() {
        assert_eq!(
            subscribe_message(42, "0xfollower", 500, 3),
            "vela:strategy:subscribe:42:0xfollower:500:3"
        );
    }
}
