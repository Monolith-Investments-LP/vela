use std::sync::Arc;
use engine::MatchingEngine;
use types::{
    AssetId, DepositRequest, FeeConfig, Market, MarketId, OrderSide, OrderType, PostOrderRequest,
    Request, UserId,
};
use api::types::Incident;
use api::wal::{Wal, WalEngineStart, WalEngineStop};

fn seed_markets(engine: &mut MatchingEngine) {
    for ticker in &[
        "ETH", "BTC", "SOL", "ARB", "OP", "AVAX", "MATIC", "LINK", "UNI", "AAVE", "DOGE",
        "PEPE", "WIF", "JUP", "PENDLE", "EIGEN",
    ] {
        engine.add_market(Market {
            id: MarketId(format!("{ticker}-USDC")),
            base: AssetId::from_str(ticker),
            quote: AssetId::from_str("USDC"),
            max_orders: 10_000,
            min_order_size: 1,
            price_tick: 1,
            quantity_tick: 1,
            maker_fee_bps: -1,
            taker_fee_bps: 5,
        });
    }

    let test_user = UserId([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

    for (i, asset) in [
        "ETH", "BTC", "SOL", "ARB", "OP", "AVAX", "MATIC", "LINK", "UNI", "AAVE", "DOGE",
        "PEPE", "WIF", "JUP", "PENDLE", "EIGEN", "USDC",
    ]
    .iter()
    .enumerate()
    {
        let mut hash = [0u8; 32];
        hash[31] = i as u8;

        engine.process(
            Request::Deposit(DepositRequest {
                user: test_user.clone(),
                asset: AssetId::from_str(asset),
                amount: 1_000_000_000_000,
                l1_tx_hash: hash,
            }),
            0,
        );
    }
}

fn seed_order_books(engine: &mut MatchingEngine) {
    let seed_user = UserId([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

    let make_hash = |a: u8, b: u8| -> [u8; 32] {
        let mut h = [0u8; 32];
        h[0] = 2;
        h[30] = a;
        h[31] = b;
        h
    };

    engine.process(
        Request::Deposit(DepositRequest {
            user: seed_user.clone(),
            asset: AssetId::from_str("USDC"),
            amount: 10_000_000_000_000_000,
            l1_tx_hash: make_hash(0, 0),
        }),
        0,
    );

    for (i, asset) in [
        "BTC", "ETH", "SOL", "AVAX", "MATIC", "LINK", "UNI", "ARB", "OP", "AAVE", "DOGE",
        "PEPE", "WIF", "JUP", "PENDLE", "EIGEN",
    ]
    .iter()
    .enumerate()
    {
        engine.process(
            Request::Deposit(DepositRequest {
                user: seed_user.clone(),
                asset: AssetId::from_str(asset),
                amount: 100_000_000_000_000,
                l1_tx_hash: make_hash(1, i as u8),
            }),
            0,
        );
    }

    let markets: &[(&str, u64, u64)] = &[
        ("BTC-USDC", 94_000 * 100_000_000, 100_000),
        ("ETH-USDC", 3_200 * 100_000_000, 500_000),
        ("SOL-USDC", 145 * 100_000_000, 10_000_000),
        ("AVAX-USDC", 35 * 100_000_000, 5_000_000),
        ("MATIC-USDC", 850_000, 1_000_000_000),
        ("LINK-USDC", 14 * 100_000_000, 50_000_000),
        ("UNI-USDC", 9 * 100_000_000, 50_000_000),
        ("ARB-USDC", 1_100_000, 500_000_000),
        ("OP-USDC", 2_400_000, 200_000_000),
        ("AAVE-USDC", 280 * 100_000_000, 1_000_000),
        ("DOGE-USDC", 180_000, 10_000_000_000),
        ("PEPE-USDC", 8, 50_000_000_000_000),
        ("WIF-USDC", 1_200_000, 500_000_000),
        ("JUP-USDC", 500_000, 1_000_000_000),
        ("PENDLE-USDC", 3_500_000, 100_000_000),
        ("EIGEN-USDC", 2_000_000, 200_000_000),
    ];

    let mut nonce: u64 = 1000;

    for &(market_name, mid, base_size) in markets {
        let half_spread = mid * 5 / 10_000;
        let best_bid = mid - half_spread;
        let best_ask = mid + half_spread;

        let mut bid_price = best_bid;
        for i in 0u64..20 {
            let quantity = ((i % 5) + 3) * base_size;
            engine.process(
                Request::PostOrder(PostOrderRequest {
                    user: seed_user.clone(),
                    market: MarketId(market_name.to_string()),
                    side: OrderSide::Bid,
                    order_type: OrderType::GoodTillCanceled,
                    price: bid_price,
                    quantity,
                    nonce,
                    client_order_id: None,
                    signature: vec![],
                }),
                0,
            );
            nonce += 1;
            bid_price = bid_price * 9_995 / 10_000;
        }

        let mut ask_price = best_ask;
        for i in 0u64..20 {
            let quantity = ((i % 5) + 3) * base_size;
            engine.process(
                Request::PostOrder(PostOrderRequest {
                    user: seed_user.clone(),
                    market: MarketId(market_name.to_string()),
                    side: OrderSide::Ask,
                    order_type: OrderType::GoodTillCanceled,
                    price: ask_price,
                    quantity,
                    nonce,
                    client_order_id: None,
                    signature: vec![],
                }),
                0,
            );
            nonce += 1;
            ask_price = ask_price * 10_005 / 10_000;
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3001);

    let wal_dir = std::path::PathBuf::from(
        std::env::var("SNAPSHOT_DIR").unwrap_or_else(|_| "/data".to_string()),
    );

    let wal = Arc::new(match Wal::new(&wal_dir) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("WAL init failed: {e} — continuing without WAL");
            Wal::new(&std::env::temp_dir().join("vela_wal_fallback")).unwrap_or_else(|_| {
                panic!("Cannot initialize WAL even in temp dir")
            })
        }
    });

    let was_clean = Wal::was_clean_shutdown_sync(&wal_dir);
    let checkpoint_seq = if !was_clean && Wal::wal_files_exist(&wal_dir) {
        Wal::find_checkpoint_sequence_sync(&wal_dir)
    } else {
        0
    };

    let mut engine = MatchingEngine::new(FeeConfig::default(), 5.0);

    let mut loaded_incidents: Vec<Incident> = Vec::new();
    let mut loaded_decisions: Vec<api::types::Decision> = Vec::new();
    let mut loaded_mms: Vec<api::types::RegisteredMM> = Vec::new();
    let mut loaded_proofs: std::collections::HashMap<u64, zkvm::BatchProof> = std::collections::HashMap::new();
    let mut loaded_attestations: std::collections::HashMap<u64, tee::AttestationRecord> = std::collections::HashMap::new();
    let mut need_restart_incident = false;

    match api::snapshot::load_snapshot().await {
        Ok(Some(snapshot)) => {
            if !snapshot.clean_shutdown {
                need_restart_incident = true;
            }
            loaded_incidents = snapshot.incidents.clone();
            loaded_decisions = snapshot.decisions.clone();
            loaded_mms = snapshot.registered_mms.clone();
            loaded_proofs = api::snapshot::extract_proofs_from_snapshot(&snapshot);
            loaded_attestations = api::snapshot::extract_attestations_from_snapshot(&snapshot);
            match api::snapshot::restore_engine_from_snapshot(&mut engine, snapshot) {
                Ok(()) => println!("Restored from snapshot"),
                Err(e) => {
                    eprintln!("Snapshot restore failed: {e} — starting fresh");
                    engine = MatchingEngine::new(FeeConfig::default(), 5.0);
                    seed_markets(&mut engine);
                    seed_order_books(&mut engine);
                    loaded_incidents.clear();
                    loaded_decisions.clear();
                    loaded_mms.clear();
                    loaded_proofs.clear();
                    loaded_attestations.clear();
                    need_restart_incident = false;
                }
            }
        }
        Ok(None) => {
            seed_markets(&mut engine);
            seed_order_books(&mut engine);
            println!("Fresh start — seeded markets");
        }
        Err(e) => {
            eprintln!("Snapshot load failed: {e} — starting fresh");
            seed_markets(&mut engine);
            seed_order_books(&mut engine);
        }
    }

    let (recovery_reason, recovered_fills, recovered_orders) =
        if !was_clean && Wal::wal_files_exist(&wal_dir) {
            let (fills, orders) =
                api::snapshot::replay_wal_entries(&mut engine, &wal_dir, checkpoint_seq);
            tracing::info!(
                "WAL replay complete: replayed entries, recovered {} orders {} fills",
                orders.len(),
                fills.len()
            );
            ("crash_recovery", fills, orders)
        } else {
            ("clean", vec![], std::collections::HashMap::new())
        };

    let previous_sequence = wal.current_sequence();

    let state = api::AppState::new(engine, Arc::clone(&wal));

    {
        *state.incidents.lock().await = loaded_incidents;
        *state.decisions.lock().await = loaded_decisions;
        *state.registered_mms.lock().await = loaded_mms;
        *state.proofs.lock().await = loaded_proofs;
        *state.attestations.lock().await = loaded_attestations;
    }

    if !recovered_fills.is_empty() || !recovered_orders.is_empty() {
        *state.fills.lock().await = recovered_fills;
        *state.stored_orders.lock().await = recovered_orders;
    }

    if need_restart_incident {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut incidents = state.incidents.lock().await;
        let next_id = incidents.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        incidents.push(Incident {
            id: next_id,
            incident_type: "RESTART".to_string(),
            started_at: now_ms.saturating_sub(1000),
            resolved_at: Some(now_ms),
            description: "Engine restarted. State restored from snapshot.".to_string(),
            impact: "Brief interruption. All state preserved.".to_string(),
        });
    }

    wal.append(api::wal::ENGINE_START, &WalEngineStart {
        version: "0.2.0".to_string(),
        reason: recovery_reason.to_string(),
        previous_sequence,
    })
    .await
    .unwrap_or_else(|e| { tracing::error!("WAL ENGINE_START write failed: {e}"); 0 });

    if let Some((anchors, count)) = api::anchor::load_anchors().await {
        let last = anchors.last().map(|a| (a.tx_hash.clone(), a.timestamp));
        *state.anchors.lock().await = anchors;
        state.anchor_count.store(count, std::sync::atomic::Ordering::Relaxed);
        if let Some((tx, ts)) = last {
            *state.last_anchor_tx.lock().await = Some(tx);
            state.last_anchor_time.store(ts, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let alchemy_url = std::env::var("ALCHEMY_API_URL")
        .unwrap_or_else(|_| "https://eth-sepolia.g.alchemy.com/v2/demo".to_string());
    let operator_key = std::env::var("OPERATOR_PRIVATE_KEY").unwrap_or_default();

    if !operator_key.is_empty() {
        let anchor_state = Arc::clone(&state);
        tokio::spawn(async move {
            api::anchor::anchor_task(anchor_state, alchemy_url, operator_key).await;
        });
    } else {
        tracing::warn!("OPERATOR_PRIVATE_KEY not set — anchor task disabled");
    }

    let mm_order_tx = state.order_tx.clone();
    tokio::spawn(async move {
        api::mm::run_mm_bot(mm_order_tx).await;
    });

    let snapshot_state = Arc::clone(&state);
    tokio::spawn(async move {
        api::snapshot::run_snapshot_task(snapshot_state).await;
    });

    let pyth_enabled = std::env::var("VELA_PYTH_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    if pyth_enabled {
        let pyth_state = Arc::clone(&state);
        tokio::spawn(async move {
            api::pyth::pyth_feed_task(pyth_state).await;
        });
    } else {
        tracing::info!("VELA_PYTH_ENABLED=false — Pyth feed task disabled");
    }

    let router = api::build_router(state);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("listening on {addr}");

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        result = axum::serve(listener, router) => {
            if let Err(e) = result {
                eprintln!("Server error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("SIGINT received, shutting down");
        }
        _ = terminate => {
            tracing::info!("SIGTERM received, shutting down");
        }
    }

    wal.append(api::wal::ENGINE_STOP, &WalEngineStop {
        reason: "clean".to_string(),
        final_sequence: wal.current_sequence(),
    })
    .await
    .unwrap_or_else(|e| { tracing::error!("WAL ENGINE_STOP write failed: {e}"); 0 });
}
