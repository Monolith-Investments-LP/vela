use crate::types::StoredFill;
use crate::AppState;
use rand::Rng;
use std::sync::Arc;

// (market_id, pyth_feed_id_hex, qty_min, qty_max) — quantities in native base units
static FEEDS: &[(&str, &str, f64, f64)] = &[
    (
        "BTC-USDC",
        "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
        0.001,
        0.05,
    ),
    (
        "ETH-USDC",
        "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace",
        0.01,
        1.0,
    ),
    (
        "SOL-USDC",
        "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
        1.0,
        50.0,
    ),
    (
        "AVAX-USDC",
        "93da3352f9f1d105fdfe4971cfa80e9dd777bfc5d0f683ebb6e1294b92137bb7",
        0.5,
        20.0,
    ),
    (
        "MATIC-USDC",
        "5de33a9112c2b700b8d30b8a3402c103578ccfa2765696471cc672bd5cf6ac52",
        100.0,
        5000.0,
    ),
    (
        "LINK-USDC",
        "8ac0c70fff57e9aefdf5edf44b51d62c2d433653cbb2cf5cc06bb115af04d221",
        5.0,
        200.0,
    ),
    (
        "UNI-USDC",
        "78d185a741d07edb3412b09008b7c5cfb9bbbd7d568bf00ba737b456ba171501",
        5.0,
        100.0,
    ),
    (
        "ARB-USDC",
        "3fa4252848f9f0a1480be62745a4629d9eb1322aebab8a791e344b3b9c1adcf5",
        100.0,
        5000.0,
    ),
    (
        "OP-USDC",
        "385f64d993f7b77d8182ed5003d97c60aa3361f3cecfe711544d2d59165e9bab",
        50.0,
        2000.0,
    ),
    (
        "AAVE-USDC",
        "2b9ab1e972a281585084148ba1389800799bd4be63b957507db1349314e47445",
        0.1,
        5.0,
    ),
    (
        "DOGE-USDC",
        "dcef50dd0a4cd2dcc17e45df1676dcb336a11a61c69df7a0299b0150c672d25c",
        500.0,
        20000.0,
    ),
];

pub async fn pyth_feed_task(state: Arc<AppState>) {
    let client = reqwest::Client::new();
    let ids_param: String = FEEDS
        .iter()
        .map(|(_, id, _, _)| format!("ids[]={}", id))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!(
        "https://hermes.pyth.network/v2/updates/price/latest?{}",
        ids_param
    );

    let mut prev_prices: std::collections::HashMap<&'static str, f64> =
        std::collections::HashMap::new();

    loop {
        let fetch_result = client.get(&url).send().await;

        match fetch_result {
            Err(e) => {
                tracing::warn!("pyth: HTTP request failed: {}", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                continue;
            }
            Ok(resp) => {
                let body: serde_json::Value = match resp.json().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("pyth: failed to parse response: {}", e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                // Build fills synchronously — ThreadRng is not Send, so it must be
                // fully dropped before the first .await below.
                let new_fills: Vec<StoredFill> = {
                    let now_us = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_micros() as u64;

                    let mut rng = rand::thread_rng();
                    let mut fills: Vec<StoredFill> = Vec::new();

                    for &(market_id, feed_id, qty_min, qty_max) in FEEDS {
                        let usd_price = match extract_price(&body, feed_id) {
                            Some(p) if p > 0.0 => p,
                            _ => {
                                tracing::debug!("pyth: no price for {}", market_id);
                                continue;
                            }
                        };

                        // Publish the observation to the process-wide
                        // oracle regardless of whether we emit synthetic
                        // fills for it — borrow-lend / portfolio-margin
                        // / perp mark-price rely on this being fresh.
                        let mark_micro_usdc = (usd_price * 1_000_000.0).round() as u128;
                        state
                            .oracle
                            .publish_from_market(market_id, mark_micro_usdc);

                        let prev = prev_prices.get(market_id).copied().unwrap_or(0.0);
                        // Only emit fills when price has moved by at least 0.001%.
                        if prev > 0.0 && (usd_price - prev).abs() / prev < 0.00001 {
                            continue;
                        }
                        prev_prices.insert(market_id, usd_price);

                        let num_fills: usize = rng.gen_range(1usize..=3);
                        for j in 0..num_fills {
                            let noise: f64 = rng.gen_range(-0.0002_f64..=0.0002_f64);
                            let fill_usd = usd_price * (1.0 + noise);
                            let stored_price = (fill_usd * 1_000_000.0).round() as u64;
                            let qty_base: f64 = rng.gen_range(qty_min..=qty_max);
                            let stored_qty = (qty_base * 1_000_000.0).round() as u64;
                            let side = if rng.gen_bool(0.5) { "buy" } else { "sell" };
                            // Offset each fill within the same tick by 1 ms to keep timestamps unique.
                            let fill_ts = now_us + j as u64 * 1_000;

                            fills.push(StoredFill {
                                id: format!("pyth-{}-{}-{}", market_id, fill_ts, j),
                                market_id: market_id.to_string(),
                                price: stored_price,
                                quantity: stored_qty,
                                maker_order_id: 0,
                                taker_order_id: 0,
                                maker_address: String::new(),
                                taker_address: String::new(),
                                timestamp: fill_ts,
                                side: side.to_string(),
                                synthetic: true,
                                toxicity_score: 0.0,
                            });
                        }
                    }
                    fills
                    // rng dropped here — before any .await
                };

                if !new_fills.is_empty() {
                    let mut fills = state.fills.lock().await;
                    for fill in new_fills {
                        let market = fill.market_id.clone();
                        fills.push(fill);
                        // Cap synthetic fills per market at 10 000 to bound memory.
                        let synth_count = fills
                            .iter()
                            .filter(|f| f.market_id == market && f.synthetic)
                            .count();
                        if synth_count > 10_000 {
                            if let Some(idx) = fills
                                .iter()
                                .position(|f| f.market_id == market && f.synthetic)
                            {
                                fills.remove(idx);
                            }
                        }
                    }
                }
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }
}

fn extract_price(body: &serde_json::Value, feed_id: &str) -> Option<f64> {
    let arr = body.get("parsed").and_then(|v| v.as_array())?;
    let entry = arr.iter().find(|e| {
        e.get("id")
            .and_then(|v| v.as_str())
            .map(|id| id.trim_start_matches("0x").eq_ignore_ascii_case(feed_id))
            .unwrap_or(false)
    })?;
    let price_obj = entry.get("price")?;
    let raw: i64 = price_obj.get("price").and_then(|v| {
        v.as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v.as_i64())
    })?;
    let expo: i32 = price_obj.get("expo").and_then(|v| v.as_i64()).unwrap_or(-8) as i32;
    Some(raw as f64 * 10_f64.powi(expo))
}
