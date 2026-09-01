//! Agent-flow toxicity tier.
//!
//! Extends the shipped hot-path toxicity scorer to output a per-agent
//! **tier** (green / amber / red) based on rolling toxicity averages.
//! The tier gates trading behavior:
//!
//! - **Green** (avg toxicity ≤ `VELA_TOX_AMBER_THRESHOLD`, default 0.3):
//!   unrestricted. Green flow is what makers want; green makers get a
//!   fee-tier bonus in a follow-up commit.
//! - **Amber** (avg ≤ `VELA_TOX_RED_THRESHOLD`, default 0.6):
//!   trading permitted but with an extra deterministic delay
//!   (`VELA_TOX_AMBER_EXTRA_BUMP_US`, default 1000 μs) added to the
//!   existing IEX-style speed bump. Amber flow still gets to trade;
//!   it just doesn't get to race quote cancellations.
//! - **Red** (avg > red threshold): new orders rejected until manual
//!   review. Trading resumes when the operator whitelists the address
//!   via the `POST /admin/agent-tier/clear` endpoint (which resets the
//!   tier to green for a configurable review window).
//!
//! Why this is agent-specific
//! ---------------------------
//! Human flow varies less in toxicity than agent flow. A retail user
//! doesn't run 2000 sniping loops per hour; a badly-tuned agent does.
//! Human-scale rate limits over-restrict humans while missing
//! agent-scale adverse selection. Per-address rolling toxicity + a
//! three-tier gate lets us underprice good agent flow and overprice
//! (or block) bad agent flow without touching the human-facing paths.
//!
//! v1 scope
//! --------
//! - Tier computation on-demand from `state.fills` (no persistent
//!   cache; recomputed per query). Fine at beta volumes.
//! - Red-tier block + amber-tier extra delay both apply to `post_order`.
//! - Fee-tier bonus for green makers is deferred to Tier 3.9-adjacent
//!   work so the tier system and the fee system integrate cleanly.
//! - Manual clear via admin token only; a signed appeal flow is v2.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

use crate::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToxicityTier {
    Green,
    Amber,
    Red,
}

impl ToxicityTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToxicityTier::Green => "green",
            ToxicityTier::Amber => "amber",
            ToxicityTier::Red => "red",
        }
    }
}

pub fn amber_threshold() -> f64 {
    std::env::var("VELA_TOX_AMBER_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.3)
}

pub fn red_threshold() -> f64 {
    std::env::var("VELA_TOX_RED_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.6)
}

pub fn amber_extra_bump_duration() -> Duration {
    let us: u64 = std::env::var("VELA_TOX_AMBER_EXTRA_BUMP_US")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000);
    Duration::from_micros(us)
}

/// Rolling 30-day toxicity aggregate for one address, drawn from
/// `state.fills`. Weighted by fill notional so a 1M-USDC toxic fill
/// counts more than a 1-USDC clean fill.
pub struct TierComputation {
    pub tier: ToxicityTier,
    pub avg_toxicity: f64,
    pub taker_fill_count: u64,
    pub cleared_until_ms: Option<u64>,
}

pub async fn compute_tier(state: &AppState, address_lower: &str) -> TierComputation {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff = now_ms.saturating_sub(30 * 24 * 60 * 60 * 1_000);

    // Operator-cleared addresses skip the red gate for their clearance
    // window; still report the underlying tier so the review trail
    // shows what was cleared.
    let cleared_until = state
        .agent_tier_clears
        .get(address_lower)
        .map(|e| *e.value());

    let fills = state.fills.lock().await;
    let mut weighted_sum = 0.0f64;
    let mut weight = 0.0f64;
    let mut count = 0u64;
    for f in fills.iter() {
        if f.timestamp < cutoff {
            continue;
        }
        // Only count fills where this address was the TAKER — that's
        // where the toxicity_score is meaningful (score attributes to
        // the taker's flow, not the maker's).
        if f.taker_address.to_ascii_lowercase() != address_lower {
            continue;
        }
        let notional = (f.price as f64 * f.quantity as f64) / 1_000_000_000_000.0;
        weighted_sum += f.toxicity_score * notional;
        weight += notional;
        count += 1;
    }
    let avg = if weight > 0.0 {
        weighted_sum / weight
    } else {
        0.0
    };

    let raw_tier = if avg > red_threshold() {
        ToxicityTier::Red
    } else if avg > amber_threshold() {
        ToxicityTier::Amber
    } else {
        ToxicityTier::Green
    };
    let effective_tier = match cleared_until {
        Some(t) if t > now_ms => ToxicityTier::Green,
        _ => raw_tier,
    };

    TierComputation {
        tier: effective_tier,
        avg_toxicity: avg,
        taker_fill_count: count,
        cleared_until_ms: cleared_until,
    }
}

/// Resolve a per-order additional speed-bump for an address based on
/// its tier. Green + Red both return `Duration::ZERO` (red is blocked
/// upstream, so no delay to apply). Amber returns the configured
/// amber extra bump.
pub async fn extra_bump_for(state: &Arc<AppState>, address_lower: &str) -> Duration {
    let tc = compute_tier(state, address_lower).await;
    if tc.tier == ToxicityTier::Amber {
        amber_extra_bump_duration()
    } else {
        Duration::ZERO
    }
}

/// Returns true iff the address is currently red-tier and orders
/// should be rejected.
pub async fn should_block(state: &Arc<AppState>, address_lower: &str) -> bool {
    compute_tier(state, address_lower).await.tier == ToxicityTier::Red
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_default_sensibly() {
        // Defaults are 0.3 and 0.6 per the module doc.
        assert!((amber_threshold() - 0.3).abs() < 1e-9);
        assert!((red_threshold() - 0.6).abs() < 1e-9);
        // Sanity: amber < red.
        assert!(amber_threshold() < red_threshold());
    }
}
