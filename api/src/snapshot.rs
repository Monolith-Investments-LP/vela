use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use anyhow::Result;
use engine::MatchingEngine;
use types::{AssetId, Balance, DepositRequest, Market, Order, UserId, UserMetadata, WithdrawalRequest};
use crate::types::{Decision, Incident, OrderFillRecord, RegisteredMM, StoredFill, StoredOrder};
use crate::wal::{Wal, WalCheckpoint, WalDeposit, WalFillCreated, WalOrderPost, WalWithdrawalRequest};
use crate::wal;
use zkvm::BatchProof;
use tee::AttestationRecord;

const SNAPSHOT_INTERVAL_SECS: u64 = 60;

fn snapshot_path() -> String {
    let dir = std::env::var("SNAPSHOT_DIR").unwrap_or_else(|_| "/data".to_string());
    format!("{dir}/engine_snapshot.json")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SerializedBalance {
    pub available: u64,
    pub locked: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EngineSnapshot {
    pub version: u32,
    pub timestamp: u64,
    pub balances: HashMap<String, HashMap<String, SerializedBalance>>,
    pub orders: HashMap<String, Vec<Order>>,
    pub markets: Vec<Market>,
    pub sequence: u64,
    pub metadata: HashMap<String, UserMetadata>,
    #[serde(default)]
    pub fee_balances: HashMap<String, u64>,
    #[serde(default)]
    pub clean_shutdown: bool,
    #[serde(default)]
    pub incidents: Vec<Incident>,
    #[serde(default)]
    pub decisions: Vec<Decision>,
    #[serde(default)]
    pub registered_mms: Vec<RegisteredMM>,
    #[serde(default)]
    pub proofs: HashMap<u64, BatchProof>,
    #[serde(default)]
    pub attestations: HashMap<u64, AttestationRecord>,
}

pub async fn save_snapshot(state: Arc<crate::AppState>, clean_shutdown: bool) -> Result<()> {
    let (timestamp, balances, orders, markets, sequence, metadata, fee_balances) = {
        let engine = state.engine.lock().await;

        let mut balances: HashMap<String, HashMap<String, SerializedBalance>> = HashMap::new();
        for ((user, asset), balance) in &engine.balances {
            balances
                .entry(user.to_hex())
                .or_default()
                .insert(asset.as_str().to_string(), SerializedBalance { available: balance.available, locked: balance.locked });
        }

        let mut orders: HashMap<String, Vec<Order>> = HashMap::new();
        for (market_id, book) in &engine.order_books {
            orders.insert(market_id.0.clone(), book.all_orders());
        }

        let markets: Vec<Market> = engine.markets.values().cloned().collect();

        let mut metadata: HashMap<String, UserMetadata> = HashMap::new();
        for (user, meta) in &engine.metadata {
            metadata.insert(user.to_hex(), meta.clone());
        }

        let fee_balances = engine.fee_balances.clone();
        let timestamp = engine.timestamp;
        let sequence = engine.next_order_id();

        (timestamp, balances, orders, markets, sequence, metadata, fee_balances)
    };

    let n_orders: usize = orders.values().map(|v| v.len()).sum();
    let n_users = balances.len();
    let n_fills = state.fills.lock().await.len();

    let incidents = state.incidents.lock().await.clone();
    let decisions = state.decisions.lock().await.clone();
    let registered_mms = state.registered_mms.lock().await.clone();
    let proofs = state.proofs.lock().await.clone();
    let attestations = state.attestations.lock().await.clone();

    let snapshot = EngineSnapshot {
        version: 1,
        timestamp,
        balances,
        orders,
        markets,
        sequence,
        metadata,
        fee_balances,
        clean_shutdown,
        incidents,
        decisions,
        registered_mms,
        proofs,
        attestations,
    };

    let json = serde_json::to_vec(&snapshot)?;

    let path = snapshot_path();
    let dir = std::path::Path::new(&path)
        .parent()
        .unwrap_or(std::path::Path::new("/data"));
    tokio::fs::create_dir_all(dir).await?;

    let tmp_path = format!("{path}.tmp");
    tokio::fs::write(&tmp_path, &json).await?;
    tokio::fs::rename(&tmp_path, &path).await?;

    tracing::info!("Snapshot saved: {n_orders} orders, {n_users} users");

    let checkpoint = WalCheckpoint {
        snapshot_path: path,
        total_orders: n_orders as u64,
        total_fills: n_fills as u64,
        total_users: n_users as u64,
    };
    if let Err(e) = state.wal.append(wal::CHECKPOINT, &checkpoint).await {
        tracing::error!("WAL CHECKPOINT write failed: {e}");
    } else if let Err(e) = state.wal.rotate().await {
        tracing::error!("WAL rotate failed: {e}");
    }

    Ok(())
}

pub async fn load_snapshot() -> Result<Option<EngineSnapshot>> {
    let path = snapshot_path();
    match tokio::fs::try_exists(&path).await {
        Ok(true) => {}
        _ => return Ok(None),
    }
    let data = tokio::fs::read(&path).await?;
    let snapshot = serde_json::from_slice(&data)?;
    Ok(Some(snapshot))
}

pub async fn run_snapshot_task(state: Arc<crate::AppState>) {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(SNAPSHOT_INTERVAL_SECS)).await;
        if let Err(e) = save_snapshot(Arc::clone(&state), false).await {
            tracing::error!("Snapshot error: {e}");
        } else {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            state.last_snapshot_ts.store(ts, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

pub fn restore_engine_from_snapshot(
    engine: &mut MatchingEngine,
    snapshot: EngineSnapshot,
) -> Result<()> {
    for market in snapshot.markets {
        engine.add_market(market);
    }

    for (user_hex, asset_balances) in snapshot.balances {
        let user = UserId::from_hex(&user_hex)?;
        for (asset_str, bal) in asset_balances {
            let asset = AssetId::from_str(&asset_str);
            engine.balances.insert(
                (user.clone(), asset),
                Balance { user: user.clone(), asset, available: bal.available, locked: bal.locked },
            );
        }
    }

    for (user_hex, meta) in snapshot.metadata {
        let user = UserId::from_hex(&user_hex)?;
        engine.metadata.insert(user, meta);
    }

    for (market_id_str, orders) in snapshot.orders {
        let market_id = types::MarketId(market_id_str);
        if let Some(book) = engine.order_books.get_mut(&market_id) {
            for order in orders {
                let _ = book.insert_resting(order);
            }
        }
    }

    engine.set_next_order_id(snapshot.sequence);

    engine.fee_balances = snapshot.fee_balances;

    tracing::info!("Engine restored from snapshot: {}", snapshot.timestamp);

    Ok(())
}

pub fn extract_proofs_from_snapshot(snapshot: &EngineSnapshot) -> HashMap<u64, BatchProof> {
    snapshot.proofs.clone()
}

pub fn extract_attestations_from_snapshot(
    snapshot: &EngineSnapshot,
) -> HashMap<u64, AttestationRecord> {
    snapshot.attestations.clone()
}

pub fn replay_wal_entries(
    engine: &mut MatchingEngine,
    wal_dir: &Path,
    after_sequence: u64,
) -> (Vec<StoredFill>, HashMap<u64, StoredOrder>) {
    let mut segments: Vec<u32> = std::fs::read_dir(wal_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let s = name.to_string_lossy().into_owned();
            s.strip_prefix("engine_wal_")
                .and_then(|s| s.strip_suffix(".log"))
                .and_then(|s| s.parse::<u32>().ok())
        })
        .collect();
    segments.sort_unstable();

    let mut recovered_fills: Vec<StoredFill> = Vec::new();
    let mut recovered_orders: HashMap<u64, StoredOrder> = HashMap::new();
    let mut fill_ids_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut replayed = 0usize;

    for seg_num in segments {
        let seg_path = wal_dir.join(format!("engine_wal_{:04}.log", seg_num));
        let entries = match Wal::read_from(&seg_path, after_sequence) {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("WAL replay: failed to read segment {seg_num}: {err}");
                continue;
            }
        };

        for entry in entries {
            replayed += 1;
            match entry.entry_type {
                wal::ORDER_POST => {
                    if let Ok(post) = entry.decode::<WalOrderPost>() {
                        let order_id = post.order_id;
                        recovered_orders.entry(order_id).or_insert_with(|| {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_micros() as u64;
                            StoredOrder {
                                id: order_id,
                                market_id: post.market_id,
                                user: post.user,
                                side: post.side,
                                price: post.price,
                                quantity: post.quantity,
                                filled_quantity: 0,
                                status: "open".to_string(),
                                order_type: post.order_type,
                                time_in_force: post.time_in_force,
                                nonce: post.nonce,
                                client_order_id: post.client_order_id,
                                signature: String::new(),
                                created_at: now,
                                updated_at: now,
                                fills: vec![],
                                da_hash: None,
                            }
                        });
                    }
                }
                wal::ORDER_PROCESSED => {
                    if let Ok(processed) = entry.decode::<crate::wal::WalOrderProcessed>() {
                        if let Some(order) = recovered_orders.get_mut(&processed.order_id) {
                            order.filled_quantity = processed.filled_quantity;
                            order.status = processed.result.clone();
                        }
                    }
                }
                wal::FILL_CREATED => {
                    if let Ok(fill) = entry.decode::<WalFillCreated>() {
                        if !fill_ids_seen.contains(&fill.fill_id) {
                            fill_ids_seen.insert(fill.fill_id.clone());
                            let fill_id = fill.fill_id.clone();
                            let sf = StoredFill {
                                id: fill.fill_id,
                                market_id: fill.market_id,
                                price: fill.price,
                                quantity: fill.quantity,
                                maker_order_id: fill.maker_order_id,
                                taker_order_id: fill.taker_order_id,
                                maker_address: fill.maker_address.clone(),
                                taker_address: fill.taker_address.clone(),
                                timestamp: entry.timestamp_ns / 1_000_000,
                                side: String::new(),
                                synthetic: false,
                            };
                            if let Some(order) = recovered_orders.get_mut(&fill.taker_order_id) {
                                order.fills.push(OrderFillRecord {
                                    fill_id: fill_id.clone(),
                                    counterparty_order_id: fill.maker_order_id,
                                    counterparty_address: fill.maker_address,
                                    price: fill.price,
                                    quantity: fill.quantity,
                                    timestamp: entry.timestamp_ns / 1_000_000,
                                });
                            }
                            if let Some(order) = recovered_orders.get_mut(&fill.maker_order_id) {
                                order.fills.push(OrderFillRecord {
                                    fill_id,
                                    counterparty_order_id: fill.taker_order_id,
                                    counterparty_address: fill.taker_address,
                                    price: fill.price,
                                    quantity: fill.quantity,
                                    timestamp: entry.timestamp_ns / 1_000_000,
                                });
                            }
                            recovered_fills.push(sf);
                        }
                    }
                }
                wal::DEPOSIT => {
                    if let Ok(dep) = entry.decode::<WalDeposit>() {
                        if let Ok(user) = types::UserId::from_hex(&dep.user) {
                            let asset = types::AssetId::from_str(&dep.asset);
                            let mut hash = [0u8; 32];
                            if let Some(tx) = &dep.tx_hash {
                                let hex_str = tx.strip_prefix("0x").unwrap_or(tx);
                                if let Ok(bytes) = hex::decode(hex_str) {
                                    let len = bytes.len().min(32);
                                    hash[..len].copy_from_slice(&bytes[..len]);
                                }
                            }
                            engine.process(
                                types::Request::Deposit(DepositRequest {
                                    user,
                                    asset,
                                    amount: dep.amount,
                                    l1_tx_hash: hash,
                                }),
                                0,
                            );
                        }
                    }
                }
                wal::WITHDRAWAL_REQUEST => {
                    if let Ok(wr) = entry.decode::<WalWithdrawalRequest>() {
                        if let Ok(user) = types::UserId::from_hex(&wr.user) {
                            let asset = types::AssetId::from_str(&wr.asset);
                            engine.process(
                                types::Request::Withdrawal(WithdrawalRequest {
                                    user,
                                    asset,
                                    amount: wr.amount,
                                    nonce: wr.nonce,
                                    signature: vec![],
                                }),
                                0,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    tracing::info!("WAL replay complete: replayed {replayed} entries, recovered {} orders", recovered_orders.len());
    (recovered_fills, recovered_orders)
}
