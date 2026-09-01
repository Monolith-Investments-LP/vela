use engine::batch_dispatcher::BatchedRequest;
use types::{MarketId, OrderSide, OrderType, PostOrderRequest, Request, UserId};

static MARKETS: &[(&str, &str, f64)] = &[
    ("BTC-USDC", "bitcoin", 0.1),
    ("ETH-USDC", "ethereum", 0.5),
    ("SOL-USDC", "solana", 10.0),
    ("AVAX-USDC", "avalanche-2", 5.0),
    ("MATIC-USDC", "matic-network", 1000.0),
    ("LINK-USDC", "chainlink", 50.0),
    ("UNI-USDC", "uniswap", 50.0),
    ("ARB-USDC", "arbitrum", 500.0),
    ("OP-USDC", "optimism", 200.0),
    ("AAVE-USDC", "aave", 1.0),
    ("DOGE-USDC", "dogecoin", 10000.0),
    ("PEPE-USDC", "pepe", 50_000_000.0),
    ("WIF-USDC", "dogwifcoin", 500.0),
    ("JUP-USDC", "jupiter-exchange-solana", 1000.0),
    ("PENDLE-USDC", "pendle", 100.0),
    ("EIGEN-USDC", "eigenlayer", 200.0),
];

pub async fn run_mm_bot(order_tx: tokio::sync::mpsc::Sender<BatchedRequest>) {
    let client = reqwest::Client::new();
    let mut iteration: u64 = 0;

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

        let ids_str = MARKETS
            .iter()
            .map(|(_, id, _)| *id)
            .collect::<Vec<_>>()
            .join(",");

        let url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
            ids_str
        );

        let prices = match client.get(&url).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("MM bot: failed to parse CoinGecko response: {}", e);
                    continue;
                }
            },
            Err(e) => {
                tracing::warn!("MM bot: failed to fetch CoinGecko prices: {}", e);
                continue;
            }
        };

        let seed_user = UserId([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let mut nonce: u64 = 200_000 + iteration * 1_000;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64;

        for &(market_name, coingecko_id, base_size) in MARKETS {
            let usd_price = match prices
                .get(coingecko_id)
                .and_then(|v| v.get("usd"))
                .and_then(|v| v.as_f64())
            {
                Some(p) => p,
                None => {
                    tracing::warn!("MM bot: no price for {}", market_name);
                    continue;
                }
            };

            let mid = (usd_price * 1_000_000.0).round() as u64;
            let half_spread = mid * 5 / 10_000;
            let base_qty = (base_size * 1_000_000.0).round() as u64;

            let mut bid_price = mid.saturating_sub(half_spread);
            for i in 0u64..10 {
                let quantity = ((i % 5) + 3) * base_qty;
                let (responder, _resp_rx) = tokio::sync::oneshot::channel();
                let _ = order_tx
                    .send(BatchedRequest {
                        request: Request::PostOrder(PostOrderRequest {
                            user: seed_user.clone(),
                            market: MarketId(market_name.to_string()),
                            side: OrderSide::Bid,
                            order_type: OrderType::GoodTillCanceled,
                            price: bid_price,
                            quantity,
                            nonce,
                            client_order_id: None,
                            signature: vec![],
                            stp: Default::default(),
                            min_quantity: None,
                        }),
                        ts,
                        responder,
                        decryption_proof: None,
                    })
                    .await;
                nonce += 1;
                bid_price = bid_price * 9_995 / 10_000;
            }

            let mut ask_price = mid + half_spread;
            for i in 0u64..10 {
                let quantity = ((i % 5) + 3) * base_qty;
                let (responder, _resp_rx) = tokio::sync::oneshot::channel();
                let _ = order_tx
                    .send(BatchedRequest {
                        request: Request::PostOrder(PostOrderRequest {
                            user: seed_user.clone(),
                            market: MarketId(market_name.to_string()),
                            side: OrderSide::Ask,
                            order_type: OrderType::GoodTillCanceled,
                            price: ask_price,
                            quantity,
                            nonce,
                            client_order_id: None,
                            signature: vec![],
                            stp: Default::default(),
                            min_quantity: None,
                        }),
                        ts,
                        responder,
                        decryption_proof: None,
                    })
                    .await;
                nonce += 1;
                ask_price = ask_price * 10_005 / 10_000;
            }

            tracing::info!("MM bot: updated {} @ ${:.2}", market_name, usd_price);
        }

        iteration += 1;
    }
}
